//! 日志写失败的处理：返回错误、只计数、不递归；诊断包失败计数行的格式。

use std::fs;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, MutexGuard};

use tempfile::TempDir;

use super::{FAILURES, append_line, format_failures_line, note_failure};
use crate::env::Env;
use crate::log;

/// 失败信息。
pub(crate) type Failure = &'static str;

/// 读写全局失败计数的测试互斥执行，保证前后差值只来自本测试。
pub(crate) static COUNTER_LOCK: Mutex<()> = Mutex::new(());

/// 取得计数锁；其他测试失败导致锁中毒时照常取得（计数本身不受影响）。
pub(crate) fn counter_guard() -> MutexGuard<'static, ()> {
    COUNTER_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 条件不成立时返回 `Err(msg)`。
///
/// # Errors
///
/// `cond` 为假时返回 `msg`。
pub(crate) const fn ensure(cond: bool, msg: Failure) -> Result<(), Failure> {
    if cond { Ok(()) } else { Err(msg) }
}

/// 数据目录位于 `data` 的测试环境。
pub(crate) fn env_at(data: &Path) -> Env {
    let mut env = Env::from_process();
    env.xr_data = data.to_path_buf();
    env.moddir = data.join("mod");
    env
}

/// 三个计数的当前值。
pub(crate) fn snapshot() -> [u64; 3] {
    [
        FAILURES.fs.load(Ordering::Relaxed),
        FAILURES.exec.load(Ordering::Relaxed),
        FAILURES.log.load(Ordering::Relaxed),
    ]
}

/// 父目录不存在时 `append_line` 返回错误，而不是静默丢弃。
///
/// # Errors
///
/// 断言不成立或临时目录创建失败时返回错误。
#[test]
fn append_line_reports_error() -> Result<(), Failure> {
    let tmp = TempDir::new().map_err(|_io_err| "临时目录")?;
    let missing = tmp.path().join("no-such-dir").join("x.log");
    ensure(append_line(&missing, "line").is_err(), "应返回错误")?;
    let ok_path = tmp.path().join("ok.log");
    ensure(append_line(&ok_path, "line").is_ok(), "可写路径应成功")?;
    let body = fs::read_to_string(&ok_path).map_err(|_io_err| "读回")?;
    ensure(body == "line\n", "内容应为一行")
}

/// 日志目录无法创建时 `log_msg` 不 panic、不递归，`log` 与 `fs` 计数各增 1。
///
/// # Errors
///
/// 断言不成立或临时目录准备失败时返回错误。
#[test]
fn log_msg_counts_write_failure() -> Result<(), Failure> {
    let _guard = counter_guard();
    let tmp = TempDir::new().map_err(|_io_err| "临时目录")?;
    let blocker = tmp.path().join("data");
    fs::write(&blocker, "not a dir").map_err(|_io_err| "准备文件")?;
    let env = env_at(&blocker);
    let [fs_before, exec_before, log_before] = snapshot();
    log::msg(&env, "INFO", "test", "hello");
    let [fs_after, exec_after, log_after] = snapshot();
    ensure(fs_after.checked_sub(fs_before) == Some(1), "fs 计数应增 1")?;
    ensure(
        log_after.checked_sub(log_before) == Some(1),
        "log 计数应增 1",
    )?;
    ensure(exec_after == exec_before, "exec 计数不变")
}

/// 日志可写时 `log_msg` 不增加任何计数。
///
/// # Errors
///
/// 断言不成立或临时目录准备失败时返回错误。
#[test]
fn log_msg_success_counts_nothing() -> Result<(), Failure> {
    let _guard = counter_guard();
    let tmp = TempDir::new().map_err(|_io_err| "临时目录")?;
    let env = env_at(&tmp.path().join("data"));
    let before = snapshot();
    log::msg(&env, "INFO", "test", "hello");
    ensure(snapshot() == before, "成功路径不应计数")?;
    let body = fs::read_to_string(env.module_log()).map_err(|_io_err| "读回")?;
    ensure(body.ends_with(" [INFO] [test] hello\n"), "日志行格式不变")
}

/// 计数到上限后保持上限，不回绕。
///
/// # Errors
///
/// 断言不成立时返回错误。
#[test]
fn note_failure_saturates() -> Result<(), Failure> {
    let counter = std::sync::atomic::AtomicU64::new(u64::MAX);
    note_failure(&counter);
    ensure(counter.load(Ordering::Relaxed) == u64::MAX, "应保持上限")?;
    let fresh = std::sync::atomic::AtomicU64::new(0);
    note_failure(&fresh);
    ensure(fresh.load(Ordering::Relaxed) == 1, "应加 1")
}

/// 计数全为 0 时不输出；任一非 0 时输出一行。
///
/// # Errors
///
/// 断言不成立时返回错误。
#[test]
fn failures_line_format() -> Result<(), Failure> {
    ensure(format_failures_line(0, 0, 0).is_empty(), "全 0 时为空")?;
    ensure(
        format_failures_line(1, 0, 2) == "best_effort_failures fs=1 exec=0 log=2\n",
        "非 0 时一行",
    )
}
