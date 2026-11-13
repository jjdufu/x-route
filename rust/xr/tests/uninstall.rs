use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use xr::env::Env;
use xr::uninstall::{uninstall_now, uninstall_worker};

fn temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let p = std::env::temp_dir().join(format!(
        "xr_uninst_test_{name}_{}_{nanos}",
        std::process::id()
    ));
    let _ = fs::create_dir_all(&p);
    p
}

#[test]
fn test_uninstall_now_requires_confirm() {
    let base = temp_dir("confirm");
    let mut env = Env::from_process();
    env.moddir = base.join("modules/xrouter");
    env.xr_data = base.join("xr_data");
    let (rc, out) = uninstall_now(&env, "");
    assert_eq!(rc, 2);
    assert!(out.contains("\"ok\":false"));
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn test_uninstall_now_removes_moddir_without_remove_marker_and_runs_worker() {
    let base = temp_dir("now");
    let moddir = base.join("modules/xrouter");
    let upd_dir = base.join("modules_update/xrouter");
    let xr_data = base.join("xr_data");
    let tmpdir = base.join("tmp");
    let marker = base.join("staged_ran.txt");

    let _ = fs::create_dir_all(moddir.join("bin"));
    let _ = fs::create_dir_all(&upd_dir);
    let _ = fs::create_dir_all(xr_data.join("run"));
    let _ = fs::create_dir_all(&tmpdir);

    let _ = fs::write(moddir.join("module.prop"), "id=xrouter\nname=XRouter\n");
    let bin_xr = moddir.join("bin/xr");
    assert!(fs::copy(env!("CARGO_BIN_EXE_xr"), &bin_xr).is_ok());
    assert!(fs::set_permissions(&bin_xr, fs::Permissions::from_mode(0o755)).is_ok());

    let uninst_sh = moddir.join("uninstall.sh");
    let script = format!("#!/bin/sh\necho ran > \"{}\"\n", marker.display());
    assert!(fs::write(&uninst_sh, script).is_ok());
    assert!(fs::set_permissions(&uninst_sh, fs::Permissions::from_mode(0o755)).is_ok());

    let mut env = Env::from_process();
    env.moddir = moddir.clone();
    env.modules_update = base.join("modules_update");
    env.xr_data = xr_data.clone();
    env.tmpdir = Some(tmpdir.clone());

    let (rc, out) = uninstall_now(&env, "confirm");
    assert_eq!(rc, 0, "uninstall_now failed: {out}");
    assert!(out.contains("\"ok\":true"));

    // Synchronous deletion: moddir & modules_update/id must be gone, with NO `remove` marker left behind.
    assert!(!moddir.exists(), "moddir should be deleted synchronously");
    assert!(
        !moddir.join("remove").exists(),
        "remove marker must not exist"
    );
    assert!(!upd_dir.exists(), "modules_update/<id> should be deleted");

    // Wait briefly for detached worker to finish staged uninstall.sh and cleanup.
    for _ in 0..60 {
        if marker.is_file() && !xr_data.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(marker.is_file(), "staged uninstall.sh should have executed");
    assert!(!xr_data.exists(), "xr_data should be removed by worker");

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn test_uninstall_worker_cleans_dirs_without_staged_sh() {
    let base = temp_dir("worker");
    let stage = base.join("stage");
    let moddir = base.join("modules/xrouter");
    let upd_dir = base.join("modules_update/xrouter");
    let xr_data = base.join("xr_data");

    let _ = fs::create_dir_all(&stage);
    let _ = fs::create_dir_all(&moddir);
    let _ = fs::create_dir_all(&upd_dir);
    let _ = fs::create_dir_all(xr_data.join("log"));

    let mut env = Env::from_process();
    env.moddir = moddir.clone();
    env.xr_data = xr_data.clone();
    env.sysbin = base.join("nosysbin");
    env.proc_base = base.join("noproc");

    let rc = uninstall_worker(
        &env,
        &stage.to_string_lossy(),
        &moddir.to_string_lossy(),
        &upd_dir.to_string_lossy(),
    );
    assert_eq!(rc, 0);
    assert!(!moddir.exists());
    assert!(!upd_dir.exists());
    assert!(!xr_data.exists());
    assert!(!stage.exists());

    let _ = fs::remove_dir_all(&base);
}
