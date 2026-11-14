//! 文件操作包装：成功不计数，失败计数一次，`NotFound` 的删除视为已达成。

use std::fs;

use tempfile::TempDir;

use super::{
    chmod_logged, copy_logged, mkdirs_logged, remove_logged, remove_tree_logged, rename_logged,
    touch_logged, write_if_changed, write_logged,
};
use xr_test_support::{Failure, ensure};

use crate::log::tests::{counter_guard, env_at, snapshot};

/// 全部包装在可写的临时目录里成功，且不增加计数。
///
/// # Errors
///
/// 断言不成立或临时目录准备失败时返回错误。
#[test]
fn wrappers_succeed_without_counting() -> Result<(), Failure> {
    let _guard = counter_guard();
    let tmp = TempDir::new().map_err(|_io_err| Failure::new("临时目录"))?;
    let env = env_at(&tmp.path().join("data"));
    let dir = tmp.path().join("a").join("b");
    let file = dir.join("f.txt");
    let copy = dir.join("g.txt");
    let moved = dir.join("h.txt");
    let before = snapshot();
    mkdirs_logged(&env, &dir);
    write_logged(&env, &file, b"body");
    copy_logged(&env, &file, &copy);
    chmod_logged(&env, &copy, 0o600);
    rename_logged(&env, &copy, &moved);
    touch_logged(&env, &file);
    let truncated = fs::read(&file).map_err(|_io_err| Failure::new("读回"))?;
    let moved_body = fs::read(&moved).map_err(|_io_err| Failure::new("读回"))?;
    remove_logged(&env, &moved);
    remove_tree_logged(&env, tmp.path().join("a"));
    ensure(snapshot() == before, "成功路径不应计数")?;
    ensure(truncated.is_empty(), "touch 应截断为空")?;
    ensure(moved_body == b"body", "复制后改名内容不变")?;
    ensure(!tmp.path().join("a").exists(), "目录树应已删除")
}

/// 删除不存在的文件或目录视为目标已达成，不计数。
///
/// # Errors
///
/// 断言不成立或临时目录准备失败时返回错误。
#[test]
fn remove_not_found_is_not_failure() -> Result<(), Failure> {
    let _guard = counter_guard();
    let tmp = TempDir::new().map_err(|_io_err| Failure::new("临时目录"))?;
    let env = env_at(&tmp.path().join("data"));
    let before = snapshot();
    remove_logged(&env, tmp.path().join("missing"));
    remove_tree_logged(&env, tmp.path().join("missing-dir"));
    ensure(snapshot() == before, "NotFound 不应计数")
}

/// 父目录不存在、目标是目录等失败各计数一次，调用方照常继续。
///
/// # Errors
///
/// 断言不成立或临时目录准备失败时返回错误。
#[test]
fn failures_are_counted_once_each() -> Result<(), Failure> {
    let _guard = counter_guard();
    let tmp = TempDir::new().map_err(|_io_err| Failure::new("临时目录"))?;
    let env = env_at(&tmp.path().join("data"));
    let missing_parent = tmp.path().join("no-dir").join("f");
    let a_dir = tmp.path().join("dir");
    fs::create_dir_all(&a_dir).map_err(|_io_err| Failure::new("准备目录"))?;
    let [fs_before, exec_before, _log_before] = snapshot();
    write_logged(&env, &missing_parent, b"x");
    copy_logged(&env, &missing_parent, a_dir.join("c"));
    rename_logged(&env, &missing_parent, a_dir.join("r"));
    touch_logged(&env, &missing_parent);
    chmod_logged(&env, &missing_parent, 0o600);
    remove_logged(&env, &a_dir);
    mkdirs_logged(&env, a_dir.join("f").join("g"));
    let [fs_after, exec_after, _log_after] = snapshot();
    ensure(
        fs_after.checked_sub(fs_before) == Some(6),
        "6 次失败应各计数一次（mkdirs 成功不计）",
    )?;
    ensure(exec_after == exec_before, "exec 计数不变")
}

/// `write_if_changed`：首次写入为 `Ok(true)`；内容相同为 `Ok(false)` 且文件未被改写；内容不同为 `Ok(true)`；父目录不存在为 `Err`。
///
/// # Errors
///
/// 断言不成立或临时目录准备失败时返回错误。
#[test]
fn write_if_changed_only_on_change() -> Result<(), Failure> {
    let tmp = TempDir::new().map_err(|_io_err| Failure::new("临时目录"))?;
    let file = tmp.path().join("r.txt");
    ensure(
        matches!(write_if_changed(&file, "a\n"), Ok(true)),
        "首次写入",
    )?;
    let first = fs::metadata(&file)
        .and_then(|meta| meta.modified())
        .map_err(|_io_err| Failure::new("读 mtime"))?;
    std::thread::sleep(std::time::Duration::from_millis(20));
    ensure(
        matches!(write_if_changed(&file, "a\n"), Ok(false)),
        "内容相同不写",
    )?;
    let second = fs::metadata(&file)
        .and_then(|meta| meta.modified())
        .map_err(|_io_err| Failure::new("读 mtime"))?;
    ensure(first == second, "内容相同时 mtime 不变")?;
    ensure(
        matches!(write_if_changed(&file, "b\n"), Ok(true)),
        "内容不同写入",
    )?;
    ensure(
        fs::read_to_string(&file).is_ok_and(|text| text == "b\n"),
        "新内容已写入",
    )?;
    ensure(
        write_if_changed(&tmp.path().join("no/such/r.txt"), "x").is_err(),
        "父目录不存在返回 Err",
    )
}
