use std::collections::BTreeSet;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use crate::cidr::norm_route;
use crate::env::Env;
use crate::log::{log_info, unix_now};
use crate::net::vpn_routes::is_default_route;
use crate::vm::{vm_running, vpn_sync_host};

pub fn vpn_slug(name: &str) -> String {
    let mut out = String::new();
    let mut last_us = false;
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b == b'_' {
            out.push(b as char);
            last_us = false;
        } else if !last_us && !out.is_empty() {
            out.push('_');
            last_us = true;
        }
        if out.len() >= 32 {
            break;
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        let mut h = 0_u32;
        for b in name.as_bytes() {
            h = h.wrapping_mul(31).wrapping_add(u32::from(*b));
        }
        format!("p_{h}")
    } else {
        trimmed
    }
}

pub fn vpn_migrate_legacy(env: &Env) {
    let vpn_dir = env.vpn_dir();
    let legacy = env.xr_data.join("config/openvpn.ovpn");
    if !vpn_dir.is_dir() && fs::metadata(&legacy).is_ok_and(|m| m.len() > 0) {
        let _ = fs::create_dir_all(&vpn_dir);
        let mn = fs::read_to_string(env.xr_data.join("config/openvpn.name"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let disp = if mn.is_empty() {
            "OpenVPN".to_string()
        } else {
            mn
        };
        let mid = vpn_slug(&disp);
        let dst_ovpn = vpn_dir.join(format!("{mid}.ovpn"));
        let _ = fs::copy(&legacy, &dst_ovpn);
        #[cfg(unix)]
        {
            let _ = fs::set_permissions(&dst_ovpn, fs::Permissions::from_mode(0o600));
        }
        let _ = fs::write(vpn_dir.join(format!("{mid}.name")), format!("{disp}\n"));
        let men = fs::read_to_string(env.xr_data.join("config/openvpn.enabled"))
            .map_or("1\n", |s| if s.trim() == "0" { "0\n" } else { "1\n" });
        let _ = fs::write(vpn_dir.join(format!("{mid}.enabled")), men);
    }
}

pub fn vpn_mirror_legacy(env: &Env) {
    let vpn_dir = env.vpn_dir();
    if !vpn_dir.is_dir() {
        return;
    }
    let mut first_id: Option<String> = None;
    let mut en_id: Option<String> = None;
    if let Ok(rd) = fs::read_dir(&vpn_dir) {
        let mut paths: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            if p.extension().and_then(|s| s.to_str()) != Some("ovpn") {
                continue;
            }
            if fs::metadata(&p).map_or(true, |m| m.len() == 0) {
                continue;
            }
            let Some(pid) = p.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if first_id.is_none() {
                first_id = Some(pid.to_string());
            }
            if en_id.is_none()
                && fs::read_to_string(vpn_dir.join(format!("{pid}.enabled")))
                    .is_ok_and(|s| s.trim() == "1")
            {
                en_id = Some(pid.to_string());
            }
        }
    }
    let pick = en_id.clone().or(first_id);
    let cfg_dir = env.xr_data.join("config");
    if let Some(pid) = pick {
        let _ = fs::create_dir_all(&cfg_dir);
        let dst = cfg_dir.join("openvpn.ovpn");
        let _ = fs::copy(vpn_dir.join(format!("{pid}.ovpn")), &dst);
        #[cfg(unix)]
        {
            let _ = fs::set_permissions(&dst, fs::Permissions::from_mode(0o600));
        }
        let nm = fs::read_to_string(vpn_dir.join(format!("{pid}.name"))).unwrap_or(pid.clone());
        let _ = fs::write(cfg_dir.join("openvpn.name"), format!("{}\n", nm.trim()));
        let _ = fs::write(
            cfg_dir.join("openvpn.enabled"),
            if en_id.is_some() { "1\n" } else { "0\n" },
        );
    } else {
        let _ = fs::remove_file(cfg_dir.join("openvpn.ovpn"));
        let _ = fs::remove_file(cfg_dir.join("openvpn.name"));
        let _ = fs::remove_file(cfg_dir.join("openvpn.enabled"));
    }
}

pub fn vpn_rejected(env: &Env) -> Vec<String> {
    if !vm_running(env) {
        return Vec::new();
    }
    let act_raw = fs::read_to_string(env.run_dir().join("routes.active")).unwrap_or_default();
    let act_set: BTreeSet<&str> = act_raw.lines().map(str::trim).collect();
    let gf = env.run_dir().join("guest/guest/openvpn.routes");
    let Ok(raw) = fs::read_to_string(gf) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for tok in raw.split(|c: char| c == ',' || c.is_whitespace()) {
        if tok.is_empty() || is_default_route(tok) {
            continue;
        }
        match norm_route(tok) {
            Some(nc) => {
                if is_default_route(&nc) {
                    continue;
                }
                if !act_set.contains(nc.as_str()) {
                    out.push(nc);
                }
            }
            None => out.push(tok.to_string()),
        }
    }
    out
}

pub fn vpn_tunnels(env: &Env, summary_st: &str) -> Vec<Value> {
    let vn = fs::read_to_string(env.xr_data.join("config/openvpn.name"))
        .unwrap_or_default()
        .trim()
        .to_string();
    let mut out = Vec::new();
    let tun_file = env.run_dir().join("guest/guest/openvpn.tunnels");
    let alive = vm_running(env);
    if alive && fs::metadata(&tun_file).is_ok_and(|m| m.len() > 0) {
        if let Ok(content) = fs::read_to_string(&tun_file) {
            for line in content.lines() {
                let mut parts = line.splitn(5, '|');
                let dev = parts.next().unwrap_or("");
                if dev.is_empty() {
                    continue;
                }
                let raw_nm = parts.next().unwrap_or("");
                let ad = parts.next().unwrap_or("");
                let mt = parts
                    .next()
                    .unwrap_or("0")
                    .trim()
                    .parse::<u64>()
                    .unwrap_or(0);
                let rt = parts.next().unwrap_or("");
                let nm = if raw_nm == "xrouter" && !vn.is_empty() {
                    vn.clone()
                } else {
                    let nf = env.vpn_dir().join(format!("{raw_nm}.name"));
                    if nf.is_file() {
                        fs::read_to_string(nf)
                            .unwrap_or_else(|_| raw_nm.to_string())
                            .trim()
                            .to_string()
                    } else {
                        raw_nm.to_string()
                    }
                };
                let routes: Vec<&str> = rt
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .filter(|s| !s.is_empty())
                    .collect();
                out.push(json!({
                    "dev": dev,
                    "name": nm,
                    "status": "up",
                    "addr": ad,
                    "mtu": mt,
                    "routes": routes,
                }));
            }
        }
    }
    if out.is_empty() && env.xr_data.join("config/openvpn.ovpn").is_file() {
        let mut st = if summary_st == "up" {
            "connecting"
        } else {
            summary_st
        };
        if !alive {
            st = "down";
        }
        let nm = if vn.is_empty() { "OpenVPN" } else { &vn };
        out.push(json!({
            "dev": "",
            "name": nm,
            "status": st,
            "addr": "",
            "mtu": 0,
            "routes": [],
        }));
    }
    out
}

pub fn vpn_profiles(env: &Env, summary_st: &str) -> Vec<Value> {
    vpn_migrate_legacy(env);
    let vpn_dir = env.vpn_dir();
    let mut out = Vec::new();
    if !vpn_dir.is_dir() {
        return out;
    }
    let alive = vm_running(env);
    let tun_raw =
        fs::read_to_string(env.run_dir().join("guest/guest/openvpn.tunnels")).unwrap_or_default();
    let multi = env.run_dir().join("guest/guest/openvpn.multi").is_file();
    let conn_raw = fs::read_to_string(env.run_dir().join("guest/guest/openvpn.connecting"))
        .unwrap_or_default();

    if let Ok(rd) = fs::read_dir(&vpn_dir) {
        let mut paths: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            if p.extension().and_then(|s| s.to_str()) != Some("ovpn") {
                continue;
            }
            if fs::metadata(&p).map_or(true, |m| m.len() == 0) {
                continue;
            }
            let Some(pid) = p.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let pnm_raw = fs::read_to_string(vpn_dir.join(format!("{pid}.name")))
                .unwrap_or_default()
                .trim()
                .to_string();
            let pnm = if pnm_raw.is_empty() {
                pid.to_string()
            } else {
                pnm_raw
            };
            let pen = fs::read_to_string(vpn_dir.join(format!("{pid}.enabled")))
                .is_ok_and(|s| s.trim() == "1");

            let mut pst = "down".to_string();
            let mut pdev = String::new();
            let mut pad = String::new();
            let mut pmt = 0_u64;
            let mut prt = String::new();

            if alive {
                for line in tun_raw.lines() {
                    let mut parts = line.splitn(5, '|');
                    let td = parts.next().unwrap_or("");
                    if td.is_empty() {
                        continue;
                    }
                    let tn = parts.next().unwrap_or("");
                    let ta = parts.next().unwrap_or("");
                    let tm = parts
                        .next()
                        .unwrap_or("0")
                        .trim()
                        .parse::<u64>()
                        .unwrap_or(0);
                    let tr = parts.next().unwrap_or("");
                    if tn == pid || (tn == "xrouter" && pen && !multi) {
                        pst = "up".to_string();
                        pdev = td.to_string();
                        pad = ta.to_string();
                        pmt = tm;
                        prt = tr.to_string();
                        break;
                    }
                }
                if pst == "down"
                    && (conn_raw.lines().any(|l| l.trim() == pid)
                        || (pen && !multi && matches!(summary_st, "up" | "connecting")))
                {
                    pst = "connecting".to_string();
                }
            }
            let routes: Vec<&str> = prt
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|s| !s.is_empty())
                .collect();
            out.push(json!({
                "id": pid,
                "name": pnm,
                "enabled": pen,
                "status": pst,
                "dev": pdev,
                "addr": pad,
                "mtu": pmt,
                "routes": routes,
            }));
        }
    }
    out
}

fn vpn_next_cmd(env: &Env, act: &str) -> String {
    let mut ts = unix_now().to_string();
    let cur = fs::read_to_string(env.run_dir().join("guest/host/openvpn.cmd")).unwrap_or_default();
    let cand = format!("{act} {ts}");
    if cur.trim() == cand {
        ts = format!("{ts}{}", std::process::id());
    }
    format!("{act} {ts}\n")
}

fn valid_profile_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

pub fn vpn_import(env: &Env, path_arg: &str, enable_flag: &str) -> (i32, String) {
    let src_s = env.canon_share_path(path_arg);
    let media_s = env.media.to_string_lossy();
    if !src_s.starts_with(&format!("{media_s}/")) || src_s.contains("..") {
        return (1, "{\"ok\":false,\"error\":\"路径无效\"}".to_string());
    }
    let src = Path::new(&src_s);
    if !src.is_file() {
        return (1, "{\"ok\":false,\"error\":\"文件不存在\"}".to_string());
    }
    let sz = fs::metadata(src).map_or(0, |m| m.len());
    if !(8..=262_144).contains(&sz) {
        return (1, "{\"ok\":false,\"error\":\"文件大小不符\"}".to_string());
    }
    let Ok(bytes) = fs::read(src) else {
        return (1, "{\"ok\":false,\"error\":\"文件不存在\"}".to_string());
    };
    if bytes
        .iter()
        .any(|&b| !b.is_ascii_graphic() && !b.is_ascii_whitespace())
    {
        return (1, "{\"ok\":false,\"error\":\"不是文本配置\"}".to_string());
    }

    vpn_migrate_legacy(env);
    let vpn_dir = env.vpn_dir();
    let _ = fs::create_dir_all(env.xr_data.join("config"));
    let _ = fs::create_dir_all(&vpn_dir);

    let raw_bn = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("OpenVPN")
        .replace(['"', '\\'], "");
    let bn = if raw_bn.is_empty() {
        "OpenVPN".to_string()
    } else {
        raw_bn
    };

    let mut id = String::new();
    if let Ok(rd) = fs::read_dir(&vpn_dir) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p.extension().and_then(|s| s.to_str()) == Some("ovpn") {
                if let Some(eid) = p.file_stem().and_then(|s| s.to_str()) {
                    let enm = fs::read_to_string(vpn_dir.join(format!("{eid}.name")))
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    if enm == bn {
                        id = eid.to_string();
                        break;
                    }
                }
            }
        }
    }
    if id.is_empty() {
        let base = vpn_slug(&bn);
        let mut cand = base.clone();
        let mut n = 2_u32;
        while vpn_dir.join(format!("{cand}.ovpn")).exists() {
            cand = format!("{base}_{n}");
            n = n.saturating_add(1);
        }
        id = cand;
    }

    let dst_ovpn = vpn_dir.join(format!("{id}.ovpn"));
    let _ = fs::copy(src, &dst_ovpn);
    #[cfg(unix)]
    {
        let _ = fs::set_permissions(&dst_ovpn, fs::Permissions::from_mode(0o600));
    }
    let _ = fs::write(vpn_dir.join(format!("{id}.name")), format!("{bn}\n"));
    let en = if enable_flag == "0" {
        fs::read_to_string(vpn_dir.join(format!("{id}.enabled"))).is_ok_and(|s| s.trim() == "1")
    } else {
        true
    };
    let _ = fs::write(
        vpn_dir.join(format!("{id}.enabled")),
        if en { "1\n" } else { "0\n" },
    );
    vpn_mirror_legacy(env);

    if vm_running(env) {
        let _ = vpn_sync_host(env);
        if en {
            let host = env.run_dir().join("guest/host");
            let _ = fs::copy(src, host.join("openvpn.ovpn"));
            let _ = fs::write(host.join("openvpn.cmd"), vpn_next_cmd(env, "enable"));
        }
    }
    log_info(env, "xr", "已导入 OpenVPN 配置");
    (
        0,
        json!({
            "ok": true,
            "id": id,
            "name": bn,
        })
        .to_string(),
    )
}

pub fn vpn_set(env: &Env, act: &str, tid: &str) -> (i32, String) {
    let want = match act {
        "enable" => true,
        "disable" => false,
        _ => {
            return (
                2,
                "{\"ok\":false,\"error\":\"用法：xr vpn-enable|vpn-disable [ID]\"}".to_string(),
            )
        }
    };
    let vpn_dir = env.vpn_dir();
    if !tid.is_empty() {
        vpn_migrate_legacy(env);
        if !valid_profile_id(tid) {
            return (1, "{\"ok\":false,\"error\":\"配置无效\"}".to_string());
        }
        let ovpn = vpn_dir.join(format!("{tid}.ovpn"));
        if !fs::metadata(&ovpn).is_ok_and(|m| m.len() > 0) {
            return (1, "{\"ok\":false,\"error\":\"配置不存在\"}".to_string());
        }
        if !vm_running(env) {
            if want {
                return (1, "{\"ok\":false,\"error\":\"服务未运行\"}".to_string());
            }
            let _ = fs::write(vpn_dir.join(format!("{tid}.enabled")), "0\n");
            vpn_mirror_legacy(env);
            return (0, "{\"ok\":true}".to_string());
        }
        let multi = env.run_dir().join("guest/guest/openvpn.multi").is_file();
        if want && !multi {
            if let Ok(rd) = fs::read_dir(&vpn_dir) {
                for ent in rd.flatten() {
                    let p = ent.path();
                    if p.extension().and_then(|s| s.to_str()) == Some("ovpn") {
                        if let Some(oid) = p.file_stem().and_then(|s| s.to_str()) {
                            let _ = fs::write(vpn_dir.join(format!("{oid}.enabled")), "0\n");
                        }
                    }
                }
            }
        }
        let _ = fs::write(
            vpn_dir.join(format!("{tid}.enabled")),
            if want { "1\n" } else { "0\n" },
        );
        vpn_mirror_legacy(env);
        let any = vpn_sync_host(env).unwrap_or(false);
        let host = env.run_dir().join("guest/host");
        if want {
            let _ = fs::copy(&ovpn, host.join("openvpn.ovpn"));
        }
        let cmd_act = if any { "enable" } else { "disable" };
        let cmd_line = vpn_next_cmd(env, cmd_act);
        let _ = fs::write(host.join("openvpn.cmd"), &cmd_line);
        let cmd_trim = cmd_line.trim();
        let guest = env.run_dir().join("guest/guest");
        for _ in 0..env.vpn_wait {
            let ack = fs::read_to_string(guest.join("openvpn.ack")).unwrap_or_default();
            if ack.trim() == cmd_trim {
                return (0, "{\"ok\":true}".to_string());
            }
            let vst = fs::read_to_string(guest.join("openvpn.status"))
                .unwrap_or_default()
                .trim()
                .to_string();
            let tun = fs::read_to_string(guest.join("openvpn.tunnels")).unwrap_or_default();
            let conn = fs::read_to_string(guest.join("openvpn.connecting")).unwrap_or_default();
            let has_tun = tun.contains(&format!("|{tid}|"));
            let has_conn = conn.lines().any(|l| l.trim() == tid);
            let is_multi = guest.join("openvpn.multi").is_file();
            if want {
                if has_tun || has_conn || (!is_multi && matches!(vst.as_str(), "up" | "connecting"))
                {
                    return (0, "{\"ok\":true}".to_string());
                }
            } else if !has_tun && !has_conn && (is_multi || vst == "down" || vst.is_empty()) {
                return (0, "{\"ok\":true}".to_string());
            }
            thread::sleep(Duration::from_secs(1));
        }
        return (1, "{\"ok\":false,\"error\":\"未响应\"}".to_string());
    }

    let legacy = env.xr_data.join("config/openvpn.ovpn");
    if !legacy.is_file() {
        return (1, "{\"ok\":false,\"error\":\"未导入配置\"}".to_string());
    }
    let _ = fs::create_dir_all(env.xr_data.join("config"));
    let _ = fs::write(
        env.xr_data.join("config/openvpn.enabled"),
        if want { "1\n" } else { "0\n" },
    );
    if vpn_dir.is_dir() {
        if let Ok(rd) = fs::read_dir(&vpn_dir) {
            let mut paths: Vec<_> = rd.flatten().map(|e| e.path()).collect();
            paths.sort();
            let mut has_en = false;
            let mut first_id: Option<String> = None;
            for p in &paths {
                if p.extension().and_then(|s| s.to_str()) == Some("ovpn")
                    && fs::metadata(p).is_ok_and(|m| m.len() > 0)
                {
                    if let Some(vid) = p.file_stem().and_then(|s| s.to_str()) {
                        if first_id.is_none() {
                            first_id = Some(vid.to_string());
                        }
                        if !want {
                            let _ = fs::write(vpn_dir.join(format!("{vid}.enabled")), "0\n");
                        } else if fs::read_to_string(vpn_dir.join(format!("{vid}.enabled")))
                            .is_ok_and(|s| s.trim() == "1")
                        {
                            has_en = true;
                        }
                    }
                }
            }
            if want && !has_en {
                if let Some(fid) = first_id {
                    let _ = fs::write(vpn_dir.join(format!("{fid}.enabled")), "1\n");
                }
            }
        }
    }
    if !vm_running(env) {
        if want {
            return (1, "{\"ok\":false,\"error\":\"服务未运行\"}".to_string());
        }
        return (0, "{\"ok\":true}".to_string());
    }
    let host = env.run_dir().join("guest/host");
    let _ = fs::create_dir_all(&host);
    if vpn_dir.is_dir() {
        let _ = vpn_sync_host(env);
    }
    if want {
        let _ = fs::copy(&legacy, host.join("openvpn.ovpn"));
    }
    let _ = fs::write(host.join("openvpn.cmd"), vpn_next_cmd(env, act));
    let guest = env.run_dir().join("guest/guest");
    for _ in 0..env.vpn_wait {
        let vst = fs::read_to_string(guest.join("openvpn.status"))
            .unwrap_or_default()
            .trim()
            .to_string();
        if want {
            if matches!(vst.as_str(), "up" | "connecting") {
                return (0, "{\"ok\":true}".to_string());
            }
        } else if vst == "down" || vst.is_empty() {
            return (0, "{\"ok\":true}".to_string());
        }
        thread::sleep(Duration::from_secs(1));
    }
    (1, "{\"ok\":false,\"error\":\"未响应\"}".to_string())
}

pub fn vpn_delete(env: &Env, did: &str) -> (i32, String) {
    vpn_migrate_legacy(env);
    if !valid_profile_id(did) {
        return (1, "{\"ok\":false,\"error\":\"配置无效\"}".to_string());
    }
    let vpn_dir = env.vpn_dir();
    let ovpn = vpn_dir.join(format!("{did}.ovpn"));
    if !ovpn.is_file() {
        return (1, "{\"ok\":false,\"error\":\"配置不存在\"}".to_string());
    }
    let was_en =
        fs::read_to_string(vpn_dir.join(format!("{did}.enabled"))).is_ok_and(|s| s.trim() == "1");
    let _ = fs::remove_file(&ovpn);
    let _ = fs::remove_file(vpn_dir.join(format!("{did}.name")));
    let _ = fs::remove_file(vpn_dir.join(format!("{did}.enabled")));
    vpn_mirror_legacy(env);
    if vm_running(env) {
        let any = vpn_sync_host(env).unwrap_or(false);
        if was_en || !any {
            let host = env.run_dir().join("guest/host");
            if !any {
                let _ = fs::remove_file(host.join("openvpn.ovpn"));
            }
            let act = if any { "enable" } else { "disable" };
            let _ = fs::write(host.join("openvpn.cmd"), vpn_next_cmd(env, act));
        }
    }
    log_info(env, "xr", "已删除 OpenVPN 配置");
    (0, "{\"ok\":true}".to_string())
}

pub fn traffic_split(env: &Env, wait_secs: u64) -> (i32, String) {
    if !vm_running(env) {
        return (1, "{\"ok\":false,\"error\":\"服务未运行\"}".to_string());
    }
    let host = env.run_dir().join("guest/host");
    let _ = fs::create_dir_all(&host);
    let ts = format!("{}{}", unix_now(), std::process::id());
    let _ = fs::write(host.join("acct.cmd"), format!("snap {ts}\n"));
    let acct_file = env.run_dir().join("guest/guest/acct");
    let expected = format!("ts={ts}");
    for _ in 0..wait_secs.saturating_mul(5) {
        if let Ok(content) = fs::read_to_string(&acct_file) {
            let mut lines = content.lines();
            if lines.next().unwrap_or("") == expected {
                let vals: Vec<u64> = lines
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .take(8)
                    .map(|s| s.parse::<u64>().unwrap_or(0))
                    .collect();
                let g = |idx: usize| vals.get(idx).copied().unwrap_or(0);
                let (pwu, pwd, plu, pld) = (g(0), g(1), g(2), g(3));
                let (hwu, hwd, hlu, hld) = (g(4), g(5), g(6), g(7));
                return (
                    0,
                    json!({
                        "ok": true,
                        "phone": {
                            "wan": {"up": pwu, "down": pwd},
                            "lan": {"up": plu, "down": pld},
                            "total": {"up": pwu.saturating_add(plu), "down": pwd.saturating_add(pld)},
                        },
                        "hotspot": {
                            "wan": {"up": hwu, "down": hwd},
                            "lan": {"up": hlu, "down": hld},
                            "total": {"up": hwu.saturating_add(hlu), "down": hwd.saturating_add(hld)},
                        }
                    })
                    .to_string(),
                );
            }
        }
        thread::sleep(Duration::from_millis(200));
    }
    (1, "{\"ok\":false,\"error\":\"服务未响应\"}".to_string())
}

pub fn list_dir(env: &Env, path_arg: &str, mode: &str) -> (i32, Vec<String>) {
    let raw = if path_arg.is_empty() {
        env.media.to_string_lossy().into_owned()
    } else {
        path_arg.to_string()
    };
    let p_str = env.canon_share_path(&raw);
    let media_s = env.media.to_string_lossy();
    if (p_str != media_s.as_ref() && !p_str.starts_with(&format!("{media_s}/")))
        || p_str.contains("..")
    {
        return (1, Vec::new());
    }
    let p = Path::new(&p_str);
    if !p.is_dir() {
        return (1, Vec::new());
    }
    let mut items = Vec::new();
    if let Ok(rd) = fs::read_dir(p) {
        for ent in rd.flatten() {
            let ep = ent.path();
            let Some(name) = ep.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            if ep.is_dir() {
                items.push(format!("d/{name}"));
            } else if mode == "ovpn"
                && ep.extension().and_then(|s| s.to_str()).is_some_and(|e| {
                    e.eq_ignore_ascii_case("ovpn") || e.eq_ignore_ascii_case("conf")
                })
            {
                items.push(format!("f/{name}"));
            }
        }
    }
    items.sort();
    (0, items)
}
