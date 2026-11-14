//! 配置备份 / 恢复：`xr config-export` 生成明文 zip（手机侧配置、`VPN` 配置与 `OpenWrt` 的 `sysupgrade` 备份），`xr config-import` 完整校验后先还原 `OpenWrt`、再写手机侧配置。
//!
//! 客机侧由 `xrouter-backup` 按 `host/backup.cmd` 执行，回执写在 `guest/backup.ack`（首行回显 ts）。

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::cli::bg_spawn;
use crate::cli::vpn_cfg::{valid_profile_id, vpn_mirror_legacy};
use crate::config::{BACKUP_EXCLUDE, Config, KEYS, set_kv, valid_kv};
use crate::daemon::vm_stop;
use crate::diag::{download_dir, module_version};
use crate::env::Env;
use crate::log;
use crate::log::{local_now, unix_now};
use crate::sys::fsx;
use crate::vm;

#[cfg(test)]
mod tests;

/// 备份包格式版本。
const FORMAT: u64 = 1;
/// 清单中的 `kind`。
const KIND: &str = "xrouter-config";
/// 导入文件大小上限（64 MiB）。
const MAX_ZIP: u64 = 64 * 1024 * 1024;
/// 导出时等待客机回执的上限。
const EXPORT_WAIT: Duration = Duration::from_secs(30);
/// 导入时等待客机回执的上限。
const IMPORT_WAIT: Duration = Duration::from_secs(60);
/// 等待回执的轮询间隔。
const POLL: Duration = Duration::from_millis(200);
/// 手机侧配置条目。
const CONF_ENTRY: &str = "phone/config.conf";
/// `OpenWrt` 备份条目。
const OPENWRT_ENTRY: &str = "openwrt/backup.tar.gz";
/// 清单条目。
const MANIFEST_ENTRY: &str = "manifest.json";
/// `VPN` 配置条目前缀。
const VPN_PREFIX: &str = "phone/vpn/";
/// 模拟校验不通过的键记录的原因。
const REASON_INVALID: &str = "取值不合法";
/// 写入配置文件失败的键记录的原因。
const REASON_WRITE: &str = "写入失败";

/// 读取备份包时的限额。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    /// 条目数上限。
    pub(crate) entries: usize,
    /// 解压后总字节数上限。
    pub(crate) unpacked: u64,
}

/// 正式限额：512 个条目、解压后 128 MiB。
const LIMITS: Limits = Limits {
    entries: 512,
    unpacked: 128 * 1024 * 1024,
};

/// 备份流程中的失败；`Display` 是给用户看的文案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fault {
    /// 虚拟机未运行。
    Stopped,
    /// 系统盘没有 `backup.cap`。
    Unsupported,
    /// 客机回执 `err <原因>`。
    Guest(String),
    /// 客机未在时限内回执。
    NoReply,
    /// 导入路径不合法或文件不可用。
    Path(&'static str),
    /// 备份包结构或清单不合法。
    Package(&'static str),
    /// 内容与清单的 sha256 不符。
    Checksum,
    /// 备份包无法打开或条目无法解压（附原始错误，只记入调试日志）。
    Unreadable(String),
    /// 本地读写失败：给用户看的文案与原始错误（只记入调试日志）。
    Write(&'static str, String),
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stopped => f.write_str("服务未运行"),
            Self::Unsupported => f.write_str("当前系统盘不支持配置备份，请先更新系统盘"),
            Self::Guest(reason) => write!(f, "OpenWrt：{reason}"),
            Self::NoReply => f.write_str("路由系统未响应"),
            Self::Path(msg) | Self::Write(msg, _) => f.write_str(msg),
            Self::Package(msg) => write!(f, "备份包无效：{msg}"),
            Self::Checksum => f.write_str("校验失败：备份包内容与清单不符"),
            Self::Unreadable(_) => f.write_str("校验失败：备份包已损坏"),
        }
    }
}

/// 把结果转成（退出码, JSON 文本）；失败的原始错误记入调试日志。
fn respond(env: &Env, res: Result<Value, Fault>) -> (i32, String) {
    match res {
        Ok(value) => (0, value.to_string()),
        Err(fault) => {
            log::dlog_env(env, "backup", &format!("失败：{fault:?}"));
            (
                1,
                json!({"ok": false, "error": fault.to_string()}).to_string(),
            )
        }
    }
}

/// 字节串的 sha256，64 个小写十六进制字符。
#[must_use]
pub(crate) fn sha_hex(body: &[u8]) -> String {
    Sha256::digest(body)
        .iter()
        .flat_map(|byte| [byte >> 4_u8, byte & 0x0f_u8])
        .filter_map(|nibble| char::from_digit(u32::from(nibble), 16))
        .collect()
}

/// 前置检查：虚拟机运行且系统盘有 `backup.cap`。
///
/// # Errors
///
/// 未运行返回 [`Fault::Stopped`]，旧系统盘返回 [`Fault::Unsupported`]。
fn precheck(env: &Env) -> Result<(), Fault> {
    if !vm::running(env) {
        return Err(Fault::Stopped);
    }
    if !env.run_dir().join("guest/guest/backup.cap").is_file() {
        return Err(Fault::Unsupported);
    }
    Ok(())
}

/// 解析客机回执：首行不是 `ts=<ts>` 或缺少状态行时为 `None`；状态 `ok` 时为其余各行，`err <原因>` 时为原因。
#[must_use]
pub(crate) fn parse_ack(body: &str, ts: &str) -> Option<Result<Vec<String>, String>> {
    let mut lines = body.lines();
    if lines.next()?.strip_prefix("ts=")? != ts {
        return None;
    }
    let status = lines.next()?;
    if status == "ok" {
        return Some(Ok(lines.map(str::to_owned).collect()));
    }
    Some(Err(status
        .strip_prefix("err")
        .map_or(status, str::trim)
        .to_owned()))
}

/// 向客机发 `<verb> <ts>` 并等回执，最多 `wait`。
///
/// # Errors
///
/// 客机回 `err` 时返回 [`Fault::Guest`]，超时返回 [`Fault::NoReply`]。
fn guest_call(env: &Env, verb: &str, wait: Duration) -> Result<Vec<String>, Fault> {
    let ts = format!("{}{}", unix_now(), std::process::id());
    fsx::write_logged(
        env,
        env.run_dir().join("guest/host/backup.cmd"),
        format!("{verb} {ts}\n"),
    );
    let ack = env.run_dir().join("guest/guest/backup.ack");
    let start = Instant::now();
    loop {
        let reply = fs::read_to_string(&ack)
            .ok()
            .and_then(|body| parse_ack(&body, &ts));
        if let Some(res) = reply {
            return res.map_err(Fault::Guest);
        }
        let left = wait.saturating_sub(start.elapsed());
        if left.is_zero() {
            return Err(Fault::NoReply);
        }
        thread::sleep(POLL.min(left));
    }
}

/// 手机侧配置文本：[`KEYS`] 去掉 [`BACKUP_EXCLUDE`]，按 [`KEYS`] 顺序输出原文件中出现过的键，每行 `KEY="VAL"`。
#[must_use]
pub(crate) fn phone_conf(text: &str) -> String {
    let cfg = Config::from_conf_str(text);
    KEYS.iter()
        .filter(|key| !BACKUP_EXCLUDE.contains(key))
        .filter_map(|key| cfg.get(key).map(|val| format!("{key}=\"{val}\"\n")))
        .collect()
}

/// 可导出的 `VPN` 配置编号：编号合法且 `.ovpn` 非空，按名字排序。
fn vpn_ids(vpn_dir: &Path) -> Vec<String> {
    let mut ids: Vec<String> = fs::read_dir(vpn_dir).ok().map_or_default(|rd| {
        rd.flatten()
            .map(|ent| ent.path())
            .filter(|path| path.extension() == Some(OsStr::new("ovpn")))
            .filter(|path| fs::metadata(path).is_ok_and(|meta| meta.len() > 0))
            .filter_map(|path| path.file_stem().and_then(OsStr::to_str).map(str::to_owned))
            .filter(|id| valid_profile_id(id))
            .collect()
    });
    ids.sort();
    ids
}

/// 本地读写失败的统一映射：保留原始错误。
///
/// # Errors
///
/// `res` 为错误时返回 [`Fault::Write`]。
fn write_err<T>(res: std::io::Result<T>, msg: &'static str) -> Result<T, Fault> {
    res.map_err(|err| Fault::Write(msg, err.to_string()))
}

/// 暂存导出内容：`phone/config.conf`、`phone/vpn/*` 与客机生成的 `openwrt/backup.tar.gz`，返回 `VPN` 配置份数。
///
/// # Errors
///
/// 任一文件写入或取回失败时返回 [`Fault::Write`]。
fn stage_export(env: &Env, stage: &Path) -> Result<usize, Fault> {
    let phone = stage.join("phone");
    let vpn = phone.join("vpn");
    let openwrt = stage.join("openwrt");
    for dir in [&vpn, &openwrt] {
        write_err(fs::create_dir_all(dir), "暂存目录创建失败")?;
    }
    let text = fs::read_to_string(env.conf_file()).unwrap_or_default();
    write_err(
        fs::write(phone.join("config.conf"), phone_conf(&text)),
        "暂存配置失败",
    )?;
    let src = env.vpn_dir();
    let ids = vpn_ids(&src);
    for id in &ids {
        for ext in ["ovpn", "enabled", "name"] {
            let from = src.join(format!("{id}.{ext}"));
            if from.is_file() {
                let copied = fs::copy(&from, vpn.join(format!("{id}.{ext}")));
                write_err(copied.map(|_bytes| ()), "暂存 VPN 配置失败")?;
            }
        }
    }
    let guest_tar = env.run_dir().join("guest/guest/backup.tar.gz");
    let fetched = fs::copy(&guest_tar, openwrt.join("backup.tar.gz"));
    write_err(fetched.map(|_bytes| ()), "取回 OpenWrt 备份失败")?;
    fsx::remove_logged(env, &guest_tar);
    Ok(ids.len())
}

/// 递归收集 `dir` 下的普通文件（相对路径用 `/` 分隔），最多下探 3 层。
///
/// # Errors
///
/// 目录不可读时返回 [`Fault::Write`]。
fn collect_files(dir: &Path, prefix: &str, out: &mut Vec<String>, depth: u8) -> Result<(), Fault> {
    let rd = write_err(fs::read_dir(dir), "读取暂存目录失败")?;
    for ent in rd.flatten() {
        let path = ent.path();
        let Some(name) = path.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        let rel = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        if path.is_dir() && depth < 3_u8 {
            collect_files(&path, &rel, out, depth.saturating_add(1))?;
        } else if path.is_file() {
            out.push(rel);
        } else {
            // 更深的目录与其他类型不打包
        }
    }
    Ok(())
}

/// `root` 下全部文件的相对路径，排序后返回。
///
/// # Errors
///
/// 目录不可读时返回 [`Fault::Write`]。
fn tree_files(root: &Path) -> Result<Vec<String>, Fault> {
    let mut files = Vec::new();
    collect_files(root, "", &mut files, 0)?;
    files.sort();
    Ok(files)
}

/// 把 `root` 下的文件（含子目录，路径分隔符为 `/`）按 Deflated 打包为 `out`。
///
/// # Errors
///
/// 读写或压缩失败时返回 [`Fault::Write`]。
pub(crate) fn write_zip_tree(out: &Path, root: &Path) -> Result<(), Fault> {
    const MSG: &str = "写入备份文件失败";
    let files = tree_files(root)?;
    let mut zw = ZipWriter::new(write_err(File::create(out), MSG)?);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for rel in &files {
        let body = write_err(fs::read(root.join(rel)), MSG)?;
        zw.start_file(rel.as_str(), opts)
            .map_err(|err| Fault::Write(MSG, err.to_string()))?;
        write_err(zw.write_all(&body), MSG)?;
    }
    let done = zw
        .finish()
        .map_err(|err| Fault::Write(MSG, err.to_string()))?;
    write_err(done.sync_all(), MSG)
}

/// 清单：格式、模块版本、导出时间、固件版本与每个文件的 sha256。
///
/// # Errors
///
/// 文件不可读时返回 [`Fault::Write`]。
fn manifest_value(
    env: &Env,
    stage: &Path,
    files: &[String],
    firmware: &str,
) -> Result<Value, Fault> {
    let sums = files
        .iter()
        .map(|rel| {
            fs::read(stage.join(rel)).map(|body| (rel.clone(), Value::String(sha_hex(&body))))
        })
        .collect::<std::io::Result<Map<String, Value>>>();
    Ok(json!({
        "kind": KIND,
        "format": FORMAT,
        "module": module_version(env),
        "created": local_now().format("%Y-%m-%d %H:%M:%S %z").to_string(),
        "firmware": firmware,
        "files": write_err(sums, "计算校验值失败")?,
    }))
}

/// 暂存、写清单并打包到下载目录（先写 `.tmp` 再改名），返回导出结果。
///
/// # Errors
///
/// 任一步失败时返回对应的 [`Fault`]，临时文件已删除。
fn build_zip(env: &Env, stage: &Path, firmware: &str) -> Result<Value, Fault> {
    let vpn = stage_export(env, stage)?;
    let files = tree_files(stage)?;
    let manifest = manifest_value(env, stage, &files, firmware)?;
    write_err(
        fs::write(stage.join(MANIFEST_ENTRY), manifest.to_string()),
        "写清单失败",
    )?;
    let dir = download_dir(env).ok_or_else(|| Fault::Write("无法访问下载目录", String::new()))?;
    let name = format!("xrouter-config-{}.zip", local_now().format("%Y%m%d-%H%M%S"));
    let tmp = dir.join(format!(".{name}.tmp"));
    let dest = dir.join(&name);
    let written = write_zip_tree(&tmp, stage)
        .and_then(|()| write_err(fs::rename(&tmp, &dest), "写入备份文件失败"));
    if let Err(fault) = written {
        fsx::remove_logged(env, &tmp);
        return Err(fault);
    }
    let size = fs::metadata(&dest).map_or(0, |meta| meta.len());
    log::info(env, "backup", &format!("配置已导出：{name}"));
    Ok(json!({
        "ok": true,
        "path": dest.to_string_lossy(),
        "size": size,
        "vpn": vpn,
        "firmware": firmware,
    }))
}

/// 导出流程：前置检查、请客机导出、暂存打包；暂存目录在所有返回路径上删除。
///
/// # Errors
///
/// 任一步失败时返回对应的 [`Fault`]。
fn export_inner(env: &Env) -> Result<Value, Fault> {
    precheck(env)?;
    let lines = guest_call(env, "export", EXPORT_WAIT)?;
    let firmware = lines
        .iter()
        .find_map(|line| line.strip_prefix("fw="))
        .unwrap_or("")
        .trim()
        .to_owned();
    let stage = env.run_dir().join(format!("cfgexp.{}", std::process::id()));
    fsx::remove_tree_logged(env, &stage);
    let res = build_zip(env, &stage, &firmware);
    fsx::remove_tree_logged(env, &stage);
    fsx::remove_logged(env, env.run_dir().join("guest/guest/backup.tar.gz"));
    res
}

/// `xr config-export`：生成 `Download/xrouter-config-<本地时间>.zip`，返回（退出码, JSON）。
#[must_use]
pub(crate) fn config_export(env: &Env) -> (i32, String) {
    respond(env, export_inner(env))
}

/// 导入文件检查：在内部存储下、不含 `..`、扩展名 `.zip`（不区分大小写）、存在且不超过 64 MiB。
///
/// # Errors
///
/// 不满足时返回 [`Fault::Path`]。
fn import_source(env: &Env, path_arg: &str) -> Result<PathBuf, Fault> {
    let src_s = env.canon_share_path(path_arg);
    let media_s = env.media.to_string_lossy();
    let is_zip = Path::new(&src_s)
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"));
    if !src_s.starts_with(&format!("{media_s}/")) || src_s.contains("..") || !is_zip {
        return Err(Fault::Path("路径无效"));
    }
    let src = PathBuf::from(&src_s);
    let Some(meta) = fs::metadata(&src).ok().filter(fs::Metadata::is_file) else {
        return Err(Fault::Path("文件不存在"));
    };
    let size = meta.len();
    if size > MAX_ZIP {
        return Err(Fault::Path("文件过大"));
    }
    Ok(src)
}

/// 条目名属于允许的几类：清单、手机配置、`OpenWrt` 备份、`phone/vpn/<合法编号>.(ovpn|enabled|name)`。
#[must_use]
pub(crate) fn allowed_entry(name: &str) -> bool {
    if [MANIFEST_ENTRY, CONF_ENTRY, OPENWRT_ENTRY].contains(&name) {
        return true;
    }
    name.strip_prefix(VPN_PREFIX)
        .and_then(|file| file.rsplit_once('.'))
        .is_some_and(|(id, ext)| valid_profile_id(id) && matches!(ext, "ovpn" | "enabled" | "name"))
}

/// 限量读取一个条目：超过 `remaining` 字节即判为超限。
///
/// # Errors
///
/// 解压出错返回 [`Fault::Checksum`]，超限返回 [`Fault::Package`]。
fn read_limited<R: Read>(reader: &mut R, remaining: u64) -> Result<Vec<u8>, Fault> {
    let mut body = Vec::new();
    let read = Read::by_ref(reader)
        .take(remaining.saturating_add(1))
        .read_to_end(&mut body)
        .map_err(|err| Fault::Unreadable(err.to_string()))?;
    if u64::try_from(read).unwrap_or(u64::MAX) > remaining {
        return Err(Fault::Package("解压后体积超限"));
    }
    Ok(body)
}

/// 读取并检查备份包结构：条目数、路径穿越、未知条目、重复条目与解压后总量，返回条目名到内容的映射（目录条目跳过）。
///
/// # Errors
///
/// 不合法时返回 [`Fault::Package`] 或 [`Fault::Checksum`]。
pub(crate) fn read_package(
    path: &Path,
    limits: Limits,
) -> Result<BTreeMap<String, Vec<u8>>, Fault> {
    let file = File::open(path).map_err(|err| Fault::Unreadable(err.to_string()))?;
    let mut archive = ZipArchive::new(file).map_err(|err| Fault::Unreadable(err.to_string()))?;
    if archive.len() > limits.entries {
        return Err(Fault::Package("条目过多"));
    }
    let mut files = BTreeMap::new();
    let mut total = 0_u64;
    for idx in 0..archive.len() {
        let mut entry = archive
            .by_index(idx)
            .map_err(|err| Fault::Unreadable(err.to_string()))?;
        if entry.enclosed_name().is_none() {
            return Err(Fault::Package("条目路径越界"));
        }
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_owned();
        if !allowed_entry(&name) {
            return Err(Fault::Package("包含未知条目"));
        }
        let body = read_limited(&mut entry, limits.unpacked.saturating_sub(total))?;
        let len = u64::try_from(body.len()).unwrap_or(u64::MAX);
        total = total
            .checked_add(len)
            .ok_or(Fault::Package("解压后体积超限"))?;
        if files.insert(name, body).is_some() {
            return Err(Fault::Package("条目重复"));
        }
    }
    Ok(files)
}

/// 检查清单：`kind`/`format`、必需条目，以及每个条目与清单的 sha256 一一对应。
///
/// # Errors
///
/// 不合法时返回 [`Fault::Package`]，内容不符返回 [`Fault::Checksum`]。
pub(crate) fn check_manifest(files: &BTreeMap<String, Vec<u8>>) -> Result<(), Fault> {
    let raw = files
        .get(MANIFEST_ENTRY)
        .ok_or(Fault::Package("缺少清单"))?;
    let Ok(man) = serde_json::from_slice::<Value>(raw) else {
        return Err(Fault::Package("清单无法解析"));
    };
    if man.get("kind").and_then(Value::as_str) != Some(KIND) {
        return Err(Fault::Package("不是 XRouter 配置备份"));
    }
    if man.get("format").and_then(Value::as_u64) != Some(FORMAT) {
        return Err(Fault::Package("格式版本不受支持"));
    }
    if !files.contains_key(CONF_ENTRY) || !files.contains_key(OPENWRT_ENTRY) {
        return Err(Fault::Package("缺少必需条目"));
    }
    let sums = man
        .get("files")
        .and_then(Value::as_object)
        .ok_or(Fault::Package("清单缺少文件列表"))?;
    if sums.len() != files.len().saturating_sub(1) {
        return Err(Fault::Checksum);
    }
    let all_match = files
        .iter()
        .filter(|(name, _)| name.as_str() != MANIFEST_ENTRY)
        .all(|(name, body)| sums.get(name).and_then(Value::as_str) == Some(sha_hex(body).as_str()));
    if all_match {
        Ok(())
    } else {
        Err(Fault::Checksum)
    }
}

/// 逐键模拟校验的结果。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ConfPlan {
    /// 通过校验、将要写入的键值（`VM_SHARE_PATH` 已换算为规范路径）。
    pub(crate) applied: Vec<(String, String)>,
    /// 跳过的键与原因。
    pub(crate) skipped: Vec<(String, &'static str)>,
}

/// 以 `base` 为起点按 [`KEYS`] 顺序逐键校验包内配置：通过的键并入模拟配置，不通过的记入跳过；[`BACKUP_EXCLUDE`] 与未知键忽略。
#[must_use]
pub(crate) fn plan_config(env: &Env, base: &Config, text: &str) -> ConfPlan {
    let incoming = Config::from_conf_str(text);
    let mut cfg = base.clone();
    let mut plan = ConfPlan::default();
    for key in KEYS.iter().filter(|key| !BACKUP_EXCLUDE.contains(key)) {
        let Some(raw) = incoming.get(key) else {
            continue;
        };
        let val = if *key == "VM_SHARE_PATH" {
            env.canon_share_path(raw)
        } else {
            raw.to_owned()
        };
        if valid_kv(env, &cfg, key, &val) {
            cfg = cfg.with(key, &val);
            plan.applied.push(((*key).to_owned(), val));
        } else {
            plan.skipped.push(((*key).to_owned(), REASON_INVALID));
        }
    }
    plan
}

/// 把 `OpenWrt` 备份交给客机还原（先写 `.tmp` 再改名，再写请求），等回执；归档在所有返回路径上删除。
///
/// # Errors
///
/// 写入失败返回 [`Fault::Write`]；客机报错或超时时返回对应的 [`Fault`]。
fn restore_openwrt(env: &Env, tar: &[u8]) -> Result<(), Fault> {
    let host = env.run_dir().join("guest/host");
    let dest = host.join("backup.in.tar.gz");
    let tmp = host.join("backup.in.tar.gz.tmp");
    let staged = fs::write(&tmp, tar).and_then(|()| fs::rename(&tmp, &dest));
    if staged.is_err() {
        fsx::remove_logged(env, &tmp);
        return Err(Fault::Write("写入控制目录失败", String::new()));
    }
    let res = guest_call(env, "import", IMPORT_WAIT);
    fsx::remove_logged(env, &dest);
    res.map(|_lines| ())
}

/// 逐键写入手机侧配置，写入失败的键追加到跳过列表；返回已写入的键名。
fn apply_config(env: &Env, plan: &mut ConfPlan) -> Vec<String> {
    let mut done = Vec::new();
    for (key, val) in &plan.applied {
        if set_kv(env, key, val) {
            done.push(key.clone());
        } else {
            plan.skipped.push((key.clone(), REASON_WRITE));
        }
    }
    done
}

/// 把包内 `phone/vpn/*` 写入新建的 `dir`（`.ovpn` 权限 600），返回 `.ovpn` 份数。
///
/// # Errors
///
/// 写入失败时返回 [`Fault::Write`]。
fn write_vpn(dir: &Path, files: &BTreeMap<String, Vec<u8>>) -> Result<usize, Fault> {
    const MSG: &str = "写入 VPN 配置失败";
    write_err(fs::create_dir_all(dir), MSG)?;
    let mut count = 0_usize;
    for (file, body) in files
        .iter()
        .filter_map(|(name, body)| Some((name.strip_prefix(VPN_PREFIX)?, body)))
    {
        let path = dir.join(file);
        write_err(fs::write(&path, body), MSG)?;
        if Path::new(file).extension() == Some(OsStr::new("ovpn")) {
            write_err(
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)),
                MSG,
            )?;
            count = count.saturating_add(1);
        }
    }
    Ok(count)
}

/// 整体替换 `VPN` 配置目录：旧目录先改名为 `vpn.prev.<ts>`，新目录写成功后删除旧目录，失败时改回原名。
///
/// # Errors
///
/// 改名或写入失败时返回 [`Fault::Write`]，手机侧 `VPN` 配置保持原样。
fn replace_vpn(env: &Env, files: &BTreeMap<String, Vec<u8>>, ts: u64) -> Result<usize, Fault> {
    const KEEP: &str = "OpenWrt 配置已还原，手机侧 VPN 未变更";
    let dir = env.vpn_dir();
    let prev = dir.with_file_name(format!("vpn.prev.{ts}"));
    let had = dir.exists();
    if had {
        write_err(fs::rename(&dir, &prev), KEEP)?;
    }
    let res = write_vpn(&dir, files);
    if res.is_err() {
        fsx::remove_tree_logged(env, &dir);
        if had {
            fsx::rename_logged(env, &prev, &dir);
        }
    } else if had {
        fsx::remove_tree_logged(env, &prev);
    } else {
        // 原本没有配置目录：无旧目录可清理
    }
    res.map_err(|fault| Fault::Write(KEEP, format!("{fault:?}")))
}

/// 导入后重启服务：同步停止，再后台启动；能拉起时为 `"started"`，否则为 true（需要手动启动）。
fn restart_now(env: &Env) -> Value {
    let stopped = vm_stop(env);
    log::dlog_env(env, "backup", &format!("导入后停止服务 rc={stopped}"));
    if bg_spawn(env, "start") {
        Value::from("started")
    } else {
        Value::Bool(true)
    }
}

/// 导入流程：先完整校验（不做任何改动），再还原 `OpenWrt`、写手机侧配置、替换 `VPN` 配置。
///
/// # Errors
///
/// 任一步失败时返回对应的 [`Fault`]；`OpenWrt` 还原失败时手机侧未改动。
fn import_inner(env: &Env, path_arg: &str, restart: bool) -> Result<Value, Fault> {
    let src = import_source(env, path_arg)?;
    precheck(env)?;
    let files = read_package(&src, LIMITS)?;
    check_manifest(&files)?;
    let conf = files.get(CONF_ENTRY).map_or_else(String::new, |body| {
        String::from_utf8_lossy(body).into_owned()
    });
    let mut plan = plan_config(env, &Config::load(env), &conf);
    restore_openwrt(env, files.get(OPENWRT_ENTRY).map_or(&[][..], Vec::as_slice))?;
    let applied = apply_config(env, &mut plan);
    let vpn = replace_vpn(env, &files, unix_now())?;
    vpn_mirror_legacy(env);
    let name = src.file_name().and_then(OsStr::to_str).unwrap_or("");
    log::info(
        env,
        "backup",
        &format!("配置已导入：{name}（跳过 {} 项）", plan.skipped.len()),
    );
    let skipped: Vec<Value> = plan
        .skipped
        .iter()
        .map(|(key, reason)| json!({"key": key, "reason": reason}))
        .collect();
    let restart_val = if restart {
        restart_now(env)
    } else {
        Value::Bool(true)
    };
    Ok(
        json!({"ok": true, "applied": applied, "skipped": skipped, "vpn": vpn, "restart": restart_val}),
    )
}

/// `xr config-import <zip> [--restart]`：校验并还原备份，返回（退出码, JSON）。
#[must_use]
pub(crate) fn config_import(env: &Env, path_arg: &str, restart: bool) -> (i32, String) {
    respond(env, import_inner(env, path_arg, restart))
}
