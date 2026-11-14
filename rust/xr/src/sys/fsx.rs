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
pub(crate) fn write_logged<P: AsRef<Path>, C: AsRef<[u8]>>(env: &Env, path: P, body: C) {
    if let Err(err) = fs::write(path.as_ref(), body) {
        record_failure(env, "write", path.as_ref(), &err);
    }
}

/// 内容与现有文件相同则不写；返回是否发生写入。
///
/// # Errors
///
/// 写入失败时返回底层 IO 错误；读取失败（含文件不存在）视为内容不同。
pub(crate) fn write_if_changed(path: &Path, body: &str) -> io::Result<bool> {
    if fs::read_to_string(path).is_ok_and(|old| old == body) {
        return Ok(false);
    }
    fs::write(path, body)?;
    Ok(true)
}

/// 与 [`write_if_changed`] 相同，失败时计数并记录、返回 `false`；返回是否发生写入。
pub(crate) fn write_if_changed_logged(env: &Env, path: &Path, body: &str) -> bool {
    match write_if_changed(path, body) {
        Ok(written) => written,
        Err(err) => {
            record_failure(env, "write", path, &err);
            false
        }
    }
}

/// 删除文件。文件本来就不存在视为目标已达成，不计数；其余失败计数并记录。
pub(crate) fn remove_logged<P: AsRef<Path>>(env: &Env, path: P) {
    if let Err(err) = fs::remove_file(path.as_ref())
        && err.kind() != ErrorKind::NotFound
    {
        record_failure(env, "remove", path.as_ref(), &err);
    }
}

/// 递归删除目录。目录本来就不存在视为目标已达成，不计数；其余失败计数并记录。
pub(crate) fn remove_tree_logged<P: AsRef<Path>>(env: &Env, path: P) {
    if let Err(err) = fs::remove_dir_all(path.as_ref())
        && err.kind() != ErrorKind::NotFound
    {
        record_failure(env, "remove_tree", path.as_ref(), &err);
    }
}

/// 递归创建目录。失败时计数并记录，调用方不会得到错误。
pub(crate) fn mkdirs_logged<P: AsRef<Path>>(env: &Env, path: P) {
    if let Err(err) = fs::create_dir_all(path.as_ref()) {
        record_failure(env, "mkdirs", path.as_ref(), &err);
    }
}

/// 复制文件内容到 `to`（覆盖）。复制的字节数没有调用方需要，成功即完成；失败时计数并记录。
pub(crate) fn copy_logged<P: AsRef<Path>, Q: AsRef<Path>>(env: &Env, from: P, to: Q) {
    if let Err(err) = fs::copy(from.as_ref(), to.as_ref()) {
        record_failure(env, "copy", from.as_ref(), &err);
    }
}

/// 设置权限位（如 `0o700`）。失败时计数并记录，调用方不会得到错误。
pub(crate) fn chmod_logged<P: AsRef<Path>>(env: &Env, path: P, mode: u32) {
    if let Err(err) = fs::set_permissions(path.as_ref(), Permissions::from_mode(mode)) {
        record_failure(env, "chmod", path.as_ref(), &err);
    }
}

/// 重命名（同一文件系统内移动）。失败时计数并记录，调用方不会得到错误。
pub(crate) fn rename_logged<P: AsRef<Path>, Q: AsRef<Path>>(env: &Env, from: P, to: Q) {
    if let Err(err) = fs::rename(from.as_ref(), to.as_ref()) {
        record_failure(env, "rename", from.as_ref(), &err);
    }
}

/// 创建空文件或把已有文件截断为空；打开的文件在函数内关闭。失败时计数并记录。
pub(crate) fn touch_logged<P: AsRef<Path>>(env: &Env, path: P) {
    if let Err(err) = File::create(path.as_ref()) {
        record_failure(env, "touch", path.as_ref(), &err);
    }
}
