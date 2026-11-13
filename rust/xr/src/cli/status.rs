use std::fs;
use std::process::Command;

use serde_json::{json, Map, Value};

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

pub fn needs_restart(env: &Env, key: &str) -> bool {
    RESTART_KEYS.contains(&key) && vm_running(env)
}

pub fn status_json(env: &Env) -> String {
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
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
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
            .to_string()
    } else {
        String::new()
    };
    let smode_out = if smode.is_empty() { "off" } else { &smode };
    let ce = ce_ready();
    let rw = !env.rw_map_opt.is_empty();

    let mut cfg_map = Map::new();
    for k in KEYS {
        cfg_map.insert((*k).to_string(), Value::String(cfg.get_raw(k).to_string()));
    }

    let mut vst = "down".to_string();
    let mut vaddr = String::new();
    if alive {
        if let Ok(s) = fs::read_to_string(run.join("guest/guest/openvpn.status")) {
            let t = s.trim();
            if !t.is_empty() {
                vst = t.to_string();
            }
        }
        vaddr = fs::read_to_string(run.join("guest/guest/openvpn.addr"))
            .unwrap_or_default()
            .trim()
            .to_string();
    }

    let profs = vpn_profiles(env, &vst);
    let tun = vpn_tunnels(env, &vst);

    let v6: Value = if alive && run.join("guest/guest/ipv6").is_file() {
        let v = fs::read_to_string(run.join("guest/guest/ipv6")).unwrap_or_default();
        Value::Bool(v.trim() == "1")
    } else {
        Value::Null
    };

    let mut vcfg = false;
    let mut ven = false;
    let vpn_dir = env.vpn_dir();
    if vpn_dir.is_dir() {
        if let Ok(rd) = fs::read_dir(&vpn_dir) {
            for ent in rd.flatten() {
                let p = ent.path();
                if p.extension().and_then(|s| s.to_str()) == Some("ovpn")
                    && fs::metadata(&p).is_ok_and(|m| m.len() > 0)
                {
                    vcfg = true;
                    if let Some(id) = p.file_stem().and_then(|s| s.to_str()) {
                        if fs::read_to_string(vpn_dir.join(format!("{id}.enabled")))
                            .is_ok_and(|s| s.trim() == "1")
                        {
                            ven = true;
                        }
                    }
                }
            }
        }
    } else if env.xr_data.join("config/openvpn.ovpn").is_file() {
        vcfg = true;
        ven = fs::read_to_string(env.xr_data.join("config/openvpn.enabled"))
            .map_or(true, |s| s.trim() != "0");
    }

    json!({
        "vm": vm_val,
        "net": net_val,
        "ipv6": v6,
        "clash": crate::clash::clash_status(env, alive),
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

pub fn status_line(env: &Env) -> String {
    let cfg = Config::load(env);
    let run = env.run_dir();
    let (r, s) = if vm_running(env) {
        let st = fs::read_to_string(run.join("state")).unwrap_or_default();
        let label = match st.trim() {
            "stable" => "运行中",
            "ready" => "已就绪",
            _ => "启动中",
        };
        ("运行中", label)
    } else {
        ("已停止", "-")
    };
    let n = fs::read_to_string(run.join("routes.active"))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    let trip = if env.tripped_file().is_file() {
        " / 熔断未确认"
    } else {
        ""
    };
    format!(
        "{r} / {s} / {}.254 / 管理访问={} / VPN网段={n}{trip}",
        cfg.net_prefix(),
        cfg.expose()
    )
}
