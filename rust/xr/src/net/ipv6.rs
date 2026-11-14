//! `IPv6`：发现上游可共享前缀、为虚拟机安装邻居代理（NDP proxy），以及 `ip6tables` 不可用时的 nft 转发规则回退。

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::cidr::{ip6_expand, ip6_format, ip6_mask, v6_is_ll, v6_is_ula, v6_pick_addr};
use crate::env::{Env, XR_IF, XR_NET6};
use crate::log;
use crate::log::{FAILURES, dlog, note_failure};
use crate::net::common::NetCtx;
use crate::net::vpn_routes::tether_ifaces;
use crate::sys::exec::note_status;
use crate::sys::fsx;

/// 客机已上报 `IPv6` 可用（`guest/guest/ipv6` 内容为 1）时返回 true。
#[must_use]
pub(crate) fn vm_v6(env: &Env) -> bool {
    fs::read_to_string(env.run_dir().join("guest/guest/ipv6")).is_ok_and(|text| text.trim() == "1")
}

/// 接口上的全局 `IPv6` 地址（带前缀长度），排除链路本地与 ULA。
#[must_use]
pub(crate) fn v6_iface_globals(ctx: &NetCtx<'_>, iface: &str) -> Vec<String> {
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
        res.push(addr.to_owned());
    }
    res
}

/// 可共享给虚拟机的上游 `IPv6` 前缀。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct V6Uplink {
    /// 前缀（如 `2001:db8:1::/64`）。
    pub(crate) pfx: String,
    /// 前缀所在接口。
    pub(crate) up_if: String,
    /// 各接口上已占用的全局地址。
    pub(crate) busy: Vec<String>,
}

/// 地址 `addr`（`地址/长度`）所在的 48–64 位前缀：返回（规范写法 `前缀/长度`, 长度）；长度不在范围或无法解析时返回 `None`。
fn prefix_of(addr: &str) -> Option<(String, u8)> {
    let (bare, len_s) = addr.split_once('/')?;
    let len = len_s.parse::<u8>().ok()?;
    if !(48..=64).contains(&len) {
        return None;
    }
    let fmt = ip6_expand(bare)
        .and_then(|x| ip6_mask(&x, len))
        .and_then(|net| ip6_format(&net))?;
    Some((format!("{fmt}/{len}"), len))
}

/// 在上游出口与共享网络接口上寻找可共享给虚拟机的 `IPv6` 前缀（48–64 位，上游出口优先）。
///
/// 先在全部接口中取最短前缀并收集已占用地址，再按上游出口顺序取第一个可用前缀覆盖之；结果写入 `net.v6pfx`、`net.v6upif`。
/// 找不到时返回 `None`。
#[must_use]
pub(crate) fn v6_discover(ctx: &NetCtx<'_>, outs6: &[String]) -> Option<V6Uplink> {
    let run = ctx.env.run_dir();
    fsx::remove_logged(ctx.env, run.join("net.v6pfx"));
    fsx::remove_logged(ctx.env, run.join("net.v6upif"));

    let mut ifaces = outs6.to_vec();
    ifaces.extend(tether_ifaces(ctx));

    let mut busy = Vec::new();
    let mut best = String::new();
    let mut best_len: u8 = 128;
    let mut best_if = String::new();
    for iface in ifaces
        .iter()
        .filter(|name| !name.is_empty() && *name != XR_IF)
    {
        for addr in v6_iface_globals(ctx, iface) {
            busy.push(addr.clone());
            if let Some((pfx, len)) = prefix_of(&addr)
                && len < best_len
            {
                best = pfx;
                best_len = len;
                best_if.clone_from(iface);
            }
        }
    }

    let preferred = outs6
        .iter()
        .filter(|name| !name.is_empty())
        .find_map(|iface| {
            v6_iface_globals(ctx, iface)
                .iter()
                .find_map(|addr| prefix_of(addr))
                .map(|(pfx, _)| (pfx, iface.clone()))
        });
    if let Some((pfx, iface)) = preferred {
        best = pfx;
        best_if = iface;
    }

    if best.is_empty() {
        return None;
    }
    fsx::write_logged(ctx.env, run.join("net.v6pfx"), format!("{best}\n"));
    fsx::write_logged(ctx.env, run.join("net.v6upif"), format!("{best_if}\n"));
    Some(V6Uplink {
        pfx: best,
        up_if: best_if,
        busy,
    })
}

/// 关闭安卓的热点硬件卸载（`settings`、`ndc`、`setprop` 三处），避免卸载路径绕过邻居代理；失败只计数。
pub(crate) fn v6_tether_offload_off(ctx: &NetCtx<'_>) {
    for (prog, args) in [
        (
            "settings",
            &["put", "global", "tether_offload_disabled", "1"][..],
        ),
        ("ndc", &["tether", "offload", "disable"][..]),
        ("setprop", &["persist.net.tethering.offload", "0"][..]),
    ] {
        let status = Command::new(prog)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        note_status(ctx.env, prog, status);
    }
}

/// 写内核参数前把原值保存到 `orig_path`（只在第一次保存），以便撤销时恢复。
fn v6_sysw_save(ctx: &NetCtx<'_>, path: &Path, val: &str, orig_path: &Path) {
    if !orig_path.is_file() {
        let orig = fs::read_to_string(path).unwrap_or_default();
        fsx::write_logged(ctx.env, orig_path, orig);
    }
    ctx.sysw_run(path, val);
}

/// 把上游 `IPv6` 出口接口名写给客机（`guest/host/ipv6.wan`），空串表示没有。
pub(crate) fn v6_write_wan(env: &Env, wan: &str) {
    let host_dir = env.run_dir().join("guest/host");
    fsx::mkdirs_logged(env, &host_dir);
    let content = if wan.is_empty() {
        String::new()
    } else {
        format!("{wan}\n")
    };
    fsx::write_logged(env, host_dir.join("ipv6.wan"), content);
}

/// 撤销 `IPv6` 邻居代理：删除代理表项与路由、恢复被改过的内核参数，删除 `run/net.v6*` 状态；已安装时记一行 INFO 说明原因。
pub(crate) fn v6_proxy_remove(ctx: &NetCtx<'_>, reason: Option<&str>) {
    let run = ctx.env.run_dir();
    let addr = fs::read_to_string(run.join("net.v6vm"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    if run.join("net.v6proxy").is_file() {
        let why = reason.unwrap_or("虚拟机 IPv6 已关闭");
        log::info(ctx.env, "net", &format!("IPv6 邻居代理已撤销（{why}）"));
    }
    let up_if = fs::read_to_string(run.join("net.v6upif"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    if !addr.is_empty() {
        let bare = addr.split('/').next().unwrap_or(&addr);
        if !up_if.is_empty() {
            ctx.ip6_run(&["neigh", "del", "proxy", bare, "dev", &up_if]);
        }
        for tether in tether_ifaces(ctx) {
            ctx.ip6_run(&["neigh", "del", "proxy", bare, "dev", &tether]);
        }
        ctx.ip6_run(&["route", "del", &addr, "dev", XR_IF]);
        let bare128 = format!("{bare}/128");
        ctx.ip6_run(&["route", "del", &bare128, "dev", XR_IF]);
    }
    if !up_if.is_empty() {
        ctx.sysw_run(
            &ctx.env
                .proc_dir
                .join(format!("sys/net/ipv6/conf/{up_if}/proxy_ndp")),
            "0",
        );
    }
    ctx.sysw_run(
        &ctx.env
            .proc_dir
            .join(format!("sys/net/ipv6/conf/{XR_IF}/proxy_ndp")),
        "0",
    );
    let orig_path = run.join("v6fwd.orig");
    if orig_path.is_file() {
        let orig = fs::read_to_string(&orig_path).unwrap_or_default();
        ctx.sysw_run(
            &ctx.env.proc_dir.join("sys/net/ipv6/conf/all/forwarding"),
            orig.trim(),
        );
        fsx::remove_logged(ctx.env, &orig_path);
    }
    v6_write_wan(ctx.env, "");
    fsx::remove_logged(ctx.env, run.join("net.v6proxy"));
    fsx::remove_logged(ctx.env, run.join("net.v6vm"));
    fsx::remove_logged(ctx.env, run.join("net.v6pfx"));
}

/// 选定邻居代理用的上游接口与虚拟机地址：无可用前缀时记一次提示（`net.v6nopfx`）并撤销代理，
/// 前缀中分不到地址时记错误并撤销；成功返回（上游接口, 地址/长度）。
fn v6_pick_uplink(ctx: &NetCtx<'_>, outs6: &[String]) -> Option<(String, String)> {
    let run = ctx.env.run_dir();
    let Some(V6Uplink { pfx, up_if, busy }) = v6_discover(ctx, outs6) else {
        if !run.join("net.v6nopfx").is_file() {
            log::info(
                ctx.env,
                "net",
                "上游没有可共享的 IPv6 前缀（可能仅有本机 /128），无法做邻居代理",
            );
        }
        fsx::touch_logged(ctx.env, run.join("net.v6nopfx"));
        v6_proxy_remove(ctx, Some("上游没有可共享的前缀"));
        return None;
    };
    fsx::remove_logged(ctx.env, run.join("net.v6nopfx"));
    let Some(wan) = v6_pick_addr(&pfx, &busy, ctx.cfg.vm_mac()) else {
        log::err(
            ctx.env,
            "net",
            &format!("无法在 {pfx} 中分到未占用的 IPv6 地址"),
        );
        v6_proxy_remove(ctx, Some("前缀中没有可用地址"));
        return None;
    };
    Some((up_if, wan))
}

/// 打开 `IPv6` 转发与 `proxy_ndp`（保存 `all/forwarding` 原值）、加代理表项与 `/128` 路由、关闭热点硬件卸载；
/// 返回上游接口的 `proxy_ndp` 是否已为 1。
fn v6_enable_proxy(ctx: &NetCtx<'_>, up_if: &str, bare: &str) -> bool {
    let run = ctx.env.run_dir();
    let proc_v6 = ctx.env.proc_dir.join("sys/net/ipv6/conf");
    fsx::mkdirs_logged(ctx.env, proc_v6.join("all"));
    fsx::mkdirs_logged(ctx.env, proc_v6.join(up_if));
    fsx::mkdirs_logged(ctx.env, proc_v6.join(XR_IF));

    v6_sysw_save(
        ctx,
        &proc_v6.join("all/forwarding"),
        "1",
        &run.join("v6fwd.orig"),
    );
    ctx.sysw_run(&proc_v6.join(format!("{up_if}/forwarding")), "1");
    ctx.sysw_run(&proc_v6.join(format!("{XR_IF}/forwarding")), "1");
    ctx.sysw_run(&proc_v6.join(format!("{up_if}/accept_ra")), "2");
    ctx.sysw_run(&proc_v6.join(format!("{up_if}/proxy_ndp")), "1");
    ctx.sysw_run(&proc_v6.join(format!("{XR_IF}/proxy_ndp")), "1");
    ctx.ip6_run(&["neigh", "replace", "proxy", bare, "dev", up_if]);
    let bare128 = format!("{bare}/128");
    ctx.ip6_run(&["route", "replace", &bare128, "dev", XR_IF]);
    v6_tether_offload_off(ctx);

    fs::read_to_string(proc_v6.join(format!("{up_if}/proxy_ndp")))
        .is_ok_and(|text| text.trim() == "1")
}

/// 为虚拟机安装 `IPv6` 邻居代理：选前缀与地址、打开转发与 `proxy_ndp`、加代理表项与 `/128` 路由，成功返回 true。
///
/// 前缀或地址未变时只补齐表项；无可用前缀或地址时撤销并返回 false。
#[must_use]
pub(crate) fn v6_proxy_install(ctx: &NetCtx<'_>, outs6: &[String]) -> bool {
    let run = ctx.env.run_dir();
    let Some((up_if, wan)) = v6_pick_uplink(ctx, outs6) else {
        return false;
    };
    let bare = wan.split('/').next().unwrap_or(&wan);

    if run.join("net.v6proxy").is_file()
        && fs::read_to_string(run.join("net.v6vm")).is_ok_and(|raw| raw.trim() == wan)
    {
        ctx.ip6_run(&["neigh", "replace", "proxy", bare, "dev", &up_if]);
        v6_write_wan(ctx.env, &wan);
        return true;
    }

    if run.join("net.v6proxy").is_file() {
        v6_proxy_remove(ctx, Some("上游前缀或地址变化，重新安装"));
    }
    let Some(V6Uplink {
        pfx: new_pfx,
        up_if: new_up_if,
        busy: new_busy,
    }) = v6_discover(ctx, outs6)
    else {
        return false;
    };
    let Some(new_wan) = v6_pick_addr(&new_pfx, &new_busy, ctx.cfg.vm_mac()) else {
        return false;
    };
    let new_bare = new_wan.split('/').next().unwrap_or(&new_wan);

    if !v6_enable_proxy(ctx, &new_up_if, new_bare) {
        log::err(
            ctx.env,
            "net",
            &format!("IPv6 邻居代理未生效（{new_up_if} proxy_ndp 写入失败）"),
        );
        fsx::write_logged(ctx.env, run.join("net.v6vm"), format!("{new_wan}\n"));
        v6_proxy_remove(ctx, Some("proxy_ndp 未生效"));
        return false;
    }

    fsx::write_logged(ctx.env, run.join("net.v6vm"), format!("{new_wan}\n"));
    v6_write_wan(ctx.env, &new_wan);
    fsx::touch_logged(ctx.env, run.join("net.v6proxy"));
    log::info(
        ctx.env,
        "net",
        &format!(
            "IPv6 邻居代理已开启：{new_wan} dev {new_up_if}（运营商若只放行本机地址，则可能无法出网）"
        ),
    );
    true
}

/// 上游出口与共享网络接口上全部全局 `IPv6` 地址的拼接串，用于判断地址是否变化。
#[must_use]
pub(crate) fn v6_src_fingerprint(ctx: &NetCtx<'_>, outs6: &[String]) -> String {
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

/// 按客机 `IPv6` 状态安装或撤销邻居代理，并写调试日志。
pub(crate) fn v6_proxy_sync(ctx: &NetCtx<'_>, outs6: &[String]) {
    let guest_v6 = fs::read_to_string(ctx.env.run_dir().join("guest/guest/ipv6"))
        .unwrap_or_else(|_| "未上报".to_owned());
    let outs6_s = if outs6.is_empty() {
        "无".to_owned()
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
    fsx::write_logged(
        ctx.env,
        ctx.env.run_dir().join("net.v6src"),
        format!("{fp} "),
    );
}

/// 与 [`nft_cmd`] 相同，只关心成败：失败（含无法执行）时计数，不返回结果。
fn nft_run(args: &[&str]) {
    if !nft_cmd(args) {
        note_failure(&FAILURES.exec);
    }
}

/// 执行 `nft`（输出丢弃），成功返回 true。
fn nft_cmd(args: &[&str]) -> bool {
    Command::new("nft")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|exit_status| exit_status.success())
}

/// 建立 nft 表 `inet xr_v6` 与转发链作为 `ip6tables` 的替代，成功时写 `run/net.fwd6nft` 并返回 true。
#[must_use]
pub(crate) fn v6_nft_setup(env: &Env) -> bool {
    if !nft_cmd(&["list", "tables"]) {
        return false;
    }
    nft_run(&["add", "table", "inet", "xr_v6"]);
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
    fsx::touch_logged(env, env.run_dir().join("net.fwd6nft"));
    true
}

/// 删除 nft 表 `inet xr_v6`（仅在 `run/net.fwd6nft` 存在时）。
pub(crate) fn v6_nft_teardown(env: &Env) {
    let marker = env.run_dir().join("net.fwd6nft");
    if !marker.is_file() {
        return;
    }
    nft_run(&["delete", "table", "inet", "xr_v6"]);
    fsx::remove_logged(env, marker);
}

/// 按网关形态重建 nft 转发链中的 `IPv6` 规则（仅在使用 nft 回退时）。
pub(crate) fn v6_nft_sync(ctx: &NetCtx<'_>, gw_mode: &str, outs6: &[String], ex: &[String]) {
    let run = ctx.env.run_dir();
    if !run.join("net.fwd6nft").is_file() {
        return;
    }
    if !nft_cmd(&["flush", "chain", "inet", "xr_v6", "fwd"]) {
        return;
    }
    let net6 = format!("{XR_NET6}::/64");
    nft_run(&[
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
    nft_run(&[
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
        for out6 in outs6 {
            nft_run(&[
                "add", "rule", "inet", "xr_v6", "fwd", "iifname", XR_IF, "oifname", out6, "accept",
            ]);
        }
    }
    let active_routes: Vec<String> = fs::read_to_string(run.join("routes.active"))
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains(':'))
        .map(str::to_owned)
        .collect();
    for i in ex {
        nft_run(&[
            "add", "rule", "inet", "xr_v6", "fwd", "iifname", i, "oifname", XR_IF, "ip6", "daddr",
            &net6, "accept",
        ]);
        for route in &active_routes {
            nft_run(&[
                "add", "rule", "inet", "xr_v6", "fwd", "iifname", i, "oifname", XR_IF, "ip6",
                "daddr", route, "accept",
            ]);
        }
    }
    let ex_set: BTreeSet<&str> = ex.iter().map(String::as_str).collect();
    for iface in outs6 {
        if !ex_set.contains(iface.as_str()) {
            nft_run(&[
                "add", "rule", "inet", "xr_v6", "fwd", "iifname", iface, "oifname", XR_IF, "drop",
            ]);
        }
    }
    if gw_mode != "off" {
        for i in tether_ifaces(ctx) {
            if gw_mode == "v6" {
                nft_run(&[
                    "add", "rule", "inet", "xr_v6", "fwd", "iifname", &i, "oifname", XR_IF,
                    "accept",
                ]);
                nft_run(&[
                    "add", "rule", "inet", "xr_v6", "fwd", "iifname", XR_IF, "oifname", &i,
                    "accept",
                ]);
            } else {
                nft_run(&[
                    "add", "rule", "inet", "xr_v6", "fwd", "iifname", &i, "oifname", XR_IF, "meta",
                    "l4proto", "tcp", "reject", "with", "tcp", "reset",
                ]);
                nft_run(&[
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
            for out_if in outs6 {
                nft_run(&[
                    "add", "rule", "inet", "xr_v6", "fwd", "iifname", &i, "oifname", out_if,
                    "meta", "l4proto", "tcp", "reject", "with", "tcp", "reset",
                ]);
                nft_run(&[
                    "add",
                    "rule",
                    "inet",
                    "xr_v6",
                    "fwd",
                    "iifname",
                    &i,
                    "oifname",
                    out_if,
                    "reject",
                    "with",
                    "icmpv6",
                    "admin-prohibited",
                ]);
            }
        }
    }
    nft_run(&[
        "add", "rule", "inet", "xr_v6", "fwd", "oifname", XR_IF, "drop",
    ]);
    nft_run(&[
        "add", "rule", "inet", "xr_v6", "fwd", "iifname", XR_IF, "drop",
    ]);
}
