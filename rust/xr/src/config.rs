use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::cidr::{hs_subnet_clash, hs_subnet_valid, in_range, valid_ip4};
use crate::env::Env;

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

pub const HASH_KEYS: [&str; 8] = [
    "XR_NET_PREFIX",
    "VM_CPUS",
    "VM_MEM_MB",
    "VM_SWIOTLB_MB",
    "VM_NET",
    "VM_DISK_RW",
    "VM_SHARE",
    "VM_SHARE_MODE",
];

pub const RESTART_KEYS: [&str; 11] = [
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

#[derive(Debug, Clone, Default)]
pub struct Config {
    values: BTreeMap<String, String>,
}

fn unquote_shell_val(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() >= 2 {
        if let Some(inner) = trimmed.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            return inner.to_string();
        }
        if let Some(inner) = trimmed
            .strip_prefix('\'')
            .and_then(|s| s.strip_suffix('\''))
        {
            return inner.to_string();
        }
    }
    trimmed.to_string()
}

impl Config {
    pub fn load(env: &Env) -> Self {
        let mut values = BTreeMap::new();
        for key in KEYS {
            if let Ok(v) = std::env::var(key) {
                values.insert(key.to_string(), v);
            }
        }
        if let Ok(content) = fs::read_to_string(env.conf_file()) {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                let Some((k, v)) = trimmed.split_once('=') else {
                    continue;
                };
                let key = k.trim();
                if KEYS.contains(&key) {
                    values.insert(key.to_string(), unquote_shell_val(v));
                }
            }
        }
        Self { values }
    }

    pub fn get_raw(&self, key: &str) -> &str {
        self.values.get(key).map_or("", String::as_str)
    }

    pub fn get_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.values
            .get(key)
            .filter(|v| !v.is_empty())
            .map_or(default, String::as_str)
    }

    pub fn net_prefix(&self) -> &str {
        self.get_or("XR_NET_PREFIX", "172.29.233")
    }

    pub fn vm_mac(&self) -> &str {
        self.get_or("XR_VM_MAC", "52:54:00:1d:e9:fe")
    }

    pub fn dns(&self) -> &str {
        self.get_or("XR_DNS", "223.5.5.5 119.29.29.29")
    }

    pub fn expose(&self) -> &str {
        self.get_or("XR_EXPOSE", "lan")
    }

    pub fn mode(&self) -> &str {
        self.get_or("XR_MODE", "global")
    }

    pub fn vm_cpus(&self) -> u32 {
        self.get_raw("VM_CPUS")
            .parse::<u32>()
            .ok()
            .filter(|v| (1..=4).contains(v))
            .unwrap_or(2)
    }

    pub fn vm_mem_mb(&self) -> u32 {
        let raw = self.get_raw("VM_MEM_MB").parse::<u32>().unwrap_or(512);
        if raw < 384 {
            512
        } else {
            raw
        }
    }

    pub fn vm_swiotlb_mb(&self) -> u32 {
        self.get_raw("VM_SWIOTLB_MB")
            .parse::<u32>()
            .ok()
            .unwrap_or(64)
    }

    pub fn vm_net(&self) -> bool {
        self.get_or("VM_NET", "1") == "1"
    }

    pub fn vm_disk_rw(&self) -> bool {
        self.get_raw("VM_DISK_RW") == "1"
    }

    pub fn vm_disk_size_mb(&self) -> u64 {
        self.get_raw("VM_DISK_SIZE_MB")
            .parse::<u64>()
            .ok()
            .filter(|&v| v > 0)
            .unwrap_or(1024)
    }

    pub fn vm_wakelock(&self) -> bool {
        self.get_raw("VM_WAKELOCK") == "1"
    }

    pub fn vm_share(&self) -> bool {
        self.get_raw("VM_SHARE") == "1"
    }

    pub fn vm_share_mode(&self) -> &str {
        self.get_or("VM_SHARE_MODE", "ro")
    }

    pub fn vm_share_path(&self) -> &str {
        self.get_raw("VM_SHARE_PATH")
    }

    pub fn autostart(&self) -> bool {
        self.get_raw("AUTOSTART") == "1"
    }

    pub fn autostart_delay_sec(&self) -> u64 {
        self.get_raw("AUTOSTART_DELAY_SEC")
            .parse::<u64>()
            .unwrap_or(60)
    }

    pub fn ready_timeout_sec(&self) -> u64 {
        self.get_raw("READY_TIMEOUT_SEC")
            .parse::<u64>()
            .ok()
            .filter(|&v| v > 0)
            .unwrap_or(180)
    }

    pub fn stable_sec(&self) -> u64 {
        self.get_raw("STABLE_SEC")
            .parse::<u64>()
            .ok()
            .filter(|&v| v > 0)
            .unwrap_or(180)
    }

    pub fn notify_traffic(&self) -> &str {
        let m = self.get_or("XR_NOTIFY_TRAFFIC", "total");
        match m {
            "total" | "wan" | "lan" => m,
            _ => "total",
        }
    }

    pub fn debug_on(&self) -> bool {
        self.get_or("XR_DEBUG", "0") == "1"
    }

    pub fn debug_hours(&self) -> u64 {
        self.get_or("XR_DEBUG_HOURS", "0")
            .parse::<u64>()
            .unwrap_or(0)
    }

    pub fn hotspot_bridge(&self) -> bool {
        self.get_or("XR_HOTSPOT_BRIDGE", "1") == "1"
    }

    pub fn hotspot_subnet(&self) -> &str {
        self.get_raw("XR_HOTSPOT_SUBNET")
    }

    pub fn clash_ui(&self) -> bool {
        self.get_or("XR_CLASH_UI", "0") == "1"
    }

    pub fn hash(&self) -> String {
        let mut hasher = Sha256::new();
        for key in HASH_KEYS {
            hasher.update(key.as_bytes());
            hasher.update(b"=");
            hasher.update(self.get_raw(key).as_bytes());
            hasher.update(b";");
        }
        let digest = hasher.finalize();
        let mut out = String::with_capacity(16);
        for byte in digest.iter().take(8) {
            let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{byte:02x}"));
        }
        out
    }
}

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
    let (Ok(a), Ok(b)) = (a_s.parse::<u16>(), b_s.parse::<u16>()) else {
        return false;
    };
    a == 10 || (a == 172 && (16..=31).contains(&b)) || (a == 192 && b == 168)
}

fn valid_vm_mac(val: &str) -> bool {
    let Some(rest) = val.strip_prefix("52:54:00:") else {
        return false;
    };
    let parts: Vec<&str> = rest.split(':').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            p.len() == 2
                && p.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

pub fn valid_kv(env: &Env, cfg: &Config, key: &str, val: &str) -> bool {
    match key {
        "XR_NET_PREFIX" => valid_net_prefix(val),
        "XR_VM_MAC" => valid_vm_mac(val),
        "XR_DNS" => {
            if !val
                .bytes()
                .all(|b| b.is_ascii_digit() || b == b'.' || b == b' ')
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
        "VM_SHARE_PATH" => {
            let sp = env.canon_share_path(val);
            let media_s = env.media.to_string_lossy();
            let media = media_s.trim_end_matches('/');
            let under_media =
                sp == media || sp.strip_prefix(media).is_some_and(|r| r.starts_with('/'));
            if !under_media {
                return false;
            }
            if sp.contains("..")
                || sp.chars().any(|c| {
                    matches!(c, '"' | '\'' | ' ' | ':' | ',' | '$' | '`' | '\\') || c.is_control()
                })
            {
                return false;
            }
            Path::new(&sp).is_dir()
        }
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

pub fn set_kv(env: &Env, key: &str, val: &str) -> bool {
    let cfg = Config::load(env);
    let sv = if key == "VM_SHARE_PATH" {
        env.canon_share_path(val)
    } else {
        val.to_string()
    };
    if !valid_kv(env, &cfg, key, &sv) {
        return false;
    }
    let conf = env.conf_file();
    if let Some(parent) = conf.parent() {
        if fs::create_dir_all(parent).is_err() {
            return false;
        }
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
        let mut f = File::create(&tmp)?;
        f.write_all(out.as_bytes())?;
        f.sync_all()?;
        fs::rename(&tmp, &conf)?;
        Ok(())
    })();
    if write_res.is_err() {
        let _ = fs::remove_file(&tmp);
        return false;
    }
    rustix::fs::sync();
    true
}

pub fn config_hash(env: &Env) -> String {
    Config::load(env).hash()
}
