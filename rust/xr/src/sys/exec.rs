use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use crate::config::Config;
use crate::env::Env;
use crate::log::dlog;

#[derive(Debug, Clone)]
pub struct ExecResult {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl ExecResult {
    pub fn success(&self) -> bool {
        self.code == 0
    }
}

pub fn is_mutating_args(args: &[&str]) -> bool {
    args.iter().any(|a| {
        matches!(
            *a,
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

fn format_cmd_for_log(env: &Env, prog: &Path, args: &[&str]) -> String {
    let sysbin_prefix = format!("{}/", env.sysbin.to_string_lossy().trim_end_matches('/'));
    let prog_s = prog.to_string_lossy();
    let short_prog = prog_s.strip_prefix(&sysbin_prefix).unwrap_or(&prog_s);
    if args.is_empty() {
        short_prog.to_string()
    } else {
        format!("{short_prog} {}", args.join(" "))
    }
}

fn exit_code_from_output(out: &Output) -> i32 {
    if let Some(code) = out.status.code() {
        code
    } else if let Some(sig) = out.status.signal() {
        128_i32.saturating_add(sig)
    } else {
        1
    }
}

pub fn xr_exec(
    env: &Env,
    cfg: &Config,
    prog: &Path,
    args: &[&str],
    pass_stderr: bool,
) -> ExecResult {
    let mutating = is_mutating_args(args);
    let boot_log = env.boot_log();
    if boot_log.is_file() && mutating {
        let cmd_str = format_cmd_for_log(env, prog, args);
        if let Ok(mut f) = OpenOptions::new().append(true).open(&boot_log) {
            let _ = writeln!(f, "  $ {cmd_str}");
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
        return ExecResult {
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
            .map(|c| if c == '\n' { ' ' } else { c })
            .collect();
        let de_trimmed = de.trim();
        let sigpipe_readonly = code == 141 && de_trimmed.is_empty() && !mutating;
        if !sigpipe_readonly && (code != 0 || !de_trimmed.is_empty() || mutating) {
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

    if pass_stderr && !stderr.is_empty() {
        let _ = std::io::stderr().write_all(stderr.as_bytes());
    }

    ExecResult {
        code,
        stdout,
        stderr,
    }
}

pub fn xr_exec_quiet(env: &Env, cfg: &Config, prog: &Path, args: &[&str]) -> ExecResult {
    xr_exec(env, cfg, prog, args, false)
}
