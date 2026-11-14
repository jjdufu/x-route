use std::collections::BTreeSet;
use std::fs;

use crate::env::XR_IF;
use crate::log::unix_now;
use crate::net::common::{
    NetCtx, TrafficCounters, TrafficSummary, combine_quad_add, combine_quad_max, read_quad,
    write_quad,
};
use crate::sys::DirLock;
use crate::sys::fsx;

fn acct_lock<'env>(ctx: &NetCtx<'env>) -> DirLock<'env> {
    DirLock::acquire(ctx.env, ctx.env.run_dir().join("acct.lock"), 30, 100, false)
}

/// `run/routes.active` 中按地址族筛出的非空行（`want_v6` 为真时取含 `:` 的行）。
fn active_routes(ctx: &NetCtx<'_>, want_v6: bool) -> Vec<String> {
    let Ok(active) = fs::read_to_string(ctx.env.run_dir().join("routes.active")) else {
        return Vec::new();
    };
    active
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && line.contains(':') == want_v6)
        .map(str::to_owned)
        .collect()
}

pub fn acct_lan4(ctx: &NetCtx<'_>) -> Vec<String> {
    let mut set: BTreeSet<String> = [
        "10.0.0.0/8",
        "172.16.0.0/12",
        "192.168.0.0/16",
        "100.64.0.0/10",
        "169.254.0.0/16",
        "127.0.0.0/8",
        "224.0.0.0/4",
        "255.255.255.255/32",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let pfx = ctx.cfg.net_prefix();
    set.extend((!pfx.is_empty()).then(|| format!("{pfx}.0/24")));
    set.extend(active_routes(ctx, false));
    set.into_iter().collect()
}

pub fn acct_lan6(ctx: &NetCtx<'_>) -> Vec<String> {
    let mut set: BTreeSet<String> = ["fc00::/7", "fe80::/10", "ff00::/8", "::1/128"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    set.extend(active_routes(ctx, true));
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
    write_quad(ctx.env, &ctx.env.acct_base_file(), total);
    write_quad(ctx.env, &ctx.env.acct_snap_file(), total);
}

fn acct_rules(ctx: &NetCtx<'_>, use_v6: bool, prefix: &str, lan_list: &[String]) {
    let up = format!("{prefix}_UP");
    let dn = format!("{prefix}_DN");
    if use_v6 {
        ctx.ip6t_run(&["-F", &up]);
        ctx.ip6t_run(&["-F", &dn]);
        for c in lan_list {
            ctx.ip6t_run(&["-A", &up, "-d", c, "-j", "RETURN"]);
            ctx.ip6t_run(&["-A", &dn, "-s", c, "-j", "RETURN"]);
        }
        ctx.ip6t_run(&["-A", &up, "-j", "RETURN"]);
        ctx.ip6t_run(&["-A", &dn, "-j", "RETURN"]);
    } else {
        ctx.ipt_run(&["-F", &up]);
        ctx.ipt_run(&["-F", &dn]);
        for c in lan_list {
            ctx.ipt_run(&["-A", &up, "-d", c, "-j", "RETURN"]);
            ctx.ipt_run(&["-A", &dn, "-s", c, "-j", "RETURN"]);
        }
        ctx.ipt_run(&["-A", &up, "-j", "RETURN"]);
        ctx.ipt_run(&["-A", &dn, "-j", "RETURN"]);
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
        ctx.ip6t_run(&["-N", &dn]);
        ctx.ip6t_run(&["-N", prefix]);
        ctx.ip6t_run(&["-F", prefix]);
        ctx.ip6t_run(&["-A", prefix, "-i", XR_IF, "-j", &up]);
        ctx.ip6t_run(&["-A", prefix, "-o", XR_IF, "-j", &dn]);
    } else {
        ctx.ipt_run(&["-N", &dn]);
        ctx.ipt_run(&["-N", prefix]);
        ctx.ipt_run(&["-F", prefix]);
        ctx.ipt_run(&["-A", prefix, "-i", XR_IF, "-j", &up]);
        ctx.ipt_run(&["-A", prefix, "-o", XR_IF, "-j", &dn]);
    }
    true
}

fn acct_hooks_one(ctx: &NetCtx<'_>, use_v6: bool, prefix: &str) {
    if use_v6 {
        if !ctx
            .ip6t(&["-C", "INPUT", "-i", XR_IF, "-j", prefix])
            .success()
        {
            ctx.ip6t_run(&["-I", "INPUT", "1", "-i", XR_IF, "-j", prefix]);
        }
        if !ctx
            .ip6t(&["-C", "OUTPUT", "-o", XR_IF, "-j", prefix])
            .success()
        {
            ctx.ip6t_run(&["-I", "OUTPUT", "1", "-o", XR_IF, "-j", prefix]);
        }
    } else {
        if !ctx
            .ipt(&["-C", "INPUT", "-i", XR_IF, "-j", prefix])
            .success()
        {
            ctx.ipt_run(&["-I", "INPUT", "1", "-i", XR_IF, "-j", prefix]);
        }
        if !ctx
            .ipt(&["-C", "OUTPUT", "-o", XR_IF, "-j", prefix])
            .success()
        {
            ctx.ipt_run(&["-I", "OUTPUT", "1", "-o", XR_IF, "-j", prefix]);
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
        write_quad(ctx.env, &ctx.env.acct_base_file(), merged);
        let since = ctx.env.acct_since_file();
        if !since.is_file() {
            fsx::write_logged(ctx.env, &since, format!("{}\n", unix_now()).as_bytes());
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
        fsx::write_logged(ctx.env, &sig_file, format!("{sig}\n").as_bytes());
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
            ctx.ip6t_run(&["-F", c]);
        }
        for c in [prefix, &up, &dn] {
            ctx.ip6t_run(&["-X", c]);
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
            ctx.ipt_run(&["-F", c]);
        }
        for c in [prefix, &up, &dn] {
            ctx.ipt_run(&["-X", c]);
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
    fsx::remove_logged(ctx.env, &ctx.env.run_dir().join("acct.sig"));
}

pub fn acct_snapshot(ctx: &NetCtx<'_>) {
    if !acct_present(ctx) {
        return;
    }
    let _lock = acct_lock(ctx);
    let total = acct_total(ctx);
    let snap_file = ctx.env.acct_snap_file();
    if total != read_quad(&snap_file) {
        write_quad(ctx.env, &snap_file, total);
    }
}

pub fn acct_reset(ctx: &NetCtx<'_>) {
    let _lock = acct_lock(ctx);
    if acct_present(ctx) {
        ctx.ipt_run(&["-Z", "XR_ACCT_UP"]);
        ctx.ipt_run(&["-Z", "XR_ACCT_DN"]);
    }
    if acct_present6(ctx) {
        ctx.ip6t_run(&["-Z", "XR_ACCT6_UP"]);
        ctx.ip6t_run(&["-Z", "XR_ACCT6_DN"]);
    }
    write_quad(ctx.env, &ctx.env.acct_base_file(), [0; 4]);
    write_quad(ctx.env, &ctx.env.acct_snap_file(), [0; 4]);
    fsx::write_logged(
        ctx.env,
        &ctx.env.acct_since_file(),
        format!("{}\n", unix_now()).as_bytes(),
    );
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
    serde_json::to_string(&acct_summary(ctx)).unwrap_or_else(|_| "{}".to_owned())
}
