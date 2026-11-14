//! 用真实的 `xr` 二进制在临时目录里导出日志包与诊断包，检查压缩包内容。

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;
use xr_test_support::{Failure, ensure, ensure_eq, ensure_ok, ensure_some};

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

/// 准备模块目录、数据目录与各类日志：每个日志都带一行密码，供检查脱敏。
///
/// # Errors
///
/// 写文件失败时返回 [`Failure`]。
fn layout(root: &Path) -> Result<(), Failure> {
    put(
        &root.join("mod/module.prop"),
        "id=xrouter\nversion=v9.8.7\n",
    )?;
    let data = root.join("data");
    put(
        &data.join("config/config.conf"),
        "# 注释行\nXR_DEBUG=\"1\"\n# 另一行注释\nXR_MODE=\"global\"\n",
    )?;
    for (rel, body) in [
        ("log/module.log", "module 当前 password=hunter2\n"),
        ("log/module.log.1", "module 上一份\n"),
        ("log/vmm.log", "vmm 日志 https://example.com/x\n"),
        ("log/console.prev.log", "上次控制台\n"),
        ("log/debug.log", "debug 当前 token: abc123\n"),
        ("log/debug.log.1", "debug 上一份\n"),
        ("run/console.log", "控制台\n"),
        ("run/vm-ctl.log", "vm-ctl\n"),
        ("run/notifier.log", "通知器\n"),
    ] {
        put(&data.join(rel), body)?;
    }
    for sub in ["bin", "sysnet", "proc", "cg", "dl"] {
        ensure_ok(fs::create_dir_all(root.join(sub)), "建目录")?;
    }
    put(&root.join("uptime"), "100.00 50.00\n")?;
    put(&root.join("mounts"), "")
}

/// 运行一次导出子命令，返回压缩包路径。
///
/// # Errors
///
/// 运行失败、退出码非 0 或输出里没有压缩包路径时返回 [`Failure`]。
fn export(root: &Path, sub: &str) -> Result<PathBuf, Failure> {
    let out = ensure_ok(
        Command::new(env!("CARGO_BIN_EXE_xr"))
            .arg(sub)
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
            .env("XR_DOWNLOAD", root.join("dl"))
            .env("BB", root.join("no-busybox"))
            .env("XR_DAEMON_FOREGROUND", "1")
            .output(),
        &format!("运行 xr {sub}"),
    )?;
    ensure(out.status.success(), &format!("xr {sub} 退出码非 0"))?;
    let text = ensure_ok(String::from_utf8(out.stdout), "stdout 是 UTF-8")?;
    let val: Value = ensure_ok(serde_json::from_str(text.trim()), "输出是 JSON")?;
    ensure_eq(&val.get("ok"), &Some(&Value::Bool(true)), "ok 为 true")?;
    let path = ensure_some(val.get("path").and_then(Value::as_str), "输出含 path")?;
    Ok(PathBuf::from(path))
}

/// 压缩包内容。
struct Pack {
    /// 包内顶层目录名集合。
    tops: BTreeSet<String>,
    /// 每个文件的（文件名, 内容）。
    files: Vec<(String, String)>,
}

/// 读出压缩包。
///
/// # Errors
///
/// 打不开或读不出压缩包时返回 [`Failure`]。
fn read_zip(path: &Path) -> Result<Pack, Failure> {
    let file = ensure_ok(File::open(path), "打开压缩包")?;
    let mut zip = ensure_ok(zip::ZipArchive::new(file), "解析压缩包")?;
    let mut tops = BTreeSet::new();
    let mut files = Vec::new();
    for idx in 0..zip.len() {
        let mut entry = ensure_ok(zip.by_index(idx), "读条目")?;
        let full = entry.name().to_owned();
        let (top, name) = ensure_some(full.split_once('/'), "条目在顶层目录下")?;
        tops.extend(std::iter::once(top.to_owned()));
        let mut body = String::new();
        let read = ensure_ok(entry.read_to_string(&mut body), "读条目内容")?;
        ensure(read == body.len(), "条目内容读完整")?;
        files.push((name.to_owned(), body));
    }
    Ok(Pack { tops, files })
}

/// 按文件名取内容。
///
/// # Errors
///
/// 没有该文件时返回 [`Failure`]。
fn body_of<'a>(files: &'a [(String, String)], name: &str) -> Result<&'a str, Failure> {
    let found = files.iter().find(|(file, _)| file == name);
    ensure_some(
        found.map(|(_, body)| body.as_str()),
        &format!("包内有 {name}"),
    )
}

/// 摘要前三行：版本行、导出时间行、调试模式行。
///
/// # Errors
///
/// 格式不符时返回 [`Failure`]。
fn check_summary(summary: &str) -> Result<(), Failure> {
    let lines: Vec<&str> = summary.lines().collect();
    ensure_eq(
        &lines.first().copied(),
        &Some("XRouter v9.8.7 /  / Android "),
        "第 1 行：版本 / 机型 / 安卓版本（宿主机无 getprop 时为空）",
    )?;
    let time_line = ensure_some(lines.get(1).copied(), "有第 2 行")?;
    let stamp = ensure_some(
        time_line.strip_prefix("导出时间 "),
        "第 2 行以\"导出时间 \"开头",
    )?;
    ensure(
        stamp.len() == "2026-01-02 03:04:05 +0800".len()
            && stamp.as_bytes().get(4) == Some(&b'-')
            && stamp.as_bytes().get(10) == Some(&b' ')
            && stamp.as_bytes().get(19) == Some(&b' '),
        &format!("第 2 行时间格式：{stamp}"),
    )?;
    ensure_eq(&lines.get(2).copied(), &Some("调试模式 开"), "第 3 行")?;
    ensure(
        lines.get(3).is_some_and(|line| line.starts_with('{')),
        "第 4 行是状态 JSON",
    )
}

/// 日志包：只含摘要与各日志（含 `.1` 轮转份），顶层目录名为 `xrouter-logs-<时间>`，文本已脱敏。
///
/// # Errors
///
/// 导出失败或内容不符时返回 [`Failure`]。
#[test]
fn log_export_contents() -> Result<(), Failure> {
    let tmp = ensure_ok(TempDir::new(), "建临时目录")?;
    layout(tmp.path())?;
    let zip_path = export(tmp.path(), "log-export")?;
    ensure(
        zip_path.starts_with(tmp.path().join("dl")),
        "压缩包在下载目录",
    )?;
    let Pack { tops, files } = read_zip(&zip_path)?;
    ensure(
        tops.len() == 1 && tops.iter().all(|top| top.starts_with("xrouter-logs-")),
        &format!("顶层目录：{tops:?}"),
    )?;
    let names: BTreeSet<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    ensure_eq(
        &names,
        &BTreeSet::from([
            "console.log",
            "console.prev.log",
            "module.log",
            "module.log.1",
            "summary.txt",
            "vm-ctl.log",
            "vmm.log",
        ]),
        "日志包文件集合",
    )?;
    check_summary(body_of(&files, "summary.txt")?)?;
    ensure_eq(
        body_of(&files, "module.log")?,
        "module 当前 password=<已隐去>\n",
        "module.log 已脱敏",
    )?;
    ensure_eq(
        body_of(&files, "vmm.log")?,
        "vmm 日志 <URL>\n",
        "vmm.log 网址已脱敏",
    )?;
    ensure(
        !tmp.path().join("data/run").read_dir().is_ok_and(|mut rd| {
            rd.any(|ent| {
                ent.is_ok_and(|entry| entry.file_name().to_string_lossy().starts_with("export."))
            })
        }),
        "临时目录已清理",
    )
}

/// 诊断包：另含手机与客机快照、logcat、热点、去注释的配置、调试日志与通知器日志。
///
/// # Errors
///
/// 导出失败或内容不符时返回 [`Failure`]。
#[test]
fn diag_export_contents() -> Result<(), Failure> {
    let tmp = ensure_ok(TempDir::new(), "建临时目录")?;
    layout(tmp.path())?;
    let zip_path = export(tmp.path(), "diag-export")?;
    let Pack { tops, files } = read_zip(&zip_path)?;
    ensure(
        tops.len() == 1 && tops.iter().all(|top| top.starts_with("xrouter-diag-")),
        &format!("顶层目录：{tops:?}"),
    )?;
    let names: BTreeSet<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    ensure_eq(
        &names,
        &BTreeSet::from([
            "config.conf",
            "console.log",
            "console.prev.log",
            "debug.log",
            "debug.log.1",
            "logcat.txt",
            "module.log",
            "module.log.1",
            "notifier.log",
            "openwrt.txt",
            "phone.txt",
            "summary.txt",
            "tethering.txt",
            "vm-ctl.log",
            "vmm.log",
        ]),
        "诊断包文件集合",
    )?;
    check_summary(body_of(&files, "summary.txt")?)?;
    ensure_eq(
        body_of(&files, "config.conf")?,
        "XR_DEBUG=\"1\"\nXR_MODE=\"global\"\n",
        "配置已去掉注释行",
    )?;
    ensure_eq(
        body_of(&files, "debug.log")?,
        "debug 当前 token: <已隐去>\n",
        "debug.log 已脱敏",
    )?;
    ensure_eq(
        body_of(&files, "openwrt.txt")?,
        "### OpenWrt 快照：服务未运行\n",
        "虚拟机未运行时的客机快照说明",
    )?;
    ensure(
        body_of(&files, "phone.txt")?.starts_with("### 手机网络快照 "),
        "phone.txt 是手机快照",
    )
}
