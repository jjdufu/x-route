use std::collections::BTreeSet;
use std::fs;
use std::iter;

use crate::cidr;
use crate::cidr::norm_route;
use crate::env::{XR_IF, XR_NET6};
use crate::log;
use crate::log::dlog;
use crate::net::common::NetCtx;
use crate::sys::fsx;

#[must_use]
pub fn guest_routes_file(ctx: &NetCtx<'_>) -> std::path::PathBuf {
    ctx.env.run_dir().join("guest/guest/openvpn.routes")
}

#[must_use]
pub fn scrape_ctl_routes(ctx: &NetCtx<'_>) -> String {
    let Ok(content) = fs::read_to_string(ctx.env.ctl_log()) else {
        return String::new();
    };
    let mut last = String::new();
    for line in content.lines() {
        if let Some((_, rhs)) = line.split_once("XR_VPN_ROUTES") {
            rhs.trim().clone_into(&mut last);
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
    raw.split(|ch: char| ch == ',' || ch.is_whitespace())
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .collect()
}

#[must_use]
pub fn is_default_route(route: &str) -> bool {
    matches!(
        route,
        "default" | "0.0.0.0/0" | "0.0.0.0/1" | "128.0.0.0/1" | "::/0" | "::/1" | "8000::/1"
    )
}

#[must_use]
pub fn is_ip6_cidr(route: &str) -> bool {
    route.contains(':')
}

#[must_use]
pub fn tether_ifaces(ctx: &NetCtx<'_>) -> Vec<String> {
    let out = ctx.ip(&["route", "show", "table", "local_network"]);
    let mut set = BTreeSet::new();
    for line in out.stdout.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        for (idx, tok) in tokens.iter().enumerate() {
            if *tok == "dev"
                && let Some(dev) = tokens.get(idx.saturating_add(1))
                && *dev != XR_IF
            {
                set.extend(iter::once((*dev).to_owned()));
            }
        }
    }
    set.into_iter().collect()
}

#[must_use]
pub fn routes_valid(ctx: &NetCtx<'_>) -> Vec<String> {
    let up = fs::read_to_string(ctx.env.run_dir().join("net.up_if")).unwrap_or_default();
    let up_trimmed = up.trim();
    let mut busy = vec![
        format!("{}.0/24", ctx.cfg.net_prefix()),
        "127.0.0.0/8".to_owned(),
        "169.254.0.0/16".to_owned(),
        format!("{XR_NET6}::/64"),
    ];
    if let Ok(hs_gw) = fs::read_to_string(ctx.env.run_dir().join("net.hs_gw")) {
        for tok in hs_gw.split_whitespace() {
            busy.push(tok.to_owned());
        }
    }
    let mut ifaces = Vec::new();
    if !up_trimmed.is_empty() {
        ifaces.push(up_trimmed.to_owned());
    }
    ifaces.extend(tether_ifaces(ctx));
    for iface in ifaces {
        let out4 = ctx.ip(&["-4", "-o", "addr", "show", "dev", &iface]);
        for line in out4.stdout.lines() {
            if let Some(cidr) = line.split_whitespace().nth(3) {
                busy.push(cidr.to_owned());
            }
        }
        let out6 = ctx.ip(&["-6", "-o", "addr", "show", "dev", &iface]);
        for line in out6.stdout.lines() {
            if let Some(cidr) = line.split_whitespace().nth(3) {
                busy.push(cidr.to_owned());
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
            log::err(ctx.env, "vpn", &format!("格式错误 {raw}"));
            continue;
        };
        if is_default_route(&nc) {
            continue;
        }
        if !cidr::is_aligned(&nc) {
            log::err(ctx.env, "vpn", &format!("主机位不为 0：{raw}"));
            continue;
        }
        if let Some(conflict) = busy
            .iter()
            .find(|used| !used.is_empty() && cidr::overlap(&nc, used))
        {
            log::err(ctx.env, "vpn", &format!("{nc} 与 {conflict} 重叠"));
            continue;
        }
        count = count.saturating_add(1);
        if count > 64 {
            log::err(ctx.env, "vpn", "超过 64 条，其余忽略");
            break;
        }
        valid_set.extend(iter::once(nc));
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

/// 与 [`ipx`] 相同（按 `cidr` 的地址族执行 `ip` 或 `ip -6`），只关心成败：失败时计数，不返回结果。
fn ipx_run(ctx: &NetCtx<'_>, cidr: &str, args: &[&str]) {
    crate::sys::exec::note_exec_result(&ipx(ctx, cidr, args));
}

pub fn route_add(ctx: &NetCtx<'_>, cidr: &str) {
    let via = route_via(ctx, cidr);
    let mode = fs::read_to_string(ctx.env.run_dir().join("net.mode")).unwrap_or_default();
    if mode.trim() == "ndc" {
        ctx.ndc_run(&["network", "route", "add", "local", XR_IF, cidr, &via]);
    } else {
        ipx_run(
            ctx,
            cidr,
            &[
                "route", "replace", cidr, "via", &via, "dev", XR_IF, "table", "97",
            ],
        );
    }
    ipx_run(ctx, cidr, &["rule", "del", "pref", "4801", "to", cidr]);
    let tbl = ctx.lnt();
    ipx_run(
        ctx,
        cidr,
        &["rule", "add", "pref", "4801", "to", cidr, "lookup", tbl],
    );
}

pub fn route_del(ctx: &NetCtx<'_>, cidr: &str) {
    let via = route_via(ctx, cidr);
    ctx.ndc_run(&["network", "route", "remove", "local", XR_IF, cidr, &via]);
    ipx_run(ctx, cidr, &["route", "del", cidr, "table", "97"]);
    ipx_run(ctx, cidr, &["rule", "del", "pref", "4801", "to", cidr]);
}

#[must_use]
pub fn cksum_crc_like(bytes: &[u8]) -> String {
    // Simple deterministic fingerprint matching cksum presence/absence semantics for net_watch
    let mut crc: u32 = 0;
    for &byte in bytes {
        crc = crc.wrapping_mul(31).wrapping_add(u32::from(byte));
    }
    format!("{crc} {}", bytes.len())
}

#[must_use]
pub fn routes_file_cksum(ctx: &NetCtx<'_>) -> String {
    fs::read(guest_routes_file(ctx))
        .map_or_else(|_| "none".to_owned(), |bytes| cksum_crc_like(&bytes))
}

pub fn routes_sync(ctx: &NetCtx<'_>) {
    let want = routes_valid(ctx);
    let active_path = ctx.env.run_dir().join("routes.active");
    let have: Vec<String> = fs::read_to_string(&active_path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
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
    for route in &have {
        if !want.contains(route) {
            route_del(ctx, route);
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
    fsx::write_logged(ctx.env, &active_path, body);
    fsx::write_logged(
        ctx.env,
        ctx.env.run_dir().join("vpn.cksum"),
        format!("{}\n", routes_file_cksum(ctx)),
    );
}

pub fn routes_reassert(ctx: &NetCtx<'_>) {
    let Ok(active) = fs::read_to_string(ctx.env.run_dir().join("routes.active")) else {
        return;
    };
    for route_cidr in active
        .lines()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        let out = ipx(ctx, route_cidr, &["route", "show", "table", "97"]);
        let needle = format!("{route_cidr} via");
        if !out.stdout.lines().any(|line| line.starts_with(&needle)) {
            route_add(ctx, route_cidr);
        }
    }
}

pub fn routes_clear(ctx: &NetCtx<'_>) {
    let active_path = ctx.env.run_dir().join("routes.active");
    if let Ok(active) = fs::read_to_string(&active_path) {
        for route in active
            .lines()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            route_del(ctx, route);
        }
    }
    fsx::remove_logged(ctx.env, &active_path);
}
