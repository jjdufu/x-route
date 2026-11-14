//! `xr` 命令行程序：把参数交给 [`xr::cli::run`]，并把返回值转成进程退出码。

use std::process::ExitCode;

/// 把 `i32` 退出码转成进程退出码：先限制在 0 到 255 之间。
fn exit_from_i32(code: i32) -> ExitCode {
    let byte = u8::try_from(code.clamp(0_i32, 255_i32)).unwrap_or(1);
    ExitCode::from(byte)
}

/// 程序入口：跳过程序名，把其余参数交给命令行分发。
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    exit_from_i32(xr::cli::run(&args))
}
