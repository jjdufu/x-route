use std::collections::BTreeSet;
use std::fs;

use crate::cidr::{cidr_aligned, cidr_overlap, norm_route};
use crate::env::{XR_IF, XR_NET6};
use crate::log::{dlog, log_err};
use crate::net::common::NetCtx;

pub fn guest_routes_file(ctx: &NetCtx<'_>) -> std::path::PathBuf {
    ctx.env.run_dir().join("guest/guest/openvpn.routes")
}

pub fn scrape_ctl_routes(ctx: &NetCtx<'_>) -> String {
    let Ok(content) = fs::read_to_string(ctx.env.ctl_log()) else {
        return String::new();
    };
    let mut last = String::new();
    for line in content.lines() {
        if let Some((_, rhs)) = line.split_once("XR_VPN_ROUTES") {
            last = rhs.trim().to_string();
        }
    }
    last
}

pub fn routes_source(ctx: &NetCtx<'_>) -> Vec<String> {
    let gf = guest_routes_file(ctx);
    let raw = if gf.is_file() {
        fs::read_to_string(&gf).unwrap_or_default()
    } else {
        scrape_ctl_routes(ctx)
    };
    raw.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect()
}

pub fn is_default_route(r: &str) -> bool {
    matches!(
        r,
        "default" | "0.0.0.0/0" | "0.0.0.0/1" | "128.0.0.0/1" | "::/0" | "::/1" | "8000::/1"
    )
}

pub fn is_ip6_cidr(r: &str) -> bool {
    r.contains(':')
}

pub fn tether_ifaces(ctx: &NetCtx<'_>) -> Vec<String> {
    let out = ctx.ip(&["route", "show", "table", "local_network"]);
    let mut set = BTreeSet::new();
    for line in out.stdout.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        for (idx, tok) in tokens.iter().enumerate() {
            if *tok == "dev" {
                if let Some(dev) = tokens.get(idx.saturating_add(1)) {
                    if *dev != XR_IF {
                        set.insert((*dev).to_string());
                    }
                }
            }
        }
    }
    set.into_iter().collect()
}

pub fn routes_valid(ctx: &NetCtx<'_>) -> Vec<String> {
    let up = fs::read_to_string(ctx.env.run_dir().join("net.up_if")).unwrap_or_default();
    let up = up.trim();
    let mut busy = vec![
        format!("{}.0/24", ctx.cfg.net_prefix()),
        "127.0.0.0/8".to_string(),
        "169.254.0.0/16".to_string(),
        format!("{XR_NET6}::/64"),
    ];
    if let Ok(hs_gw) = fs::read_to_string(ctx.env.run_dir().join("net.hs_gw")) {
        for tok in hs_gw.split_whitespace() {
            busy.push(tok.to_string());
        }
    }
    let mut ifaces = Vec::new();
    if !up.is_empty() {
        ifaces.push(up.to_string());
    }
    ifaces.extend(tether_ifaces(ctx));
    for iface in ifaces {
        let out4 = ctx.ip(&["-4", "-o", "addr", "show", "dev", &iface]);
        for line in out4.stdout.lines() {
            if let Some(cidr) = line.split_whitespace().nth(3) {
                busy.push(cidr.to_string());
            }
        }
        let out6 = ctx.ip(&["-6", "-o", "addr", "show", "dev", &iface]);
        for line in out6.stdout.lines() {
            if let Some(cidr) = line.split_whitespace().nth(3) {
                busy.push(cidr.to_string());
            }
        }
    }

    let mut count = 0_usize;
    let mut valid_set = BTreeSet::new();
    for raw in routes_source(ctx) {
        if is_default_route(&raw) {
            continue;
        }
        let Some(nc) = norm_route(&raw) else {
            log_err(ctx.env, "vpn", &format!("格式错误 {raw}"));
            continue;
        };
        if is_default_route(&nc) {
            continue;
        }
        if !cidr_aligned(&nc) {
            log_err(ctx.env, "vpn", &format!("主机位不为 0：{raw}"));
            continue;
        }
        if let Some(conflict) = busy.iter().find(|b| !b.is_empty() && cidr_overlap(&nc, b)) {
            log_err(ctx.env, "vpn", &format!("{nc} 与 {conflict} 重叠"));
            continue;
        }
        count = count.saturating_add(1);
        if count > 64 {
            log_err(ctx.env, "vpn", "超过 64 条，其余忽略");
            break;
        }
        valid_set.insert(nc);
    }
    valid_set.into_iter().collect()
}

fn route_via(ctx: &NetCtx<'_>, cidr: &str) -> String {
    if is_ip6_cidr(cidr) {
        format!("{XR_NET6}::254")
    } else {
        format!("{}.254", ctx.cfg.net_prefix())
    }
}

fn ipx(ctx: &NetCtx<'_>, cidr: &str, args: &[&str]) -> crate::sys::exec::ExecResult {
    if is_ip6_cidr(cidr) {
        ctx.ip6(args)
    } else {
        ctx.ip(args)
    }
}

pub fn route_add(ctx: &NetCtx<'_>, cidr: &str) {
    let via = route_via(ctx, cidr);
    let mode = fs::read_to_string(ctx.env.run_dir().join("net.mode")).unwrap_or_default();
    if mode.trim() == "ndc" {
        let _ = ctx.ndc(&["network", "route", "add", "local", XR_IF, cidr, &via]);
    } else {
        let _ = ipx(
            ctx,
            cidr,
            &[
                "route", "replace", cidr, "via", &via, "dev", XR_IF, "table", "97",
            ],
        );
    }
    let _ = ipx(ctx, cidr, &["rule", "del", "pref", "4801", "to", cidr]);
    let tbl = ctx.lnt();
    let _ = ipx(
        ctx,
        cidr,
        &["rule", "add", "pref", "4801", "to", cidr, "lookup", tbl],
    );
}

pub fn route_del(ctx: &NetCtx<'_>, cidr: &str) {
    let via = route_via(ctx, cidr);
    let _ = ctx.ndc(&["network", "route", "remove", "local", XR_IF, cidr, &via]);
    let _ = ipx(ctx, cidr, &["route", "del", cidr, "table", "97"]);
    let _ = ipx(ctx, cidr, &["rule", "del", "pref", "4801", "to", cidr]);
}

pub fn cksum_crc_like(bytes: &[u8]) -> String {
    // Simple deterministic fingerprint matching cksum presence/absence semantics for net_watch
    let mut crc: u32 = 0;
    for &b in bytes {
        crc = crc.wrapping_mul(31).wrapping_add(u32::from(b));
    }
    format!("{crc} {}", bytes.len())
}

pub fn routes_file_cksum(ctx: &NetCtx<'_>) -> String {
    match fs::read(guest_routes_file(ctx)) {
        Ok(bytes) => cksum_crc_like(&bytes),
        Err(_) => "none".to_string(),
    }
}

pub fn routes_sync(ctx: &NetCtx<'_>) {
    let want = routes_valid(ctx);
    let active_path = ctx.env.run_dir().join("routes.active");
    let have: Vec<String> = fs::read_to_string(&active_path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect();

    if want != have {
        dlog(
            ctx.env,
            ctx.cfg,
            "vpn",
            &format!(
                "OpenVPN 路由变化：[{}] -> [{}]",
                have.join(" "),
                want.join(" ")
            ),
        );
    }
    for h in &have {
        if !want.contains(h) {
            route_del(ctx, h);
        }
    }
    for w in &want {
        if !have.contains(w) {
            route_add(ctx, w);
        }
    }
    let body = if want.is_empty() {
        String::new()
    } else {
        format!("{}\n", want.join("\n"))
    };
    let _ = fs::write(&active_path, body);
    let _ = fs::write(
        ctx.env.run_dir().join("vpn.cksum"),
        format!("{}\n", routes_file_cksum(ctx)),
    );
}

pub fn routes_reassert(ctx: &NetCtx<'_>) {
    let Ok(active) = fs::read_to_string(ctx.env.run_dir().join("routes.active")) else {
        return;
    };
    for c in active.lines().map(str::trim).filter(|s| !s.is_empty()) {
        let out = ipx(ctx, c, &["route", "show", "table", "97"]);
        let needle = format!("{c} via");
        if !out.stdout.lines().any(|l| l.starts_with(&needle)) {
            route_add(ctx, c);
        }
    }
}

pub fn routes_clear(ctx: &NetCtx<'_>) {
    let active_path = ctx.env.run_dir().join("routes.active");
    if let Ok(active) = fs::read_to_string(&active_path) {
        for c in active.lines().map(str::trim).filter(|s| !s.is_empty()) {
            route_del(ctx, c);
        }
    }
    let _ = fs::remove_file(&active_path);
}
