//! 输出层：内容与换行正确，写失败以错误返回而不是 panic。

use std::io::{self, Write};

use xr_test_support::{Failure, ensure};

use super::{OUT_WRITE_FAILED, join_lines, line_to, raw_to};

/// 任何写入都失败的 writer（模拟读端已关闭的管道）。
#[derive(Debug)]
struct Broken;

impl Write for Broken {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }
}

/// `line_to` 写入文本加一个换行，`raw_to` 原样写入。
///
/// # Errors
///
/// 写入失败或内容不符时返回错误。
#[test]
fn line_and_raw_bytes() -> Result<(), Failure> {
    let mut sink: Vec<u8> = Vec::new();
    line_to(&mut sink, "{\"ok\":true}").map_err(|_io_err| Failure::new("写 Vec 不应失败"))?;
    raw_to(&mut sink, "R0\u{1f}S-").map_err(|_io_err| Failure::new("写 Vec 不应失败"))?;
    ensure(
        sink == b"{\"ok\":true}\nR0\x1fS-",
        "字节与 println/print 宏相同",
    )
}

/// 写到总是失败的 writer 时返回错误（不 panic），退出码常量为 141。
///
/// # Errors
///
/// 断言不成立时返回错误。
#[test]
fn broken_writer_is_error() -> Result<(), Failure> {
    let mut sink = Broken;
    ensure(line_to(&mut sink, "x").is_err(), "line_to 应返回错误")?;
    ensure(raw_to(&mut sink, "x").is_err(), "raw_to 应返回错误")?;
    ensure(OUT_WRITE_FAILED == 141_i32, "D5-B 退出码为 141")
}

/// 多行拼接：每行后一个换行，空迭代为空串。
///
/// # Errors
///
/// 断言不成立时返回错误。
#[test]
fn join_lines_matches_println_loop() -> Result<(), Failure> {
    ensure(
        join_lines(["a=1", "b=2"]) == "a=1\nb=2\n",
        "逐行 println 宏的字节",
    )?;
    ensure(join_lines(Vec::<String>::new()).is_empty(), "无行时不输出")
}
