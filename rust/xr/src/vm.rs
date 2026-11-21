//! 虚拟机生命周期：`crosvm` 的启动与停止、就绪与稳定判定（启动保险丝）、`VPN` 配置下发与 `vm status` 状态。

pub(crate) mod disk;
pub(crate) mod share;

use std::fs::{self, OpenOptions};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
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

/// `crosvm` 在运行时返回 true：`run/crosvm.pid` 的进程存在，且命令行（能读到时）含有 `crosvm`。
#[must_use]
pub(crate) fn running(env: &Env) -> bool {
    let pid = fs::read_to_string(env.run_dir().join("crosvm.pid")).unwrap_or_default();
    let trimmed = pid.trim();
    if !is_pid_running(env, trimmed) {
        return false;
    }
    proc_cmdline(env, trimmed).is_none_or(|cmdline| cmdline.contains("crosvm"))
}

/// 启动失败：记 ERROR、把给界面看的原因写入 `run/last_error`，5 秒后在后台删除开机日志。
pub(crate) fn fail(env: &Env, log_msg: &str, ui_msg: &str) {
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

/// 在开机日志里记一行就绪等待进度（已等待秒数与串口输出量）。
pub(crate) fn boot_progress(env: &Env, secs: u64) {
    let sz = fs::metadata(env.console_log()).map_or(0, |meta| meta.len());
    let kb = sz.checked_div(1024_u64).unwrap_or(0_u64);
    blog(
        env,
        "vm",
        &format!("等待 OpenWrt 就绪 {secs}s · 串口已输出 {kb}KB"),
    );
}

/// 在后台线程里回收子进程（避免僵尸）；退出状态无人需要，等待本身失败时计数。
pub(crate) fn reap_in_background(mut child: Child) {
    // 分离线程：显式丢弃句柄即 detach，与原来不 join 的语义相同
    drop(thread::spawn(move || {
        if child.wait().is_err() {
            note_failure(&FAILURES.exec);
        }
    }));
}

/// 装上启动保险丝：写入"当前时间 配置指纹"并落盘，然后等 1 秒。
pub(crate) fn fuse_arm(env: &Env) {
    let line = format!("{} {}\n", unix_now(), config::hash(env));
    fsx::write_logged(env, env.fuse_file(), line);
    if let Ok(fuse) = fs::File::open(env.fuse_file())
        && let Err(err) = fuse.sync_all()
    {
        fsx::record_failure(env, "sync", &env.fuse_file(), &err);
    }
    thread::sleep(Duration::from_secs(1));
}

/// 客机已报告就绪（`vm-ctl.log` 有以 `XR_READY` 开头的行，或串口日志含 `XR_READY`）时返回 true。
#[must_use]
pub(crate) fn ready_seen(env: &Env) -> bool {
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

/// 客机报告就绪所用的通道：`hvc`（控制通道）、`ttyS0`（串口）或 `null`（尚未就绪）。
#[must_use]
pub(crate) fn ready_channel(env: &Env) -> &'static str {
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

/// 执行模块的 `scripts/net.sh <op>`（存在时）并返回其退出码；脚本不存在时调用内置的 `xr net <op>`。
#[must_use]
pub(crate) fn run_net_hook(env: &Env, op: &str) -> i32 {
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

/// 系统单调运行时长（秒，取 `uptime` 整数部分）；测试可用 `proc_base/uptime` 覆盖。
#[must_use]
pub(crate) fn mono_now(env: &Env) -> Option<u64> {
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

/// 就绪等待：每秒检查一次，最多 `ready_to` 秒；`crosvm` 先退出时拆保险丝并记失败。
///
/// 返回 `ControlFlow::Break` 表示看守到此结束（已处理失败），`Continue` 表示继续判断是否就绪。
fn wait_ready(env: &Env, ready_to: u64) -> ControlFlow<()> {
    let mut waited = 0_u64;
    while waited < ready_to {
        if !running(env) {
            fsx::remove_logged(env, env.fuse_file());
            fail(env, "就绪前 crosvm 已退出，详见 vmm.log", "启动中退出");
            return ControlFlow::Break(());
        }
        if ready_seen(env) {
            break;
        }
        thread::sleep(Duration::from_secs(1));
        waited = waited.saturating_add(1);
        boot_progress(env, waited);
    }
    ControlFlow::Continue(())
}

/// 客机就绪后：记就绪时刻与状态、清上次错误、重建网络规则、记录接管方式并后台抓诊断快照。
fn on_ready(env: &Env) {
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
}

/// 稳定期内 `crosvm` 退出：客机自行重启待处理且守护进程在时通知守护进程（`USR2`）接手；否则拆保险丝并记失败。
fn on_stable_exit(env: &Env) {
    if crate::daemon::keepalive::vm_guest_reset_pending(env) {
        let dpid = crate::daemon::pid(env);
        if is_pid_running(env, &dpid) {
            send_signal_logged(env, &dpid, Signal::USR2);
            return;
        }
    }
    fsx::remove_logged(env, env.fuse_file());
    fail(env, "稳定期内 crosvm 退出", "服务意外退出");
}

/// 稳定期确认：每 5 秒检查一次 `crosvm` 存活，满 `STABLE_SEC` 后拆保险丝、写 `last_good`、状态记为 `stable`。
fn wait_stable(env: &Env) {
    let stable_sec = Config::load(env).stable_sec();
    let mut st = 0_u64;
    while st < stable_sec {
        if !running(env) {
            on_stable_exit(env);
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
    fsx::write_logged(env, env.run_dir().join("state"), "stable\n");
    log::info(env, "vm", "稳定，熔断已解除，last_good 已更新");
    fsx::remove_logged(env, env.boot_log());
}

/// 若 `run/fuse_watch.pid` 记录的仍是当前进程号则将其删除，避免看守退出后留下死进程号。
fn clear_self_fuse_pid(env: &Env) {
    let pid_file = env.run_dir().join("fuse_watch.pid");
    let self_pid = std::process::id().to_string();
    if fs::read_to_string(&pid_file).is_ok_and(|raw| raw.trim() == self_pid) {
        fsx::remove_logged(env, pid_file);
    }
}

/// 启动保险丝看守主流程：等就绪、重建网络规则、再确认稳定期存活。
fn fuse_watch_inner(env: &Env) {
    let ready_to = Config::load(env).ready_timeout_sec();
    if wait_ready(env, ready_to).is_break() {
        return;
    }
    if !ready_seen(env) {
        let msg = format!("{ready_to} 秒内未就绪，停止 VM");
        let ui = format!("{ready_to} 秒内未就绪");
        fail(env, &msg, &ui);
        note_rc(env, "vm_stop_exec", stop_exec(env, true));
        return;
    }
    on_ready(env);
    wait_stable(env);
}

/// 启动保险丝看守：在就绪超时内等客机就绪，再在稳定期内持续确认 `crosvm` 存活；成功后拆除保险丝并写 `last_good`，退出前清理自身 `PID` 文件。
///
/// 期间 `crosvm` 退出或超时未就绪时调用 [`fail`] 记录原因。
pub(crate) fn fuse_watch(env: &Env) {
    fuse_watch_inner(env);
    clear_self_fuse_pid(env);
}

/// 删除 `guest/host/vpn/` 里已没有对应非空 `config/vpn/<名>.ovpn` 的文件。
fn prune_host_vpn(env: &Env, host_vpn: &Path, vpn_dir: &Path) {
    let Ok(rd) = fs::read_dir(host_vpn) else {
        return;
    };
    for ent in rd.flatten() {
        let target = ent.path();
        let Some(stem) = target.file_stem().and_then(|stem_os| stem_os.to_str()) else {
            continue;
        };
        let ovpn = vpn_dir.join(format!("{stem}.ovpn"));
        let keep = fs::metadata(&ovpn).is_ok_and(|ovpn_meta| ovpn_meta.len() > 0);
        if !keep {
            fsx::remove_logged(env, &target);
        }
    }
}

/// 把 `config/vpn/*.ovpn` 及其启用标记同步到给客机的 `guest/host/vpn/`，并删除已不存在的配置。
///
/// 返回 `Some(是否有启用的配置)`；没有 `VPN` 配置目录时返回 `None`。
#[must_use]
pub(crate) fn vpn_sync_host(env: &Env) -> Option<bool> {
    let host_dir = env.run_dir().join("guest/host");
    fsx::mkdirs_logged(env, &host_dir);
    let vpn_dir = env.vpn_dir();
    if !vpn_dir.is_dir() {
        return None;
    }
    let host_vpn = host_dir.join("vpn");
    fsx::mkdirs_logged(env, &host_vpn);
    prune_host_vpn(env, &host_vpn, &vpn_dir);
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

/// `config/vpn/` 里至少有一份非空 `.ovpn` 配置时返回 true。
fn has_profile(env: &Env) -> bool {
    let Ok(rd) = fs::read_dir(env.vpn_dir()) else {
        return false;
    };
    rd.flatten().any(|ent| {
        let file_path = ent.path();
        file_path.extension().and_then(|os_str| os_str.to_str()) == Some("ovpn")
            && fs::metadata(&file_path).is_ok_and(|meta| meta.len() > 0)
    })
}

/// 开机前按 `config/vpn/` 同步配置给客机，有启用的配置下发 `enable`，否则下发 `disable`（有配置时记"保持停止"）。
fn boot_profiles(env: &Env, host: &Path) {
    let Some(any) = vpn_sync_host(env) else {
        return;
    };
    if any {
        fsx::write_logged(env, host.join("openvpn.cmd"), "enable\n");
        blog(env, "vpn", "下发 OpenVPN 配置");
    } else {
        fsx::write_logged(env, host.join("openvpn.cmd"), "disable\n");
        if has_profile(env) {
            blog(env, "vpn", "OpenVPN 保持停止");
        }
    }
}

/// 开机前准备 `VPN`：清掉客机上次的应答文件，按配置下发 `openvpn.cmd`（`enable`/`disable`）；兼容旧版单文件 `config/openvpn.ovpn`。
pub(crate) fn vpn_boot_prepare(env: &Env) {
    let guest = env.run_dir().join("guest/guest");
    let host = env.run_dir().join("guest/host");
    for name in ["openvpn.ack", "openvpn.multi", "openvpn.connecting"] {
        fsx::remove_logged(env, guest.join(name));
    }
    if env.vpn_dir().is_dir() {
        boot_profiles(env, &host);
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

/// 启动所需的内核与系统盘镜像路径。
#[derive(Debug)]
struct Images {
    /// 内核镜像 `Image`。
    image: PathBuf,
    /// 系统盘 `rootfs` 镜像。
    rootfs: PathBuf,
}

/// 启动前检查：已在运行时返回 `Err(0)`；清理上次的停机标记并记准备日志；有未确认熔断或缺少 `crosvm`/镜像时返回 `Err(1)`。
///
/// # Errors
///
/// 不需要或不能继续启动时返回应使用的退出码。
fn preflight(env: &Env, cfg: &Config) -> Result<Images, i32> {
    if running(env) {
        log::info(env, "vm", "已在运行");
        return Err(0);
    }
    let run = env.run_dir();
    if !env.boot_log().is_file() {
        fsx::write_logged(env, env.boot_log(), "");
    }
    for marker in ["teardown.last", "shutdown", "shutdown.noted", "stopping"] {
        fsx::remove_logged(env, run.join(marker));
    }
    let disk_lbl = if cfg.vm_disk_rw() { "读写" } else { "只读" };
    blog(
        env,
        "vm",
        &format!(
            "准备启动：CPU {} 核 · 内存 {}MB · 系统盘 {disk_lbl}",
            cfg.vm_cpus(),
            cfg.vm_mem_mb()
        ),
    );
    fsx::remove_logged(env, run.join("last_error"));
    if env.tripped_file().is_file() {
        log::err(env, "vm", "熔断未确认（xr ack-trip）");
        return Err(1);
    }
    let image = env.img_dir().join("Image");
    let rootfs = env.rootfs_img();
    if !crate::env::is_executable(&env.crosvm) || !image.is_file() || !rootfs.is_file() {
        fail(env, "缺少 crosvm 或镜像", "缺少虚拟化组件或系统镜像");
        return Err(1);
    }
    if !ce_ready() {
        log::info(env, "vm", "警告：手机未解锁（CE 不可用）");
    }
    let img_mb = fs::metadata(&image).map_or(0, |img_meta| {
        img_meta.len().checked_div(1_048_576_u64).unwrap_or(0_u64)
    });
    let rfs_mb = fs::metadata(&rootfs).map_or(0, |meta| {
        meta.len().checked_div(1_048_576_u64).unwrap_or(0_u64)
    });
    blog(
        env,
        "vm",
        &format!("内核 {img_mb}MB · 系统盘 {rfs_mb}MB · crosvm 就绪"),
    );
    Ok(Images { image, rootfs })
}

/// 建好控制通道目录 `run/guest/{host,guest}`（权限 700）并删除上次运行留下的控制与状态文件；返回 `host` 目录。
fn prepare_ctl_dirs(env: &Env, run: &Path) -> PathBuf {
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
        "host/stats.cmd",
        "guest/stats",
        "guest/stats.cap",
        "host/backup.cmd",
        "host/backup.in.tar.gz",
        "guest/backup.ack",
        "guest/backup.tar.gz",
        "guest/backup.cap",
        "guest/openclash.status",
        "guest/openvpn.tunnels",
        "guest/openvpn.status",
        "guest/openvpn.addr",
        "guest/openvpn.routes",
    ] {
        fsx::remove_logged(env, run.join("guest").join(file_name));
    }
    fsx::remove_logged(env, run.join("routes.rejected"));
    host_dir
}

/// 开启组网（`VM_NET`）时拉起手机侧网络并返回 `crosvm` 的 `--net` 参数（`xr0`，有热点口 `xr1` 时再加一组）；未开启时为空。
///
/// # Errors
///
/// 手机侧网络初始化失败时记失败原因并返回退出码 1。
fn net_args(env: &Env, cfg: &Config) -> Result<Vec<String>, i32> {
    let mut args: Vec<String> = Vec::new();
    if !cfg.vm_net() {
        return Ok(args);
    }
    let run = env.run_dir();
    blog(
        env,
        "net",
        "创建 xr0 / xr1、登记 netd 本地网络、建立转发规则",
    );
    if run_net_hook(env, "up") != 0_i32 {
        fail(env, "net up 失败", "网络初始化失败");
        return Err(1);
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
    args.push("--net".to_owned());
    args.push(format!("tap-name=xr0,mac={}", cfg.vm_mac()));
    if env.sysnet.join("xr1").is_dir() {
        let mac1 = hs_mac(cfg.vm_mac()).unwrap_or_else(|| "52:54:00:1d:e9:ff".to_owned());
        args.push("--net".to_owned());
        args.push(format!("tap-name=xr1,mac={mac1}"));
    }
    Ok(args)
}

/// 内核命令行：根设备与读写方式、串口、客机地址/网关/`DNS`（`IPv4` 与 `IPv6`）、共享方式与启动时刻。
fn kernel_params(cfg: &Config, rootmode: &str, share_mode: &str, boot_ts: u64) -> String {
    let dns_csv = cfg.dns().split_whitespace().collect::<Vec<_>>().join(",");
    format!(
        "root=/dev/vda {rootmode} rootwait console=ttyS0,115200 earlycon panic=5 xr.ip={}.254/24 xr.gw={}.1 xr.dns={dns_csv} xr.ip6={}::254/64 xr.gw6={}::1 xr.share={share_mode} xr.time={boot_ts}",
        cfg.net_prefix(),
        cfg.net_prefix(),
        crate::env::XR_NET6,
        crate::env::XR_NET6,
    )
}

/// 拼装 `crosvm` 命令行所需的全部取值。
#[derive(Debug)]
struct StartPlan {
    /// 虚拟 `CPU` 数。
    cpus: u32,
    /// 内存（`MB`）。
    mem: u32,
    /// `swiotlb` 大小（`MB`）。
    swiotlb: u32,
    /// `--net` 参数。
    net_args: Vec<String>,
    /// `--block` 的系统盘描述。
    blk: String,
    /// 共享目录需要的前置包装参数。
    wrap: Vec<String>,
    /// 共享目录的 `--shared-dir` 描述，空表示不共享。
    shared_dir: String,
    /// 控制通道的 `--shared-dir` 描述。
    ctl_spec: String,
    /// 内核命令行。
    params: String,
    /// 内核镜像。
    image: PathBuf,
    /// `crosvm` 控制套接字。
    sock: PathBuf,
}

/// 经 `xr internal exec-crosvm` 跳板启动 `crosvm` 的完整参数。
fn crosvm_args(env: &Env, plan: StartPlan) -> Vec<String> {
    let mut tramp_args: Vec<String> = vec!["internal".to_owned(), "exec-crosvm".to_owned()];
    tramp_args.extend(plan.wrap);
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
        plan.swiotlb.to_string(),
        "--cpus".to_owned(),
        plan.cpus.to_string(),
        "--mem".to_owned(),
        plan.mem.to_string(),
    ]);
    tramp_args.extend(plan.net_args);
    tramp_args.extend([
        "--block".to_owned(),
        plan.blk,
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
            env.run_dir().join("clash-ctl.sock").display()
        ),
        "--socket".to_owned(),
        plan.sock.to_string_lossy().into_owned(),
        "--shared-dir".to_owned(),
        plan.ctl_spec,
    ]);
    if !plan.shared_dir.is_empty() {
        tramp_args.push("--shared-dir".to_owned());
        tramp_args.push(plan.shared_dir);
    }
    tramp_args.extend([
        "--params".to_owned(),
        plan.params,
        plan.image.to_string_lossy().into_owned(),
    ]);
    tramp_args
}

/// 在 `vmm.log` 写入本次运行标记，并以它为标准输出与错误输出拉起跳板进程（打不开日志时输出丢弃）。
///
/// # Errors
///
/// 进程无法拉起时返回对应错误。
fn spawn_crosvm(env: &Env, tramp_args: &[String], boot_ts: u64) -> std::io::Result<Child> {
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
    out_f.map_or_else(
        |_| {
            Command::new(env.self_exe())
                .args(tramp_args)
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
                .args(tramp_args)
                .stdin(Stdio::null())
                .stdout(out_std)
                .stderr(err_std)
                .spawn()
        },
    )
}

/// 清空串口与控制通道日志，删除上次的控制套接字、状态与就绪时刻；返回控制套接字路径。
fn reset_boot_files(env: &Env, run: &Path) -> PathBuf {
    fsx::write_logged(env, env.console_log(), "");
    fsx::write_logged(env, env.ctl_log(), "");
    let sock = run.join("crosvm.sock");
    fsx::remove_logged(env, &sock);
    fsx::remove_logged(env, run.join("state"));
    fsx::remove_logged(env, run.join("ready_at"));
    sock
}

/// 拉起 `crosvm` 前布防保险丝、启动 `OpenClash` 复用通道，按配置持唤醒锁。
fn arm_start(env: &Env, cfg: &Config) {
    fuse_arm(env);
    crate::clash::start_clash_mux(env);
    blog(env, "vm", "熔断已布防，启动 crosvm（gunyah）");
    if cfg.vm_wakelock() {
        wakelock_on(env);
    }
}

/// 启动失败后的回退：停 `OpenClash` 复用通道、释放唤醒锁，开启组网时撤销手机侧网络。
fn rollback_start(env: &Env, vm_net: bool) {
    crate::clash::stop_clash_mux(env);
    wakelock_off(env);
    if vm_net {
        note_rc(env, "run_net_hook", run_net_hook(env, "down"));
    }
}

/// 拉起之后：记 `crosvm.pid`，分离线程等待其退出（守护进程内通知守护进程），2 秒后确认仍在运行；
/// 立即退出时回退并返回 `None`，否则拉起保险丝看守、记共享方式、逃出应用 `cgroup` 并返回 `crosvm` 的进程号。
fn after_spawn(env: &Env, mut ch: Child, vm_net: bool, share_mode: &str) -> Option<String> {
    let run = env.run_dir();
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
        rollback_start(env, vm_net);
        return None;
    }

    if let Ok(fw) = spawn::self_detached(env, &["internal", "fuse-watch"]) {
        fsx::write_logged(env, run.join("fuse_watch.pid"), format!("{}\n", fw.id()));
        if env.role.is_daemon() {
            reap_in_background(fw);
        }
    }
    fsx::write_logged(env, run.join("share.mode"), format!("{share_mode}\n"));
    crate::daemon::keepalive::cgroup_escape_tree(env, std::process::id());
    Some(pid_str)
}

/// 系统盘的 `--block` 描述与内核命令行里的读写方式（`rw`/`ro`）。
fn disk_block(rootfs: &Path, disk_rw: bool) -> (String, &'static str) {
    if disk_rw {
        (format!("{},lock=false,root=true", rootfs.display()), "rw")
    } else {
        (
            format!("{},lock=false,root=true,ro=true", rootfs.display()),
            "ro",
        )
    }
}

/// 调试日志记一行启动参数：`CPU`、内存、`swiotlb`、网络、系统盘读写方式、共享方式与内核命令行。
fn log_launch(env: &Env, cfg: &Config, plan: &StartPlan, rootmode: &str, share_mode: &str) {
    let net_desc = if plan.net_args.is_empty() {
        "无".to_owned()
    } else {
        plan.net_args.join(" ")
    };
    dlog(
        env,
        cfg,
        "vm",
        &format!(
            "启动参数：cpus={} mem={} swiotlb={} net={net_desc} disk={rootmode} share={share_mode} params={}",
            plan.cpus, plan.mem, plan.swiotlb, plan.params
        ),
    );
}

/// 一次启动的参数与共享方式、启动时刻。
#[derive(Debug)]
struct Launch {
    /// `crosvm` 命令行取值。
    plan: StartPlan,
    /// 共享方式。
    share_mode: String,
    /// 启动时刻（秒）。
    boot_ts: u64,
}

/// 准备共享目录与 `VPN`、写启动时刻、生成内核命令行、重置运行文件，记启动参数并布防；返回本次启动的参数。
fn prepare_launch(
    env: &Env,
    cfg: &Config,
    images: Images,
    net: Vec<String>,
    host_dir: &Path,
) -> Launch {
    let run = env.run_dir();
    let (blk, rootmode) = disk_block(&images.rootfs, cfg.vm_disk_rw());
    let share = share::prepare(env, cfg);
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
    let params = kernel_params(cfg, rootmode, &share.mode, boot_ts);
    let sock = reset_boot_files(env, &run);
    let plan = StartPlan {
        cpus: cfg.vm_cpus(),
        mem: cfg.vm_mem_mb(),
        swiotlb: cfg.vm_swiotlb_mb(),
        net_args: net,
        blk,
        wrap: share.wrap,
        shared_dir: share.shared_dir,
        ctl_spec,
        params,
        image: images.image,
        sock,
    };
    log_launch(env, cfg, &plan, rootmode, &share.mode);
    arm_start(env, cfg);
    Launch {
        plan,
        share_mode: share.mode,
        boot_ts,
    }
}

/// `xr start` 的实际执行：检查熔断与镜像、准备共享目录与系统盘、启动 `crosvm`、拉起网络与保险丝看守；返回进程退出码。
#[must_use]
pub(crate) fn start_exec(env: &Env) -> i32 {
    let cfg = Config::load(env);
    let images = match preflight(env, &cfg) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let host_dir = prepare_ctl_dirs(env, &env.run_dir());
    let vm_net = cfg.vm_net();
    let net = match net_args(env, &cfg) {
        Ok(args) => args,
        Err(code) => return code,
    };
    let Launch {
        plan,
        share_mode,
        boot_ts,
    } = prepare_launch(env, &cfg, images, net, &host_dir);
    let (cpus, mem) = (plan.cpus, plan.mem);
    let tramp_args = crosvm_args(env, plan);
    let Ok(ch) = spawn_crosvm(env, &tramp_args, boot_ts) else {
        fail(env, "无法拉起 crosvm", "无法启动");
        fsx::remove_logged(env, env.fuse_file());
        rollback_start(env, vm_net);
        return 1;
    };
    let Some(pid_str) = after_spawn(env, ch, vm_net, &share_mode) else {
        return 1;
    };
    log::info(
        env,
        "vm",
        &format!("已启动 pid={pid_str} mem={mem}MB cpus={cpus} share={share_mode}"),
    );
    0
}

/// `xr stop` 的实际执行：通知看守退出、先 `TERM` 后 `KILL` 停止 `crosvm`，拆除网络并清理运行状态；返回进程退出码。
///
/// `in_watch` 为 true 表示由保险丝看守自身调用，此时不再向看守发信号。
#[must_use]
pub(crate) fn stop_exec(env: &Env, in_watch: bool) -> i32 {
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

/// `xr internal vm status` 输出的虚拟机状态（序列化为 JSON，字段名即 JSON 键）。
#[derive(Debug, Serialize)]
pub(crate) struct StatusJson {
    /// `crosvm` 是否在运行。
    pub(crate) running: bool,
    /// `crosvm` 进程号，未运行时为 `null`。
    pub(crate) pid: Option<u64>,
    /// 状态：`stopped`、`booting`、`ready`、`stable` 等（取自 `run/state`）。
    pub(crate) state: String,
    /// 就绪后的运行秒数，未就绪时为 0。
    pub(crate) uptime_s: u64,
    /// 熔断原因（`config/tripped` 的内容），未熔断时为 `null`。
    pub(crate) tripped: Option<String>,
    /// 当前配置指纹与 `last_good` 一致时为 true。
    pub(crate) last_good_match: bool,
    /// 就绪通道 `hvc` 或 `ttyS0`，未就绪时为 `null`。
    pub(crate) ready_channel: Option<String>,
    /// 最近一次启动失败的原因（`run/last_error` 首行，去掉引号与反斜杠），没有时为空串。
    pub(crate) error: String,
}

/// 运行中的状态文字：`run/state` 的内容，空时为 `booting`；未运行为 `stopped`。
fn vm_state(env: &Env, running: bool) -> String {
    if !running {
        return "stopped".to_owned();
    }
    let state_text = fs::read_to_string(env.run_dir().join("state")).unwrap_or_default();
    let trimmed = state_text.trim();
    if trimmed.is_empty() {
        "booting".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// 就绪后的运行秒数：优先用就绪时刻 `ready_at`，取不到时用 `crosvm` 进程启动时间；都取不到为 0。
fn vm_uptime(env: &Env, pid_s: &str) -> u64 {
    let ready_at = fs::read_to_string(env.run_dir().join("ready_at"))
        .ok()
        .and_then(|uptime_text| uptime_text.trim().parse::<u64>().ok());
    if let (Some(r0), Some(now)) = (ready_at, mono_now(env)) {
        return now.saturating_sub(r0);
    }
    if let Ok(stat) = fs::read_to_string(env.proc_base.join(pid_s).join("stat"))
        && let (Some(st), Some(now)) = (
            stat.split_whitespace()
                .nth(21)
                .and_then(|content| content.parse::<u64>().ok()),
            mono_now(env),
        )
    {
        return now.saturating_sub(st.checked_div(100_u64).unwrap_or(0_u64));
    }
    0
}

/// 上次失败原因：`run/last_error` 的第一行，去掉引号与反斜杠。
fn last_error(env: &Env) -> String {
    fs::read_to_string(env.run_dir().join("last_error"))
        .unwrap_or_default()
        .lines()
        .next()
        .unwrap_or("")
        .replace(['"', '\\'], "")
}

/// 从运行状态文件汇总虚拟机状态。
#[must_use]
pub(crate) fn status(env: &Env) -> StatusJson {
    let run = env.run_dir();
    let running = running(env);
    let mut pid_num = None;
    let mut uptime_s = 0_u64;
    let state = vm_state(env, running);
    if running {
        let pid_s = fs::read_to_string(run.join("crosvm.pid"))
            .unwrap_or_default()
            .trim()
            .to_owned();
        pid_num = pid_s.parse::<u64>().ok();
        if matches!(state.as_str(), "ready" | "stable") {
            uptime_s = vm_uptime(env, &pid_s);
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
    StatusJson {
        running,
        pid: pid_num,
        state,
        uptime_s,
        tripped,
        last_good_match: lg,
        ready_channel: ch,
        error: last_error(env),
    }
}

/// 虚拟机状态的 JSON 文本；序列化失败时返回 `{"running":false,"state":"stopped"}`。
#[must_use]
pub(crate) fn status_json(env: &Env) -> String {
    serde_json::to_string(&status(env))
        .unwrap_or_else(|_| "{\"running\":false,\"state\":\"stopped\"}".to_owned())
}
