//! `ProcRole`、`DaemonMode`：由环境变量标志得到的角色与运行方式（替代原来的 4 个 bool 字段）。

use super::{DaemonMode, ProcRole};
use xr_test_support::{Failure, ensure};

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
