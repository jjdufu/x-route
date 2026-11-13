use std::process::ExitCode;

fn exit_from_i32(code: i32) -> ExitCode {
    let byte = u8::try_from(code.clamp(0, 255)).unwrap_or(1);
    ExitCode::from(byte)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    exit_from_i32(xr::cli::run_cli(&args))
}
