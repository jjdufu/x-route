use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use rustix::process::Signal;

use crate::daemon::keepalive::cgroup_escape;
use crate::daemon::vm_stop;
use crate::env::Env;
use crate::log::{log_info, unix_now};
use crate::net::run_net_cli;
use crate::sys::{getprop, send_signal};

fn sys_run(bin: &Path, args: &[&str]) {
    let _ = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn sys_ok(bin: &Path, args: &[&str]) -> bool {
    Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn kill_matching_procs(env: &Env, sig: Signal) {
    let self_pid = std::process::id().to_string();
    let xr_data_s = env.xr_data.to_string_lossy();
    let moddir_s = env.moddir.to_string_lossy();
    let Ok(rd) = fs::read_dir(&env.proc_base) else {
        return;
    };
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        if name == self_pid || name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
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
            .map(|b| if b == 0 { b' ' } else { b })
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
            let _ = send_signal(&name, sig);
        }
    }
}

fn notify_present() -> bool {
    Command::new("dumpsys")
        .arg("notification")
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("id=233 tag=xrouter"))
}

fn cancel_notifications(env: &Env) {
    let jar = env.notifier_jar.clone();
    let jar_fallback = env.moddir.join("bin/xrnotify.jar");
    let jar_path = if jar.is_file() { jar } else { jar_fallback };
    if !jar_path.is_file() {
        return;
    }
    let cp = format!("-Djava.class.path={}", jar_path.display());
    if getprop("sys.boot_completed") == "1" {
        for _ in 0..3 {
            sys_run(
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
            #[cfg(unix)]
            {
                let _ = fs::set_permissions(tj, fs::Permissions::from_mode(0o644));
            }
            let ap = env.app_process.to_string_lossy().into_owned();
            let script = format!(
                "until [ \"$(getprop sys.boot_completed)\" = 1 ]; do sleep 3; done; sleep 10; \"{ap}\" -Djava.class.path=\"{}\" /system/bin --nice-name=xrnotify-cancel com.xrouter.notify.Main cancel; rm -f \"{}\"",
                tj.display(),
                tj.display()
            );
            let _ = Command::new("sh")
                .args(["-c", &script])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }
}

pub fn uninstall_standard(env: &Env) -> i32 {
    let run = env.run_dir();
    let _ = vm_stop(env);
    let _ = run_net_cli(env, &["down".to_string()]);

    if let Ok(rd) = fs::read_dir(&run) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p.extension().and_then(|s| s.to_str()) == Some("pid") {
                if let Ok(pid) = fs::read_to_string(&p) {
                    let _ = send_signal(pid.trim(), Signal::TERM);
                }
            }
        }
    }
    let sock = run.join("crosvm.sock");
    if sock.exists() {
        sys_run(&env.crosvm, &["stop", &sock.to_string_lossy()]);
    }
    kill_matching_procs(env, Signal::TERM);
    thread::sleep(Duration::from_secs(1));
    if let Ok(rd) = fs::read_dir(&run) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p.extension().and_then(|s| s.to_str()) == Some("pid") {
                if let Ok(pid) = fs::read_to_string(&p) {
                    let _ = send_signal(pid.trim(), Signal::KILL);
                }
            }
        }
    }
    kill_matching_procs(env, Signal::KILL);

    let ipt = env.sysbin_cmd("iptables");
    let ip6t = env.sysbin_cmd("ip6tables");
    let ip = env.sysbin_cmd("ip");
    let ndc = env.sysbin_cmd("ndc");

    while sys_ok(&ipt, &["-w", "-D", "FORWARD", "-j", "XR_FWD"]) {}
    sys_run(&ipt, &["-w", "-F", "XR_FWD"]);
    sys_run(&ipt, &["-w", "-X", "XR_FWD"]);
    while sys_ok(
        &ipt,
        &["-w", "-t", "nat", "-D", "POSTROUTING", "-j", "XR_NAT"],
    ) {}
    sys_run(&ipt, &["-w", "-t", "nat", "-F", "XR_NAT"]);
    sys_run(&ipt, &["-w", "-t", "nat", "-X", "XR_NAT"]);

    while sys_ok(&ip6t, &["-w", "-D", "FORWARD", "-j", "XR_FWD6"]) {}
    sys_run(&ip6t, &["-w", "-F", "XR_FWD6"]);
    sys_run(&ip6t, &["-w", "-X", "XR_FWD6"]);
    while sys_ok(
        &ip6t,
        &["-w", "-t", "nat", "-D", "POSTROUTING", "-j", "XR_NAT6"],
    ) {}
    sys_run(&ip6t, &["-w", "-t", "nat", "-F", "XR_NAT6"]);
    sys_run(&ip6t, &["-w", "-t", "nat", "-X", "XR_NAT6"]);

    for bin in [&ipt, &ip6t] {
        for c in ["XR_ACCT", "XR_ACCT6"] {
            while sys_ok(bin, &["-w", "-D", "INPUT", "-i", "xr0", "-j", c]) {}
            while sys_ok(bin, &["-w", "-D", "OUTPUT", "-o", "xr0", "-j", c]) {}
            for s in [c, &format!("{c}_UP"), &format!("{c}_DN")] {
                sys_run(bin, &["-w", "-F", s]);
            }
            for s in [c, &format!("{c}_UP"), &format!("{c}_DN")] {
                sys_run(bin, &["-w", "-X", s]);
            }
        }
    }

    while sys_ok(&ipt, &["-w", "-D", "OUTPUT", "-j", "XR_HS"]) {}
    sys_run(&ipt, &["-w", "-F", "XR_HS"]);
    sys_run(&ipt, &["-w", "-X", "XR_HS"]);
    while sys_ok(&ip6t, &["-w", "-D", "OUTPUT", "-j", "XR_HS6"]) {}
    sys_run(&ip6t, &["-w", "-F", "XR_HS6"]);
    sys_run(&ip6t, &["-w", "-X", "XR_HS6"]);

    for bin in [&ipt, &ip6t] {
        while sys_ok(
            bin,
            &["-w", "-t", "mangle", "-D", "POSTROUTING", "-j", "XR_MSS"],
        ) {}
        sys_run(bin, &["-w", "-t", "mangle", "-F", "XR_MSS"]);
        sys_run(bin, &["-w", "-t", "mangle", "-X", "XR_MSS"]);
    }

    let br_dir = env.sysnet.join("xrbr1");
    if br_dir.is_dir() {
        if let Ok(rd) = fs::read_dir(br_dir.join("brif")) {
            for ent in rd.flatten() {
                let port = ent.file_name().to_string_lossy().into_owned();
                sys_run(&ip, &["link", "set", &port, "nomaster"]);
            }
        }
        sys_run(&ip, &["link", "del", "xrbr1"]);
    }
    sys_run(&ip, &["link", "del", "xr1"]);
    sys_run(&ndc, &["ipfwd", "disable", "xrouter"]);
    sys_run(&ndc, &["ipfwd", "disable", "xroute"]);

    for pref in [
        "4790", "4800", "4801", "18800", "18900", "19050", "19060", "19100",
    ] {
        while sys_ok(&ip, &["rule", "del", "pref", pref]) {}
        while sys_ok(&ip, &["-6", "rule", "del", "pref", pref]) {}
    }
    sys_run(&ip, &["route", "flush", "table", "97", "dev", "xr0"]);
    sys_run(&ip, &["route", "flush", "table", "233"]);
    sys_run(&ip, &["route", "flush", "table", "234"]);
    sys_run(&ip, &["-6", "route", "flush", "table", "97", "dev", "xr0"]);
    sys_run(&ip, &["-6", "route", "flush", "table", "233"]);
    sys_run(&ip, &["-6", "route", "flush", "table", "234"]);
    sys_run(&ip, &["-6", "route", "flush", "table", "235"]);
    sys_run(&ip, &["link", "del", "xr0"]);

    cancel_notifications(env);
    let _ = fs::write("/sys/power/wake_unlock", "xrouter\n");
    let _ = fs::write("/sys/power/wake_unlock", "x-route\n");

    let _ = fs::remove_dir_all(&env.xr_data);
    let _ = fs::remove_dir_all("/data/adb/x-route");
    rustix::fs::sync();
    0
}

fn pick_stage_base(env: &Env) -> PathBuf {
    let name = format!(".xr-uninst-{}-{}", unix_now(), std::process::id());
    if let Some(t) = &env.tmpdir {
        let p = t.join(&name);
        if fs::create_dir_all(&p).is_ok() {
            return p;
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
    let _ = fs::create_dir_all(&dev);
    dev
}

pub fn uninstall_now(env: &Env, confirm_arg: &str) -> (i32, String) {
    if confirm_arg != "confirm" {
        return (2, "{\"ok\":false,\"error\":\"需要确认\"}".to_string());
    }
    let id = fs::read_to_string(env.moddir.join("module.prop"))
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix("id="))
        .unwrap_or("")
        .trim()
        .to_string();
    let dir_name = env
        .moddir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if id.is_empty() || dir_name != id {
        return (1, "{\"ok\":false,\"error\":\"模块目录异常\"}".to_string());
    }
    let uninst_sh = env.moddir.join("uninstall.sh");
    if !uninst_sh.is_file() {
        return (
            1,
            "{\"ok\":false,\"error\":\"缺少 uninstall.sh\"}".to_string(),
        );
    }

    log_info(env, "xr", "WebUI 请求立即卸载");

    let stage = pick_stage_base(env);
    let stage_bin = stage.join("bin");
    let _ = fs::create_dir_all(&stage_bin);

    let staged_xr = stage_bin.join("xr");
    let self_exe = env.self_exe();
    if fs::copy(&self_exe, &staged_xr).is_ok() {
        #[cfg(unix)]
        {
            let _ = fs::set_permissions(&staged_xr, fs::Permissions::from_mode(0o755));
        }
    }
    let jar = env.moddir.join("bin/xrnotify.jar");
    if jar.is_file() {
        let _ = fs::copy(&jar, stage_bin.join("xrnotify.jar"));
    }
    let staged_sh = stage.join("uninstall.sh");
    let _ = fs::copy(&uninst_sh, &staged_sh);
    #[cfg(unix)]
    {
        let _ = fs::set_permissions(&staged_sh, fs::Permissions::from_mode(0o755));
    }

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
        for _ in 0..400 {
            if ready_file.is_file() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    let _ = fs::remove_dir_all(&env.moddir);
    let _ = fs::remove_dir_all(&upd_dir);
    if env.moddir.exists() {
        let _ = fs::write(env.moddir.join("remove"), "");
    }
    rustix::fs::sync();

    (0, "{\"ok\":true}".to_string())
}

pub fn uninstall_worker(env: &Env, stage_s: &str, moddir_s: &str, upd_s: &str) -> i32 {
    crate::sys::spawn::enter_detached_session();
    let _ = std::env::set_current_dir("/");
    let _ = cgroup_escape(env, &std::process::id().to_string());

    let stage = Path::new(stage_s);
    let _ = fs::write(stage.join("worker.ready"), "1\n");

    thread::sleep(Duration::from_millis(200));

    let staged_sh = stage.join("uninstall.sh");
    if staged_sh.is_file() {
        let _ = Command::new("sh")
            .arg(&staged_sh)
            .current_dir("/")
            .env("MODDIR", stage)
            .env("XR_DATA", &env.xr_data)
            .env("XR_UNINSTALL_STAGED", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    } else {
        let _ = uninstall_standard(env);
    }

    let _ = fs::remove_dir_all(moddir_s);
    let _ = fs::remove_dir_all(upd_s);
    let _ = fs::remove_dir_all(&env.xr_data);
    let _ = fs::remove_dir_all(stage);
    rustix::fs::sync();
    0
}
