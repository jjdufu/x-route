//! 卸载：标准清理流程（停止进程、撤销网络规则、撤销通知、删除数据），以及 `WebUI` 发起的立即卸载。

use std::fs;
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use rustix::process::Signal;

use crate::daemon::keepalive::cgroup_escape;
use crate::daemon::vm_stop;
use crate::env::Env;
use crate::log;
use crate::log::note_ok;
use crate::log::note_rc;
use crate::log::unix_now;
use crate::net::run_net_cli;
use crate::sys::exec::note_spawn;
use crate::sys::exec::note_status;
use crate::sys::fsx;
use crate::sys::getprop;
use crate::sys::send_signal_logged;

/// 同步运行一条系统命令（丢弃输出）；无法执行时计数并写调试日志，退出码不影响卸载流程。
fn sys_run(env: &Env, bin: &Path, args: &[&str]) {
    let status = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    note_status(env, &bin.to_string_lossy(), status);
}

/// 同步运行一条系统命令（丢弃输出），成功退出时返回 true。
fn sys_ok(bin: &Path, args: &[&str]) -> bool {
    Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|exit_status| exit_status.success())
}

/// 向命令行属于本模块的进程（`crosvm`、模块脚本、`bin/xr`、通知器等）发送信号，跳过自己和卸载进程。
fn kill_matching_procs(env: &Env, sig: Signal) {
    let self_pid = std::process::id().to_string();
    let xr_data_s = env.xr_data.to_string_lossy();
    let moddir_s = env.moddir.to_string_lossy();
    let Ok(rd) = fs::read_dir(&env.proc_base) else {
        return;
    };
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        if name == self_pid || name.is_empty() || !name.bytes().all(|octet| octet.is_ascii_digit())
        {
            continue;
        }
        let Ok(bytes) = fs::read(ent.path().join("cmdline")) else {
            continue;
        };
        if bytes.is_empty() {
            continue;
        }
        let mapped: Vec<u8> = bytes
            .into_iter()
            .map(|byte| if byte == 0 { b' ' } else { byte })
            .collect();
        let cmd = String::from_utf8_lossy(&mapped);
        if cmd.contains("uninstall") {
            continue;
        }
        let is_match = (cmd.contains("crosvm") && cmd.contains(xr_data_s.as_ref()))
            || cmd.contains(&format!("{xr_data_s}/images"))
            || cmd.contains(&format!("{moddir_s}/scripts/daemon.sh"))
            || cmd.contains(&format!("{moddir_s}/scripts/watchdog.sh"))
            || cmd.contains(&format!("{moddir_s}/scripts/net.sh"))
            || cmd.contains(&format!("{moddir_s}/bin/xr"))
            || cmd.trim() == "xrnotify"
            || cmd.contains("xrnotify ");
        if is_match {
            send_signal_logged(env, &name, sig);
        }
    }
}

/// 系统通知栏里还有本模块的常驻通知时返回 true。
fn notify_present() -> bool {
    Command::new("dumpsys")
        .arg("notification")
        .output()
        .is_ok_and(|result| String::from_utf8_lossy(&result.stdout).contains("id=233 tag=xrouter"))
}

/// 撤销常驻通知：已开机时最多重试 3 次；未开机完成时复制通知器到临时目录，在后台等开机后再撤销。
fn cancel_notifications(env: &Env) {
    let jar = env.notifier_jar.clone();
    let jar_fallback = env.moddir.join("bin/xrnotify.jar");
    let jar_path = if jar.is_file() { jar } else { jar_fallback };
    if !jar_path.is_file() {
        return;
    }
    let cp = format!("-Djava.class.path={}", jar_path.display());
    if getprop("sys.boot_completed") == "1" {
        for _ in 0_i32..3_i32 {
            sys_run(
                env,
                &env.app_process,
                &[
                    &cp,
                    "/system/bin",
                    "--nice-name=xrnotify-cancel",
                    "com.xrouter.notify.Main",
                    "cancel",
                ],
            );
            if !notify_present() {
                break;
            }
            thread::sleep(Duration::from_secs(1));
        }
    } else {
        let tj = Path::new("/data/local/tmp/xrnotify-cancel.jar");
        if fs::copy(&jar_path, tj).is_ok() {
            fsx::chmod_logged(env, tj, 0o644);
            let ap = env.app_process.to_string_lossy().into_owned();
            let script = format!(
                "until [ \"$(getprop sys.boot_completed)\" = 1 ]; do sleep 3; done; sleep 10; \"{ap}\" -Djava.class.path=\"{}\" /system/bin --nice-name=xrnotify-cancel com.xrouter.notify.Main cancel; rm -f \"{}\"",
                tj.display(),
                tj.display()
            );
            let child = Command::new("sh")
                .args(["-c", &script])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            note_spawn(env, "xrnotify-cancel", child);
        }
    }
}

/// 先礼后兵地结束本模块的进程：按 `run/*.pid` 发 `TERM`、让 `crosvm` 自行停止、按名字匹配发 `TERM`，等 1 秒后对残留进程发 `KILL`。
fn stop_processes(env: &Env) {
    let run = env.run_dir();
    if let Ok(rd) = fs::read_dir(&run) {
        for ent in rd.flatten() {
            let target = ent.path();
            if target.extension().and_then(|raw_name| raw_name.to_str()) == Some("pid")
                && let Ok(pid) = fs::read_to_string(&target)
            {
                send_signal_logged(env, pid.trim(), Signal::TERM);
            }
        }
    }
    let sock = run.join("crosvm.sock");
    if sock.exists() {
        sys_run(env, &env.crosvm, &["stop", &sock.to_string_lossy()]);
    }
    kill_matching_procs(env, Signal::TERM);
    thread::sleep(Duration::from_secs(1));
    if let Ok(rd) = fs::read_dir(&run) {
        for ent in rd.flatten() {
            let file_path = ent.path();
            if file_path.extension().and_then(|os_str| os_str.to_str()) == Some("pid")
                && let Ok(pid) = fs::read_to_string(&file_path)
            {
                send_signal_logged(env, pid.trim(), Signal::KILL);
            }
        }
    }
    kill_matching_procs(env, Signal::KILL);
}

/// 删除转发与 `NAT` 链（`XR_FWD`、`XR_NAT` 及其 `IPv6` 对应链）：先反复删除跳转规则直到失败，再清空并删除链。
fn drop_nat_chains(env: &Env, iptables_bin: &Path, ip6tables_cmd: &Path) {
    while sys_ok(iptables_bin, &["-w", "-D", "FORWARD", "-j", "XR_FWD"]) {}
    sys_run(env, iptables_bin, &["-w", "-F", "XR_FWD"]);
    sys_run(env, iptables_bin, &["-w", "-X", "XR_FWD"]);
    while sys_ok(
        iptables_bin,
        &["-w", "-t", "nat", "-D", "POSTROUTING", "-j", "XR_NAT"],
    ) {}
    sys_run(env, iptables_bin, &["-w", "-t", "nat", "-F", "XR_NAT"]);
    sys_run(env, iptables_bin, &["-w", "-t", "nat", "-X", "XR_NAT"]);

    while sys_ok(ip6tables_cmd, &["-w", "-D", "FORWARD", "-j", "XR_FWD6"]) {}
    sys_run(env, ip6tables_cmd, &["-w", "-F", "XR_FWD6"]);
    sys_run(env, ip6tables_cmd, &["-w", "-X", "XR_FWD6"]);
    while sys_ok(
        ip6tables_cmd,
        &["-w", "-t", "nat", "-D", "POSTROUTING", "-j", "XR_NAT6"],
    ) {}
    sys_run(env, ip6tables_cmd, &["-w", "-t", "nat", "-F", "XR_NAT6"]);
    sys_run(env, ip6tables_cmd, &["-w", "-t", "nat", "-X", "XR_NAT6"]);
}

/// 删除流量统计、热点与 `MSS` 链（`XR_ACCT*`、`XR_HS*`、`XR_MSS`）：先反复删除跳转规则直到失败，再清空并删除链。
fn drop_chains(env: &Env, iptables_bin: &Path, ip6tables_cmd: &Path) {
    drop_nat_chains(env, iptables_bin, ip6tables_cmd);
    for bin in [iptables_bin, ip6tables_cmd] {
        for chain in ["XR_ACCT", "XR_ACCT6"] {
            while sys_ok(bin, &["-w", "-D", "INPUT", "-i", "xr0", "-j", chain]) {}
            while sys_ok(bin, &["-w", "-D", "OUTPUT", "-o", "xr0", "-j", chain]) {}
            for raw in [chain, &format!("{chain}_UP"), &format!("{chain}_DN")] {
                sys_run(env, bin, &["-w", "-F", raw]);
            }
            for text in [chain, &format!("{chain}_UP"), &format!("{chain}_DN")] {
                sys_run(env, bin, &["-w", "-X", text]);
            }
        }
    }

    while sys_ok(iptables_bin, &["-w", "-D", "OUTPUT", "-j", "XR_HS"]) {}
    sys_run(env, iptables_bin, &["-w", "-F", "XR_HS"]);
    sys_run(env, iptables_bin, &["-w", "-X", "XR_HS"]);
    while sys_ok(ip6tables_cmd, &["-w", "-D", "OUTPUT", "-j", "XR_HS6"]) {}
    sys_run(env, ip6tables_cmd, &["-w", "-F", "XR_HS6"]);
    sys_run(env, ip6tables_cmd, &["-w", "-X", "XR_HS6"]);

    for bin in [iptables_bin, ip6tables_cmd] {
        while sys_ok(
            bin,
            &["-w", "-t", "mangle", "-D", "POSTROUTING", "-j", "XR_MSS"],
        ) {}
        sys_run(env, bin, &["-w", "-t", "mangle", "-F", "XR_MSS"]);
        sys_run(env, bin, &["-w", "-t", "mangle", "-X", "XR_MSS"]);
    }
}

/// 拆除热点网桥 `xrbr1`（先把成员口移出）与虚拟网卡 `xr1`，并撤销 `ndc` 转发登记。
fn drop_bridge(env: &Env, ip_cmd: &Path, ndc: &Path) {
    let br_dir = env.sysnet.join("xrbr1");
    if br_dir.is_dir() {
        if let Ok(rd) = fs::read_dir(br_dir.join("brif")) {
            for ent in rd.flatten() {
                let port = ent.file_name().to_string_lossy().into_owned();
                sys_run(env, ip_cmd, &["link", "set", &port, "nomaster"]);
            }
        }
        sys_run(env, ip_cmd, &["link", "del", "xrbr1"]);
    }
    sys_run(env, ip_cmd, &["link", "del", "xr1"]);
    sys_run(env, ndc, &["ipfwd", "disable", "xrouter"]);
    sys_run(env, ndc, &["ipfwd", "disable", "xroute"]);
}

/// 删除本模块的策略路由规则（`IPv4`/`IPv6` 各优先级，反复删到失败）、清空路由表，并删除虚拟网卡 `xr0`。
fn drop_rules(env: &Env, ip_cmd: &Path) {
    for pref in [
        "4790", "4800", "4801", "18800", "18900", "19050", "19060", "19100",
    ] {
        while sys_ok(ip_cmd, &["rule", "del", "pref", pref]) {}
        while sys_ok(ip_cmd, &["-6", "rule", "del", "pref", pref]) {}
    }
    sys_run(
        env,
        ip_cmd,
        &["route", "flush", "table", "97", "dev", "xr0"],
    );
    sys_run(env, ip_cmd, &["route", "flush", "table", "233"]);
    sys_run(env, ip_cmd, &["route", "flush", "table", "234"]);
    sys_run(
        env,
        ip_cmd,
        &["-6", "route", "flush", "table", "97", "dev", "xr0"],
    );
    sys_run(env, ip_cmd, &["-6", "route", "flush", "table", "233"]);
    sys_run(env, ip_cmd, &["-6", "route", "flush", "table", "234"]);
    sys_run(env, ip_cmd, &["-6", "route", "flush", "table", "235"]);
    sys_run(env, ip_cmd, &["link", "del", "xr0"]);
}

/// 标准卸载：停止虚拟机与网络、结束本模块的所有进程、删除 `iptables`/`ip6tables` 链与策略路由、撤销通知、释放唤醒锁并删除数据目录；总是返回 0。
#[must_use]
pub(crate) fn standard(env: &Env) -> i32 {
    note_rc(env, "vm_stop", vm_stop(env));
    note_rc(env, "run_net_cli", run_net_cli(env, &["down".to_owned()]));
    stop_processes(env);

    let iptables_bin = env.sysbin_cmd("iptables");
    let ip6tables_cmd = env.sysbin_cmd("ip6tables");
    let ip_cmd = env.sysbin_cmd("ip");
    let ndc = env.sysbin_cmd("ndc");
    drop_chains(env, &iptables_bin, &ip6tables_cmd);
    drop_bridge(env, &ip_cmd, &ndc);
    drop_rules(env, &ip_cmd);

    cancel_notifications(env);
    fsx::write_logged(env, "/sys/power/wake_unlock", "xrouter\n");
    fsx::write_logged(env, "/sys/power/wake_unlock", "x-route\n");

    fsx::remove_tree_logged(env, &env.xr_data);
    fsx::remove_tree_logged(env, "/data/adb/x-route");
    rustix::fs::sync();
    0
}

/// 选一个可写的临时暂存目录：依次尝试 `XR_TMPDIR`、`/data/local/tmp`、系统临时目录与 `/dev`。
fn pick_stage_base(env: &Env) -> PathBuf {
    let name = format!(".xr-uninst-{}-{}", unix_now(), std::process::id());
    if let Some(tmp_dir) = &env.tmpdir {
        let path = tmp_dir.join(&name);
        if fs::create_dir_all(&path).is_ok() {
            return path;
        }
    }
    let tmp = Path::new("/data/local/tmp").join(&name);
    if fs::create_dir_all(&tmp).is_ok() {
        return tmp;
    }
    let sys_tmp = std::env::temp_dir().join(&name);
    if fs::create_dir_all(&sys_tmp).is_ok() {
        return sys_tmp;
    }
    let dev = Path::new("/dev").join(&name);
    fsx::mkdirs_logged(env, &dev);
    dev
}

/// `xr uninstall-now confirm`：校验模块目录后把 `xr` 与 `uninstall.sh` 复制到暂存目录，启动后台卸载进程，再删除模块目录。
///
/// 返回（退出码, JSON 结果）；未确认时返回 2，模块目录异常或缺少 `uninstall.sh` 时返回 1。
#[must_use]
pub fn now(env: &Env, confirm_arg: &str) -> (i32, String) {
    if confirm_arg != "confirm" {
        return (2, "{\"ok\":false,\"error\":\"需要确认\"}".to_owned());
    }
    let id = fs::read_to_string(env.moddir.join("module.prop"))
        .unwrap_or_default()
        .lines()
        .find_map(|line| line.strip_prefix("id="))
        .unwrap_or("")
        .trim()
        .to_owned();
    let dir_name = env
        .moddir
        .file_name()
        .and_then(|os_str| os_str.to_str())
        .unwrap_or("");
    if id.is_empty() || dir_name != id {
        return (1, "{\"ok\":false,\"error\":\"模块目录异常\"}".to_owned());
    }
    let uninst_sh = env.moddir.join("uninstall.sh");
    if !uninst_sh.is_file() {
        return (
            1,
            "{\"ok\":false,\"error\":\"缺少 uninstall.sh\"}".to_owned(),
        );
    }

    log::info(env, "xr", "WebUI 请求立即卸载");

    let stage = pick_stage_base(env);
    let stage_bin = stage.join("bin");
    fsx::mkdirs_logged(env, &stage_bin);

    let staged_xr = stage_bin.join("xr");
    let self_exe = env.self_exe();
    if fs::copy(&self_exe, &staged_xr).is_ok() {
        fsx::chmod_logged(env, &staged_xr, 0o755);
    }
    let jar = env.moddir.join("bin/xrnotify.jar");
    if jar.is_file() {
        fsx::copy_logged(env, &jar, stage_bin.join("xrnotify.jar"));
    }
    let staged_sh = stage.join("uninstall.sh");
    fsx::copy_logged(env, &uninst_sh, &staged_sh);
    fsx::chmod_logged(env, &staged_sh, 0o755);

    let upd_dir = env.modules_update.join(&id);
    let worker_bin = if staged_xr.is_file() {
        &staged_xr
    } else {
        &self_exe
    };
    let spawn_res = Command::new(worker_bin)
        .args([
            "internal",
            "uninstall-worker",
            &stage.to_string_lossy(),
            &env.moddir.to_string_lossy(),
            &upd_dir.to_string_lossy(),
        ])
        .current_dir("/")
        .env("MODDIR", &stage)
        .env("XR_DATA", &env.xr_data)
        .env("XR_UNINSTALL_STAGED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    if spawn_res.is_ok() {
        let ready_file = stage.join("worker.ready");
        for _ in 0_i32..400_i32 {
            if ready_file.is_file() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fsx::remove_tree_logged(env, &env.moddir);
    fsx::remove_tree_logged(env, &upd_dir);
    if env.moddir.exists() {
        fsx::write_logged(env, env.moddir.join("remove"), "");
    }
    rustix::fs::sync();

    (0, "{\"ok\":true}".to_owned())
}

/// `xr internal uninstall-worker`：脱离会话后执行暂存的 `uninstall.sh`（缺失时执行内置的标准卸载），再删除模块、更新目录、数据与暂存目录；返回进程退出码。
#[must_use]
pub fn worker(env: &Env, stage_s: &str, moddir_s: &str, upd_s: &str) -> i32 {
    crate::sys::spawn::enter_detached_session(env);
    if let Err(err) = std::env::set_current_dir("/") {
        fsx::record_failure(env, "chdir", Path::new("/"), &err);
    }
    note_ok(
        env,
        "cgroup_escape",
        cgroup_escape(env, &std::process::id().to_string()),
    );

    let stage = Path::new(stage_s);
    fsx::write_logged(env, stage.join("worker.ready"), "1\n");

    thread::sleep(Duration::from_millis(200));

    let staged_sh = stage.join("uninstall.sh");
    if staged_sh.is_file() {
        let status = Command::new("sh")
            .arg(&staged_sh)
            .current_dir("/")
            .env("MODDIR", stage)
            .env("XR_DATA", &env.xr_data)
            .env("XR_UNINSTALL_STAGED", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        note_status(env, "uninstall.sh", status);
    } else {
        note_rc(env, "uninstall_standard", standard(env));
    }

    fsx::remove_tree_logged(env, moddir_s);
    fsx::remove_tree_logged(env, upd_s);
    fsx::remove_tree_logged(env, &env.xr_data);
    fsx::remove_tree_logged(env, stage);
    rustix::fs::sync();
    0
}
