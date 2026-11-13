use std::collections::BTreeSet;
use std::fs;

use crate::env::XR_IF;
use crate::log::unix_now;
use crate::net::common::{
    combine_quad_add, combine_quad_max, read_quad, write_quad, NetCtx, TrafficCounters,
    TrafficSummary,
};
use crate::sys::DirLock;

fn acct_lock(ctx: &NetCtx<'_>) -> DirLock {
    DirLock::acquire(ctx.env.run_dir().join("acct.lock"), 30, 100, false)
}

pub fn acct_lan4(ctx: &NetCtx<'_>) -> Vec<String> {
    let mut set = BTreeSet::new();
    for base in [
        "10.0.0.0/8",
        "172.16.0.0/12",
        "192.168.0.0/16",
        "100.64.0.0/10",
        "169.254.0.0/16",
        "127.0.0.0/8",
        "224.0.0.0/4",
        "255.255.255.255/32",
    ] {
        set.insert(base.to_string());
    }
    let pfx = ctx.cfg.net_prefix();
    if !pfx.is_empty() {
        set.insert(format!("{pfx}.0/24"));
    }
    if let Ok(active) = fs::read_to_string(ctx.env.run_dir().join("routes.active")) {
        for line in active.lines().map(str::trim).filter(|s| !s.is_empty()) {
            if !line.contains(':') {
                set.insert(line.to_string());
            }
        }
    }
    set.into_iter().collect()
}

pub fn acct_lan6(ctx: &NetCtx<'_>) -> Vec<String> {
    let mut set = BTreeSet::new();
    for base in ["fc00::/7", "fe80::/10", "ff00::/8", "::1/128"] {
        set.insert(base.to_string());
    }
    if let Ok(active) = fs::read_to_string(ctx.env.run_dir().join("routes.active")) {
        for line in active.lines().map(str::trim).filter(|s| !s.is_empty()) {
            if line.contains(':') {
                set.insert(line.to_string());
            }
        }
    }
    set.into_iter().collect()
}

fn acct_chain_bytes(ctx: &NetCtx<'_>, use_v6: bool, chain: &str) -> (u64, u64) {
    let out = if use_v6 {
        ctx.ip6t(&["-nvxL", chain])
    } else {
        ctx.ipt(&["-nvxL", chain])
    };
    let mut bytes_vec = Vec::new();
    for line in out.stdout.lines().skip(2) {
        if let Some(tok) = line.split_whitespace().nth(1) {
            if tok.bytes().all(|b| b.is_ascii_digit()) {
                bytes_vec.push(tok.parse::<u64>().unwrap_or(0));
            }
        }
    }
    let Some((&wan, lan_slice)) = bytes_vec.split_last() else {
        return (0, 0);
    };
    let mut lan = 0_u64;
    for &b in lan_slice {
        lan = lan.saturating_add(b);
    }
    (lan, wan)
}

pub fn acct_present(ctx: &NetCtx<'_>) -> bool {
    ctx.ipt(&["-S", "XR_ACCT_UP"]).success()
}

pub fn acct_present6(ctx: &NetCtx<'_>) -> bool {
    ctx.ip6t(&["-S", "XR_ACCT6_UP"]).success()
}

pub fn acct_live(ctx: &NetCtx<'_>) -> [u64; 4] {
    let (u_lan, u_wan) = acct_chain_bytes(ctx, false, "XR_ACCT_UP");
    let (d_lan, d_wan) = acct_chain_bytes(ctx, false, "XR_ACCT_DN");
    let (u6_lan, u6_wan, d6_lan, d6_wan) = if acct_present6(ctx) {
        let (ul, uw) = acct_chain_bytes(ctx, true, "XR_ACCT6_UP");
        let (dl, dw) = acct_chain_bytes(ctx, true, "XR_ACCT6_DN");
        (ul, uw, dl, dw)
    } else {
        (0, 0, 0, 0)
    };
    [
        u_wan.saturating_add(u6_wan),
        d_wan.saturating_add(d6_wan),
        u_lan.saturating_add(u6_lan),
        d_lan.saturating_add(d6_lan),
    ]
}

pub fn acct_total(ctx: &NetCtx<'_>) -> [u64; 4] {
    let base = read_quad(&ctx.env.acct_base_file());
    if acct_present(ctx) {
        combine_quad_add(base, acct_live(ctx))
    } else {
        combine_quad_max(base, read_quad(&ctx.env.acct_snap_file()))
    }
}

fn acct_fold(ctx: &NetCtx<'_>) {
    let total = combine_quad_add(read_quad(&ctx.env.acct_base_file()), acct_live(ctx));
    write_quad(&ctx.env.acct_base_file(), total);
    write_quad(&ctx.env.acct_snap_file(), total);
}

fn acct_rules(ctx: &NetCtx<'_>, use_v6: bool, prefix: &str, lan_list: &[String]) {
    let up = format!("{prefix}_UP");
    let dn = format!("{prefix}_DN");
    if use_v6 {
        let _ = ctx.ip6t(&["-F", &up]);
        let _ = ctx.ip6t(&["-F", &dn]);
        for c in lan_list {
            let _ = ctx.ip6t(&["-A", &up, "-d", c, "-j", "RETURN"]);
            let _ = ctx.ip6t(&["-A", &dn, "-s", c, "-j", "RETURN"]);
        }
        let _ = ctx.ip6t(&["-A", &up, "-j", "RETURN"]);
        let _ = ctx.ip6t(&["-A", &dn, "-j", "RETURN"]);
    } else {
        let _ = ctx.ipt(&["-F", &up]);
        let _ = ctx.ipt(&["-F", &dn]);
        for c in lan_list {
            let _ = ctx.ipt(&["-A", &up, "-d", c, "-j", "RETURN"]);
            let _ = ctx.ipt(&["-A", &dn, "-s", c, "-j", "RETURN"]);
        }
        let _ = ctx.ipt(&["-A", &up, "-j", "RETURN"]);
        let _ = ctx.ipt(&["-A", &dn, "-j", "RETURN"]);
    }
}

fn acct_new(ctx: &NetCtx<'_>, use_v6: bool, prefix: &str) -> bool {
    let up = format!("{prefix}_UP");
    let dn = format!("{prefix}_DN");
    let ok = if use_v6 {
        ctx.ip6t(&["-N", &up]).success()
    } else {
        ctx.ipt(&["-N", &up]).success()
    };
    if !ok {
        return false;
    }
    if use_v6 {
        let _ = ctx.ip6t(&["-N", &dn]);
        let _ = ctx.ip6t(&["-N", prefix]);
        let _ = ctx.ip6t(&["-F", prefix]);
        let _ = ctx.ip6t(&["-A", prefix, "-i", XR_IF, "-j", &up]);
        let _ = ctx.ip6t(&["-A", prefix, "-o", XR_IF, "-j", &dn]);
    } else {
        let _ = ctx.ipt(&["-N", &dn]);
        let _ = ctx.ipt(&["-N", prefix]);
        let _ = ctx.ipt(&["-F", prefix]);
        let _ = ctx.ipt(&["-A", prefix, "-i", XR_IF, "-j", &up]);
        let _ = ctx.ipt(&["-A", prefix, "-o", XR_IF, "-j", &dn]);
    }
    true
}

fn acct_hooks_one(ctx: &NetCtx<'_>, use_v6: bool, prefix: &str) {
    if use_v6 {
        if !ctx
            .ip6t(&["-C", "INPUT", "-i", XR_IF, "-j", prefix])
            .success()
        {
            let _ = ctx.ip6t(&["-I", "INPUT", "1", "-i", XR_IF, "-j", prefix]);
        }
        if !ctx
            .ip6t(&["-C", "OUTPUT", "-o", XR_IF, "-j", prefix])
            .success()
        {
            let _ = ctx.ip6t(&["-I", "OUTPUT", "1", "-o", XR_IF, "-j", prefix]);
        }
    } else {
        if !ctx
            .ipt(&["-C", "INPUT", "-i", XR_IF, "-j", prefix])
            .success()
        {
            let _ = ctx.ipt(&["-I", "INPUT", "1", "-i", XR_IF, "-j", prefix]);
        }
        if !ctx
            .ipt(&["-C", "OUTPUT", "-o", XR_IF, "-j", prefix])
            .success()
        {
            let _ = ctx.ipt(&["-I", "OUTPUT", "1", "-o", XR_IF, "-j", prefix]);
        }
    }
}

pub fn acct_hooks(ctx: &NetCtx<'_>) {
    if acct_present(ctx) {
        acct_hooks_one(ctx, false, "XR_ACCT");
    }
    if acct_present6(ctx) {
        acct_hooks_one(ctx, true, "XR_ACCT6");
    }
}

pub fn acct_setup(ctx: &NetCtx<'_>, with_v6: bool) {
    let l4 = acct_lan4(ctx);
    let l6 = if with_v6 { acct_lan6(ctx) } else { Vec::new() };
    let sig = format!("4:{}|6:{}", l4.join(" "), l6.join(" "));
    let lock = acct_lock(ctx);
    let fresh = acct_new(ctx, false, "XR_ACCT");
    let fresh6 = with_v6 && acct_new(ctx, true, "XR_ACCT6");
    if fresh {
        let merged = combine_quad_max(
            read_quad(&ctx.env.acct_base_file()),
            read_quad(&ctx.env.acct_snap_file()),
        );
        write_quad(&ctx.env.acct_base_file(), merged);
        let since = ctx.env.acct_since_file();
        if !since.is_file() {
            let _ = fs::write(since, format!("{}\n", unix_now()));
        }
    }
    let sig_file = ctx.env.run_dir().join("acct.sig");
    let old_sig = fs::read_to_string(&sig_file).unwrap_or_default();
    if fresh || fresh6 || old_sig.trim() != sig.trim() {
        if !fresh {
            acct_fold(ctx);
        }
        acct_rules(ctx, false, "XR_ACCT", &l4);
        if with_v6 {
            acct_rules(ctx, true, "XR_ACCT6", &l6);
        }
        let _ = fs::write(sig_file, format!("{sig}\n"));
    }
    drop(lock);
    acct_hooks(ctx);
}

fn acct_drop(ctx: &NetCtx<'_>, use_v6: bool, prefix: &str) {
    let up = format!("{prefix}_UP");
    let dn = format!("{prefix}_DN");
    if use_v6 {
        while ctx
            .ip6t(&["-D", "INPUT", "-i", XR_IF, "-j", prefix])
            .success()
        {}
        while ctx
            .ip6t(&["-D", "OUTPUT", "-o", XR_IF, "-j", prefix])
            .success()
        {}
        for c in [prefix, &up, &dn] {
            let _ = ctx.ip6t(&["-F", c]);
        }
        for c in [prefix, &up, &dn] {
            let _ = ctx.ip6t(&["-X", c]);
        }
    } else {
        while ctx
            .ipt(&["-D", "INPUT", "-i", XR_IF, "-j", prefix])
            .success()
        {}
        while ctx
            .ipt(&["-D", "OUTPUT", "-o", XR_IF, "-j", prefix])
            .success()
        {}
        for c in [prefix, &up, &dn] {
            let _ = ctx.ipt(&["-F", c]);
        }
        for c in [prefix, &up, &dn] {
            let _ = ctx.ipt(&["-X", c]);
        }
    }
}

pub fn acct_teardown(ctx: &NetCtx<'_>) {
    let _lock = acct_lock(ctx);
    if acct_present(ctx) {
        acct_fold(ctx);
    }
    acct_drop(ctx, false, "XR_ACCT");
    acct_drop(ctx, true, "XR_ACCT6");
    let _ = fs::remove_file(ctx.env.run_dir().join("acct.sig"));
}

pub fn acct_snapshot(ctx: &NetCtx<'_>) {
    if !acct_present(ctx) {
        return;
    }
    let _lock = acct_lock(ctx);
    let total = acct_total(ctx);
    let snap_file = ctx.env.acct_snap_file();
    if total != read_quad(&snap_file) {
        write_quad(&snap_file, total);
    }
}

pub fn acct_reset(ctx: &NetCtx<'_>) {
    let _lock = acct_lock(ctx);
    if acct_present(ctx) {
        let _ = ctx.ipt(&["-Z", "XR_ACCT_UP"]);
        let _ = ctx.ipt(&["-Z", "XR_ACCT_DN"]);
    }
    if acct_present6(ctx) {
        let _ = ctx.ip6t(&["-Z", "XR_ACCT6_UP"]);
        let _ = ctx.ip6t(&["-Z", "XR_ACCT6_DN"]);
    }
    write_quad(&ctx.env.acct_base_file(), [0; 4]);
    write_quad(&ctx.env.acct_snap_file(), [0; 4]);
    let _ = fs::write(ctx.env.acct_since_file(), format!("{}\n", unix_now()));
}

pub fn acct_summary(ctx: &NetCtx<'_>) -> TrafficSummary {
    let [wu, wd, lu, ld] = acct_total(ctx);
    let since = fs::read_to_string(ctx.env.acct_since_file())
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    TrafficSummary {
        wan: TrafficCounters { up: wu, down: wd },
        lan: TrafficCounters { up: lu, down: ld },
        total: TrafficCounters {
            up: wu.saturating_add(lu),
            down: wd.saturating_add(ld),
        },
        since,
        counting: acct_present(ctx),
        ipv6: acct_present6(ctx),
    }
}

pub fn acct_json(ctx: &NetCtx<'_>) -> String {
    serde_json::to_string(&acct_summary(ctx)).unwrap_or_else(|_| "{}".to_string())
}
