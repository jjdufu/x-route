use std::fs;
use std::process::Command;

use serde_json::{Map, Value, json};

use crate::cli::vpn_cfg::{vpn_profiles, vpn_rejected, vpn_tunnels};
use crate::config::{Config, KEYS};
use crate::env::Env;
use crate::net::net_status_json;
use crate::sys::ce_ready;
use crate::vm::{vm_running, vm_status};

pub const RESTART_KEYS: &[&str] = &[
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

#[must_use]
pub fn needs_restart(env: &Env, key: &str) -> bool {
    RESTART_KEYS.contains(&key) && vm_running(env)
}

#[must_use]
pub fn json(env: &Env) -> String {
    let cfg = Config::load(env);
    let run = env.run_dir();
    let vm_st = vm_status(env);
    let vm_val = serde_json::to_value(&vm_st).unwrap_or_else(|_| json!({}));

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
        net_status_json(env)
    };
    let net_val: Value =
        serde_json::from_str(net_raw.trim()).unwrap_or_else(|_| json!({"mode": "down"}));

    let act_raw = fs::read_to_string(run.join("routes.active")).unwrap_or_default();
    let act: Vec<&str> = act_raw.split_whitespace().collect();

    let alive = vm_st.running;
    let smode = if alive {
        fs::read_to_string(run.join("share.mode"))
            .unwrap_or_default()
            .trim()
            .to_owned()
    } else {
        String::new()
    };
    let smode_out = if smode.is_empty() { "off" } else { &smode };
    let ce = ce_ready();
    let rw = !env.rw_map_opt.is_empty();

    let cfg_map: Map<String, Value> = KEYS
        .iter()
        .map(|key| {
            (
                (*key).to_owned(),
                Value::String(cfg.get_raw(key).to_owned()),
            )
        })
        .collect();

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

    let profs = vpn_profiles(env, &vst);
    let tun = vpn_tunnels(env, &vst);

    let v6: Value = if alive && run.join("guest/guest/ipv6").is_file() {
        let value = fs::read_to_string(run.join("guest/guest/ipv6")).unwrap_or_default();
        Value::Bool(value.trim() == "1")
    } else {
        Value::Null
    };

    let mut vcfg = false;
    let mut ven = false;
    let vpn_dir = env.vpn_dir();
    if vpn_dir.is_dir() {
        if let Ok(rd) = fs::read_dir(&vpn_dir) {
            for ent in rd.flatten() {
                let file_path = ent.path();
                if file_path.extension().and_then(|raw_name| raw_name.to_str()) == Some("ovpn")
                    && fs::metadata(&file_path).is_ok_and(|meta| meta.len() > 0)
                {
                    vcfg = true;
                    if let Some(id) = file_path.file_stem().and_then(|os_str| os_str.to_str())
                        && fs::read_to_string(vpn_dir.join(format!("{id}.enabled")))
                            .is_ok_and(|raw| raw.trim() == "1")
                    {
                        ven = true;
                    }
                }
            }
        }
    } else if env.xr_data.join("config/openvpn.ovpn").is_file() {
        vcfg = true;
        ven = fs::read_to_string(env.xr_data.join("config/openvpn.enabled"))
            .map_or(true, |text| text.trim() != "0");
    }

    json!({
        "vm": vm_val,
        "net": net_val,
        "ipv6": v6,
        "clash": crate::clash::status(env, alive),
        "vpn": {
            "status": vst,
            "addr": vaddr,
            "routes_active": act,
            "routes_rejected": vpn_rejected(env),
            "tunnels": tun,
            "profiles": profs,
            "configured": vcfg,
            "enabled": ven,
        },
        "share": {
            "enabled": cfg.vm_share(),
            "mode": smode_out,
            "path": cfg.vm_share_path(),
            "ce_ready": ce,
            "rw_supported": rw,
        },
        "config": cfg_map,
    })
    .to_string()
}

#[must_use]
pub fn line(env: &Env) -> String {
    let cfg = Config::load(env);
    let run = env.run_dir();
    let (vm_label, vm_detail) = if vm_running(env) {
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
