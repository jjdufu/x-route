//! 配置：`config.conf` 的读取、取值默认值、合法性校验与原子写入。
//!
//! 文件是 shell 可 `source` 的 `KEY="值"` 格式；环境变量中的同名键作为文件缺省时的初值。

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write as _;
use std::path::Path;

use sha2::{Digest as _, Sha256};

use crate::cidr::{hs_subnet_clash, hs_subnet_valid, in_range, valid_ip4};
use crate::env::Env;
use crate::sys::fsx;

#[cfg(test)]
mod tests;

/// 全部合法配置键；不在表中的键在读取时忽略、写入时拒绝。
pub const KEYS: [&str; 25] = [
    "XR_NET_PREFIX",
    "XR_VM_MAC",
    "XR_DNS",
    "XR_EXPOSE",
    "XR_MODE",
    "VM_CPUS",
    "VM_MEM_MB",
    "VM_SWIOTLB_MB",
    "VM_NET",
    "VM_DISK_RW",
    "VM_DISK_SIZE_MB",
    "VM_WAKELOCK",
    "VM_SHARE",
    "VM_SHARE_MODE",
    "VM_SHARE_PATH",
    "AUTOSTART",
    "AUTOSTART_DELAY_SEC",
    "READY_TIMEOUT_SEC",
    "STABLE_SEC",
    "XR_NOTIFY_TRAFFIC",
    "XR_DEBUG",
    "XR_DEBUG_HOURS",
    "XR_HOTSPOT_BRIDGE",
    "XR_HOTSPOT_SUBNET",
    "XR_CLASH_UI",
];

/// 不随配置备份迁移的键：磁盘镜像大小是本机的物理状态。
pub(crate) const BACKUP_EXCLUDE: [&str; 1] = ["VM_DISK_SIZE_MB"];

/// 参与配置哈希的键：这些键变化意味着虚拟机硬件或网络形态变化，需要重新确认稳定性。
pub(crate) const HASH_KEYS: [&str; 8] = [
    "XR_NET_PREFIX",
    "VM_CPUS",
    "VM_MEM_MB",
    "VM_SWIOTLB_MB",
    "VM_NET",
    "VM_DISK_RW",
    "VM_SHARE",
    "VM_SHARE_MODE",
];

/// 修改后需要重启虚拟机才能生效的键；虚拟机运行中修改它们时提示重启。
pub(crate) const RESTART_KEYS: [&str; 11] = [
    "XR_NET_PREFIX",
    "XR_VM_MAC",
    "XR_DNS",
    "VM_CPUS",
    "VM_MEM_MB",
    "VM_SWIOTLB_MB",
    "VM_NET",
    "VM_DISK_RW",
    "VM_SHARE",
    "VM_SHARE_MODE",
    "VM_SHARE_PATH",
];

/// 一次读取到的配置键值表（只含 [`KEYS`] 中的键）。
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// 键到去引号后取值的映射。
    values: BTreeMap<String, String>,
}

/// 去掉值两端成对的双引号或单引号（先去首尾空白），否则原样返回。
fn unquote_shell_val(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() >= 2 {
        if let Some(inner) = trimmed
            .strip_prefix('"')
            .and_then(|content| content.strip_suffix('"'))
        {
            return inner.to_owned();
        }
        if let Some(inner) = trimmed
            .strip_prefix('\'')
            .and_then(|text| text.strip_suffix('\''))
        {
            return inner.to_owned();
        }
    }
    trimmed.to_owned()
}

/// 解析配置文件的一行：跳过空行、注释和未知键，返回 `(键, 去引号后的值)`。
fn conf_line_entry(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let (raw_key, raw_val) = trimmed.split_once('=')?;
    let key = raw_key.trim();
    KEYS.contains(&key)
        .then(|| (key.to_owned(), unquote_shell_val(raw_val)))
}

impl Config {
    /// 读取配置：先取环境变量中的同名键，再用配置文件中的键覆盖；文件不存在时只有环境变量的值。
    #[must_use]
    pub fn load(env: &Env) -> Self {
        let mut values: BTreeMap<String, String> = KEYS
            .iter()
            .filter_map(|key| std::env::var(key).ok().map(|val| ((*key).to_owned(), val)))
            .collect();
        if let Ok(content) = fs::read_to_string(env.conf_file()) {
            values.extend(Self::from_conf_str(&content).values);
        }
        Self { values }
    }

    /// 只解析配置文本（不读环境变量）：跳过空行、注释和未知键，同名键以后出现的为准。
    #[must_use]
    pub(crate) fn from_conf_str(text: &str) -> Self {
        Self {
            values: text.lines().filter_map(conf_line_entry).collect(),
        }
    }

    /// 返回把 `key` 设为 `value` 后的副本（不写文件），供逐键模拟校验。
    #[must_use]
    pub(crate) fn with(&self, key: &str, value: &str) -> Self {
        let values = self
            .values
            .iter()
            .map(|(name, val)| (name.clone(), val.clone()))
            .chain(std::iter::once((key.to_owned(), value.to_owned())))
            .collect();
        Self { values }
    }

    /// 键在配置中出现过时返回其取值（可能为空串），否则 `None`。
    #[must_use]
    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// 键的原始取值，未设置时为空串。
    pub(crate) fn get_raw(&self, key: &str) -> &str {
        self.values.get(key).map_or("", String::as_str)
    }

    /// 键的取值，未设置或为空时返回 `default`。
    pub(crate) fn get_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.values
            .get(key)
            .filter(|value| !value.is_empty())
            .map_or(default, String::as_str)
    }

    /// 内网前三段 `XR_NET_PREFIX`（默认 `172.29.233`），`xr0` 网段为 `<前缀>.0/24`。
    #[must_use]
    pub(crate) fn net_prefix(&self) -> &str {
        self.get_or("XR_NET_PREFIX", "172.29.233")
    }

    /// 虚拟机网卡 MAC `XR_VM_MAC`（默认 `52:54:00:1d:e9:fe`）。
    #[must_use]
    pub(crate) fn vm_mac(&self) -> &str {
        self.get_or("XR_VM_MAC", "52:54:00:1d:e9:fe")
    }

    /// 下发给客机的 DNS 服务器列表 `XR_DNS`（空格分隔，默认 `223.5.5.5 119.29.29.29`）。
    #[must_use]
    pub(crate) fn dns(&self) -> &str {
        self.get_or("XR_DNS", "223.5.5.5 119.29.29.29")
    }

    /// 管理界面开放范围 `XR_EXPOSE`：`local`、`lan`（默认）或 `all`。
    #[must_use]
    pub fn expose(&self) -> &str {
        self.get_or("XR_EXPOSE", "lan")
    }

    /// 网关模式 `XR_MODE`：`global`（默认）或 `bypass`。
    #[must_use]
    pub(crate) fn mode(&self) -> &str {
        self.get_or("XR_MODE", "global")
    }

    /// 虚拟机 CPU 核数 `VM_CPUS`，只接受 1–4，否则取 2。
    #[must_use]
    pub(crate) fn vm_cpus(&self) -> u32 {
        self.get_raw("VM_CPUS")
            .parse::<u32>()
            .ok()
            .filter(|value| (1..=4).contains(value))
            .unwrap_or(2)
    }

    /// 虚拟机内存 `VM_MEM_MB`（MB），无法解析时取 512，小于 384 时也取 512。
    #[must_use]
    pub(crate) fn vm_mem_mb(&self) -> u32 {
        let raw = self.get_raw("VM_MEM_MB").parse::<u32>().unwrap_or(512);
        if raw < 384 { 512 } else { raw }
    }

    /// 虚拟机 swiotlb 大小 `VM_SWIOTLB_MB`（MB），无法解析时取 64。
    #[must_use]
    pub(crate) fn vm_swiotlb_mb(&self) -> u32 {
        self.get_raw("VM_SWIOTLB_MB")
            .parse::<u32>()
            .ok()
            .unwrap_or(64)
    }

    /// 是否给虚拟机配网卡 `VM_NET`（默认开）。
    #[must_use]
    pub(crate) fn vm_net(&self) -> bool {
        self.get_or("VM_NET", "1") == "1"
    }

    /// 系统盘是否读写挂载 `VM_DISK_RW`（默认只读）。
    #[must_use]
    pub(crate) fn vm_disk_rw(&self) -> bool {
        self.get_raw("VM_DISK_RW") == "1"
    }

    /// 读写系统盘大小 `VM_DISK_SIZE_MB`（MB），无法解析或为 0 时取 1024。
    #[must_use]
    pub(crate) fn vm_disk_size_mb(&self) -> u64 {
        self.get_raw("VM_DISK_SIZE_MB")
            .parse::<u64>()
            .ok()
            .filter(|&value| value > 0)
            .unwrap_or(1024)
    }

    /// 运行期间是否持有唤醒锁 `VM_WAKELOCK`（默认关）。
    #[must_use]
    pub(crate) fn vm_wakelock(&self) -> bool {
        self.get_raw("VM_WAKELOCK") == "1"
    }

    /// 是否向虚拟机共享内部存储目录 `VM_SHARE`（默认关）。
    #[must_use]
    pub(crate) fn vm_share(&self) -> bool {
        self.get_raw("VM_SHARE") == "1"
    }

    /// 共享目录模式 `VM_SHARE_MODE`：`ro`（默认）或 `rw`。
    #[must_use]
    pub(crate) fn vm_share_mode(&self) -> &str {
        self.get_or("VM_SHARE_MODE", "ro")
    }

    /// 共享目录路径 `VM_SHARE_PATH`，未设置时为空串。
    #[must_use]
    pub(crate) fn vm_share_path(&self) -> &str {
        self.get_raw("VM_SHARE_PATH")
    }

    /// 开机是否自动启动虚拟机 `AUTOSTART`（默认关）。
    #[must_use]
    pub(crate) fn autostart(&self) -> bool {
        self.get_raw("AUTOSTART") == "1"
    }

    /// 开机自动启动前的延迟秒数 `AUTOSTART_DELAY_SEC`，无法解析时取 60。
    #[must_use]
    pub(crate) fn autostart_delay_sec(&self) -> u64 {
        self.get_raw("AUTOSTART_DELAY_SEC")
            .parse::<u64>()
            .unwrap_or(60)
    }

    /// 等待客机就绪的超时秒数 `READY_TIMEOUT_SEC`，无法解析或为 0 时取 180。
    #[must_use]
    pub(crate) fn ready_timeout_sec(&self) -> u64 {
        self.get_raw("READY_TIMEOUT_SEC")
            .parse::<u64>()
            .ok()
            .filter(|&value| value > 0)
            .unwrap_or(180)
    }

    /// 启动后视为稳定所需的连续运行秒数 `STABLE_SEC`，无法解析或为 0 时取 180。
    #[must_use]
    pub(crate) fn stable_sec(&self) -> u64 {
        self.get_raw("STABLE_SEC")
            .parse::<u64>()
            .ok()
            .filter(|&value| value > 0)
            .unwrap_or(180)
    }

    /// 通知栏流量显示口径 `XR_NOTIFY_TRAFFIC`：`total`（默认）、`wan` 或 `lan`，其他值按 `total`。
    #[must_use]
    pub(crate) fn notify_traffic(&self) -> &str {
        let mode = self.get_or("XR_NOTIFY_TRAFFIC", "total");
        match mode {
            "total" | "wan" | "lan" => mode,
            _ => "total",
        }
    }

    /// 调试模式 `XR_DEBUG` 是否打开。
    #[must_use]
    pub(crate) fn debug_on(&self) -> bool {
        self.get_or("XR_DEBUG", "0") == "1"
    }

    /// 调试模式自动关闭前的小时数 `XR_DEBUG_HOURS`，0 表示不自动关闭。
    #[must_use]
    pub(crate) fn debug_hours(&self) -> u64 {
        self.get_or("XR_DEBUG_HOURS", "0")
            .parse::<u64>()
            .unwrap_or(0)
    }

    /// 热点接管时是否桥接到虚拟机 `XR_HOTSPOT_BRIDGE`（默认开）。
    #[must_use]
    pub(crate) fn hotspot_bridge(&self) -> bool {
        self.get_or("XR_HOTSPOT_BRIDGE", "1") == "1"
    }

    /// 热点子网 `XR_HOTSPOT_SUBNET`，空串表示沿用系统热点网段。
    #[must_use]
    pub(crate) fn hotspot_subnet(&self) -> &str {
        self.get_raw("XR_HOTSPOT_SUBNET")
    }

    /// 按 [`HASH_KEYS`] 的取值计算配置指纹：SHA-256 的前 8 字节，16 个小写十六进制字符。
    #[must_use]
    pub(crate) fn hash(&self) -> String {
        let mut hasher = Sha256::new();
        for key in HASH_KEYS {
            hasher.update(key.as_bytes());
            hasher.update(b"=");
            hasher.update(self.get_raw(key).as_bytes());
            hasher.update(b";");
        }
        let digest = hasher.finalize();
        digest
            .iter()
            .take(8)
            .flat_map(|byte| [byte >> 4_u8, byte & 0x0f_u8])
            .filter_map(|nibble| char::from_digit(u32::from(nibble), 16))
            .collect()
    }
}

/// `XR_NET_PREFIX` 的校验：三段点分十进制，且位于 `10/8`、`172.16/12` 或 `192.168/16`。
fn valid_net_prefix(val: &str) -> bool {
    let full = format!("{val}.0");
    if !valid_ip4(&full) {
        return false;
    }
    let mut parts = val.split('.');
    let (Some(a_s), Some(b_s), Some(_c_s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let (Ok(oct1), Ok(oct2)) = (a_s.parse::<u16>(), b_s.parse::<u16>()) else {
        return false;
    };
    oct1 == 10 || (oct1 == 172 && (16..=31).contains(&oct2)) || (oct1 == 192 && oct2 == 168)
}

/// `XR_VM_MAC` 的校验：`52:54:00:` 开头，后三段为两位小写十六进制。
fn valid_vm_mac(val: &str) -> bool {
    let Some(rest) = val.strip_prefix("52:54:00:") else {
        return false;
    };
    let parts: Vec<&str> = rest.split(':').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            part.len() == 2
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

/// `XR_DNS`：只含数字、点和空格，以空格分隔 1–3 个合法 `IPv4` 地址。
fn valid_dns(val: &str) -> bool {
    if !val
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'.' || byte == b' ')
    {
        return false;
    }
    let mut count = 0_usize;
    for item in val.split_whitespace() {
        if !valid_ip4(item) {
            return false;
        }
        count = count.saturating_add(1);
    }
    (1..=3).contains(&count)
}

/// `VM_SHARE_PATH`：规范化后位于内部存储下、不含 `..` 与特殊字符，并且是已存在的目录。
fn valid_share_path(env: &Env, val: &str) -> bool {
    let sp = env.canon_share_path(val);
    let media_s = env.media.to_string_lossy();
    let media = media_s.trim_end_matches('/');
    let under_media = sp == media
        || sp
            .strip_prefix(media)
            .is_some_and(|rest| rest.starts_with('/'));
    if !under_media {
        return false;
    }
    if sp.contains("..")
        || sp.chars().any(|ch| {
            matches!(ch, '"' | '\'' | ' ' | ':' | ',' | '$' | '`' | '\\') || ch.is_control()
        })
    {
        return false;
    }
    Path::new(&sp).is_dir()
}

/// 校验键值对：每个键有自己的取值范围；未知键一律返回 false。
///
/// `VM_SHARE_MODE=rw` 要求环境支持读写映射；`VM_SHARE_PATH` 要求是内部存储下已存在且不含特殊字符的目录；热点子网还要求与现有网段不冲突。
#[must_use]
pub fn valid_kv(env: &Env, cfg: &Config, key: &str, val: &str) -> bool {
    match key {
        "XR_NET_PREFIX" => valid_net_prefix(val),
        "XR_VM_MAC" => valid_vm_mac(val),
        "XR_DNS" => valid_dns(val),
        "XR_EXPOSE" => matches!(val, "local" | "lan" | "all"),
        "XR_MODE" => matches!(val, "global" | "bypass"),
        "VM_CPUS" => in_range(val, 1, 4),
        "VM_MEM_MB" => in_range(val, 384, 2048),
        "VM_SWIOTLB_MB" => matches!(val, "32" | "64" | "128"),
        "VM_NET" | "VM_DISK_RW" | "VM_WAKELOCK" | "VM_SHARE" | "AUTOSTART" | "XR_DEBUG"
        | "XR_HOTSPOT_BRIDGE" => matches!(val, "0" | "1"),
        "XR_CLASH_UI" => matches!(val, "0" | "1"),
        "VM_DISK_SIZE_MB" => in_range(val, 512, 16384),
        "VM_SHARE_MODE" => match val {
            "ro" => true,
            "rw" => !env.rw_map_opt.is_empty(),
            _ => false,
        },
        "VM_SHARE_PATH" => valid_share_path(env, val),
        "AUTOSTART_DELAY_SEC" => in_range(val, 0, 600),
        "READY_TIMEOUT_SEC" | "STABLE_SEC" => in_range(val, 60, 900),
        "XR_NOTIFY_TRAFFIC" => matches!(val, "total" | "wan" | "lan"),
        "XR_DEBUG_HOURS" => matches!(val, "0" | "1" | "6" | "24" | "72"),
        "XR_HOTSPOT_SUBNET" => {
            val.is_empty()
                || (hs_subnet_valid(val)
                    && hs_subnet_clash(env, cfg.net_prefix(), val, None).is_none())
        }
        _ => false,
    }
}

/// 校验后把 `key="值"` 写入配置文件：替换已有行或追加，经临时文件 `sync` 后改名，保证原子。
///
/// `VM_SHARE_PATH` 先换算成规范路径；校验失败或写入失败时返回 false，配置文件保持原样。
#[must_use]
pub fn set_kv(env: &Env, key: &str, val: &str) -> bool {
    let cfg = Config::load(env);
    let sv = if key == "VM_SHARE_PATH" {
        env.canon_share_path(val)
    } else {
        val.to_owned()
    };
    if !valid_kv(env, &cfg, key, &sv) {
        return false;
    }
    let conf = env.conf_file();
    if let Some(parent) = conf.parent()
        && fs::create_dir_all(parent).is_err()
    {
        return false;
    }
    let existing = fs::read_to_string(&conf).unwrap_or_default();
    let prefix = format!("{key}=");
    let replacement = format!("{key}=\"{sv}\"");
    let mut found = false;
    let mut out = String::with_capacity(
        existing
            .len()
            .saturating_add(replacement.len())
            .saturating_add(2),
    );
    for line in existing.lines() {
        if line.starts_with(&prefix) {
            out.push_str(&replacement);
            out.push('\n');
            found = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !found {
        out.push_str(&replacement);
        out.push('\n');
    }
    let pid = std::process::id();
    let tmp = conf.with_extension(format!("conf.tmp.{pid}"));
    let write_res = (|| -> std::io::Result<()> {
        let mut file = File::create(&tmp)?;
        file.write_all(out.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, &conf)?;
        Ok(())
    })();
    if write_res.is_err() {
        fsx::remove_logged(env, &tmp);
        return false;
    }
    rustix::fs::sync();
    true
}

/// 读取当前配置并返回其指纹：SHA-256 的前 8 字节，16 个小写十六进制字符。
#[must_use]
pub fn hash(env: &Env) -> String {
    Config::load(env).hash()
}
