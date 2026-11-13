use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Serialize;

use crate::config::Config;
use crate::env::{is_executable, Env};
use crate::log::log_info;

#[derive(Debug, Clone)]
pub struct NetCtx<'a> {
    pub env: &'a Env,
    pub cfg: &'a Config,
}

impl<'a> NetCtx<'a> {
    pub fn new(env: &'a Env, cfg: &'a Config) -> Self {
        Self { env, cfg }
    }

    pub fn ip(&self, args: &[&str]) -> crate::sys::exec::ExecResult {
        crate::sys::exec::xr_exec(self.env, self.cfg, &self.env.sysbin_cmd("ip"), args, true)
    }

    pub fn ip6(&self, args: &[&str]) -> crate::sys::exec::ExecResult {
        let mut full = Vec::with_capacity(args.len().saturating_add(1));
        full.push("-6");
        full.extend_from_slice(args);
        crate::sys::exec::xr_exec(self.env, self.cfg, &self.env.sysbin_cmd("ip"), &full, true)
    }

    pub fn ndc(&self, args: &[&str]) -> crate::sys::exec::ExecResult {
        crate::sys::exec::xr_exec(self.env, self.cfg, &self.env.sysbin_cmd("ndc"), args, true)
    }

    pub fn ndc_ok(&self, args: &[&str]) -> bool {
        let res = self.ndc(args);
        res.stdout.lines().any(|l| l.starts_with("200 "))
            || res.stderr.lines().any(|l| l.starts_with("200 "))
    }

    pub fn ipt(&self, args: &[&str]) -> crate::sys::exec::ExecResult {
        let mut full = Vec::with_capacity(args.len().saturating_add(1));
        full.push("-w");
        full.extend_from_slice(args);
        crate::sys::exec::xr_exec(
            self.env,
            self.cfg,
            &self.env.sysbin_cmd("iptables"),
            &full,
            true,
        )
    }

    pub fn ip6t(&self, args: &[&str]) -> crate::sys::exec::ExecResult {
        let mut full = Vec::with_capacity(args.len().saturating_add(1));
        full.push("-w");
        full.extend_from_slice(args);
        crate::sys::exec::xr_exec(
            self.env,
            self.cfg,
            &self.env.sysbin_cmd("ip6tables"),
            &full,
            true,
        )
    }

    pub fn sysw(&self, path: &Path, val: &str) -> bool {
        let is_mock = self.env.proc_dir != Path::new("/proc");
        if is_mock {
            if let Some(p) = path.parent() {
                let _ = fs::create_dir_all(p);
            }
        }
        let ok = OpenOptions::new()
            .write(true)
            .create(is_mock)
            .truncate(true)
            .open(path)
            .and_then(|mut f| writeln!(f, "{val}"))
            .is_ok();
        let proc_prefix = format!(
            "{}/",
            self.env.proc_dir.to_string_lossy().trim_end_matches('/')
        );
        let path_s = path.to_string_lossy();
        let short = path_s.strip_prefix(&proc_prefix).unwrap_or(&path_s);
        let rc = i32::from(!ok);
        crate::log::dlog(
            self.env,
            self.cfg,
            "sysctl",
            &format!("rc={rc} {short} = {val}"),
        );
        ok
    }

    pub fn lnt(&self) -> &'static str {
        let mode = fs::read_to_string(self.env.run_dir().join("net.mode")).unwrap_or_default();
        if mode.trim() == "ndc" {
            "local_network"
        } else {
            "97"
        }
    }
}

pub fn diag_snap_cmd_available(env: &Env) -> bool {
    is_executable(&env.moddir.join("bin/xr")) || std::env::current_exe().is_ok()
}

pub fn trigger_diag_snap_bg(env: &Env, cfg: &Config, reason: &str) {
    if !cfg.debug_on() {
        return;
    }
    let exe = env.self_exe();
    let _ = Command::new(exe)
        .args(["internal", "diag-snap", reason])
        .env("MODDIR", &env.moddir)
        .env("XR_DATA", &env.xr_data)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct TrafficCounters {
    pub up: u64,
    pub down: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrafficSummary {
    pub wan: TrafficCounters,
    pub lan: TrafficCounters,
    pub total: TrafficCounters,
    pub since: u64,
    pub counting: bool,
    pub ipv6: bool,
}

pub fn read_quad(path: &Path) -> [u64; 4] {
    let Ok(content) = fs::read_to_string(path) else {
        return [0; 4];
    };
    let Some(first_line) = content.lines().next() else {
        return [0; 4];
    };
    let mut vals = [0_u64; 4];
    let mut parts = first_line.split_whitespace();
    for slot in &mut vals {
        if let Some(tok) = parts.next() {
            if tok.bytes().all(|b| b.is_ascii_digit()) {
                *slot = tok.parse::<u64>().unwrap_or(0);
            }
        }
    }
    vals
}

pub fn write_quad(path: &Path, vals: [u64; 4]) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let pid = std::process::id();
    let tmp = path.with_extension(format!("tmp.{pid}"));
    let line = format!("{} {} {} {}\n", vals[0], vals[1], vals[2], vals[3]);
    if fs::write(&tmp, line).is_ok() {
        let _ = fs::rename(&tmp, path);
    } else {
        let _ = fs::remove_file(&tmp);
    }
}

pub fn combine_quad_add(a: [u64; 4], b: [u64; 4]) -> [u64; 4] {
    [
        a[0].saturating_add(b[0]),
        a[1].saturating_add(b[1]),
        a[2].saturating_add(b[2]),
        a[3].saturating_add(b[3]),
    ]
}

pub fn combine_quad_max(a: [u64; 4], b: [u64; 4]) -> [u64; 4] {
    [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

#[allow(clippy::cast_precision_loss)]
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut val = bytes as f64;
    let mut idx = 0_usize;
    while val >= 1024.0 && idx < 4 {
        val /= 1024.0;
        idx = idx.saturating_add(1);
    }
    if idx == 0 {
        format!("{bytes} B")
    } else {
        let unit = UNITS.get(idx).copied().unwrap_or("TB");
        format!("{val:.1} {unit}")
    }
}

pub fn log_net_mode_info(env: &Env, msg: &str) {
    log_info(env, "net", msg);
}
