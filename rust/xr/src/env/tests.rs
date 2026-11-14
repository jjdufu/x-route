//! `ProcRole`、`DaemonMode`：由环境变量标志得到的角色与运行方式（替代原来的 4 个 bool 字段）。
//!
//! `Env::from_process`：在子进程中设置环境变量，把构造结果的调试输出与金样比对（`XR_GOLDEN_RECORD=1` 时录制）。

use std::fs;
use std::path::Path;
use std::process::Command;

use super::{DaemonMode, Env, ProcRole};
use tempfile::TempDir;
use xr_test_support::{Failure, ensure, ensure_eq, ensure_ok};

/// `XR_IN_DAEMON` / `XR_IN_WATCHDOG` 的 4 种组合（只有值为 "1" 才算设置）：两者都设置时与旧代码一样按守护进程处理。
///
/// # Errors
///
/// 任一组合得到的角色与期望不同时返回错误。
#[test]
fn role_from_flags_all_combinations() -> Result<(), Failure> {
    ensure(
        ProcRole::from_values(None, None) == ProcRole::Cli,
        "都不设：命令行",
    )?;
    ensure(
        ProcRole::from_values(Some("0"), Some("1")) == ProcRole::Watchdog,
        "只设看门狗",
    )?;
    ensure(
        ProcRole::from_values(Some("1"), None) == ProcRole::Daemon,
        "只设守护进程",
    )?;
    ensure(
        ProcRole::from_values(Some("1"), Some("1")) == ProcRole::Daemon,
        "都设：守护进程优先",
    )
}

/// 角色查询与旧 bool 字段一一对应。
///
/// # Errors
///
/// 查询结果与旧字段语义不同时返回错误。
#[test]
fn role_queries_match_old_fields() -> Result<(), Failure> {
    ensure(
        ProcRole::Daemon.is_daemon() && !ProcRole::Daemon.is_watchdog(),
        "守护进程",
    )?;
    ensure(
        ProcRole::Watchdog.is_watchdog() && !ProcRole::Watchdog.is_daemon(),
        "看门狗",
    )?;
    ensure(
        !ProcRole::Cli.is_daemon() && !ProcRole::Cli.is_watchdog(),
        "命令行",
    )
}

/// `XR_DAEMON_FOREGROUND == "1"` 映射为前台模式，否则为后台。
///
/// # Errors
///
/// 映射结果与期望不同时返回错误。
#[test]
fn daemon_mode_from_flag() -> Result<(), Failure> {
    ensure(
        DaemonMode::from_value(Some("1")).is_foreground(),
        "1 → 前台",
    )?;
    ensure(
        !DaemonMode::from_value(Some("0")).is_foreground(),
        "0 → 后台",
    )?;
    ensure(
        !DaemonMode::from_value(Some("")).is_foreground(),
        "空 → 后台",
    )?;
    ensure(
        !DaemonMode::from_value(None).is_foreground(),
        "未设置 → 后台",
    )
}

/// 子进程模式：设置时 [`from_process_dump_child`] 把 `Env::from_process()` 的调试输出写到该变量给出的文件。
const DUMP_OUT: &str = "XR_ENV_DUMP_OUT";

/// 所有变量都设置为互不相同的值（`XR_MEDIA` 与 `MEDIA` 同时设置时前者优先）。
const ALL_SET: &[(&str, &str)] = &[
    ("MODDIR", "/t/moddir"),
    ("XR_DATA", "/t/data"),
    ("BB", "/t/busybox"),
    ("XR_MEDIA", "/t/xr-media"),
    ("MEDIA", "/t/media"),
    ("RW_MAP_OPT", "rw-opt"),
    ("XR_SYSBIN", "/t/sysbin"),
    ("XR_PROC", "/t/proc"),
    ("XR_PROC_BASE", "/t/proc-base"),
    ("XR_PROC_UPTIME", "/t/uptime"),
    ("XR_PROC_MOUNTS", "/t/mounts"),
    ("XR_SYSNET", "/t/sysnet"),
    ("XR_CGROUP_ROOT", "/t/cgroup"),
    ("CROSVM", "/t/crosvm"),
    ("XR_NOTIFIER_JAR", "/t/notify.jar"),
    ("XR_APP_PROCESS", "/t/app_process"),
    ("XR_SCREEN", "off"),
    ("XR_DOWNLOAD", "/t/download"),
    ("XR_MODULES_UPDATE", "/t/modules_update"),
    ("XR_TMPDIR", "/t/tmp"),
    ("XR_HS_ANY", "1"),
    ("XR_IPV6_WAIT", "1"),
    ("XR_VPN_WAIT", "2"),
    ("XR_NOTIFY_WAIT", "3"),
    ("XR_NOTIFY_INTERVAL", "4"),
    ("XR_WATCHDOG_INTERVAL", "5"),
    ("XR_CARRIER_GRACE", "8"),
    ("XR_SNAP_MIN_BYTES", "6"),
    ("XR_SNAP_MAX_AGE", "7"),
    ("XR_IN_WATCHDOG", "1"),
    ("XR_DAEMON_FOREGROUND", "1"),
    ("XR_FAKE_DATE_FAIL", "1"),
];

/// 回退规则：空值视为未设置、只设 `MEDIA`、派生路径跟随 `MODDIR`/`XR_SYSBIN`、数字解析失败取默认、非 "1" 的开关不生效。
const FALLBACK: &[(&str, &str)] = &[
    ("MODDIR", "/f/moddir"),
    ("XR_DATA", ""),
    ("MEDIA", "/f/media"),
    ("RW_MAP_OPT", ""),
    ("XR_SYSBIN", "/f/sysbin"),
    ("XR_PROC", ""),
    ("XR_SCREEN", ""),
    ("XR_HS_ANY", ""),
    ("XR_IPV6_WAIT", "abc"),
    ("XR_VPN_WAIT", "-1"),
    ("XR_NOTIFY_WAIT", ""),
    ("XR_IN_DAEMON", "1"),
    ("XR_IN_WATCHDOG", "1"),
    ("XR_DAEMON_FOREGROUND", "0"),
    ("XR_FAKE_DATE_FAIL", "2"),
];

/// 子进程入口：未设置 [`DUMP_OUT`] 时什么都不做。
///
/// # Errors
///
/// 写出调试输出失败时返回错误。
#[test]
fn from_process_dump_child() -> Result<(), Failure> {
    let Some(out) = std::env::var_os(DUMP_OUT) else {
        return Ok(());
    };
    let dump = format!("{:#?}\n", Env::from_process());
    ensure_ok(fs::write(out, dump), "写出调试输出")
}

/// 在清空环境的子进程里只设置 `vars`，返回 `Env::from_process()` 的调试输出。
///
/// # Errors
///
/// 子进程启动失败、退出码非 0 或读不到输出时返回错误。
fn dump_with(vars: &[(&str, &str)]) -> Result<String, Failure> {
    let tmp = ensure_ok(TempDir::new(), "建临时目录")?;
    let out = tmp.path().join("env.txt");
    let exe = ensure_ok(std::env::current_exe(), "取测试程序路径")?;
    let status = ensure_ok(
        Command::new(exe)
            .args([
                "--exact",
                "env::tests::from_process_dump_child",
                "--test-threads=1",
                "-q",
            ])
            .env_clear()
            .envs(vars.iter().copied())
            .env(DUMP_OUT, &out)
            .output(),
        "运行子进程",
    )?;
    ensure(status.status.success(), "子进程退出码为 0")?;
    ensure_ok(fs::read_to_string(&out), "读调试输出")
}

/// 与金样比对；`XR_GOLDEN_RECORD=1` 时把输出写成金样。
///
/// # Errors
///
/// 录制失败或与金样不一致时返回错误。
fn check_env_golden(got: &str, golden: &str, name: &str) -> Result<(), Failure> {
    if std::env::var_os("XR_GOLDEN_RECORD").is_some() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/env/golden")
            .join(name);
        return ensure_ok(fs::write(path, got), "录制金样");
    }
    ensure_eq(got, golden, &format!("{name} 与输出一致"))
}

/// 所有变量都设置时，每个变量映射到对应字段。
///
/// # Errors
///
/// 输出与金样不一致时返回错误。
#[test]
fn from_process_all_vars() -> Result<(), Failure> {
    let got = dump_with(ALL_SET)?;
    check_env_golden(&got, include_str!("golden/all.txt"), "all.txt")
}

/// 一个变量都不设置时，各字段取真机默认值。
///
/// # Errors
///
/// 输出与金样不一致时返回错误。
#[test]
fn from_process_defaults() -> Result<(), Failure> {
    let got = dump_with(&[])?;
    check_env_golden(&got, include_str!("golden/defaults.txt"), "defaults.txt")
}

/// 空值、`MEDIA` 回退、派生路径、数字解析失败与开关取值的回退规则。
///
/// # Errors
///
/// 输出与金样不一致时返回错误。
#[test]
fn from_process_fallbacks() -> Result<(), Failure> {
    let got = dump_with(FALLBACK)?;
    check_env_golden(&got, include_str!("golden/fallback.txt"), "fallback.txt")
}
