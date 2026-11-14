//! 用真实的 `xr` 二进制在临时目录里构造状态，比较 `status-json` 输出与金样（临时目录路径归一为 `TMP`）。
//!
//! 设置环境变量 `XR_GOLDEN_RECORD=1` 时改为录制金样。

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;
use xr_test_support::{Failure, ensure, ensure_eq, ensure_ok};

/// "已停止"状态的金样。
const STOPPED: &str = include_str!("../fixtures/status_stopped.json");
/// "运行中"状态的金样。
const RUNNING: &str = include_str!("../fixtures/status_running.json");
/// 只有旧版单份 `OpenVPN` 配置（没有 `config/vpn/` 目录）时的金样。
const LEGACY: &str = include_str!("../fixtures/status_legacy_vpn.json");

/// 写一个文件，先建好父目录。
///
/// # Errors
///
/// 建目录或写文件失败时返回 [`Failure`]。
fn put(path: &Path, body: &str) -> Result<(), Failure> {
    if let Some(parent) = path.parent() {
        ensure_ok(
            fs::create_dir_all(parent),
            &format!("建 {}", parent.display()),
        )?;
    }
    ensure_ok(fs::write(path, body), &format!("写 {}", path.display()))
}

/// 准备"已停止"的数据目录：默认配置、空的 `sysbin`、`sysnet` 与 `proc` 替身。
///
/// # Errors
///
/// 准备文件失败时返回 [`Failure`]。
fn base_layout(root: &Path) -> Result<(), Failure> {
    let conf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../module/config/config.conf");
    let conf_text = ensure_ok(fs::read_to_string(conf), "读默认配置")?;
    put(&root.join("data/config/config.conf"), &conf_text)?;
    for dir in [
        "data/run",
        "data/log",
        "mod",
        "media/Download",
        "bin",
        "sysnet",
        "proc",
    ] {
        ensure_ok(fs::create_dir_all(root.join(dir)), &format!("建 {dir}"))?;
    }
    put(&root.join("uptime"), "1000.50 10.00\n")
}

/// 在"已停止"的基础上构造"运行中"：替身 `crosvm` 进程、稳定状态、就绪时刻、`VPN` 配置与隧道、共享与 `OpenClash` 状态。
///
/// # Errors
///
/// 准备文件失败时返回 [`Failure`]。
fn running_layout(root: &Path) -> Result<(), Failure> {
    let run = root.join("data/run");
    put(&root.join("proc/4242/cmdline"), "crosvm\0run\0")?;
    put(
        &root.join("proc/4242/stat"),
        "4242 (crosvm) S 1 4242 4242 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 100 0 0\n",
    )?;
    put(&root.join("proc/uptime"), "160.00 10.00\n")?;
    put(&run.join("crosvm.pid"), "4242\n")?;
    put(&run.join("state"), "stable\n")?;
    put(&run.join("ready_at"), "100\n")?;
    put(&run.join("routes.active"), "10.8.0.0/24\n")?;
    put(&run.join("share.mode"), "ro\n")?;
    put(
        &run.join("guest/guest/openclash.status"),
        "installed=1\nrunning=1\nport=9095\n",
    )?;
    put(&run.join("guest/guest/ipv6"), "1\n")?;
    put(
        &run.join("guest/guest/openvpn.routes"),
        "10.8.0.0/24,10.9.0.0/24\n",
    )?;
    put(&run.join("guest/guest/openvpn.status"), "up\n")?;
    put(
        &run.join("guest/guest/openvpn.tunnels"),
        "tun0|office|10.8.0.6/24|1400|10.8.0.0/24,10.9.0.0/24\ntun1|home|10.20.0.2/24|1500|\n",
    )?;
    let vpn = root.join("data/config/vpn");
    put(
        &vpn.join("office.ovpn"),
        "client\nremote vpn.example.com 1194\n",
    )?;
    put(&vpn.join("office.name"), "办公室\n")?;
    put(&vpn.join("office.enabled"), "1\n")?;
    put(
        &vpn.join("home.ovpn"),
        "client\nremote home.example.org 1194\n",
    )?;
    put(&vpn.join("home.enabled"), "0\n")?;
    put(
        &vpn.join("spare.ovpn"),
        "client\nremote spare.example.net 1194\n",
    )
}

/// 用临时目录里的环境变量运行一次 `xr status-json`，返回把临时目录路径换成 `TMP` 后的标准输出。
///
/// # Errors
///
/// 运行失败、退出码非 0 或输出不是 UTF-8 时返回 [`Failure`]。
fn status_json(root: &Path) -> Result<String, Failure> {
    let out = ensure_ok(
        Command::new(env!("CARGO_BIN_EXE_xr"))
            .arg("status-json")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("MODDIR", root.join("mod"))
            .env("XR_DATA", root.join("data"))
            .env("XR_MEDIA", root.join("media"))
            .env("XR_SYSBIN", root.join("bin"))
            .env("XR_SYSNET", root.join("sysnet"))
            .env("XR_PROC_BASE", root.join("proc"))
            .env("XR_PROC_UPTIME", root.join("uptime"))
            .env("XR_PROC_MOUNTS", root.join("mounts"))
            .env("XR_CGROUP_ROOT", root.join("cg"))
            .env("BB", root.join("no-busybox"))
            .env("XR_DAEMON_FOREGROUND", "1")
            .output(),
        "运行 xr status-json",
    )?;
    ensure(out.status.success(), "xr status-json 退出码非 0")?;
    let text = ensure_ok(String::from_utf8(out.stdout), "stdout 是 UTF-8")?;
    Ok(text.replace(&root.to_string_lossy().into_owned(), "TMP"))
}

/// 比较输出与金样（按 JSON 值比较，键集合与取值都一致）；`XR_GOLDEN_RECORD=1` 时把输出写成金样。
///
/// # Errors
///
/// 输出不是 JSON、录制失败或与金样不一致时返回 [`Failure`]。
fn check_golden(got: &str, golden: &str, name: &str) -> Result<(), Failure> {
    if std::env::var_os("XR_GOLDEN_RECORD").is_some() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        return ensure_ok(fs::write(path, got), "录制金样");
    }
    let got_val: Value = ensure_ok(serde_json::from_str(got.trim()), "输出是 JSON")?;
    let want_val: Value = ensure_ok(serde_json::from_str(golden.trim()), "金样是 JSON")?;
    ensure_eq(&got_val, &want_val, &format!("{name} 与输出一致"))
}

/// 虚拟机已停止时，`status-json` 与金样一致。
///
/// # Errors
///
/// 准备失败或输出与金样不一致时返回 [`Failure`]。
#[test]
fn stopped_matches_golden() -> Result<(), Failure> {
    let tmp = ensure_ok(TempDir::new(), "建临时目录")?;
    base_layout(tmp.path())?;
    let got = status_json(tmp.path())?;
    check_golden(&got, STOPPED, "status_stopped.json")
}

/// 虚拟机稳定运行、有 `VPN` 配置与隧道、开启共享时，`status-json` 与金样一致。
///
/// # Errors
///
/// 准备失败或输出与金样不一致时返回 [`Failure`]。
#[test]
fn running_matches_golden() -> Result<(), Failure> {
    let tmp = ensure_ok(TempDir::new(), "建临时目录")?;
    base_layout(tmp.path())?;
    running_layout(tmp.path())?;
    let got = status_json(tmp.path())?;
    check_golden(&got, RUNNING, "status_running.json")
}

/// 只有旧版单份 `config/openvpn.ovpn`、没有 `openvpn.enabled` 时，`status-json` 与金样一致。
///
/// # Errors
///
/// 准备失败或输出与金样不一致时返回 [`Failure`]。
#[test]
fn legacy_vpn_matches_golden() -> Result<(), Failure> {
    let tmp = ensure_ok(TempDir::new(), "建临时目录")?;
    base_layout(tmp.path())?;
    put(
        &tmp.path().join("data/config/openvpn.ovpn"),
        "client\nremote old.example.com 1194\n",
    )?;
    let got = status_json(tmp.path())?;
    check_golden(&got, LEGACY, "status_legacy_vpn.json")
}
