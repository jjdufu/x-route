//! 卸载流程：未确认时拒绝；确认后同步删除模块目录并由后台进程执行暂存的 `uninstall.sh`；后台进程清理全部目录。

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::thread;
use std::time::Duration;

use tempfile::TempDir;
use xr::env::Env;
use xr::uninstall;
use xr_test_support::{Failure, ensure, ensure_eq, ensure_ok};

/// 未带 `confirm` 参数时拒绝卸载：退出码 2，结果为 `ok:false`。
///
/// # Errors
///
/// 任一检查不成立时返回 [`Failure`]。
#[test]
fn uninstall_now_requires_confirm() -> Result<(), Failure> {
    let base = ensure_ok(TempDir::new(), "建临时目录")?;
    let mut env = Env::from_process();
    env.moddir = base.path().join("modules/xrouter");
    env.xr_data = base.path().join("xr_data");
    let (rc, out) = uninstall::now(&env, "");
    ensure_eq(&rc, &2_i32, "未确认时退出码")?;
    ensure(out.contains("\"ok\":false"), "未确认时结果为 ok:false")
}

/// 确认卸载：模块目录与更新目录同步删除且不留 `remove` 标记；后台进程执行暂存的 `uninstall.sh` 并删除数据目录。
///
/// # Errors
///
/// 准备文件失败或任一检查不成立时返回 [`Failure`]。
#[test]
fn uninstall_now_removes_moddir_without_remove_marker_and_runs_worker() -> Result<(), Failure> {
    let base = ensure_ok(TempDir::new(), "建临时目录")?;
    let moddir = base.path().join("modules/xrouter");
    let upd_dir = base.path().join("modules_update/xrouter");
    let xr_data = base.path().join("xr_data");
    let tmpdir = base.path().join("tmp");
    let marker = base.path().join("staged_ran.txt");

    for dir in [
        moddir.join("bin"),
        upd_dir.clone(),
        xr_data.join("run"),
        tmpdir.clone(),
    ] {
        ensure_ok(fs::create_dir_all(&dir), &format!("准备 {}", dir.display()))?;
    }

    ensure_ok(
        fs::write(moddir.join("module.prop"), "id=xrouter\nname=XRouter\n"),
        "写 module.prop",
    )?;
    let bin_xr = moddir.join("bin/xr");
    let _copied = ensure_ok(fs::copy(env!("CARGO_BIN_EXE_xr"), &bin_xr), "复制 xr")?;
    ensure_ok(
        fs::set_permissions(&bin_xr, fs::Permissions::from_mode(0o755)),
        "xr 可执行",
    )?;

    let uninst_sh = moddir.join("uninstall.sh");
    let script = format!("#!/bin/sh\necho ran > \"{}\"\n", marker.display());
    ensure_ok(fs::write(&uninst_sh, script), "写 uninstall.sh")?;
    ensure_ok(
        fs::set_permissions(&uninst_sh, fs::Permissions::from_mode(0o755)),
        "uninstall.sh 可执行",
    )?;

    let mut env = Env::from_process();
    env.moddir.clone_from(&moddir);
    env.modules_update = base.path().join("modules_update");
    env.xr_data.clone_from(&xr_data);
    env.tmpdir = Some(tmpdir);

    let (rc, out) = uninstall::now(&env, "confirm");
    ensure_eq(&rc, &0_i32, &format!("uninstall::now failed: {out}"))?;
    ensure(out.contains("\"ok\":true"), "确认后结果为 ok:true")?;

    // 同步删除：模块目录与 modules_update/<id> 必须已不存在，且不留 `remove` 标记
    ensure(!moddir.exists(), "moddir should be deleted synchronously")?;
    ensure(
        !moddir.join("remove").exists(),
        "remove marker must not exist",
    )?;
    ensure(!upd_dir.exists(), "modules_update/<id> should be deleted")?;

    // 稍等后台进程执行暂存的 uninstall.sh 并完成清理
    for _ in 0_u32..60_u32 {
        if marker.is_file() && !xr_data.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    ensure(marker.is_file(), "staged uninstall.sh should have executed")?;
    ensure(!xr_data.exists(), "xr_data should be removed by worker")
}

/// 没有暂存的 `uninstall.sh` 时，后台卸载进程执行内置清理并删除模块、更新、数据与暂存目录。
///
/// # Errors
///
/// 准备目录失败或任一检查不成立时返回 [`Failure`]。
#[test]
fn uninstall_worker_cleans_dirs_without_staged_sh() -> Result<(), Failure> {
    let base = ensure_ok(TempDir::new(), "建临时目录")?;
    let stage = base.path().join("stage");
    let moddir = base.path().join("modules/xrouter");
    let upd_dir = base.path().join("modules_update/xrouter");
    let xr_data = base.path().join("xr_data");

    for dir in [
        stage.clone(),
        moddir.clone(),
        upd_dir.clone(),
        xr_data.join("log"),
    ] {
        ensure_ok(fs::create_dir_all(&dir), &format!("准备 {}", dir.display()))?;
    }

    let mut env = Env::from_process();
    env.moddir.clone_from(&moddir);
    env.xr_data.clone_from(&xr_data);
    env.sysbin = base.path().join("nosysbin");
    env.proc_base = base.path().join("noproc");

    let rc = uninstall::worker(
        &env,
        &stage.to_string_lossy(),
        &moddir.to_string_lossy(),
        &upd_dir.to_string_lossy(),
    );
    ensure_eq(&rc, &0_i32, "worker 退出码")?;
    ensure(!moddir.exists(), "模块目录已删除")?;
    ensure(!upd_dir.exists(), "更新目录已删除")?;
    ensure(!xr_data.exists(), "数据目录已删除")?;
    ensure(!stage.exists(), "暂存目录已删除")
}
