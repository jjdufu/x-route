//! `xr status-json` 与 `xr status-line`：汇总虚拟机、网络、`OpenClash`、`VPN`、共享与配置的状态。

use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::cli::vpn_cfg::{vpn_local_routes, vpn_profiles, vpn_rejected, vpn_tunnels};
use crate::config::{Config, KEYS, RESTART_KEYS};
use crate::env::Env;
use crate::net;
use crate::sys::ce_ready;
use crate::vm;

/// 修改 `key` 需要重启服务（在 [`RESTART_KEYS`] 中）且虚拟机正在运行时返回 true。
#[must_use]
pub(crate) fn needs_restart(env: &Env, key: &str) -> bool {
    RESTART_KEYS.contains(&key) && vm::running(env)
}

/// 手机侧网络状态：有 `scripts/net.sh` 时取它的 `status` 输出，否则用内置实现；解析失败时为 `{"mode":"down"}`。
fn net_section(env: &Env) -> Value {
    let net_sh = env.moddir.join("scripts/net.sh");
    let net_raw = if net_sh.is_file() {
        Command::new("sh")
            .arg(&net_sh)
            .arg("status")
            .env("MODDIR", &env.moddir)
            .env("XR_DATA", &env.xr_data)
            .output()
            .ok()
            .map_or_default(|result| String::from_utf8_lossy(&result.stdout).into_owned())
    } else {
        net::status_json(env)
    };
    serde_json::from_str(net_raw.trim()).unwrap_or_else(|_| json!({"mode": "down"}))
}

/// 共享目录状态：开关、当前挂载方式（虚拟机未运行或未挂载时为 `off`）、路径、存储是否已解锁、是否支持读写挂载。
fn share_section(env: &Env, cfg: &Config, alive: bool) -> Value {
    let smode = if alive {
        fs::read_to_string(env.run_dir().join("share.mode"))
            .unwrap_or_default()
            .trim()
            .to_owned()
    } else {
        String::new()
    };
    let smode_out = if smode.is_empty() { "off" } else { &smode };
    json!({
        "enabled": cfg.vm_share(),
        "mode": smode_out,
        "path": cfg.vm_share_path(),
        "ce_ready": ce_ready(),
        "rw_supported": !env.rw_map_opt.is_empty(),
    })
}

/// 当前配置：[`KEYS`] 中每个键的原始取值。
fn config_section(cfg: &Config) -> Map<String, Value> {
    KEYS.iter()
        .map(|key| {
            (
                (*key).to_owned(),
                Value::String(cfg.get_raw(key).to_owned()),
            )
        })
        .collect()
}

/// 客机 `IPv6` 开关：虚拟机运行且客机上报过时为布尔值，否则为 `null`。
fn ipv6_value(env: &Env, alive: bool) -> Value {
    let flag = env.run_dir().join("guest/guest/ipv6");
    if alive && flag.is_file() {
        let value = fs::read_to_string(flag).unwrap_or_default();
        Value::Bool(value.trim() == "1")
    } else {
        Value::Null
    }
}

/// `OpenVPN` 配置概况。
#[derive(Debug, Clone, Copy, Default)]
struct VpnConfigured {
    /// 至少有一份非空配置。
    configured: bool,
    /// 至少有一份配置处于启用状态。
    enabled: bool,
}

/// `config/vpn/` 目录里的一项：非空 `.ovpn` 记为已配置，对应 `.enabled` 为 `1` 时记为已启用。
fn scan_vpn_entry(vpn_dir: &Path, file_path: &Path, acc: &mut VpnConfigured) {
    if file_path.extension().and_then(|raw_name| raw_name.to_str()) != Some("ovpn")
        || !fs::metadata(file_path).is_ok_and(|meta| meta.len() > 0)
    {
        return;
    }
    acc.configured = true;
    if let Some(id) = file_path.file_stem().and_then(|os_str| os_str.to_str())
        && fs::read_to_string(vpn_dir.join(format!("{id}.enabled")))
            .is_ok_and(|raw| raw.trim() == "1")
    {
        acc.enabled = true;
    }
}

/// 统计 `OpenVPN` 是否已配置、是否已启用：优先看 `config/vpn/` 目录，没有时看旧版单份 `config/openvpn.ovpn`。
fn vpn_configured(env: &Env) -> VpnConfigured {
    let mut acc = VpnConfigured::default();
    let vpn_dir = env.vpn_dir();
    if vpn_dir.is_dir() {
        if let Ok(rd) = fs::read_dir(&vpn_dir) {
            for ent in rd.flatten() {
                scan_vpn_entry(&vpn_dir, &ent.path(), &mut acc);
            }
        }
    } else if env.xr_data.join("config/openvpn.ovpn").is_file() {
        acc.configured = true;
        acc.enabled = fs::read_to_string(env.xr_data.join("config/openvpn.enabled"))
            .map_or(true, |text| text.trim() != "0");
    } else {
        // 没有任何 OpenVPN 配置：configured、enabled 保持 false
    }
    acc
}

/// `VPN` 状态：客机上报的连接状态与地址、已生效与被拒的网段、隧道、各份配置，以及配置概况。
fn vpn_section(env: &Env, alive: bool) -> Value {
    let run = env.run_dir();
    let mut vst = "down".to_owned();
    let mut vaddr = String::new();
    if alive {
        if let Ok(content) = fs::read_to_string(run.join("guest/guest/openvpn.status")) {
            let trimmed = content.trim();
            if !trimmed.is_empty() {
                trimmed.clone_into(&mut vst);
            }
        }
        fs::read_to_string(run.join("guest/guest/openvpn.addr"))
            .unwrap_or_default()
            .trim()
            .clone_into(&mut vaddr);
    }
    let act_raw = fs::read_to_string(run.join("routes.active")).unwrap_or_default();
    let act: Vec<&str> = act_raw.split_whitespace().collect();
    let profs = vpn_profiles(env, &vst);
    let tun = vpn_tunnels(env, &vst);
    let conf = vpn_configured(env);
    json!({
        "status": vst,
        "addr": vaddr,
        "routes_active": act,
        "routes_rejected": vpn_rejected(env),
        "routes_local": vpn_local_routes(env),
        "tunnels": tun,
        "profiles": profs,
        "configured": conf.configured,
        "enabled": conf.enabled,
    })
}

/// 向守护进程的 `run/stats.sock` 发一行请求并读一行 JSON 回复；连接、超时或解析任一步失败返回 `None`。
fn stats_query(env: &Env, request: &str, timeout: Duration) -> Option<Value> {
    let stream = UnixStream::connect(env.stats_sock()).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(timeout)).ok()?;
    let mut writer = &stream;
    writer.write_all(format!("{request}\n").as_bytes()).ok()?;
    let mut line = String::new();
    let read = BufReader::new(&stream).read_line(&mut line).ok()?;
    if read == 0 {
        return None;
    }
    serde_json::from_str(&line).ok()
}

/// 累计流量起始时间（`config/traffic.since` 中的 Unix 秒）；文件缺失或无法解析时为 0。
fn traffic_since(env: &Env) -> u64 {
    fs::read_to_string(env.acct_since_file())
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

/// 状态页的 `traffic` 段：向守护进程查询实时速率与累计值；守护进程不在时各项为 null、`source` 为 `none`（不回退到直接执行 iptables）。
fn traffic_section(env: &Env) -> Value {
    let since = traffic_since(env);
    let Some(reply) = stats_query(env, "rate", Duration::from_millis(800)) else {
        return json!({ "counting": null, "rate": null, "total": null, "since": since, "source": "none" });
    };
    let field = |key: &str| reply.get(key).cloned().unwrap_or(Value::Null);
    json!({
        "counting": field("counting"),
        "rate": field("rate"),
        "total": field("total"),
        "since": since,
        "source": "daemon",
    })
}

/// `xr status-json` 的 JSON 文本：虚拟机、手机侧网络（`scripts/net.sh status` 或内置实现）、客机 `IPv6`、`OpenClash`、`VPN`、共享目录与当前配置。
#[must_use]
pub(crate) fn json(env: &Env) -> String {
    let cfg = Config::load(env);
    let vm_st = vm::status(env);
    let vm_val = serde_json::to_value(&vm_st).unwrap_or_else(|_| json!({}));
    let alive = vm_st.running;
    let net_val = net_section(env);
    let share = share_section(env, &cfg, alive);
    let cfg_map = config_section(&cfg);
    let vpn = vpn_section(env, alive);
    json!({
        "vm": vm_val,
        "net": net_val,
        "ipv6": ipv6_value(env, alive),
        "clash": crate::clash::status(env, alive),
        "vpn": vpn,
        "share": share,
        "config": cfg_map,
        "traffic": traffic_section(env),
    })
    .to_string()
}

/// `xr status-line` 的一行文字：运行状态、管理地址、管理访问方式、已生效 `VPN` 网段数与熔断提示。
#[must_use]
pub(crate) fn line(env: &Env) -> String {
    let cfg = Config::load(env);
    let run = env.run_dir();
    let (vm_label, vm_detail) = if vm::running(env) {
        let st = fs::read_to_string(run.join("state")).unwrap_or_default();
        match st.trim() {
            "stable" => ("运行中", "运行中"),
            "ready" => ("运行中", "已就绪"),
            _ => ("启动中", "启动中"),
        }
    } else {
        ("已停止", "-")
    };
    let n = fs::read_to_string(run.join("routes.active"))
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    let trip = if env.tripped_file().is_file() {
        " / 熔断未确认"
    } else {
        ""
    };
    format!(
        "{vm_label} / {vm_detail} / {}.254 / 管理访问={} / VPN网段={n}{trip}",
        cfg.net_prefix(),
        cfg.expose()
    )
}
