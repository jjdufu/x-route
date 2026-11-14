pub mod disk;
pub mod share;

use std::fs::{self, OpenOptions};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use rustix::process::Signal;
use serde::Serialize;

use crate::cidr::hs_mac;
use crate::config;
use crate::config::Config;
use crate::diag;
use crate::env::Env;
use crate::log;
use crate::log::FAILURES;
use crate::log::note_failure;
use crate::log::note_rc;
use crate::log::{blog, dlog, unix_now};
use crate::net::run_net_cli;
use crate::sys::exec::note_status;
use crate::sys::fsx;
use crate::sys::send_signal_logged;
use crate::sys::spawn;
use crate::sys::{ce_ready, is_pid_running, proc_cmdline, wakelock_off, wakelock_on};

#[must_use]
pub fn running(env: &Env) -> bool {
    let pid = fs::read_to_string(env.run_dir().join("crosvm.pid")).unwrap_or_default();
    let trimmed = pid.trim();
    if !is_pid_running(env, trimmed) {
        return false;
    }
    proc_cmdline(env, trimmed).is_none_or(|cmdline| cmdline.contains("crosvm"))
}

pub fn fail(env: &Env, log_msg: &str, ui_msg: &str) {
    log::err(env, "vm", log_msg);
    fsx::write_logged(env, env.run_dir().join("last_error"), format!("{ui_msg}\n"));
    let boot_log = env.boot_log();
    let env_t = env.clone();
    // 分离线程：显式丢弃句柄即 detach，与原来不 join 的语义相同
    drop(thread::spawn(move || {
        thread::sleep(Duration::from_secs(5));
        fsx::remove_logged(&env_t, boot_log);
    }));
}

pub fn boot_progress(env: &Env, secs: u64) {
    let sz = fs::metadata(env.console_log()).map_or(0, |meta| meta.len());
    let kb = sz / 1024;
    blog(
        env,
        "vm",
        &format!("等待 OpenWrt 就绪 {secs}s · 串口已输出 {kb}KB"),
    );
}

/// 在后台线程里回收子进程（避免僵尸）；退出状态无人需要，等待本身失败时计数。
pub(crate) fn reap_in_background(mut child: std::process::Child) {
    // 分离线程：显式丢弃句柄即 detach，与原来不 join 的语义相同
    drop(thread::spawn(move || {
        if child.wait().is_err() {
            note_failure(&FAILURES.exec);
        }
    }));
}

pub fn fuse_arm(env: &Env) {
    let line = format!("{} {}\n", unix_now(), config::hash(env));
    fsx::write_logged(env, env.fuse_file(), line);
    if let Ok(fuse) = fs::File::open(env.fuse_file())
        && let Err(err) = fuse.sync_all()
    {
        fsx::record_failure(env, "sync", &env.fuse_file(), &err);
    }
    thread::sleep(Duration::from_secs(1));
}

#[must_use]
pub fn ready_seen(env: &Env) -> bool {
    if let Ok(ctl) = fs::read_to_string(env.ctl_log())
        && ctl.lines().any(|line| line.starts_with("XR_READY"))
    {
        return true;
    }
    if let Ok(con) = fs::read(env.console_log()) {
        let text = String::from_utf8_lossy(&con);
        if text.contains("XR_READY") {
            return true;
        }
    }
    false
}

#[must_use]
pub fn ready_channel(env: &Env) -> &'static str {
    if let Ok(ctl) = fs::read_to_string(env.ctl_log())
        && ctl.lines().any(|line| line.starts_with("XR_READY"))
    {
        return "hvc";
    }
    if let Ok(con) = fs::read(env.console_log()) {
        let text = String::from_utf8_lossy(&con);
        if text.contains("XR_READY") {
            return "ttyS0";
        }
    }
    "null"
}

#[must_use]
pub fn run_net_hook(env: &Env, op: &str) -> i32 {
    let net_sh = env.moddir.join("scripts/net.sh");
    if net_sh.is_file() {
        let st = Command::new("sh")
            .arg(&net_sh)
            .arg(op)
            .env("MODDIR", &env.moddir)
            .env("XR_DATA", &env.xr_data)
            .status();
        return st.map_or(1, |exit_status| exit_status.code().unwrap_or(1_i32));
    }
    run_net_cli(env, &[op.to_owned()])
}

#[must_use]
pub fn mono_now(env: &Env) -> Option<u64> {
    let custom = env.proc_base.join("uptime");
    let raw = if custom.is_file() {
        fs::read_to_string(custom).ok()?
    } else {
        fs::read_to_string("/proc/uptime").ok()?
    };
    raw.split('.')
        .next()
        .and_then(|text| text.trim().parse::<u64>().ok())
}

pub fn fuse_watch(env: &Env) {
    let cfg = Config::load(env);
    let ready_to = cfg.ready_timeout_sec();
    let mut waited = 0_u64;
    while waited < ready_to {
        if !running(env) {
            fsx::remove_logged(env, env.fuse_file());
            fail(env, "就绪前 crosvm 已退出，详见 vmm.log", "启动中退出");
            return;
        }
        if ready_seen(env) {
            break;
        }
        thread::sleep(Duration::from_secs(1));
        waited = waited.saturating_add(1);
        boot_progress(env, waited);
    }
    if !ready_seen(env) {
        let msg = format!("{ready_to} 秒内未就绪，停止 VM");
        let ui = format!("{ready_to} 秒内未就绪");
        fail(env, &msg, &ui);
        note_rc(env, "vm_stop_exec", stop_exec(env, true));
        return;
    }

    let run = env.run_dir();
    if let Some(now_m) = mono_now(env) {
        fsx::write_logged(env, run.join("ready_at"), format!("{now_m}\n"));
    }
    fsx::write_logged(env, run.join("state"), "ready\n");
    fsx::remove_logged(env, run.join("last_error"));
    log::info(env, "vm", &format!("XR_READY（{}）", ready_channel(env)));
    blog(env, "net", "重建转发 / NAT / 接管规则");
    note_rc(env, "run_net_hook", run_net_hook(env, "sync"));
    if let Some(now_m) = mono_now(env) {
        fsx::write_logged(env, run.join("ready_at"), format!("{now_m}\n"));
    }
    let gw = fs::read_to_string(run.join("net.gw"))
        .ok()
        .map(|raw| raw.trim().to_owned())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "off".to_owned());
    blog(env, "net", &format!("网络规则就绪，接管：{gw}"));
    diag::snap_bg(env, "服务就绪");

    let cfg_now = Config::load(env);
    let stable_sec = cfg_now.stable_sec();
    let mut st = 0_u64;
    while st < stable_sec {
        if !running(env) {
            if crate::daemon::keepalive::vm_guest_reset_pending(env) {
                let dpid = crate::daemon::pid(env);
                if is_pid_running(env, &dpid) {
                    send_signal_logged(env, &dpid, Signal::USR2);
                    return;
                }
            }
            fsx::remove_logged(env, env.fuse_file());
            fail(env, "稳定期内 crosvm 退出", "服务意外退出");
            return;
        }
        thread::sleep(Duration::from_secs(5));
        st = st.saturating_add(5);
    }
    fsx::remove_logged(env, env.fuse_file());
    fsx::write_logged(
        env,
        env.last_good_file(),
        format!("{}\n", config::hash(env)),
    );
    fsx::write_logged(env, run.join("state"), "stable\n");
    log::info(env, "vm", "稳定，熔断已解除，last_good 已更新");
    fsx::remove_logged(env, env.boot_log());
}

#[must_use]
pub fn vpn_sync_host(env: &Env) -> Option<bool> {
    let host_dir = env.run_dir().join("guest/host");
    fsx::mkdirs_logged(env, &host_dir);
    let vpn_dir = env.vpn_dir();
    if !vpn_dir.is_dir() {
        return None;
    }
    let host_vpn = host_dir.join("vpn");
    fsx::mkdirs_logged(env, &host_vpn);
    if let Ok(rd) = fs::read_dir(&host_vpn) {
        for ent in rd.flatten() {
            let target = ent.path();
            if let Some(stem) = target.file_stem().and_then(|stem_os| stem_os.to_str()) {
                let ovpn = vpn_dir.join(format!("{stem}.ovpn"));
                let keep = fs::metadata(&ovpn).is_ok_and(|ovpn_meta| ovpn_meta.len() > 0);
                if !keep {
                    fsx::remove_logged(env, target);
                }
            }
        }
    }
    let mut any = false;
    let mut first = None;
    if let Ok(rd) = fs::read_dir(&vpn_dir) {
        let mut paths: Vec<_> = rd.flatten().map(|entry| entry.path()).collect();
        paths.sort();
        for file_path in paths {
            if file_path.extension().and_then(|raw_name| raw_name.to_str()) != Some("ovpn") {
                continue;
            }
            if fs::metadata(&file_path).map_or(true, |meta| meta.len() == 0) {
                continue;
            }
            let Some(pid) = file_path.file_stem().and_then(|os_str| os_str.to_str()) else {
                continue;
            };
            if first.is_none() {
                first = Some(file_path.clone());
            }
            fsx::copy_logged(env, &file_path, host_vpn.join(format!("{pid}.ovpn")));
            let en = fs::read_to_string(vpn_dir.join(format!("{pid}.enabled")))
                .is_ok_and(|text| text.trim() == "1");
            fsx::write_logged(
                env,
                host_vpn.join(format!("{pid}.enabled")),
                if en { "1\n" } else { "0\n" },
            );
            if en {
                any = true;
                fsx::copy_logged(env, &file_path, host_dir.join("openvpn.ovpn"));
            }
        }
    }
    if !any && let Some(first_file) = first {
        fsx::copy_logged(env, first_file, host_dir.join("openvpn.ovpn"));
    }
    Some(any)
}

pub fn vpn_boot_prepare(env: &Env) {
    let guest = env.run_dir().join("guest/guest");
    let host = env.run_dir().join("guest/host");
    for name in ["openvpn.ack", "openvpn.multi", "openvpn.connecting"] {
        fsx::remove_logged(env, guest.join(name));
    }
    if env.vpn_dir().is_dir() {
        if let Some(any) = vpn_sync_host(env) {
            if any {
                fsx::write_logged(env, host.join("openvpn.cmd"), "enable\n");
                blog(env, "vpn", "下发 OpenVPN 配置");
            } else {
                fsx::write_logged(env, host.join("openvpn.cmd"), "disable\n");
                if let Ok(rd) = fs::read_dir(env.vpn_dir()) {
                    for ent in rd.flatten() {
                        let file_path = ent.path();
                        if file_path.extension().and_then(|os_str| os_str.to_str()) == Some("ovpn")
                            && fs::metadata(&file_path).is_ok_and(|meta| meta.len() > 0)
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
    fsx::mkdirs_logged(env, &host);
    fsx::copy_logged(env, &legacy, host.join("openvpn.ovpn"));
    let en = fs::read_to_string(env.xr_data.join("config/openvpn.enabled"))
        .map_or(true, |text| text.trim() != "0");
    if en {
        fsx::write_logged(env, host.join("openvpn.cmd"), "enable\n");
        blog(env, "vpn", "下发 OpenVPN 配置");
    } else {
        fsx::write_logged(env, host.join("openvpn.cmd"), "disable\n");
        blog(env, "vpn", "OpenVPN 保持停止");
    }
}

#[must_use]
pub fn start_exec(env: &Env) -> i32 {
    let cfg = Config::load(env);
    if running(env) {
        log::info(env, "vm", "已在运行");
        return 0;
    }
    let run = env.run_dir();
    if !env.boot_log().is_file() {
        fsx::write_logged(env, env.boot_log(), "");
    }
    for marker in ["teardown.last", "shutdown", "shutdown.noted", "stopping"] {
        fsx::remove_logged(env, run.join(marker));
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
    fsx::remove_logged(env, run.join("last_error"));
    if env.tripped_file().is_file() {
        log::err(env, "vm", "熔断未确认（xr ack-trip）");
        return 1;
    }
    let image_path = env.img_dir().join("Image");
    let rootfs = env.rootfs_img();
    if !crate::env::is_executable(&env.crosvm) || !image_path.is_file() || !rootfs.is_file() {
        fail(env, "缺少 crosvm 或镜像", "缺少虚拟化组件或系统镜像");
        return 1;
    }
    if !ce_ready() {
        log::info(env, "vm", "警告：手机未解锁（CE 不可用）");
    }
    let img_mb = fs::metadata(&image_path).map_or(0, |img_meta| img_meta.len() / 1_048_576);
    let rfs_mb = fs::metadata(&rootfs).map_or(0, |meta| meta.len() / 1_048_576);
    blog(
        env,
        "vm",
        &format!("内核 {img_mb}MB · 系统盘 {rfs_mb}MB · crosvm 就绪"),
    );

    let host_dir = run.join("guest/host");
    let guest_dir = run.join("guest/guest");
    fsx::mkdirs_logged(env, &host_dir);
    fsx::mkdirs_logged(env, &guest_dir);
    fsx::chmod_logged(env, run.join("guest"), 0o700);
    for file_name in [
        "host/ipv6.cmd",
        "guest/ipv6",
        "host/diag.cmd",
        "guest/diag.txt",
        "host/hotspot.cmd",
        "guest/hotspot",
        "host/acct.cmd",
        "guest/acct",
        "guest/openclash.status",
        "guest/openvpn.tunnels",
        "guest/openvpn.status",
        "guest/openvpn.addr",
        "guest/openvpn.routes",
    ] {
        fsx::remove_logged(env, run.join("guest").join(file_name));
    }

    let mut net_args: Vec<String> = Vec::new();
    let vm_net = cfg.vm_net();
    if vm_net {
        blog(
            env,
            "net",
            "创建 xr0 / xr1、登记 netd 本地网络、建立转发规则",
        );
        if run_net_hook(env, "up") != 0_i32 {
            fail(env, "net up 失败", "网络初始化失败");
            return 1;
        }
        let up_if = fs::read_to_string(run.join("net.up_if"))
            .ok()
            .map(|raw| raw.trim().to_owned())
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "无".to_owned());
        let mode = fs::read_to_string(run.join("net.mode"))
            .unwrap_or_default()
            .trim()
            .to_owned();
        blog(
            env,
            "net",
            &format!("手机侧网络就绪：上游 {up_if} · 模式 {mode}"),
        );
        net_args.push("--net".to_owned());
        net_args.push(format!("tap-name=xr0,mac={}", cfg.vm_mac()));
        if env.sysnet.join("xr1").is_dir() {
            let mac1 = hs_mac(cfg.vm_mac()).unwrap_or_else(|| "52:54:00:1d:e9:ff".to_owned());
            net_args.push("--net".to_owned());
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

    let share = share::prepare(env, &cfg);
    blog(
        env,
        "vm",
        &format!("共享目录：{} · 控制通道 virtiofs", share.mode),
    );
    let ctl_spec = format!(
        "{}:xrctl:type=fs:cache=auto:timeout=0",
        run.join("guest").display()
    );
    vpn_boot_prepare(env);

    let boot_ts = unix_now();
    fsx::write_logged(env, host_dir.join("time"), format!("{boot_ts}\n"));
    let dns_csv = cfg.dns().split_whitespace().collect::<Vec<_>>().join(",");
    let params = format!(
        "root=/dev/vda {rootmode} rootwait console=ttyS0,115200 earlycon panic=5 xr.ip={}.254/24 xr.gw={}.1 xr.dns={dns_csv} xr.ip6={}::254/64 xr.gw6={}::1 xr.share={} xr.time={boot_ts}",
        cfg.net_prefix(),
        cfg.net_prefix(),
        crate::env::XR_NET6,
        crate::env::XR_NET6,
        share.mode
    );

    fsx::write_logged(env, env.console_log(), "");
    fsx::write_logged(env, env.ctl_log(), "");
    let sock = run.join("crosvm.sock");
    fsx::remove_logged(env, &sock);
    fsx::remove_logged(env, run.join("state"));
    fsx::remove_logged(env, run.join("ready_at"));

    let swiotlb = cfg.vm_swiotlb_mb();
    dlog(
        env,
        &cfg,
        "vm",
        &format!(
            "启动参数：cpus={cpus} mem={mem} swiotlb={swiotlb} net={} disk={rootmode} share={} params={params}",
            if net_args.is_empty() {
                "无".to_owned()
            } else {
                net_args.join(" ")
            },
            share.mode
        ),
    );
    fuse_arm(env);
    crate::clash::start_clash_mux(env);
    blog(env, "vm", "熔断已布防，启动 crosvm（gunyah）");
    if cfg.vm_wakelock() {
        wakelock_on(env);
    }

    let mut tramp_args: Vec<String> = vec!["internal".to_owned(), "exec-crosvm".to_owned()];
    tramp_args.extend(share.wrap);
    tramp_args.push(env.crosvm.to_string_lossy().into_owned());
    tramp_args.extend([
        "--log-level=info".to_owned(),
        "run".to_owned(),
        "--hypervisor".to_owned(),
        "gunyah".to_owned(),
        "--protected-vm-without-firmware".to_owned(),
        "--disable-sandbox".to_owned(),
        "--no-balloon".to_owned(),
        "--swiotlb".to_owned(),
        swiotlb.to_string(),
        "--cpus".to_owned(),
        cpus.to_string(),
        "--mem".to_owned(),
        mem.to_string(),
    ]);
    tramp_args.extend(net_args);
    tramp_args.extend([
        "--block".to_owned(),
        blk,
        "--serial".to_owned(),
        format!(
            "type=file,path={},hardware=serial,num=1,console=true,earlycon=true",
            env.console_log().display()
        ),
        "--serial".to_owned(),
        format!(
            "type=file,path={},hardware=virtio-console,num=1",
            env.ctl_log().display()
        ),
        "--serial".to_owned(),
        format!(
            "type=unix-stream,hardware=virtio-console,num=2,path={},input-unix-stream",
            run.join("clash-ctl.sock").display()
        ),
        "--socket".to_owned(),
        sock.to_string_lossy().into_owned(),
        "--shared-dir".to_owned(),
        ctl_spec,
    ]);
    if !share.spec.is_empty() {
        tramp_args.push("--shared-dir".to_owned());
        tramp_args.push(share.spec);
    }
    tramp_args.extend([
        "--params".to_owned(),
        params,
        image_path.to_string_lossy().into_owned(),
    ]);

    let vmm_log = env.vmm_log();
    if let Some(file_path) = vmm_log.parent() {
        fsx::mkdirs_logged(env, file_path);
    }
    let out_f = OpenOptions::new().create(true).append(true).open(&vmm_log);
    if let Ok(mut mf) = OpenOptions::new().create(true).append(true).open(&vmm_log)
        && let Err(err) = std::io::Write::write_all(
            &mut mf,
            format!(
                "{}ts={boot_ts} ===\n",
                crate::daemon::keepalive::VMM_RUN_MARKER
            )
            .as_bytes(),
        )
    {
        fsx::record_failure(env, "write", &vmm_log, &err);
    }
    let child = out_f.map_or_else(
        |_| {
            Command::new(env.self_exe())
                .args(&tramp_args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        },
        |log_file| {
            let out_std = Stdio::from(log_file);
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
        },
    );

    let Ok(mut ch) = child else {
        fail(env, "无法拉起 crosvm", "无法启动");
        fsx::remove_logged(env, env.fuse_file());
        crate::clash::stop_clash_mux(env);
        wakelock_off(env);
        if vm_net {
            note_rc(env, "run_net_hook", run_net_hook(env, "down"));
        }
        return 1;
    };
    let pid_str = ch.id().to_string();
    fsx::write_logged(env, run.join("crosvm.pid"), format!("{pid_str}\n"));
    let notify_daemon = env.role.is_daemon().then(|| std::process::id().to_string());
    let env_t = env.clone();
    // 分离线程：显式丢弃句柄即 detach，与原来不 join 的语义相同
    drop(thread::spawn(move || {
        if let Err(err) = ch.wait() {
            fsx::record_failure(&env_t, "wait crosvm", &env_t.crosvm, &err);
        }
        if let Some(dp) = notify_daemon {
            send_signal_logged(&env_t, &dp, Signal::USR2);
        }
    }));
    blog(env, "vm", &format!("crosvm pid {pid_str}，等待内核输出"));
    thread::sleep(Duration::from_secs(2));
    if !running(env) {
        fail(env, "crosvm 立即退出，详见 vmm.log", "无法启动");
        fsx::remove_logged(env, env.fuse_file());
        fsx::remove_logged(env, run.join("crosvm.pid"));
        crate::clash::stop_clash_mux(env);
        wakelock_off(env);
        if vm_net {
            note_rc(env, "run_net_hook", run_net_hook(env, "down"));
        }
        return 1;
    }

    if let Ok(fw) = spawn::self_detached(env, &["internal", "fuse-watch"]) {
        fsx::write_logged(env, run.join("fuse_watch.pid"), format!("{}\n", fw.id()));
        if env.role.is_daemon() {
            reap_in_background(fw);
        }
    }
    fsx::write_logged(env, run.join("share.mode"), format!("{}\n", share.mode));
    crate::daemon::keepalive::cgroup_escape_tree(env, std::process::id());
    log::info(
        env,
        "vm",
        &format!(
            "已启动 pid={pid_str} mem={mem}MB cpus={cpus} share={}",
            share.mode
        ),
    );
    0
}

#[must_use]
pub fn stop_exec(env: &Env, in_watch: bool) -> i32 {
    let run = env.run_dir();
    fsx::write_logged(
        env,
        run.join("stopping"),
        format!("{}\n", std::process::id()),
    );
    fsx::remove_logged(env, env.fuse_file());
    if !in_watch && let Ok(wp) = fs::read_to_string(run.join("fuse_watch.pid")) {
        let wp_trimmed = wp.trim();
        if !wp_trimmed.is_empty() {
            send_signal_logged(env, wp_trimmed, Signal::TERM);
        }
    }
    let pid = fs::read_to_string(run.join("crosvm.pid"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    diag::snap(env, "停止前", 2);
    if running(env) {
        let sock = run.join("crosvm.sock");
        let stop_status = Command::new(&env.crosvm)
            .args(["stop", &sock.to_string_lossy()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        note_status(env, "crosvm stop", stop_status);
        for _ in 0_i32..10_i32 {
            if !running(env) {
                break;
            }
            thread::sleep(Duration::from_secs(1));
        }
        if running(env) && !pid.is_empty() {
            send_signal_logged(env, &pid, Signal::TERM);
            thread::sleep(Duration::from_secs(5));
        }
        if running(env) && !pid.is_empty() {
            send_signal_logged(env, &pid, Signal::KILL);
        }
    }
    crate::clash::stop_clash_mux(env);
    for path in [
        run.join("crosvm.pid"),
        run.join("crosvm.sock"),
        run.join("state"),
        run.join("ready_at"),
        run.join("fuse_watch.pid"),
        run.join("share.mode"),
        run.join("guest/guest/openclash.status"),
        env.fuse_file(),
        env.boot_log(),
    ] {
        fsx::remove_logged(env, path);
    }
    note_rc(env, "run_net_hook", run_net_hook(env, "down"));
    wakelock_off(env);
    fsx::remove_logged(env, run.join("stopping"));
    log::info(env, "vm", "已停止");
    0
}

#[derive(Serialize)]
pub struct StatusJson {
    pub running: bool,
    pub pid: Option<u64>,
    pub state: String,
    pub uptime_s: u64,
    pub tripped: Option<String>,
    pub last_good_match: bool,
    pub ready_channel: Option<String>,
    pub error: String,
}

#[must_use]
pub fn status(env: &Env) -> StatusJson {
    let run = env.run_dir();
    let running = running(env);
    let mut pid_num = None;
    let mut uptime_s = 0_u64;
    let state = if running {
        let state_text = fs::read_to_string(run.join("state")).unwrap_or_default();
        let trimmed = state_text.trim();
        if trimmed.is_empty() {
            "booting".to_owned()
        } else {
            trimmed.to_owned()
        }
    } else {
        "stopped".to_owned()
    };
    if running {
        let pid_s = fs::read_to_string(run.join("crosvm.pid"))
            .unwrap_or_default()
            .trim()
            .to_owned();
        if let Ok(pid_text) = pid_s.parse::<u64>() {
            pid_num = Some(pid_text);
        }
        if matches!(state.as_str(), "ready" | "stable") {
            let ready_at = fs::read_to_string(run.join("ready_at"))
                .ok()
                .and_then(|uptime_text| uptime_text.trim().parse::<u64>().ok());
            if let (Some(r0), Some(now)) = (ready_at, mono_now(env)) {
                uptime_s = now.saturating_sub(r0);
            } else if let Ok(stat) = fs::read_to_string(env.proc_base.join(&pid_s).join("stat"))
                && let (Some(st), Some(now)) = (
                    stat.split_whitespace()
                        .nth(21)
                        .and_then(|content| content.parse::<u64>().ok()),
                    mono_now(env),
                )
            {
                uptime_s = now.saturating_sub(st / 100);
            }
        }
    }
    let tripped = fs::read_to_string(env.tripped_file())
        .ok()
        .map(|raw| raw.trim().to_owned())
        .filter(|_| env.tripped_file().is_file());
    let lg =
        fs::read_to_string(env.last_good_file()).is_ok_and(|text| text.trim() == config::hash(env));
    let ch = match ready_channel(env) {
        "null" => None,
        other => Some(other.to_owned()),
    };
    let error = fs::read_to_string(run.join("last_error"))
        .unwrap_or_default()
        .lines()
        .next()
        .unwrap_or("")
        .replace(['"', '\\'], "");

    StatusJson {
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

#[must_use]
pub fn status_json(env: &Env) -> String {
    serde_json::to_string(&status(env))
        .unwrap_or_else(|_| "{\"running\":false,\"state\":\"stopped\"}".to_owned())
}
