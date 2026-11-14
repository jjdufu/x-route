//! 尽力而为的文件操作包装：失败时计数（`FAILURES.fs`），调试开关打开时写一行调试日志，然后继续。
//!
//! 调用方不会得到错误；需要在失败时改变流程的地方不要用这里的函数。

use std::fs::{self, File, Permissions};
use std::io::{self, ErrorKind};
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::atomic::Ordering;

use crate::env::Env;
use crate::log::{FAILURES, dlog_env, note_failure};

#[cfg(test)]
mod tests;

/// 记录一次文件操作失败：计数，并在调试开关打开时写调试日志（含累计次数）。
pub(crate) fn record_failure(env: &Env, op: &str, path: &Path, err: &io::Error) {
    note_failure(&FAILURES.fs);
    let total = FAILURES.fs.load(Ordering::Relaxed);
    dlog_env(
        env,
        "fsx",
        &format!("{op} {} 失败（本进程第 {total} 次）：{err}", path.display()),
    );
}

/// 写整个文件（覆盖）。失败时计数并记录，调用方不会得到错误。
pub(crate) fn write_logged(env: &Env, path: &Path, body: &[u8]) {
    if let Err(err) = fs::write(path, body) {
        record_failure(env, "write", path, &err);
    }
}

/// 删除文件。文件本来就不存在视为目标已达成，不计数；其余失败计数并记录。
pub(crate) fn remove_logged(env: &Env, path: &Path) {
    if let Err(err) = fs::remove_file(path)
        && err.kind() != ErrorKind::NotFound
    {
        record_failure(env, "remove", path, &err);
    }
}

/// 递归删除目录。目录本来就不存在视为目标已达成，不计数；其余失败计数并记录。
pub(crate) fn remove_tree_logged(env: &Env, path: &Path) {
    if let Err(err) = fs::remove_dir_all(path)
        && err.kind() != ErrorKind::NotFound
    {
        record_failure(env, "remove_tree", path, &err);
    }
}

/// 递归创建目录。失败时计数并记录，调用方不会得到错误。
pub(crate) fn mkdirs_logged(env: &Env, path: &Path) {
    if let Err(err) = fs::create_dir_all(path) {
        record_failure(env, "mkdirs", path, &err);
    }
}

/// 复制文件内容到 `to`（覆盖）。复制的字节数没有调用方需要，成功即完成；失败时计数并记录。
pub(crate) fn copy_logged(env: &Env, from: &Path, to: &Path) {
    if let Err(err) = fs::copy(from, to) {
        record_failure(env, "copy", from, &err);
    }
}

/// 设置权限位（如 `0o700`）。失败时计数并记录，调用方不会得到错误。
pub(crate) fn chmod_logged(env: &Env, path: &Path, mode: u32) {
    if let Err(err) = fs::set_permissions(path, Permissions::from_mode(mode)) {
        record_failure(env, "chmod", path, &err);
    }
}

/// 重命名（同一文件系统内移动）。失败时计数并记录，调用方不会得到错误。
pub(crate) fn rename_logged(env: &Env, from: &Path, to: &Path) {
    if let Err(err) = fs::rename(from, to) {
        record_failure(env, "rename", from, &err);
    }
}

/// 创建空文件或把已有文件截断为空；打开的文件在函数内关闭。失败时计数并记录。
pub(crate) fn touch_logged(env: &Env, path: &Path) {
    if let Err(err) = File::create(path) {
        record_failure(env, "touch", path, &err);
    }
}
