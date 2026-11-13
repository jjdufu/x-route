use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::cidr::{ip6_expand, ip6_format, ip6_mask, v6_is_ll, v6_is_ula, v6_pick_addr};
use crate::env::{Env, XR_IF, XR_NET6};
use crate::log::{dlog, log_err, log_info};
use crate::net::common::NetCtx;
use crate::net::vpn_routes::tether_ifaces;

pub fn vm_v6(env: &Env) -> bool {
    fs::read_to_string(env.run_dir().join("guest/guest/ipv6")).is_ok_and(|s| s.trim() == "1")
}

pub fn v6_iface_globals(ctx: &NetCtx<'_>, iface: &str) -> Vec<String> {
    if iface.is_empty() {
        return Vec::new();
    }
    let out = ctx.ip(&["-6", "-o", "addr", "show", "dev", iface]);
    let mut res = Vec::new();
    for line in out.stdout.lines() {
        let Some(addr) = line.split_whitespace().nth(3) else {
            continue;
        };
        if v6_is_ll(addr) || v6_is_ula(addr) {
            continue;
        }
        res.push(addr.to_string());
    }
    res
}

pub fn v6_discover(ctx: &NetCtx<'_>, outs6: &[String]) -> Option<(String, String, Vec<String>)> {
    let run = ctx.env.run_dir();
    let _ = fs::remove_file(run.join("net.v6pfx"));
    let _ = fs::remove_file(run.join("net.v6upif"));

    let mut ifaces = outs6.to_vec();
    ifaces.extend(tether_ifaces(ctx));

    let mut busy = Vec::new();
    let mut best = String::new();
    let mut best_len: u8 = 128;
    let mut best_if = String::new();

    for iface in &ifaces {
        if iface.is_empty() || iface == XR_IF {
            continue;
        }
        for addr in v6_iface_globals(ctx, iface) {
            busy.push(addr.clone());
            let Some((bare, len_s)) = addr.split_once('/') else {
                continue;
            };
            let Ok(len) = len_s.parse::<u8>() else {
                continue;
            };
            if (48..=64).contains(&len) && len < best_len {
                if let (Some(h), Some(net)) = (
                    ip6_expand(bare),
                    ip6_expand(bare).and_then(|x| ip6_mask(&x, len)),
                ) {
                    let _ = h;
                    if let Some(fmt) = ip6_format(&net) {
                        best = format!("{fmt}/{len}");
                        best_len = len;
                        best_if.clone_from(iface);
                    }
                }
            }
        }
    }

    'outer: for iface in outs6 {
        if iface.is_empty() {
            continue;
        }
        for addr in v6_iface_globals(ctx, iface) {
            let Some((bare, len_s)) = addr.split_once('/') else {
                continue;
            };
            let Ok(len) = len_s.parse::<u8>() else {
                continue;
            };
            if !(48..=64).contains(&len) {
                continue;
            }
            if let Some(fmt) = ip6_expand(bare)
                .and_then(|x| ip6_mask(&x, len))
                .and_then(|net| ip6_format(&net))
            {
                best = format!("{fmt}/{len}");
                best_if.clone_from(iface);
                break 'outer;
            }
        }
    }

    if best.is_empty() {
        return None;
    }
    let _ = fs::write(run.join("net.v6pfx"), format!("{best}\n"));
    let _ = fs::write(run.join("net.v6upif"), format!("{best_if}\n"));
    Some((best, best_if, busy))
}

pub fn v6_tether_offload_off() {
    let _ = Command::new("settings")
        .args(["put", "global", "tether_offload_disabled", "1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = Command::new("ndc")
        .args(["tether", "offload", "disable"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = Command::new("setprop")
        .args(["persist.net.tethering.offload", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn v6_sysw_save(ctx: &NetCtx<'_>, path: &Path, val: &str, orig_path: &Path) {
    if !orig_path.is_file() {
        let orig = fs::read_to_string(path).unwrap_or_default();
        let _ = fs::write(orig_path, orig);
    }
    ctx.sysw(path, val);
}

pub fn v6_write_wan(env: &Env, wan: &str) {
    let host_dir = env.run_dir().join("guest/host");
    let _ = fs::create_dir_all(&host_dir);
    let content = if wan.is_empty() {
        String::new()
    } else {
        format!("{wan}\n")
    };
    let _ = fs::write(host_dir.join("ipv6.wan"), content);
}

pub fn v6_proxy_remove(ctx: &NetCtx<'_>, reason: Option<&str>) {
    let run = ctx.env.run_dir();
    let addr = fs::read_to_string(run.join("net.v6vm"))
        .unwrap_or_default()
        .trim()
        .to_string();
    if run.join("net.v6proxy").is_file() {
        let r = reason.unwrap_or("虚拟机 IPv6 已关闭");
        log_info(ctx.env, "net", &format!("IPv6 邻居代理已撤销（{r}）"));
    }
    let up_if = fs::read_to_string(run.join("net.v6upif"))
        .unwrap_or_default()
        .trim()
        .to_string();
    if !addr.is_empty() {
        let bare = addr.split('/').next().unwrap_or(&addr);
        if !up_if.is_empty() {
            let _ = ctx.ip6(&["neigh", "del", "proxy", bare, "dev", &up_if]);
        }
        for t in tether_ifaces(ctx) {
            let _ = ctx.ip6(&["neigh", "del", "proxy", bare, "dev", &t]);
        }
        let _ = ctx.ip6(&["route", "del", &addr, "dev", XR_IF]);
        let bare128 = format!("{bare}/128");
        let _ = ctx.ip6(&["route", "del", &bare128, "dev", XR_IF]);
    }
    if !up_if.is_empty() {
        ctx.sysw(
            &ctx.env
                .proc_dir
                .join(format!("sys/net/ipv6/conf/{up_if}/proxy_ndp")),
            "0",
        );
    }
    ctx.sysw(
        &ctx.env
            .proc_dir
            .join(format!("sys/net/ipv6/conf/{XR_IF}/proxy_ndp")),
        "0",
    );
    let orig_path = run.join("v6fwd.orig");
    if orig_path.is_file() {
        let orig = fs::read_to_string(&orig_path).unwrap_or_default();
        ctx.sysw(
            &ctx.env.proc_dir.join("sys/net/ipv6/conf/all/forwarding"),
            orig.trim(),
        );
        let _ = fs::remove_file(&orig_path);
    }
    v6_write_wan(ctx.env, "");
    let _ = fs::remove_file(run.join("net.v6proxy"));
    let _ = fs::remove_file(run.join("net.v6vm"));
    let _ = fs::remove_file(run.join("net.v6pfx"));
}

pub fn v6_proxy_install(ctx: &NetCtx<'_>, outs6: &[String]) -> bool {
    let run = ctx.env.run_dir();
    let Some((pfx, up_if, busy)) = v6_discover(ctx, outs6) else {
        if !run.join("net.v6nopfx").is_file() {
            log_info(
                ctx.env,
                "net",
                "上游没有可共享的 IPv6 前缀（可能仅有本机 /128），无法做邻居代理",
            );
        }
        let _ = fs::File::create(run.join("net.v6nopfx"));
        v6_proxy_remove(ctx, Some("上游没有可共享的前缀"));
        return false;
    };
    let _ = fs::remove_file(run.join("net.v6nopfx"));
    let Some(wan) = v6_pick_addr(&pfx, &busy, ctx.cfg.vm_mac()) else {
        log_err(
            ctx.env,
            "net",
            &format!("无法在 {pfx} 中分到未占用的 IPv6 地址"),
        );
        v6_proxy_remove(ctx, Some("前缀中没有可用地址"));
        return false;
    };
    let bare = wan.split('/').next().unwrap_or(&wan);

    if run.join("net.v6proxy").is_file()
        && fs::read_to_string(run.join("net.v6vm")).is_ok_and(|s| s.trim() == wan)
    {
        let _ = ctx.ip6(&["neigh", "replace", "proxy", bare, "dev", &up_if]);
        v6_write_wan(ctx.env, &wan);
        return true;
    }

    if run.join("net.v6proxy").is_file() {
        v6_proxy_remove(ctx, Some("上游前缀或地址变化，重新安装"));
    }
    let Some((pfx, up_if, busy)) = v6_discover(ctx, outs6) else {
        return false;
    };
    let Some(wan) = v6_pick_addr(&pfx, &busy, ctx.cfg.vm_mac()) else {
        return false;
    };
    let bare = wan.split('/').next().unwrap_or(&wan);

    let proc_v6 = ctx.env.proc_dir.join("sys/net/ipv6/conf");
    let _ = fs::create_dir_all(proc_v6.join("all"));
    let _ = fs::create_dir_all(proc_v6.join(&up_if));
    let _ = fs::create_dir_all(proc_v6.join(XR_IF));

    v6_sysw_save(
        ctx,
        &proc_v6.join("all/forwarding"),
        "1",
        &run.join("v6fwd.orig"),
    );
    ctx.sysw(&proc_v6.join(format!("{up_if}/forwarding")), "1");
    ctx.sysw(&proc_v6.join(format!("{XR_IF}/forwarding")), "1");
    ctx.sysw(&proc_v6.join(format!("{up_if}/accept_ra")), "2");
    ctx.sysw(&proc_v6.join(format!("{up_if}/proxy_ndp")), "1");
    ctx.sysw(&proc_v6.join(format!("{XR_IF}/proxy_ndp")), "1");
    let _ = ctx.ip6(&["neigh", "replace", "proxy", bare, "dev", &up_if]);
    let bare128 = format!("{bare}/128");
    let _ = ctx.ip6(&["route", "replace", &bare128, "dev", XR_IF]);
    v6_tether_offload_off();

    let ok = fs::read_to_string(proc_v6.join(format!("{up_if}/proxy_ndp")))
        .is_ok_and(|s| s.trim() == "1");
    if !ok {
        log_err(
            ctx.env,
            "net",
            &format!("IPv6 邻居代理未生效（{up_if} proxy_ndp 写入失败）"),
        );
        let _ = fs::write(run.join("net.v6vm"), format!("{wan}\n"));
        v6_proxy_remove(ctx, Some("proxy_ndp 未生效"));
        return false;
    }

    let _ = fs::write(run.join("net.v6vm"), format!("{wan}\n"));
    v6_write_wan(ctx.env, &wan);
    let _ = fs::File::create(run.join("net.v6proxy"));
    log_info(
        ctx.env,
        "net",
        &format!(
            "IPv6 邻居代理已开启：{wan} dev {up_if}（运营商若只放行本机地址，则可能无法出网）"
        ),
    );
    true
}

pub fn v6_src_fingerprint(ctx: &NetCtx<'_>, outs6: &[String]) -> String {
    let mut ifaces = outs6.to_vec();
    ifaces.extend(tether_ifaces(ctx));
    let mut addrs = Vec::new();
    for iface in ifaces {
        if !iface.is_empty() {
            addrs.extend(v6_iface_globals(ctx, &iface));
        }
    }
    addrs.join(" ")
}

pub fn v6_proxy_sync(ctx: &NetCtx<'_>, outs6: &[String]) {
    let guest_v6 = fs::read_to_string(ctx.env.run_dir().join("guest/guest/ipv6"))
        .unwrap_or_else(|_| "未上报".to_string());
    let outs6_s = if outs6.is_empty() {
        "无".to_string()
    } else {
        outs6.join(" ")
    };
    dlog(
        ctx.env,
        ctx.cfg,
        "v6",
        &format!(
            "邻居代理同步：客侧 IPv6={} 上游 IPv6 出口={outs6_s}",
            guest_v6.trim()
        ),
    );
    if vm_v6(ctx.env) {
        if !v6_proxy_install(ctx, outs6) {
            v6_write_wan(ctx.env, "");
        }
    } else {
        v6_proxy_remove(ctx, None);
    }
    let fp = v6_src_fingerprint(ctx, outs6);
    let _ = fs::write(ctx.env.run_dir().join("net.v6src"), format!("{fp} "));
}

fn nft_cmd(args: &[&str]) -> bool {
    Command::new("nft")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

pub fn v6_nft_setup(env: &Env) -> bool {
    if !nft_cmd(&["list", "tables"]) {
        return false;
    }
    let _ = nft_cmd(&["add", "table", "inet", "xr_v6"]);
    if !nft_cmd(&[
        "add",
        "chain",
        "inet",
        "xr_v6",
        "fwd",
        "{ type filter hook forward priority -10; policy accept; }",
    ]) {
        return false;
    }
    let _ = fs::File::create(env.run_dir().join("net.fwd6nft"));
    true
}

pub fn v6_nft_teardown(env: &Env) {
    let marker = env.run_dir().join("net.fwd6nft");
    if !marker.is_file() {
        return;
    }
    let _ = nft_cmd(&["delete", "table", "inet", "xr_v6"]);
    let _ = fs::remove_file(marker);
}

pub fn v6_nft_sync(ctx: &NetCtx<'_>, gw_mode: &str, outs6: &[String], ex: &[String]) {
    let run = ctx.env.run_dir();
    if !run.join("net.fwd6nft").is_file() {
        return;
    }
    if !nft_cmd(&["flush", "chain", "inet", "xr_v6", "fwd"]) {
        return;
    }
    let net6 = format!("{XR_NET6}::/64");
    let _ = nft_cmd(&[
        "add",
        "rule",
        "inet",
        "xr_v6",
        "fwd",
        "oifname",
        XR_IF,
        "ct",
        "state",
        "related,established",
        "accept",
    ]);
    let _ = nft_cmd(&[
        "add",
        "rule",
        "inet",
        "xr_v6",
        "fwd",
        "iifname",
        XR_IF,
        "ct",
        "state",
        "related,established",
        "accept",
    ]);
    if vm_v6(ctx.env) {
        for o in outs6 {
            let _ = nft_cmd(&[
                "add", "rule", "inet", "xr_v6", "fwd", "iifname", XR_IF, "oifname", o, "accept",
            ]);
        }
    }
    let active_routes: Vec<String> = fs::read_to_string(run.join("routes.active"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains(':'))
        .map(ToString::to_string)
        .collect();
    for i in ex {
        let _ = nft_cmd(&[
            "add", "rule", "inet", "xr_v6", "fwd", "iifname", i, "oifname", XR_IF, "ip6", "daddr",
            &net6, "accept",
        ]);
        for c in &active_routes {
            let _ = nft_cmd(&[
                "add", "rule", "inet", "xr_v6", "fwd", "iifname", i, "oifname", XR_IF, "ip6",
                "daddr", c, "accept",
            ]);
        }
    }
    let ex_set: BTreeSet<&str> = ex.iter().map(String::as_str).collect();
    for o in outs6 {
        if !ex_set.contains(o.as_str()) {
            let _ = nft_cmd(&[
                "add", "rule", "inet", "xr_v6", "fwd", "iifname", o, "oifname", XR_IF, "drop",
            ]);
        }
    }
    if gw_mode != "off" {
        for i in tether_ifaces(ctx) {
            if gw_mode == "v6" {
                let _ = nft_cmd(&[
                    "add", "rule", "inet", "xr_v6", "fwd", "iifname", &i, "oifname", XR_IF,
                    "accept",
                ]);
                let _ = nft_cmd(&[
                    "add", "rule", "inet", "xr_v6", "fwd", "iifname", XR_IF, "oifname", &i,
                    "accept",
                ]);
            } else {
                let _ = nft_cmd(&[
                    "add", "rule", "inet", "xr_v6", "fwd", "iifname", &i, "oifname", XR_IF, "meta",
                    "l4proto", "tcp", "reject", "with", "tcp", "reset",
                ]);
                let _ = nft_cmd(&[
                    "add",
                    "rule",
                    "inet",
                    "xr_v6",
                    "fwd",
                    "iifname",
                    &i,
                    "oifname",
                    XR_IF,
                    "reject",
                    "with",
                    "icmpv6",
                    "admin-prohibited",
                ]);
            }
            for o in outs6 {
                let _ = nft_cmd(&[
                    "add", "rule", "inet", "xr_v6", "fwd", "iifname", &i, "oifname", o, "meta",
                    "l4proto", "tcp", "reject", "with", "tcp", "reset",
                ]);
                let _ = nft_cmd(&[
                    "add",
                    "rule",
                    "inet",
                    "xr_v6",
                    "fwd",
                    "iifname",
                    &i,
                    "oifname",
                    o,
                    "reject",
                    "with",
                    "icmpv6",
                    "admin-prohibited",
                ]);
            }
        }
    }
    let _ = nft_cmd(&[
        "add", "rule", "inet", "xr_v6", "fwd", "oifname", XR_IF, "drop",
    ]);
    let _ = nft_cmd(&[
        "add", "rule", "inet", "xr_v6", "fwd", "iifname", XR_IF, "drop",
    ]);
}
