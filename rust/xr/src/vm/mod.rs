pub mod disk;
pub mod share;

use std::fs::{self, OpenOptions};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use rustix::process::Signal;
use serde::Serialize;

use crate::cidr::hs_mac;
use crate::config::{config_hash, Config};
use crate::diag::{diag_snap, diag_snap_bg};
use crate::env::Env;
use crate::log::{blog, dlog, log_err, log_info, unix_now};
use crate::net::run_net_cli;
use crate::sys::spawn::spawn_self_detached;
use crate::sys::{ce_ready, is_pid_running, proc_cmdline, send_signal, wakelock_off, wakelock_on};
use crate::vm::share::share_prepare;

pub fn vm_running(env: &Env) -> bool {
    let pid = fs::read_to_string(env.run_dir().join("crosvm.pid")).unwrap_or_default();
    let p = pid.trim();
    if !is_pid_running(env, p) {
        return false;
    }
    proc_cmdline(env, p).map_or(true, |c| c.contains("crosvm"))
}

pub fn vm_fail(env: &Env, log_msg: &str, ui_msg: &str) {
    log_err(env, "vm", log_msg);
    let _ = fs::write(env.run_dir().join("last_error"), format!("{ui_msg}\n"));
    let boot_log = env.boot_log();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(5));
        let _ = fs::remove_file(boot_log);
    });
}

pub fn boot_progress(env: &Env, secs: u64) {
    let sz = fs::metadata(env.console_log()).map_or(0, |m| m.len());
    let kb = sz / 1024;
    blog(
        env,
        "vm",
        &format!("等待 OpenWrt 就绪 {secs}s · 串口已输出 {kb}KB"),
    );
}

pub fn fuse_arm(env: &Env) {
    let line = format!("{} {}\n", unix_now(), config_hash(env));
    let _ = fs::write(env.fuse_file(), line);
    if let Ok(f) = fs::File::open(env.fuse_file()) {
        let _ = f.sync_all();
    }
    thread::sleep(Duration::from_secs(1));
}

pub fn ready_seen(env: &Env) -> bool {
    if let Ok(ctl) = fs::read_to_string(env.ctl_log()) {
        if ctl.lines().any(|l| l.starts_with("XR_READY")) {
            return true;
        }
    }
    if let Ok(con) = fs::read(env.console_log()) {
        let s = String::from_utf8_lossy(&con);
        if s.contains("XR_READY") {
            return true;
        }
    }
    false
}

pub fn ready_channel(env: &Env) -> &'static str {
    if let Ok(ctl) = fs::read_to_string(env.ctl_log()) {
        if ctl.lines().any(|l| l.starts_with("XR_READY")) {
            return "hvc";
        }
    }
    if let Ok(con) = fs::read(env.console_log()) {
        let s = String::from_utf8_lossy(&con);
        if s.contains("XR_READY") {
            return "ttyS0";
        }
    }
    "null"
}

pub fn run_net_hook(env: &Env, op: &str) -> i32 {
    let net_sh = env.moddir.join("scripts/net.sh");
    if net_sh.is_file() {
        let st = Command::new("sh")
            .arg(&net_sh)
            .arg(op)
            .env("MODDIR", &env.moddir)
            .env("XR_DATA", &env.xr_data)
            .status();
        return st.map_or(1, |s| s.code().unwrap_or(1));
    }
    run_net_cli(env, &[op.to_string()])
}

pub fn fuse_watch(env: &Env) {
    let cfg = Config::load(env);
    let ready_to = cfg.ready_timeout_sec();
    let mut t = 0_u64;
    while t < ready_to {
        if !vm_running(env) {
            let _ = fs::remove_file(env.fuse_file());
            vm_fail(env, "就绪前 crosvm 已退出，详见 vmm.log", "启动中退出");
            return;
        }
        if ready_seen(env) {
            break;
        }
        thread::sleep(Duration::from_secs(1));
        t = t.saturating_add(1);
        boot_progress(env, t);
    }
    if !ready_seen(env) {
        let msg = format!("{ready_to} 秒内未就绪，停止 VM");
        let ui = format!("{ready_to} 秒内未就绪");
        vm_fail(env, &msg, &ui);
        let _ = vm_stop_exec(env, true);
        return;
    }

    let run = env.run_dir();
    let _ = fs::write(run.join("state"), "ready\n");
    let _ = fs::remove_file(run.join("last_error"));
    log_info(env, "vm", &format!("XR_READY（{}）", ready_channel(env)));
    blog(env, "net", "重建转发 / NAT / 接管规则");
    let _ = run_net_hook(env, "sync");
    let gw = fs::read_to_string(run.join("net.gw"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "off".to_string());
    blog(env, "net", &format!("网络规则就绪，接管：{gw}"));
    diag_snap_bg(env, "服务就绪");

    let cfg = Config::load(env);
    let stable_sec = cfg.stable_sec();
    let mut st = 0_u64;
    while st < stable_sec {
        if !vm_running(env) {
            let _ = fs::remove_file(env.fuse_file());
            vm_fail(env, "稳定期内 crosvm 退出", "服务意外退出");
            return;
        }
        thread::sleep(Duration::from_secs(5));
        st = st.saturating_add(5);
    }
    let _ = fs::remove_file(env.fuse_file());
    let _ = fs::write(env.last_good_file(), format!("{}\n", config_hash(env)));
    let _ = fs::write(run.join("state"), "stable\n");
    log_info(env, "vm", "稳定，熔断已解除，last_good 已更新");
    let _ = fs::remove_file(env.boot_log());
}

pub fn vpn_sync_host(env: &Env) -> Option<bool> {
    let host_dir = env.run_dir().join("guest/host");
    let _ = fs::create_dir_all(&host_dir);
    let vpn_dir = env.vpn_dir();
    if !vpn_dir.is_dir() {
        return None;
    }
    let host_vpn = host_dir.join("vpn");
    let _ = fs::create_dir_all(&host_vpn);
    if let Ok(rd) = fs::read_dir(&host_vpn) {
        for ent in rd.flatten() {
            let p = ent.path();
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                let ovpn = vpn_dir.join(format!("{stem}.ovpn"));
                let keep = fs::metadata(&ovpn).is_ok_and(|m| m.len() > 0);
                if !keep {
                    let _ = fs::remove_file(p);
                }
            }
        }
    }
    let mut any = false;
    let mut first = None;
    if let Ok(rd) = fs::read_dir(&vpn_dir) {
        let mut paths: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            if p.extension().and_then(|s| s.to_str()) != Some("ovpn") {
                continue;
            }
            if fs::metadata(&p).map_or(true, |m| m.len() == 0) {
                continue;
            }
            let Some(pid) = p.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if first.is_none() {
                first = Some(p.clone());
            }
            let _ = fs::copy(&p, host_vpn.join(format!("{pid}.ovpn")));
            let en = fs::read_to_string(vpn_dir.join(format!("{pid}.enabled")))
                .is_ok_and(|s| s.trim() == "1");
            let _ = fs::write(
                host_vpn.join(format!("{pid}.enabled")),
                if en { "1\n" } else { "0\n" },
            );
            if en {
                any = true;
                let _ = fs::copy(&p, host_dir.join("openvpn.ovpn"));
            }
        }
    }
    if !any {
        if let Some(f) = first {
            let _ = fs::copy(f, host_dir.join("openvpn.ovpn"));
        }
    }
    Some(any)
}

pub fn vpn_boot_prepare(env: &Env) {
    let guest = env.run_dir().join("guest/guest");
    let host = env.run_dir().join("guest/host");
    for f in ["openvpn.ack", "openvpn.multi", "openvpn.connecting"] {
        let _ = fs::remove_file(guest.join(f));
    }
    if env.vpn_dir().is_dir() {
        if let Some(any) = vpn_sync_host(env) {
            if any {
                let _ = fs::write(host.join("openvpn.cmd"), "enable\n");
                blog(env, "vpn", "下发 OpenVPN 配置");
            } else {
                let _ = fs::write(host.join("openvpn.cmd"), "disable\n");
                if let Ok(rd) = fs::read_dir(env.vpn_dir()) {
                    for ent in rd.flatten() {
                        let p = ent.path();
                        if p.extension().and_then(|s| s.to_str()) == Some("ovpn")
                            && fs::metadata(&p).is_ok_and(|m| m.len() > 0)
                        {
                            blog(env, "vpn", "OpenVPN 保持停止");
                            break;
                        }
                    }
                }
            }
        }
        return;
    }
    let legacy = env.xr_data.join("config/openvpn.ovpn");
    if !legacy.is_file() {
        return;
    }
    let _ = fs::create_dir_all(&host);
    let _ = fs::copy(&legacy, host.join("openvpn.ovpn"));
    let en = fs::read_to_string(env.xr_data.join("config/openvpn.enabled"))
        .map_or(true, |s| s.trim() != "0");
    if en {
        let _ = fs::write(host.join("openvpn.cmd"), "enable\n");
        blog(env, "vpn", "下发 OpenVPN 配置");
    } else {
        let _ = fs::write(host.join("openvpn.cmd"), "disable\n");
        blog(env, "vpn", "OpenVPN 保持停止");
    }
}

pub fn vm_start_exec(env: &Env) -> i32 {
    let cfg = Config::load(env);
    if vm_running(env) {
        log_info(env, "vm", "已在运行");
        return 0;
    }
    let run = env.run_dir();
    if !env.boot_log().is_file() {
        let _ = fs::write(env.boot_log(), "");
    }
    for f in ["teardown.last", "shutdown", "shutdown.noted", "stopping"] {
        let _ = fs::remove_file(run.join(f));
    }
    let cpus = cfg.vm_cpus();
    let mem = cfg.vm_mem_mb();
    let disk_rw = cfg.vm_disk_rw();
    let disk_lbl = if disk_rw { "读写" } else { "只读" };
    blog(
        env,
        "vm",
        &format!("准备启动：CPU {cpus} 核 · 内存 {mem}MB · 系统盘 {disk_lbl}"),
    );
    let _ = fs::remove_file(run.join("last_error"));
    if env.tripped_file().is_file() {
        log_err(env, "vm", "熔断未确认（xr ack-trip）");
        return 1;
    }
    let image_path = env.img_dir().join("Image");
    let rootfs = env.rootfs_img();
    if !crate::env::is_executable(&env.crosvm) || !image_path.is_file() || !rootfs.is_file() {
        vm_fail(env, "缺少 crosvm 或镜像", "缺少虚拟化组件或系统镜像");
        return 1;
    }
    if !ce_ready() {
        log_info(env, "vm", "警告：手机未解锁（CE 不可用）");
    }
    let img_mb = fs::metadata(&image_path).map_or(0, |m| m.len() / 1_048_576);
    let rfs_mb = fs::metadata(&rootfs).map_or(0, |m| m.len() / 1_048_576);
    blog(
        env,
        "vm",
        &format!("内核 {img_mb}MB · 系统盘 {rfs_mb}MB · crosvm 就绪"),
    );

    let host_dir = run.join("guest/host");
    let guest_dir = run.join("guest/guest");
    let _ = fs::create_dir_all(&host_dir);
    let _ = fs::create_dir_all(&guest_dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(run.join("guest"), fs::Permissions::from_mode(0o700));
    }
    for f in [
        "host/ipv6.cmd",
        "guest/ipv6",
        "host/diag.cmd",
        "guest/diag.txt",
        "host/hotspot.cmd",
        "guest/hotspot",
        "host/acct.cmd",
        "guest/acct",
        "guest/openvpn.tunnels",
        "guest/openvpn.status",
        "guest/openvpn.addr",
        "guest/openvpn.routes",
    ] {
        let _ = fs::remove_file(run.join("guest").join(f));
    }

    let mut net_args: Vec<String> = Vec::new();
    let vm_net = cfg.vm_net();
    if vm_net {
        blog(
            env,
            "net",
            "创建 xr0 / xr1、登记 netd 本地网络、建立转发规则",
        );
        if run_net_hook(env, "up") != 0 {
            vm_fail(env, "net up 失败", "网络初始化失败");
            return 1;
        }
        let up_if = fs::read_to_string(run.join("net.up_if"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "无".to_string());
        let mode = fs::read_to_string(run.join("net.mode"))
            .unwrap_or_default()
            .trim()
            .to_string();
        blog(
            env,
            "net",
            &format!("手机侧网络就绪：上游 {up_if} · 模式 {mode}"),
        );
        net_args.push("--net".to_string());
        net_args.push(format!("tap-name=xr0,mac={}", cfg.vm_mac()));
        if env.sysnet.join("xr1").is_dir() {
            let mac1 = hs_mac(cfg.vm_mac()).unwrap_or_else(|| "52:54:00:1d:e9:ff".to_string());
            net_args.push("--net".to_string());
            net_args.push(format!("tap-name=xr1,mac={mac1}"));
        }
    }

    let (blk, rootmode) = if disk_rw {
        (format!("{},lock=false,root=true", rootfs.display()), "rw")
    } else {
        (
            format!("{},lock=false,root=true,ro=true", rootfs.display()),
            "ro",
        )
    };

    let share = share_prepare(env, &cfg);
    blog(
        env,
        "vm",
        &format!("共享目录：{} · 控制通道 virtiofs", share.mode),
    );
    let ctl_spec = format!(
        "{}:xrctl:type=fs:cache=auto:timeout=1",
        run.join("guest").display()
    );
    vpn_boot_prepare(env);

    let boot_ts = unix_now();
    let _ = fs::write(host_dir.join("time"), format!("{boot_ts}\n"));
    let dns_csv = cfg.dns().split_whitespace().collect::<Vec<_>>().join(",");
    let params = format!(
        "root=/dev/vda {rootmode} rootwait console=ttyS0,115200 earlycon panic=5 xr.ip={}.254/24 xr.gw={}.1 xr.dns={dns_csv} xr.ip6={}::254/64 xr.gw6={}::1 xr.share={} xr.time={boot_ts}",
        cfg.net_prefix(),
        cfg.net_prefix(),
        crate::env::XR_NET6,
        crate::env::XR_NET6,
        share.mode
    );

    let _ = fs::write(env.console_log(), "");
    let _ = fs::write(env.ctl_log(), "");
    let sock = run.join("crosvm.sock");
    let _ = fs::remove_file(&sock);
    let _ = fs::remove_file(run.join("state"));

    let swiotlb = cfg.vm_swiotlb_mb();
    dlog(
        env,
        &cfg,
        "vm",
        &format!(
            "启动参数：cpus={cpus} mem={mem} swiotlb={swiotlb} net={} disk={rootmode} share={} params={params}",
            if net_args.is_empty() {
                "无".to_string()
            } else {
                net_args.join(" ")
            },
            share.mode
        ),
    );
    fuse_arm(env);
    blog(env, "vm", "熔断已布防，启动 crosvm（gunyah）");
    if cfg.vm_wakelock() {
        wakelock_on();
    }

    let mut tramp_args: Vec<String> = vec!["internal".to_string(), "exec-crosvm".to_string()];
    tramp_args.extend(share.wrap);
    tramp_args.push(env.crosvm.to_string_lossy().into_owned());
    tramp_args.extend([
        "--log-level=info".to_string(),
        "run".to_string(),
        "--hypervisor".to_string(),
        "gunyah".to_string(),
        "--protected-vm-without-firmware".to_string(),
        "--disable-sandbox".to_string(),
        "--no-balloon".to_string(),
        "--swiotlb".to_string(),
        swiotlb.to_string(),
        "--cpus".to_string(),
        cpus.to_string(),
        "--mem".to_string(),
        mem.to_string(),
    ]);
    tramp_args.extend(net_args);
    tramp_args.extend([
        "--block".to_string(),
        blk,
        "--serial".to_string(),
        format!(
            "type=file,path={},hardware=serial,num=1,console=true,earlycon=true",
            env.console_log().display()
        ),
        "--serial".to_string(),
        format!(
            "type=file,path={},hardware=virtio-console,num=1",
            env.ctl_log().display()
        ),
        "--socket".to_string(),
        sock.to_string_lossy().into_owned(),
        "--shared-dir".to_string(),
        ctl_spec,
    ]);
    if !share.spec.is_empty() {
        tramp_args.push("--shared-dir".to_string());
        tramp_args.push(share.spec);
    }
    tramp_args.extend([
        "--params".to_string(),
        params,
        image_path.to_string_lossy().into_owned(),
    ]);

    let vmm_log = env.vmm_log();
    if let Some(p) = vmm_log.parent() {
        let _ = fs::create_dir_all(p);
    }
    let out_f = OpenOptions::new().create(true).append(true).open(&vmm_log);
    let child = match out_f {
        Ok(f) => {
            let out_std = Stdio::from(f);
            let err_std = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&vmm_log)
                .map_or_else(|_| Stdio::null(), Stdio::from);
            Command::new(env.self_exe())
                .args(&tramp_args)
                .stdin(Stdio::null())
                .stdout(out_std)
                .stderr(err_std)
                .spawn()
        }
        Err(_) => Command::new(env.self_exe())
            .args(&tramp_args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn(),
    };

    let Ok(ch) = child else {
        vm_fail(env, "无法拉起 crosvm", "无法启动");
        let _ = fs::remove_file(env.fuse_file());
        wakelock_off();
        if vm_net {
            let _ = run_net_hook(env, "down");
        }
        return 1;
    };
    let pid_str = ch.id().to_string();
    let _ = fs::write(run.join("crosvm.pid"), format!("{pid_str}\n"));
    blog(env, "vm", &format!("crosvm pid {pid_str}，等待内核输出"));
    thread::sleep(Duration::from_secs(2));
    if !vm_running(env) {
        vm_fail(env, "crosvm 立即退出，详见 vmm.log", "无法启动");
        let _ = fs::remove_file(env.fuse_file());
        let _ = fs::remove_file(run.join("crosvm.pid"));
        wakelock_off();
        if vm_net {
            let _ = run_net_hook(env, "down");
        }
        return 1;
    }

    if let Ok(fw) = spawn_self_detached(env, &["internal", "fuse-watch"]) {
        let _ = fs::write(run.join("fuse_watch.pid"), format!("{}\n", fw.id()));
    }
    let _ = fs::write(run.join("share.mode"), format!("{}\n", share.mode));
    crate::daemon::keepalive::cgroup_escape_tree(env, std::process::id());
    log_info(
        env,
        "vm",
        &format!(
            "已启动 pid={pid_str} mem={mem}MB cpus={cpus} share={}",
            share.mode
        ),
    );
    0
}

pub fn vm_stop_exec(env: &Env, in_watch: bool) -> i32 {
    let run = env.run_dir();
    let _ = fs::write(run.join("stopping"), format!("{}\n", std::process::id()));
    let _ = fs::remove_file(env.fuse_file());
    if !in_watch {
        if let Ok(wp) = fs::read_to_string(run.join("fuse_watch.pid")) {
            let wp = wp.trim();
            if !wp.is_empty() {
                let _ = send_signal(wp, Signal::TERM);
            }
        }
    }
    let pid = fs::read_to_string(run.join("crosvm.pid"))
        .unwrap_or_default()
        .trim()
        .to_string();
    diag_snap(env, "停止前", 2);
    if vm_running(env) {
        let sock = run.join("crosvm.sock");
        let _ = Command::new(&env.crosvm)
            .args(["stop", &sock.to_string_lossy()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        for _ in 0..10 {
            if !vm_running(env) {
                break;
            }
            thread::sleep(Duration::from_secs(1));
        }
        if vm_running(env) && !pid.is_empty() {
            let _ = send_signal(&pid, Signal::TERM);
            thread::sleep(Duration::from_secs(5));
        }
        if vm_running(env) && !pid.is_empty() {
            let _ = send_signal(&pid, Signal::KILL);
        }
    }
    for p in [
        run.join("crosvm.pid"),
        run.join("crosvm.sock"),
        run.join("state"),
        run.join("fuse_watch.pid"),
        run.join("share.mode"),
        env.fuse_file(),
        env.boot_log(),
    ] {
        let _ = fs::remove_file(p);
    }
    let _ = run_net_hook(env, "down");
    wakelock_off();
    let _ = fs::remove_file(run.join("stopping"));
    log_info(env, "vm", "已停止");
    0
}

#[derive(Serialize)]
pub struct VmStatusJson {
    pub running: bool,
    pub pid: Option<u64>,
    pub state: String,
    pub uptime_s: u64,
    pub tripped: Option<String>,
    pub last_good_match: bool,
    pub ready_channel: Option<String>,
    pub error: String,
}

pub fn vm_status(env: &Env) -> VmStatusJson {
    let run = env.run_dir();
    let running = vm_running(env);
    let mut pid_num = None;
    let mut uptime_s = 0_u64;
    if running {
        let pid_s = fs::read_to_string(run.join("crosvm.pid"))
            .unwrap_or_default()
            .trim()
            .to_string();
        if let Ok(p) = pid_s.parse::<u64>() {
            pid_num = Some(p);
        }
        if let Ok(stat) = fs::read_to_string(env.proc_base.join(&pid_s).join("stat")) {
            if let Some(st) = stat
                .split_whitespace()
                .nth(21)
                .and_then(|s| s.parse::<u64>().ok())
            {
                if let Ok(up_raw) = fs::read_to_string("/proc/uptime") {
                    if let Some(now) = up_raw
                        .split('.')
                        .next()
                        .and_then(|s| s.trim().parse::<u64>().ok())
                    {
                        uptime_s = now.saturating_sub(st / 100);
                    }
                }
            }
        }
    }
    let state = if running {
        let s = fs::read_to_string(run.join("state")).unwrap_or_default();
        let t = s.trim();
        if t.is_empty() {
            "booting".to_string()
        } else {
            t.to_string()
        }
    } else {
        "stopped".to_string()
    };
    let tripped = fs::read_to_string(env.tripped_file())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|_| env.tripped_file().is_file());
    let lg = fs::read_to_string(env.last_good_file()).is_ok_and(|s| s.trim() == config_hash(env));
    let ch = match ready_channel(env) {
        "null" => None,
        other => Some(other.to_string()),
    };
    let error = fs::read_to_string(run.join("last_error"))
        .unwrap_or_default()
        .lines()
        .next()
        .unwrap_or("")
        .replace(['"', '\\'], "");

    VmStatusJson {
        running,
        pid: pid_num,
        state,
        uptime_s,
        tripped,
        last_good_match: lg,
        ready_channel: ch,
        error,
    }
}

pub fn vm_status_json(env: &Env) -> String {
    serde_json::to_string(&vm_status(env))
        .unwrap_or_else(|_| "{\"running\":false,\"state\":\"stopped\"}".to_string())
}
