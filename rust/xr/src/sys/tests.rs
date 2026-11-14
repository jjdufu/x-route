//! `PathLock`：并发互斥、stale pid 清理、旧版本目录锁的兼容、超时接管与 Drop 释放。

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use super::{LockWait, PathLock};
use crate::log::tests::{Failure, ensure, env_at};

/// 已不存在的进程号（远大于 Linux 默认 `pid_max` 4194304）。
const DEAD_PID: &str = "999999999\n";

/// 不查 pid、等待 `max_iters` 轮、每轮 `sleep_ms` 毫秒的参数。
const fn plain(max_iters: u32, sleep_ms: u64) -> LockWait {
    LockWait {
        max_iters,
        sleep_ms,
        check_pid: false,
    }
}

/// 取同一把锁 50 次；进入临界区时已有别的线程在内、或离开时计数不是 1，都记为重叠。
fn hammer(env: &crate::env::Env, path: &Path, inside: &AtomicU32, overlap: &AtomicU32) {
    for _round in 0_u32..50 {
        let held = PathLock::acquire(env, path.to_path_buf(), plain(10_000, 1));
        let before = inside.fetch_add(1, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(1));
        let leaving = inside.fetch_sub(1, Ordering::SeqCst);
        if before != 0 || leaving != 1 {
            overlap.store(1, Ordering::SeqCst);
        }
        drop(held);
    }
}

/// 8 个线程各取同一把锁 50 次：任何时刻临界区内最多 1 个线程；结束后锁文件不残留。
///
/// # Errors
///
/// 断言不成立、临时目录准备失败或线程异常结束时返回错误。
#[test]
fn concurrent_threads_are_mutually_exclusive() -> Result<(), Failure> {
    let tmp = TempDir::new().map_err(|_io_err| "临时目录")?;
    let env = env_at(&tmp.path().join("data"));
    let lock_path = tmp.path().join("run").join("acct.lock");
    let inside = Arc::new(AtomicU32::new(0));
    let overlap = Arc::new(AtomicU32::new(0));
    let mut workers = Vec::new();
    for _worker in 0_u32..8 {
        let env_w = env.clone();
        let path_w = lock_path.clone();
        let inside_w = Arc::clone(&inside);
        let overlap_w = Arc::clone(&overlap);
        workers.push(thread::spawn(move || {
            hammer(&env_w, &path_w, &inside_w, &overlap_w);
        }));
    }
    for handle in workers {
        handle.join().map_err(|_panic| "工作线程异常结束")?;
    }
    ensure(
        overlap.load(Ordering::SeqCst) == 0,
        "临界区内同时出现过 2 个线程",
    )?;
    ensure(!lock_path.exists(), "全部释放后锁文件不应残留")
}

/// 锁文件里记录的持有者已退出（stale pid）：`check_pid` 时立即接管，不等待。
///
/// # Errors
///
/// 断言不成立或临时目录准备失败时返回错误。
#[test]
fn stale_pid_file_is_taken_at_once() -> Result<(), Failure> {
    let tmp = TempDir::new().map_err(|_io_err| "临时目录")?;
    let env = env_at(&tmp.path().join("data"));
    let lock_path = tmp.path().join(".net.lock");
    fs::write(&lock_path, DEAD_PID).map_err(|_io_err| "写旧锁")?;
    let started = Instant::now();
    let held = PathLock::acquire(
        &env,
        lock_path.clone(),
        LockWait {
            max_iters: 120,
            sleep_ms: 500,
            check_pid: true,
        },
    );
    ensure(started.elapsed() < Duration::from_millis(400), "不应等待")?;
    let owner = fs::read_to_string(&lock_path).map_err(|_io_err| "读锁文件")?;
    ensure(
        owner.trim() == std::process::id().to_string(),
        "锁文件记录本进程 pid",
    )?;
    drop(held);
    ensure(!lock_path.exists(), "Drop 后锁文件被删除")
}

/// 旧版本留下的目录锁、其中 `pid` 指向已退出的进程：立即清理目录并改为锁文件。
///
/// # Errors
///
/// 断言不成立或临时目录准备失败时返回错误。
#[test]
fn stale_legacy_dir_lock_is_replaced() -> Result<(), Failure> {
    let tmp = TempDir::new().map_err(|_io_err| "临时目录")?;
    let env = env_at(&tmp.path().join("data"));
    let lock_path = tmp.path().join(".net.lock");
    fs::create_dir_all(&lock_path).map_err(|_io_err| "建旧目录锁")?;
    fs::write(lock_path.join("pid"), DEAD_PID).map_err(|_io_err| "写旧 pid")?;
    let started = Instant::now();
    let held = PathLock::acquire(
        &env,
        lock_path.clone(),
        LockWait {
            max_iters: 120,
            sleep_ms: 500,
            check_pid: true,
        },
    );
    ensure(started.elapsed() < Duration::from_millis(400), "不应等待")?;
    ensure(lock_path.is_file(), "旧目录被删，新锁是文件")?;
    drop(held);
    ensure(!lock_path.exists(), "Drop 后锁文件被删除")
}

/// 旧版本留下的空目录锁（无 pid、不查 pid）：等满 `max_iters` 轮后强制接管。
///
/// # Errors
///
/// 断言不成立或临时目录准备失败时返回错误。
#[test]
fn legacy_dir_lock_is_forced_after_timeout() -> Result<(), Failure> {
    let tmp = TempDir::new().map_err(|_io_err| "临时目录")?;
    let env = env_at(&tmp.path().join("data"));
    let lock_path = tmp.path().join("acct.lock");
    fs::create_dir_all(&lock_path).map_err(|_io_err| "建旧目录锁")?;
    let started = Instant::now();
    let held = PathLock::acquire(&env, lock_path.clone(), plain(2, 50));
    ensure(
        started.elapsed() >= Duration::from_millis(100),
        "应先等 2 轮",
    )?;
    ensure(lock_path.is_file(), "强制接管后锁是文件")?;
    drop(held);
    ensure(!lock_path.exists(), "Drop 后锁文件被删除")
}

/// 锁被另一个持有者占着、`max_iters = 1`：等 1 轮后强制接管并返回（与原目录锁一致）。
///
/// # Errors
///
/// 断言不成立或临时目录准备失败时返回错误。
#[test]
fn held_lock_is_forced_after_timeout() -> Result<(), Failure> {
    let tmp = TempDir::new().map_err(|_io_err| "临时目录")?;
    let env = env_at(&tmp.path().join("data"));
    let lock_path = tmp.path().join(".snap.lock");
    let first = PathLock::acquire(&env, lock_path.clone(), plain(1, 10));
    let started = Instant::now();
    let second = PathLock::acquire(&env, lock_path.clone(), plain(1, 50));
    ensure(
        started.elapsed() >= Duration::from_millis(50),
        "应先等 1 轮",
    )?;
    ensure(lock_path.is_file(), "接管后锁文件存在")?;
    drop(second);
    drop(first);
    ensure(!Path::new(&lock_path).exists(), "全部释放后锁文件不残留")
}
