pub mod common;
pub mod hotspot;
pub mod ipv6;
pub mod vpn_routes;

use std::collections::BTreeSet;
use std::fs;
use std::iter;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use serde::Serialize;

use crate::config::Config;
use crate::env::{Env, HS_BR, HS_TAP, XR_IF, XR_NET6};
use crate::log::{dlog, log_err, log_info, unix_now};
use crate::net::common::{NetCtx, trigger_diag_snap_bg};
use crate::net::hotspot::{hs_fwd6_rules, hs_sync, hs_tap_down, hs_tap_up, vm_alive, vm_is_ready};
use crate::net::ipv6::{
    v6_nft_setup, v6_nft_sync, v6_nft_teardown, v6_proxy_remove, v6_proxy_sync, v6_src_fingerprint,
    vm_v6,
};
use crate::net::vpn_routes::{
    routes_clear, routes_file_cksum, routes_reassert, routes_sync, tether_ifaces,
};
use crate::out::emit;
use crate::sys::exec::note_exec_result;
use crate::sys::fsx;
use crate::sys::spawn::spawn_self_detached;
use crate::sys::{LockWait, PathLock, send_signal};
use crate::traffic::{acct_hooks, acct_present, acct_present6, acct_setup, acct_teardown};

const GW_PREF: &str = "19100";
const GW_TABLE: &str = "233";
const REJ6_PREF: &str = "19060";
const REJ6_TABLE: &str = "235";

fn is_cell_iface(name: &str) -> bool {
    name.starts_with("rmnet")
        || name.starts_with("ccmni")
        || name.starts_with("seth")
        || name.starts_with("v4-rmnet")
        || name.starts_with("v4-ccmni")
}

fn net_lock(env: &Env) -> PathLock {
    PathLock::acquire(
        env,
        env.run_dir().join(".net.lock"),
        LockWait {
            max_iters: 120,
            sleep_ms: 500,
            check_pid: true,
        },
    )
}

#[must_use]
pub fn upstream_iface(ctx: &NetCtx<'_>) -> String {
    let out = ctx.ip(&["rule"]);
    for line in out.stdout.lines() {
        if let Some((_, rhs)) = line.split_once("fwmark 0x0/0xffff iif lo lookup ")
            && let Some(iface) = rhs.split_whitespace().next()
        {
            return iface.to_owned();
        }
    }
    String::new()
}

#[must_use]
pub fn upstream_out(ctx: &NetCtx<'_>, up: &str) -> Vec<String> {
    if up.is_empty() {
        return Vec::new();
    }
    let mut set = BTreeSet::new();
    set.extend(iter::once(up.to_owned()));
    let out = ctx.ip(&["-4", "route", "show", "table", up]);
    for line in out.stdout.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.first().copied() == Some("default") {
            for (idx, tok) in tokens.iter().enumerate() {
                if *tok == "dev"
                    && let Some(dev) = tokens.get(idx.saturating_add(1))
                {
                    set.extend(iter::once((*dev).to_owned()));
                }
            }
        }
    }
    set.into_iter().collect()
}

#[must_use]
pub fn upstream_out6(ctx: &NetCtx<'_>, up: &str) -> Vec<String> {
    if up.is_empty() {
        return Vec::new();
    }
    let mut set = BTreeSet::new();
    if !up.starts_with("v4-") {
        set.extend(iter::once(up.to_owned()));
    }
    let out = ctx.ip6(&["route", "show", "table", up]);
    for line in out.stdout.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.first().copied() == Some("default") {
            for (idx, tok) in tokens.iter().enumerate() {
                if *tok == "dev"
                    && let Some(dev) = tokens.get(idx.saturating_add(1))
                    && !dev.starts_with("v4-")
                {
                    set.extend(iter::once((*dev).to_owned()));
                }
            }
        }
    }
    set.into_iter().collect()
}

#[must_use]
pub fn expose_ifaces(ctx: &NetCtx<'_>, up: &str) -> Vec<String> {
    let mut set = BTreeSet::new();
    match ctx.cfg.expose() {
        "lan" => {
            for i in tether_ifaces(ctx) {
                set.extend(iter::once(i));
            }
        }
        "all" => {
            for i in tether_ifaces(ctx) {
                set.extend(iter::once(i));
            }
            if !up.is_empty() {
                set.extend(iter::once(up.to_owned()));
            }
        }
        _ => {}
    }
    set.into_iter().filter(|i| !is_cell_iface(i)).collect()
}

#[must_use]
pub fn upstream_mtu(ctx: &NetCtx<'_>, up: &str, outs: &[String]) -> u32 {
    let mut m: u32 = 1500;
    for o in outs {
        if let Ok(raw) = fs::read_to_string(ctx.env.sysnet.join(o).join("mtu"))
            && let Ok(v) = raw.trim().parse::<u32>()
            && v < m
        {
            m = v;
        }
    }
    if !up.is_empty() {
        for out in [
            ctx.ip(&["-4", "route", "show", "table", up]),
            ctx.ip6(&["route", "show", "table", up]),
        ] {
            for line in out.stdout.lines() {
                let tokens: Vec<&str> = line.split_whitespace().collect();
                if tokens.first().copied() == Some("default") {
                    for (idx, tok) in tokens.iter().enumerate() {
                        if *tok == "mtu"
                            && let Some(v) = tokens
                                .get(idx.saturating_add(1))
                                .and_then(|s| s.parse::<u32>().ok())
                            && v < m
                        {
                            m = v;
                        }
                    }
                }
            }
        }
    }
    m.max(1280)
}

pub fn mss_rules(ctx: &NetCtx<'_>, outs: &[String]) {
    let mut targets = vec![XR_IF.to_owned()];
    targets.extend_from_slice(outs);
    for use_v6 in [false, true] {
        let run_t = |args: &[&str]| {
            if use_v6 {
                ctx.ip6t(args)
            } else {
                ctx.ipt(args)
            }
        };
        note_exec_result(&run_t(&["-t", "mangle", "-N", "XR_MSS"]));
        if !run_t(&["-t", "mangle", "-F", "XR_MSS"]).success() {
            continue;
        }
        for o in &targets {
            note_exec_result(&run_t(&[
                "-t",
                "mangle",
                "-A",
                "XR_MSS",
                "-o",
                o,
                "-p",
                "tcp",
                "--tcp-flags",
                "SYN,RST",
                "SYN",
                "-j",
                "TCPMSS",
                "--clamp-mss-to-pmtu",
            ]));
        }
        if !run_t(&["-t", "mangle", "-C", "POSTROUTING", "-j", "XR_MSS"]).success() {
            note_exec_result(&run_t(&[
                "-t",
                "mangle",
                "-I",
                "POSTROUTING",
                "1",
                "-j",
                "XR_MSS",
            ]));
        }
    }
}

pub fn mss_clear(ctx: &NetCtx<'_>) {
    for use_v6 in [false, true] {
        let run_t = |args: &[&str]| {
            if use_v6 {
                ctx.ip6t(args)
            } else {
                ctx.ipt(args)
            }
        };
        while run_t(&["-t", "mangle", "-D", "POSTROUTING", "-j", "XR_MSS"]).success() {}
        note_exec_result(&run_t(&["-t", "mangle", "-F", "XR_MSS"]));
        note_exec_result(&run_t(&["-t", "mangle", "-X", "XR_MSS"]));
    }
}

pub fn mtu_sync(ctx: &NetCtx<'_>, up: &str, outs: &[String]) {
    let mt = upstream_mtu(ctx, up, outs);
    let mt_s = mt.to_string();
    let cur = fs::read_to_string(ctx.env.sysnet.join(XR_IF).join("mtu"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    if cur != mt_s && ctx.ip(&["link", "set", XR_IF, "mtu", &mt_s]).success() {
        let cur_disp = if cur.is_empty() { "?" } else { &cur };
        let up_disp = if up.is_empty() { "无" } else { up };
        log_info(
            ctx.env,
            "net",
            &format!("xr0 MTU {cur_disp} -> {mt_s}（跟随上游 {up_disp}）"),
        );
    }
    fsx::write_logged(
        ctx.env,
        ctx.env.run_dir().join("net.mtu"),
        format!("{mt_s}\n"),
    );
    let host_dir = ctx.env.run_dir().join("guest/host");
    fsx::mkdirs_logged(ctx.env, &host_dir);
    let guest_mtu = host_dir.join("mtu");
    if fs::read_to_string(&guest_mtu).map_or(true, |s| s.trim() != mt_s) {
        fsx::write_logged(ctx.env, guest_mtu, format!("{mt_s}\n"));
    }
    mss_rules(ctx, outs);
}

pub fn time_push(env: &Env) {
    let host_dir = env.run_dir().join("guest/host");
    if !host_dir.is_dir() {
        return;
    }
    let now = unix_now();
    let time_file = host_dir.join("time");
    let tp = fs::read_to_string(&time_file)
        .ok()
        .and_then(|s| {
            let t = s.trim();
            if t.len() <= 10 && t.bytes().all(|b| b.is_ascii_digit()) {
                t.parse::<u64>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    if now >= tp && now.saturating_sub(tp) < 1800 {
        return;
    }
    fsx::write_logged(env, time_file, format!("{now}\n"));
}

fn fwd_first(ctx: &NetCtx<'_>) {
    let out = ctx.ipt(&["-S", "FORWARD"]);
    if out.stdout.lines().nth(1) == Some("-A FORWARD -j XR_FWD") {
        return;
    }
    while ctx.ipt(&["-D", "FORWARD", "-j", "XR_FWD"]).success() {}
    ctx.ipt_run(&["-I", "FORWARD", "1", "-j", "XR_FWD"]);
}

fn nat_first(ctx: &NetCtx<'_>) {
    if !ctx
        .ipt(&["-t", "nat", "-C", "POSTROUTING", "-j", "XR_NAT"])
        .success()
    {
        ctx.ipt_run(&["-t", "nat", "-I", "POSTROUTING", "1", "-j", "XR_NAT"]);
    }
}

fn fwd6_first(ctx: &NetCtx<'_>) {
    let out = ctx.ip6t(&["-S", "FORWARD"]);
    if out.stdout.lines().nth(1) == Some("-A FORWARD -j XR_FWD6") {
        return;
    }
    while ctx.ip6t(&["-D", "FORWARD", "-j", "XR_FWD6"]).success() {}
    ctx.ip6t_run(&["-I", "FORWARD", "1", "-j", "XR_FWD6"]);
}

#[must_use]
pub fn ip6_nat_ok(env: &Env) -> bool {
    let cache = env.run_dir().join("net.ip6nat");
    if let Ok(raw) = fs::read_to_string(&cache) {
        let t = raw.trim();
        if !t.is_empty() {
            return t == "1";
        }
    }
    let ok = Command::new(env.sysbin_cmd("ip6tables"))
        .args(["-w", "-t", "nat", "-S", "POSTROUTING"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    fsx::write_logged(env, cache, if ok { "1\n" } else { "0\n" });
    ok
}

fn nat6_first(ctx: &NetCtx<'_>) {
    if !ip6_nat_ok(ctx.env) {
        return;
    }
    while ctx
        .ip6t(&["-t", "nat", "-D", "POSTROUTING", "-j", "XR_NAT6"])
        .success()
    {}
}

fn want_gateway(ctx: &NetCtx<'_>) -> bool {
    ctx.cfg.mode() == "global" && vm_is_ready(ctx.env)
}

fn gw_want(ctx: &NetCtx<'_>) -> &'static str {
    if !want_gateway(ctx) {
        "off"
    } else if vm_v6(ctx.env) && ctx.env.run_dir().join("net.v6proxy").is_file() {
        "v6"
    } else {
        "block"
    }
}

fn gw_reason(ctx: &NetCtx<'_>) -> String {
    let vm_st = if vm_alive(ctx.env) {
        fs::read_to_string(ctx.env.run_dir().join("state")).unwrap_or_else(|_| "booting".to_owned())
    } else {
        "未运行".to_owned()
    };
    let g_v6 = fs::read_to_string(ctx.env.run_dir().join("guest/guest/ipv6"))
        .unwrap_or_else(|_| "未上报".to_owned());
    let nd = if ctx.env.run_dir().join("net.v6proxy").is_file() {
        "已安装"
    } else {
        "未安装"
    };
    format!(
        "模式={} 虚拟机={} 客侧IPv6={} 邻居代理={nd}",
        ctx.cfg.mode(),
        vm_st.trim(),
        g_v6.trim()
    )
}

fn copy_dev_routes(ctx: &NetCtx<'_>, dev: &str) {
    if dev.is_empty() || dev == XR_IF {
        return;
    }
    let out = ctx.ip(&["-4", "route", "show", "dev", dev]);
    for line in out.stdout.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.is_empty() || tokens.first().copied() == Some("default") {
            continue;
        }
        let mut args: Vec<&str> = Vec::with_capacity(tokens.len().saturating_add(5));
        args.extend(["route", "replace"]);
        args.extend(tokens);
        args.extend(["dev", dev, "table", GW_TABLE]);
        ctx.ip_run(&args);
    }
}

fn copy_dev_routes6(ctx: &NetCtx<'_>, dev: &str) {
    if dev.is_empty() || dev == XR_IF {
        return;
    }
    for tb in ["main", "local_network"] {
        let out = ctx.ip6(&["route", "show", "table", tb, "dev", dev]);
        for line in out.stdout.lines() {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            let Some(&first) = tokens.first() else {
                continue;
            };
            if matches!(first, "default" | "unreachable" | "multicast" | "ff00::/8") {
                continue;
            }
            let mut args: Vec<&str> = Vec::with_capacity(tokens.len().saturating_add(5));
            args.extend(["route", "replace"]);
            let mut skip_next = false;
            for tok in tokens {
                if skip_next {
                    skip_next = false;
                    continue;
                }
                if tok == "expires" {
                    skip_next = true;
                    continue;
                }
                args.push(tok);
            }
            args.extend(["dev", dev, "table", GW_TABLE]);
            ctx.ip6_run(&args);
        }
    }
}

fn gw_install(ctx: &NetCtx<'_>, g: &str, outs: &[String], outs6: &[String]) {
    ctx.ip_run(&["route", "flush", "table", GW_TABLE]);
    let tethers = tether_ifaces(ctx);
    for d in outs.iter().chain(tethers.iter()) {
        copy_dev_routes(ctx, d);
    }
    let via4 = format!("{}.254", ctx.cfg.net_prefix());
    let src4 = format!("{}.1", ctx.cfg.net_prefix());
    ctx.ip_run(&[
        "route", "replace", "default", "via", &via4, "dev", XR_IF, "src", &src4, "table", GW_TABLE,
    ]);
    while ctx.ip(&["rule", "del", "pref", GW_PREF]).success() {}
    ctx.ip_run(&["rule", "add", "pref", GW_PREF, "lookup", GW_TABLE]);

    ctx.ip6_run(&["route", "flush", "table", GW_TABLE]);
    for d in outs6.iter().chain(tethers.iter()) {
        copy_dev_routes6(ctx, d);
    }
    if g == "v6" {
        let via6 = format!("{XR_NET6}::254");
        let src6 = format!("{XR_NET6}::1");
        ctx.ip6_run(&[
            "route", "replace", "default", "via", &via6, "dev", XR_IF, "src", &src6, "table",
            GW_TABLE,
        ]);
    } else {
        ctx.ip6_run(&[
            "route",
            "replace",
            "unreachable",
            "default",
            "table",
            GW_TABLE,
        ]);
    }
    while ctx.ip6(&["rule", "del", "pref", GW_PREF]).success() {}
    ctx.ip6_run(&["rule", "add", "pref", GW_PREF, "lookup", GW_TABLE]);

    while ctx.ip6(&["rule", "del", "pref", REJ6_PREF]).success() {}
    ctx.ip6_run(&["route", "flush", "table", REJ6_TABLE]);
    if g == "block" {
        ctx.ip6_run(&[
            "route", "replace", "default", "dev", XR_IF, "table", REJ6_TABLE,
        ]);
        for i in &tethers {
            if i == HS_BR {
                continue;
            }
            ctx.ip6_run(&[
                "rule", "add", "pref", REJ6_PREF, "iif", i, "lookup", REJ6_TABLE,
            ]);
        }
    }
    fsx::write_logged(ctx.env, ctx.env.run_dir().join("net.gw"), format!("{g}\n"));
}

fn gw_remove(ctx: &NetCtx<'_>) {
    while ctx.ip(&["rule", "del", "pref", GW_PREF]).success() {}
    ctx.ip_run(&["route", "flush", "table", GW_TABLE]);
    while ctx.ip6(&["rule", "del", "pref", GW_PREF]).success() {}
    ctx.ip6_run(&["route", "flush", "table", GW_TABLE]);
    while ctx.ip6(&["rule", "del", "pref", REJ6_PREF]).success() {}
    ctx.ip6_run(&["route", "flush", "table", REJ6_TABLE]);
    fsx::remove_logged(ctx.env, ctx.env.run_dir().join("net.gw"));
}

fn rules_set(ctx: &NetCtx<'_>, use_v6: bool, up: &str) {
    let run_ip = |args: &[&str]| {
        if use_v6 { ctx.ip6(args) } else { ctx.ip(args) }
    };
    while run_ip(&["rule", "del", "pref", "18800"]).success() {}
    while run_ip(&["rule", "del", "pref", "18900"]).success() {}
    if up.is_empty() {
        return;
    }
    let tbl = ctx.lnt();
    note_exec_result(&run_ip(&[
        "rule", "add", "pref", "18800", "iif", XR_IF, "lookup", tbl,
    ]));
    note_exec_result(&run_ip(&[
        "rule", "add", "pref", "18900", "iif", XR_IF, "lookup", up,
    ]));
}

fn sync_v6(ctx: &NetCtx<'_>, gw: &str, outs6: &[String], ex: &[String]) {
    let run = ctx.env.run_dir();
    let outs6_str = if outs6.is_empty() {
        String::new()
    } else {
        format!("{} \n", outs6.join(" "))
    };
    fsx::write_logged(ctx.env, run.join("net.up_out6"), outs6_str);
    fsx::write_logged(
        ctx.env,
        run.join("net.v6"),
        if vm_v6(ctx.env) { "1\n" } else { "" },
    );

    ctx.ip6t_run(&["-F", "XR_FWD6"]);
    if acct_present6(ctx) {
        ctx.ip6t_run(&["-A", "XR_FWD6", "-j", "XR_ACCT6"]);
    }
    hs_fwd6_rules(ctx);
    ctx.ip6t_run(&[
        "-A",
        "XR_FWD6",
        "-o",
        XR_IF,
        "-m",
        "conntrack",
        "--ctstate",
        "RELATED,ESTABLISHED",
        "-j",
        "ACCEPT",
    ]);
    ctx.ip6t_run(&[
        "-A",
        "XR_FWD6",
        "-i",
        XR_IF,
        "-m",
        "conntrack",
        "--ctstate",
        "RELATED,ESTABLISHED",
        "-j",
        "ACCEPT",
    ]);
    if vm_v6(ctx.env) {
        for o in outs6 {
            ctx.ip6t_run(&["-A", "XR_FWD6", "-i", XR_IF, "-o", o, "-j", "ACCEPT"]);
        }
    }
    let net6 = format!("{XR_NET6}::/64");
    let active_v6: Vec<String> = fs::read_to_string(run.join("routes.active"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains(':'))
        .map(str::to_owned)
        .collect();
    for i in ex {
        ctx.ip6t_run(&[
            "-A", "XR_FWD6", "-i", i, "-o", XR_IF, "-d", &net6, "-j", "ACCEPT",
        ]);
        for c in &active_v6 {
            ctx.ip6t_run(&[
                "-A", "XR_FWD6", "-i", i, "-o", XR_IF, "-d", c, "-j", "ACCEPT",
            ]);
        }
    }
    let ex_set: BTreeSet<&str> = ex.iter().map(String::as_str).collect();
    for o in outs6 {
        if !ex_set.contains(o.as_str()) {
            ctx.ip6t_run(&["-A", "XR_FWD6", "-i", o, "-o", XR_IF, "-j", "DROP"]);
        }
    }
    if gw != "off" {
        for i in tether_ifaces(ctx) {
            if gw == "v6" {
                ctx.ip6t_run(&["-A", "XR_FWD6", "-i", &i, "-o", XR_IF, "-j", "ACCEPT"]);
                ctx.ip6t_run(&["-A", "XR_FWD6", "-i", XR_IF, "-o", &i, "-j", "ACCEPT"]);
            } else {
                ctx.ip6t_run(&[
                    "-A",
                    "XR_FWD6",
                    "-i",
                    &i,
                    "-o",
                    XR_IF,
                    "-p",
                    "tcp",
                    "-j",
                    "REJECT",
                    "--reject-with",
                    "tcp-reset",
                ]);
                ctx.ip6t_run(&[
                    "-A",
                    "XR_FWD6",
                    "-i",
                    &i,
                    "-o",
                    XR_IF,
                    "-j",
                    "REJECT",
                    "--reject-with",
                    "icmp6-adm-prohibited",
                ]);
            }
            for o in outs6 {
                ctx.ip6t_run(&[
                    "-A",
                    "XR_FWD6",
                    "-i",
                    &i,
                    "-o",
                    o,
                    "-p",
                    "tcp",
                    "-j",
                    "REJECT",
                    "--reject-with",
                    "tcp-reset",
                ]);
                ctx.ip6t_run(&[
                    "-A",
                    "XR_FWD6",
                    "-i",
                    &i,
                    "-o",
                    o,
                    "-j",
                    "REJECT",
                    "--reject-with",
                    "icmp6-adm-prohibited",
                ]);
            }
        }
    }
    ctx.ip6t_run(&["-A", "XR_FWD6", "-o", XR_IF, "-j", "DROP"]);
    ctx.ip6t_run(&["-A", "XR_FWD6", "-i", XR_IF, "-j", "DROP"]);
    fwd6_first(ctx);
    v6_nft_sync(ctx, gw, outs6, ex);
}

fn sync_all_unlocked(env: &Env) {
    let cfg = Config::load(env);
    let ctx = NetCtx::new(env, &cfg);
    let run = env.run_dir();

    let up = upstream_iface(&ctx);
    let outs = upstream_out(&ctx, &up);
    let outs6 = upstream_out6(&ctx, &up);

    let up_if_file = run.join("net.up_if");
    let up_out_file = run.join("net.up_out");
    let old_up = fs::read_to_string(&up_if_file).unwrap_or_default();
    let old_up_trimmed = old_up.trim();
    let old_outs_raw = fs::read_to_string(&up_out_file).unwrap_or_default();
    let outs_joined = if outs.is_empty() {
        String::new()
    } else {
        format!("{} ", outs.join(" "))
    };

    if up != old_up_trimmed
        || outs_joined != old_outs_raw.trim_end_matches('\n')
        || !up_if_file.is_file()
    {
        for o in old_outs_raw.split_whitespace() {
            ctx.ndc_run(&["ipfwd", "remove", XR_IF, o]);
        }
        if !up.is_empty() {
            for o in &outs {
                ctx.ndc_run(&["ipfwd", "add", XR_IF, o]);
            }
        }
        rules_set(&ctx, false, &up);
        rules_set(&ctx, true, &up);
        fsx::write_logged(env, &up_if_file, format!("{up}\n"));
        fsx::write_logged(env, &up_out_file, format!("{outs_joined}\n"));
        let old_disp = if old_up_trimmed.is_empty() {
            "无"
        } else {
            old_up_trimmed
        };
        let new_disp = if up.is_empty() { "无" } else { &up };
        let outs_disp = if outs.is_empty() {
            "无".to_owned()
        } else {
            outs.join(" ")
        };
        let outs6_disp = if outs6.is_empty() {
            "无".to_owned()
        } else {
            outs6.join(" ")
        };
        log_info(
            env,
            "net",
            &format!("上游 {old_disp} -> {new_disp}（出口 {outs_disp}）"),
        );
        dlog(
            env,
            &cfg,
            "net",
            &format!(
                "上游变化：{old_disp}({old_outs_raw}) -> {new_disp}({outs_disp})，IPv6 出口 {outs6_disp}"
            ),
        );
        if !old_up_trimmed.is_empty() {
            trigger_diag_snap_bg(
                env,
                &cfg,
                &format!("上游变化 {old_up_trimmed} -> {new_disp}"),
            );
        }
    }

    let net4 = format!("{}.0/24", cfg.net_prefix());
    ctx.ipt_run(&["-t", "nat", "-F", "XR_NAT"]);
    for o in &outs {
        ctx.ipt_run(&[
            "-t",
            "nat",
            "-A",
            "XR_NAT",
            "-s",
            &net4,
            "-o",
            o,
            "-j",
            "MASQUERADE",
        ]);
    }
    mtu_sync(&ctx, &up, &outs);

    let ex = expose_ifaces(&ctx, &up);
    let ex_str = if ex.is_empty() {
        String::new()
    } else {
        format!("{} ", ex.join(" "))
    };
    fsx::write_logged(env, run.join("net.expose_if"), format!("{ex_str}\n"));

    routes_sync(&ctx);
    acct_setup(&ctx, run.join("net.fwd6").is_file());

    ctx.ipt_run(&["-F", "XR_FWD"]);
    if acct_present(&ctx) {
        ctx.ipt_run(&["-A", "XR_FWD", "-j", "XR_ACCT"]);
    }
    ctx.ipt_run(&[
        "-A",
        "XR_FWD",
        "-o",
        XR_IF,
        "-m",
        "conntrack",
        "--ctstate",
        "RELATED,ESTABLISHED",
        "-j",
        "ACCEPT",
    ]);
    ctx.ipt_run(&[
        "-A",
        "XR_FWD",
        "-i",
        XR_IF,
        "-m",
        "conntrack",
        "--ctstate",
        "RELATED,ESTABLISHED",
        "-j",
        "ACCEPT",
    ]);
    for o in &outs {
        ctx.ipt_run(&["-A", "XR_FWD", "-i", XR_IF, "-o", o, "-j", "ACCEPT"]);
    }
    let active_v4: Vec<String> = fs::read_to_string(run.join("routes.active"))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.contains(':'))
        .map(str::to_owned)
        .collect();
    for i in &ex {
        ctx.ipt_run(&[
            "-A", "XR_FWD", "-i", i, "-o", XR_IF, "-d", &net4, "-j", "ACCEPT",
        ]);
        for c in &active_v4 {
            ctx.ipt_run(&[
                "-A", "XR_FWD", "-i", i, "-o", XR_IF, "-d", c, "-j", "ACCEPT",
            ]);
        }
    }
    let ex_set: BTreeSet<&str> = ex.iter().map(String::as_str).collect();
    for o in &outs {
        if !ex_set.contains(o.as_str()) {
            ctx.ipt_run(&["-A", "XR_FWD", "-i", o, "-o", XR_IF, "-j", "DROP"]);
        }
    }
    if want_gateway(&ctx) {
        for i in tether_ifaces(&ctx) {
            ctx.ipt_run(&["-A", "XR_FWD", "-i", &i, "-o", XR_IF, "-j", "ACCEPT"]);
            ctx.ipt_run(&["-A", "XR_FWD", "-i", XR_IF, "-o", &i, "-j", "ACCEPT"]);
            for o in &outs {
                ctx.ipt_run(&["-A", "XR_FWD", "-i", &i, "-o", o, "-j", "DROP"]);
            }
        }
    }
    ctx.ipt_run(&["-A", "XR_FWD", "-o", XR_IF, "-j", "DROP"]);
    ctx.ipt_run(&["-A", "XR_FWD", "-i", XR_IF, "-j", "DROP"]);
    fwd_first(&ctx);

    hs_sync(&ctx);
    v6_proxy_sync(&ctx, &outs6);
    let gw = gw_want(&ctx);
    dlog(
        env,
        &cfg,
        "net",
        &format!(
            "同步：上游={} 出口={} IPv6出口={} 暴露={} 热点={} 接管={gw}（{}）",
            if up.is_empty() { "无" } else { &up },
            if outs.is_empty() {
                "无".to_owned()
            } else {
                outs.join(" ")
            },
            if outs6.is_empty() {
                "无".to_owned()
            } else {
                outs6.join(" ")
            },
            if ex.is_empty() {
                "无".to_owned()
            } else {
                ex.join(" ")
            },
            tether_ifaces(&ctx).join(" "),
            gw_reason(&ctx)
        ),
    );
    if gw == "block" {
        let v6_state = if vm_v6(env) {
            "已开但邻居代理不可用"
        } else {
            "已关闭"
        };
        dlog(
            env,
            &cfg,
            "net",
            &format!(
                "IPv6 拦截：虚拟机 IPv6 {v6_state}，手机与热点的 IPv6 出网被拒绝，应用回落 IPv4"
            ),
        );
    }
    sync_v6(&ctx, gw, &outs6, &ex);
    if gw == "off" {
        gw_remove(&ctx);
    } else {
        gw_install(&ctx, gw, &outs, &outs6);
    }
}

pub fn net_sync(env: &Env) {
    if !env.sysnet.join(XR_IF).is_dir() {
        return;
    }
    time_push(env);
    let _lock = net_lock(env);
    sync_all_unlocked(env);
}

#[must_use]
pub fn net_up(env: &Env) -> i32 {
    if env.ensure_dirs().is_err() {
        return 1;
    }
    let cfg = Config::load(env);
    let ctx = NetCtx::new(env, &cfg);
    let run = env.run_dir();

    if !ctx.ip(&["link", "show", XR_IF]).success()
        && !ctx.ip(&["tuntap", "add", XR_IF, "mode", "tap"]).success()
    {
        log_err(env, "net", &format!("创建 {XR_IF} 失败"));
        return 1;
    }
    ctx.ip_run(&["addr", "flush", "dev", XR_IF]);
    let ip4_cidr = format!("{}.1/24", cfg.net_prefix());
    ctx.ip_run(&["addr", "add", &ip4_cidr, "dev", XR_IF]);
    ctx.sysw_run(
        &env.proc_dir
            .join(format!("sys/net/ipv6/conf/{XR_IF}/disable_ipv6")),
        "0",
    );
    ctx.sysw_run(
        &env.proc_dir
            .join(format!("sys/net/ipv6/conf/{XR_IF}/accept_ra")),
        "0",
    );
    ctx.sysw_run(
        &env.proc_dir
            .join(format!("sys/net/ipv4/conf/{XR_IF}/rp_filter")),
        "0",
    );
    ctx.ip_run(&["link", "set", XR_IF, "up"]);
    let ip6_cidr = format!("{XR_NET6}::1/64");
    ctx.ip6_run(&["addr", "add", &ip6_cidr, "dev", XR_IF, "nodad"]);
    if !hs_tap_up(&ctx) {
        // 创建失败已由 hs_tap_up 写入 module.log；xr0 照常继续，热点接管稍后由 sync 重试
        dlog(
            env,
            &cfg,
            "net",
            &format!("{HS_TAP} 未就绪，继续启动 {XR_IF}"),
        );
    }

    let net4 = format!("{}.0/24", cfg.net_prefix());
    let net6 = format!("{XR_NET6}::/64");
    if ctx.ndc_ok(&["network", "interface", "add", "local", XR_IF])
        && ctx.ndc_ok(&["network", "route", "add", "local", XR_IF, &net4])
    {
        fsx::write_logged(env, run.join("net.mode"), "ndc\n");
        ctx.ndc_run(&["network", "route", "add", "local", XR_IF, &net6]);
    } else {
        fsx::write_logged(env, run.join("net.mode"), "fallback\n");
        ctx.ip_run(&["route", "replace", &net4, "dev", XR_IF, "table", "97"]);
        ctx.ip6_run(&["route", "replace", &net6, "dev", XR_IF, "table", "97"]);
        log_info(env, "net", "ndc 不可用，改用 table 97，并由对账循环补回");
    }

    let tbl = ctx.lnt();
    ctx.ip_run(&["rule", "del", "pref", "4800", "to", &net4]);
    ctx.ip_run(&["rule", "add", "pref", "4800", "to", &net4, "lookup", tbl]);
    ctx.ip6_run(&["rule", "del", "pref", "4800", "to", &net6]);
    ctx.ip6_run(&["rule", "add", "pref", "4800", "to", &net6, "lookup", tbl]);

    if !ctx.ndc_ok(&["ipfwd", "enable", "xrouter"]) {
        let fwd_path = env.proc_dir.join("sys/net/ipv4/ip_forward");
        let orig = fs::read_to_string(&fwd_path).unwrap_or_default();
        fsx::write_logged(env, run.join("ip_forward.orig"), orig);
        ctx.sysw_run(&fwd_path, "1");
    }

    ctx.ipt_run(&["-N", "XR_FWD"]);
    ctx.ipt_run(&["-t", "nat", "-N", "XR_NAT"]);
    fwd_first(&ctx);
    nat_first(&ctx);
    fsx::remove_logged(env, run.join("net.nat6"));

    if ctx.ip6t(&["-N", "XR_FWD6"]).success() || ctx.ip6t(&["-S", "XR_FWD6"]).success() {
        fsx::touch_logged(env, run.join("net.fwd6"));
        fwd6_first(&ctx);
    } else if v6_nft_setup(env) {
        log_info(env, "net", "ip6tables filter 不可用，IPv6 转发改用 nft");
    } else {
        log_err(
            env,
            "net",
            "ip6tables filter 与 nft 均不可用，IPv6 仅靠路由表拦截旁路",
        );
    }
    nat6_first(&ctx);
    fsx::remove_logged(env, run.join("net.up_if"));

    {
        let _lock = net_lock(env);
        sync_all_unlocked(env);
    }

    if let Ok(child) = spawn_self_detached(env, &["internal", "net", "watch"]) {
        fsx::write_logged(env, run.join("net_watch.pid"), format!("{}\n", child.id()));
    }
    let mode_s = fs::read_to_string(run.join("net.mode")).unwrap_or_default();
    log_info(
        env,
        "net",
        &format!("xr0 {ip4_cidr} 已就绪（{}）", mode_s.trim()),
    );
    0
}

#[must_use]
pub fn net_watch(env: &Env) -> i32 {
    while env.sysnet.join(XR_IF).is_dir() {
        thread::sleep(Duration::from_secs(10));
        if !env.sysnet.join(XR_IF).is_dir() {
            break;
        }
        let cfg = Config::load(env);
        let ctx = NetCtx::new(env, &cfg);
        let run = env.run_dir();
        time_push(env);

        let up = upstream_iface(&ctx);
        let outs = upstream_out(&ctx, &up);
        let ex = expose_ifaces(&ctx, &up);
        let ex_str = if ex.is_empty() {
            String::new()
        } else {
            format!("{} ", ex.join(" "))
        };
        let outs_str = if outs.is_empty() {
            String::new()
        } else {
            format!("{} ", outs.join(" "))
        };
        let outs6 = upstream_out6(&ctx, &up);
        let outs6_str = if outs6.is_empty() {
            String::new()
        } else {
            format!("{} ", outs6.join(" "))
        };

        let have_gw = fs::read_to_string(run.join("net.gw")).unwrap_or_else(|_| "off".to_owned());
        let have_gw = have_gw.trim();
        let want_gw = gw_want(&ctx);
        let ck = routes_file_cksum(&ctx);
        let mtu_now = upstream_mtu(&ctx, &up, &outs).to_string();

        let old_up = fs::read_to_string(run.join("net.up_if")).unwrap_or_default();
        let old_outs = fs::read_to_string(run.join("net.up_out")).unwrap_or_default();
        let old_ex = fs::read_to_string(run.join("net.expose_if")).unwrap_or_default();
        let old_outs6 = fs::read_to_string(run.join("net.up_out6")).unwrap_or_default();
        let old_v6 = fs::read_to_string(run.join("net.v6")).unwrap_or_default();
        let old_v6src = fs::read_to_string(run.join("net.v6src")).unwrap_or_default();
        let old_ck = fs::read_to_string(run.join("vpn.cksum")).unwrap_or_default();
        let old_mtu = fs::read_to_string(run.join("net.mtu")).unwrap_or_default();

        let vm_v6_s = if vm_v6(env) { "1" } else { "" };
        let v6src_now = format!("{} ", v6_src_fingerprint(&ctx, &outs6));

        if up != old_up.trim()
            || outs_str.trim() != old_outs.trim()
            || ex_str.trim() != old_ex.trim()
            || want_gw != have_gw
            || outs6_str.trim() != old_outs6.trim()
            || vm_v6_s != old_v6.trim()
            || v6src_now.trim() != old_v6src.trim()
            || ck != old_ck.trim()
            || mtu_now != old_mtu.trim()
        {
            dlog(
                env,
                &cfg,
                "watch",
                &format!(
                    "状态变化触发同步：上游 {}->{up} 接管 {have_gw}->{want_gw} 暴露 [{}]->[{ex_str}] MTU {}->{mtu_now}",
                    old_up.trim(),
                    old_ex.trim(),
                    old_mtu.trim()
                ),
            );
            let _lock = net_lock(env);
            sync_all_unlocked(env);
        }

        fwd_first(&ctx);
        nat_first(&ctx);
        fwd6_first(&ctx);
        nat6_first(&ctx);
        acct_hooks(&ctx);

        if !ctx
            .ipt(&["-t", "mangle", "-C", "POSTROUTING", "-j", "XR_MSS"])
            .success()
        {
            let _lock = net_lock(env);
            mss_rules(&ctx, &outs);
        }
        {
            let _lock = net_lock(env);
            hs_sync(&ctx);
        }

        let net4 = format!("{}.0/24", cfg.net_prefix());
        let net6 = format!("{XR_NET6}::/64");
        let tbl = ctx.lnt();
        if !ctx
            .ip(&["rule"])
            .stdout
            .lines()
            .any(|l| l.starts_with("4800:") && l.contains(&format!("to {net4}")))
        {
            ctx.ip_run(&["rule", "add", "pref", "4800", "to", &net4, "lookup", tbl]);
        }
        if !ctx
            .ip6(&["rule"])
            .stdout
            .lines()
            .any(|l| l.starts_with("4800:") && l.contains(&format!("to {net6}")))
        {
            ctx.ip6_run(&["rule", "add", "pref", "4800", "to", &net6, "lookup", tbl]);
        }
        if !up.is_empty() {
            for use_v6 in [false, true] {
                let rules_out = if use_v6 {
                    ctx.ip6(&["rule"]).stdout
                } else {
                    ctx.ip(&["rule"]).stdout
                };
                if !rules_out
                    .lines()
                    .any(|l| l.starts_with("18800:") && l.contains(&format!("iif {XR_IF}")))
                {
                    let args = ["rule", "add", "pref", "18800", "iif", XR_IF, "lookup", tbl];
                    if use_v6 {
                        ctx.ip6_run(&args);
                    } else {
                        ctx.ip_run(&args);
                    }
                }
                if !rules_out.lines().any(|l| {
                    l.starts_with("18900:") && l.contains(&format!("iif {XR_IF} lookup {up}"))
                }) {
                    let args = ["rule", "add", "pref", "18900", "iif", XR_IF, "lookup", &up];
                    if use_v6 {
                        ctx.ip6_run(&args);
                    } else {
                        ctx.ip_run(&args);
                    }
                }
            }
        }
        if fs::read_to_string(run.join("net.mode")).is_ok_and(|s| s.trim() == "fallback") {
            if !ctx
                .ip(&["route", "show", "table", "97"])
                .stdout
                .lines()
                .any(|l| l.starts_with(&format!("{net4} dev {XR_IF}")))
            {
                ctx.ip_run(&["route", "replace", &net4, "dev", XR_IF, "table", "97"]);
            }
            if !ctx
                .ip6(&["route", "show", "table", "97"])
                .stdout
                .lines()
                .any(|l| l.starts_with(&format!("{net6} dev {XR_IF}")))
            {
                ctx.ip6_run(&["route", "replace", &net6, "dev", XR_IF, "table", "97"]);
            }
            routes_reassert(&ctx);
        }
    }
    0
}

#[must_use]
pub fn net_down(env: &Env) -> i32 {
    let cfg = Config::load(env);
    let ctx = NetCtx::new(env, &cfg);
    let run = env.run_dir();

    if let Ok(w) = fs::read_to_string(run.join("net_watch.pid"))
        && !send_signal(&w, rustix::process::Signal::TERM)
    {
        dlog(
            env,
            &cfg,
            "net",
            &format!("net_watch（pid {}）已不在，无需停止", w.trim()),
        );
    }
    {
        let _lock = net_lock(env);
        hs_tap_down(&ctx);
    }
    v6_proxy_remove(&ctx, Some("XRouter 网络停止"));
    v6_nft_teardown(env);
    gw_remove(&ctx);
    routes_clear(&ctx);

    while ctx.ipt(&["-D", "FORWARD", "-j", "XR_FWD"]).success() {}
    ctx.ipt_run(&["-F", "XR_FWD"]);
    ctx.ipt_run(&["-X", "XR_FWD"]);
    while ctx
        .ipt(&["-t", "nat", "-D", "POSTROUTING", "-j", "XR_NAT"])
        .success()
    {}
    ctx.ipt_run(&["-t", "nat", "-F", "XR_NAT"]);
    ctx.ipt_run(&["-t", "nat", "-X", "XR_NAT"]);

    if let Ok(old_outs) = fs::read_to_string(run.join("net.up_out")) {
        for o in old_outs.split_whitespace() {
            ctx.ndc_run(&["ipfwd", "remove", XR_IF, o]);
        }
    }

    while ctx.ip6t(&["-D", "FORWARD", "-j", "XR_FWD6"]).success() {}
    ctx.ip6t_run(&["-F", "XR_FWD6"]);
    ctx.ip6t_run(&["-X", "XR_FWD6"]);
    if ip6_nat_ok(env) {
        while ctx
            .ip6t(&["-t", "nat", "-D", "POSTROUTING", "-j", "XR_NAT6"])
            .success()
        {}
        ctx.ip6t_run(&["-t", "nat", "-F", "XR_NAT6"]);
        ctx.ip6t_run(&["-t", "nat", "-X", "XR_NAT6"]);
    }
    mss_clear(&ctx);
    acct_teardown(&ctx);

    for pref in ["18800", "18900", "4800"] {
        while ctx.ip(&["rule", "del", "pref", pref]).success() {}
        while ctx.ip6(&["rule", "del", "pref", pref]).success() {}
    }

    ctx.ndc_run(&["ipfwd", "disable", "xrouter"]);
    let orig_fwd = run.join("ip_forward.orig");
    if orig_fwd.is_file() {
        let v = fs::read_to_string(&orig_fwd).unwrap_or_default();
        ctx.sysw_run(&env.proc_dir.join("sys/net/ipv4/ip_forward"), v.trim());
    }
    let net4 = format!("{}.0/24", cfg.net_prefix());
    let net6 = format!("{XR_NET6}::/64");
    ctx.ndc_run(&["network", "route", "remove", "local", XR_IF, &net4]);
    ctx.ndc_run(&["network", "route", "remove", "local", XR_IF, &net6]);
    ctx.ndc_run(&["network", "interface", "remove", "local", XR_IF]);
    ctx.ip_run(&["route", "flush", "table", "97", "dev", XR_IF]);
    ctx.ip6_run(&["route", "flush", "table", "97", "dev", XR_IF]);
    ctx.ip_run(&["link", "del", XR_IF]);

    if let Ok(entries) = fs::read_dir(&run) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if s.starts_with("net.") {
                fsx::remove_logged(env, entry.path());
            }
        }
    }
    fsx::remove_logged(env, run.join("routes.active"));
    fsx::remove_logged(env, run.join("ip_forward.orig"));
    fsx::remove_logged(env, run.join("net_watch.pid"));
    0
}

#[derive(Debug, Serialize)]
pub struct NetStatusJson {
    pub mode: String,
    pub up_if: String,
    pub expose: String,
    pub expose_if: Vec<String>,
    pub vm_ip: String,
    pub host_ip: String,
    pub gw: String,
    pub hotspot: String,
    pub hotspot_net: String,
}

pub fn net_status(env: &Env) -> NetStatusJson {
    let cfg = Config::load(env);
    let run = env.run_dir();
    let mode = fs::read_to_string(run.join("net.mode"))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "down".to_owned());
    let up_if = fs::read_to_string(run.join("net.up_if"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    let expose_if = fs::read_to_string(run.join("net.expose_if"))
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let gw = fs::read_to_string(run.join("net.gw"))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "off".to_owned());
    let hotspot = fs::read_to_string(run.join("net.hs_if"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    let hotspot_net = fs::read_to_string(run.join("net.hs_gw"))
        .unwrap_or_default()
        .trim()
        .to_owned();

    NetStatusJson {
        mode,
        up_if,
        expose: cfg.expose().to_owned(),
        expose_if,
        vm_ip: format!("{}.254", cfg.net_prefix()),
        host_ip: format!("{}.1", cfg.net_prefix()),
        gw,
        hotspot,
        hotspot_net,
    }
}

#[must_use]
pub fn net_status_json(env: &Env) -> String {
    serde_json::to_string(&net_status(env)).unwrap_or_else(|_| "{\"mode\":\"down\"}".to_owned())
}

pub fn run_net_cli(env: &Env, args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("up") => net_up(env),
        Some("down") => net_down(env),
        Some("watch") => net_watch(env),
        Some("sync") => {
            net_sync(env);
            0
        }
        Some("status") => emit(env, &net_status_json(env), 0),
        _ => emit(env, "usage: net.sh up|down|watch|sync|status", 2),
    }
}
