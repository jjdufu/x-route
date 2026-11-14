//! 标准输出写入层：所有面向 `WebUI` / shell 的输出都经过这里，写失败以错误返回，不 panic。
//!
//! 原来的 `println` 宏 在 stdout 写失败（读端已关闭、磁盘满）时 panic，release 构建随即
//! SIGABRT（退出码 134）。这里改为返回错误，由调用方以 [`OUT_WRITE_FAILED`]（141）结束命令（D5-B）。

use std::io::{self, Write};

use crate::env::Env;
use crate::log::dlog_env;

#[cfg(test)]
mod tests;

/// stdout 写失败时命令的退出码（D5-B：141，沿用 shell 对 SIGPIPE 的 128+13 惯例）。
pub(crate) const OUT_WRITE_FAILED: i32 = 141;

/// 把 `text` 和一个换行写到 `sink` 并立即 flush。
///
/// # Errors
///
/// 写入或 flush 失败时返回对应的 `io::Error`。
pub(crate) fn line_to<W: Write>(sink: &mut W, text: &str) -> io::Result<()> {
    sink.write_all(text.as_bytes())?;
    sink.write_all(b"\n")?;
    sink.flush()
}

/// 把 `text` 原样（不加换行）写到 `sink` 并立即 flush。
///
/// # Errors
///
/// 写入或 flush 失败时返回对应的 `io::Error`。
pub(crate) fn raw_to<W: Write>(sink: &mut W, text: &str) -> io::Result<()> {
    sink.write_all(text.as_bytes())?;
    sink.flush()
}

/// 把 `text` 和一个换行写到进程 stdout（加锁）。
///
/// # Errors
///
/// 同 [`line_to`]。
pub(crate) fn line(text: &str) -> io::Result<()> {
    line_to(&mut io::stdout().lock(), text)
}

/// 把 `text` 原样写到进程 stdout（加锁）。
///
/// # Errors
///
/// 同 [`raw_to`]。
pub(crate) fn raw(text: &str) -> io::Result<()> {
    raw_to(&mut io::stdout().lock(), text)
}

/// 处理一次 stdout 写入的结果：成功返回 `rc`；失败时（调试开关打开）写调试日志并返回
/// [`OUT_WRITE_FAILED`]。
pub(crate) fn finish(env: &Env, res: io::Result<()>, rc: i32) -> i32 {
    match res {
        Ok(()) => rc,
        Err(err) => {
            dlog_env(env, "out", &format!("stdout 写失败: {err}"));
            OUT_WRITE_FAILED
        }
    }
}

/// 输出一行后以 `rc` 结束命令；写失败时以 [`OUT_WRITE_FAILED`] 结束。
pub(crate) fn emit(env: &Env, text: &str, rc: i32) -> i32 {
    finish(env, line(text), rc)
}

/// 原样输出（不加换行）后以 `rc` 结束命令；写失败时以 [`OUT_WRITE_FAILED`] 结束。
pub(crate) fn emit_raw(env: &Env, text: &str, rc: i32) -> i32 {
    finish(env, raw(text), rc)
}

/// 把多行按行输出（每行后一个换行，与逐行 `println` 宏 的字节相同）后以 `rc` 结束命令；
/// 没有任何行时不输出；写失败时以 [`OUT_WRITE_FAILED`] 结束。
pub(crate) fn emit_lines<I, S>(env: &Env, lines: I, rc: i32) -> i32
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    finish(env, raw(&join_lines(lines)), rc)
}

/// 把每行加上换行后拼接（没有行时为空串）。
pub(crate) fn join_lines<I, S>(lines: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut body = String::new();
    for item in lines {
        body.push_str(item.as_ref());
        body.push('\n');
    }
    body
}
