//! 外部命令执行：统一的执行入口（记录调试日志、开机日志、失败计数）与结果类型。

use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::process::{self, Child, Command, ExitStatus, Stdio};

use crate::config::Config;
use crate::env::Env;
use crate::log::{FAILURES, dlog, dlog_env, note_failure};

/// 一次外部命令的结果。
#[derive(Debug, Clone)]
pub struct Output {
    /// 退出码：被信号终止时为 128 + 信号值，无法执行时为 127。
    pub(crate) code: i32,
    /// 标准输出（按 UTF-8 宽松解码）。
    pub(crate) stdout: String,
    /// 标准错误（按 UTF-8 宽松解码）。
    pub(crate) stderr: String,
}

impl Output {
    /// 退出码为 0 时返回 true。
    #[must_use]
    pub(crate) const fn success(&self) -> bool {
        self.code == 0
    }
}

/// 参数中含有修改类动词（`add`、`del`、`flush`、`set` 等）时返回 true，用于决定是否记入开机日志和调试日志。
#[must_use]
pub(crate) fn is_mutating_args(args: &[&str]) -> bool {
    args.iter().any(|arg| {
        matches!(
            *arg,
            "add"
                | "del"
                | "delete"
                | "replace"
                | "flush"
                | "set"
                | "change"
                | "append"
                | "prepend"
                | "-A"
                | "-I"
                | "-D"
                | "-F"
                | "-N"
                | "-X"
                | "-R"
                | "-P"
                | "-Z"
                | "enable"
                | "disable"
                | "remove"
                | "put"
        )
    })
}

/// 日志里显示的命令行：系统命令目录下的程序只写程序名，后接空格分隔的参数。
fn format_cmd_for_log(env: &Env, prog: &Path, args: &[&str]) -> String {
    let sysbin_prefix = format!("{}/", env.sysbin.to_string_lossy().trim_end_matches('/'));
    let prog_s = prog.to_string_lossy();
    let short_prog = prog_s.strip_prefix(&sysbin_prefix).unwrap_or(&prog_s);
    if args.is_empty() {
        short_prog.to_owned()
    } else {
        format!("{short_prog} {}", args.join(" "))
    }
}

/// 从进程结果取退出码：正常退出取退出码，被信号终止取 128 + 信号值，都取不到时为 1。
fn exit_code_from_output(out: &process::Output) -> i32 {
    out.status.code().unwrap_or_else(|| {
        out.status
            .signal()
            .map_or(1_i32, |sig| 128_i32.saturating_add(sig))
    })
}

/// 执行外部命令并收集输出，标准输入接空。
///
/// 修改类命令在开机日志存在时先记一行；调试开关打开时，失败、有错误输出或修改类的命令记入调试日志（只读命令的 141 除外）。`pass_stderr` 为 true 时把标准错误转发到本进程的标准错误。无法执行时返回退出码 127。
#[must_use]
pub fn run_xr(env: &Env, cfg: &Config, prog: &Path, args: &[&str], pass_stderr: bool) -> Output {
    let mutating = is_mutating_args(args);
    let boot_log = env.boot_log();
    if boot_log.is_file() && mutating {
        let cmd_str = format_cmd_for_log(env, prog, args);
        if let Ok(mut file) = OpenOptions::new().append(true).open(&boot_log)
            && writeln!(file, "  $ {cmd_str}").is_err()
        {
            note_failure(&FAILURES.log);
        }
    }

    let Ok(out) = Command::new(prog).args(args).stdin(Stdio::null()).output() else {
        if cfg.debug_on() {
            let cmd_str = format_cmd_for_log(env, prog, args);
            dlog(
                env,
                cfg,
                "cmd",
                &format!("rc=127 {cmd_str} | stderr: exec failed"),
            );
        }
        return Output {
            code: 127,
            stdout: String::new(),
            stderr: String::new(),
        };
    };

    let code = exit_code_from_output(&out);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    if cfg.debug_on() {
        let de: String = stderr
            .chars()
            .take(600)
            .map(|ch| if ch == '\n' { ' ' } else { ch })
            .collect();
        let de_trimmed = de.trim();
        let sigpipe_readonly = code == 141_i32 && de_trimmed.is_empty() && !mutating;
        if !sigpipe_readonly && (code != 0_i32 || !de_trimmed.is_empty() || mutating) {
            let cmd_str = format_cmd_for_log(env, prog, args);
            if de_trimmed.is_empty() {
                dlog(env, cfg, "cmd", &format!("rc={code} {cmd_str}"));
            } else {
                dlog(
                    env,
                    cfg,
                    "cmd",
                    &format!("rc={code} {cmd_str} | stderr: {de_trimmed}"),
                );
            }
        }
    }

    if pass_stderr
        && !stderr.is_empty()
        && let Err(err) = std::io::stderr().write_all(stderr.as_bytes())
    {
        note_failure(&FAILURES.log);
        dlog(env, cfg, "cmd", &format!("stderr 转发失败：{err}"));
    }

    Output {
        code,
        stdout,
        stderr,
    }
}

/// 读取一次命令结果的成败：非 0 退出或无法执行时计数到 `FAILURES.exec`。
///
/// 调试日志已由 [`run_xr`] 写过，这里不重复写。
pub(crate) fn note_exec_result(res: &Output) {
    if !res.success() {
        note_failure(&FAILURES.exec);
    }
}

/// 处理不经 [`run_xr`] 直接运行并等待的命令结果：非 0 退出或无法执行时计数，
/// 无法执行时（调试开关打开）写调试日志。调用方继续执行。
pub(crate) fn note_status(env: &Env, what: &str, res: std::io::Result<ExitStatus>) {
    match res {
        Ok(status) => {
            if !status.success() {
                note_failure(&FAILURES.exec);
            }
        }
        Err(err) => {
            note_failure(&FAILURES.exec);
            dlog_env(env, "cmd", &format!("{what} 无法执行：{err}"));
        }
    }
}

/// 处理后台启动、不等待的命令：启动成功时（调试开关打开）记下子进程 pid，子进程句柄随即释放，
/// 与原来不等待的语义相同；启动失败时计数并写调试日志。
pub(crate) fn note_spawn(env: &Env, what: &str, res: std::io::Result<Child>) {
    match res {
        Ok(child) => dlog_env(
            env,
            "cmd",
            &format!("{what} 已在后台启动 pid={}", child.id()),
        ),
        Err(err) => {
            note_failure(&FAILURES.exec);
            dlog_env(env, "cmd", &format!("{what} 无法启动：{err}"));
        }
    }
}
