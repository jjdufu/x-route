use std::fs;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use crate::cidr;
use crate::cidr::hs_subnet_clash;
use crate::env::{Env, HS_BR, HS_TAP, XR_IF, is_executable};
use crate::log;
use crate::log::{dlog, unix_now};
use crate::net::common::{NetCtx, trigger_diag_snap_bg};
use crate::net::ipv6::v6_tether_offload_off;
use crate::net::vpn_routes::tether_ifaces;
use crate::sys::exec::note_spawn;
use crate::sys::{fsx, is_pid_running, proc_cmdline};

const HS_TABLE: &str = "234";
const HS_PREF: &str = "19050";
const HS_PREF4: &str = "4790";

#[must_use]
pub fn vm_alive(env: &Env) -> bool {
    let pid = fs::read_to_string(env.run_dir().join("crosvm.pid")).unwrap_or_default();
    let trimmed = pid.trim();
    if !is_pid_running(env, trimmed) {
        return false;
    }
    proc_cmdline(env, trimmed).is_none_or(|cmdline| cmdline.contains("crosvm"))
}

#[must_use]
pub fn vm_is_ready(env: &Env) -> bool {
    if !vm_alive(env) {
        return false;
    }
    let st = fs::read_to_string(env.run_dir().join("state")).unwrap_or_default();
    matches!(st.trim(), "ready" | "stable")
}

#[must_use]
pub fn hs_tap_up(ctx: &NetCtx<'_>) -> bool {
    if !ctx.ip(&["link", "show", HS_TAP]).success()
        && !ctx.ip(&["tuntap", "add", HS_TAP, "mode", "tap"]).success()
    {
        log::err(ctx.env, "hotspot", &format!("创建 {HS_TAP} 失败"));
        return false;
    }
    ctx.ip_run(&["addr", "flush", "dev", HS_TAP]);
    ctx.sysw_run(
        &ctx.env
            .proc_dir
            .join(format!("sys/net/ipv6/conf/{HS_TAP}/disable_ipv6")),
        "1",
    );
    ctx.ip_run(&["link", "set", HS_TAP, "up"]);
    true
}

pub fn hs_tap_down(ctx: &NetCtx<'_>) {
    hs_unbridge(ctx);
    ctx.ip_run(&["link", "del", HS_TAP]);
}

#[must_use]
pub fn hs_master(env: &Env, iface: &str) -> String {
    fs::read_link(env.sysnet.join(iface).join("master"))
        .ok()
        .and_then(|path| path.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_default()
}

#[must_use]
pub fn hs_pick(ctx: &NetCtx<'_>) -> Option<String> {
    let mut cands = Vec::new();
    if let Ok(prev) = fs::read_to_string(ctx.env.run_dir().join("net.hs_if")) {
        for tok in prev.split_whitespace() {
            cands.push(tok.to_owned());
        }
    }
    cands.extend(tether_ifaces(ctx));
    for iface in cands {
        if iface.is_empty() || iface == XR_IF || iface == HS_TAP || iface == HS_BR {
            continue;
        }
        let dir = ctx.env.sysnet.join(&iface);
        if !dir.is_dir() {
            continue;
        }
        let is_wifi = dir.join("wireless").is_dir() || dir.join("phy80211").is_dir();
        if !is_wifi && !ctx.env.hs_any {
            continue;
        }
        let oper = fs::read_to_string(dir.join("operstate")).unwrap_or_default();
        if oper.trim() == "down" {
            continue;
        }
        return Some(iface);
    }
    None
}

#[must_use]
pub fn hs_guest_ok(env: &Env) -> bool {
    env.run_dir().join("guest/guest/hotspot").is_file()
}

#[must_use]
pub fn hs_want(ctx: &NetCtx<'_>) -> bool {
    ctx.cfg.hotspot_bridge()
        && ctx.cfg.vm_net()
        && vm_is_ready(ctx.env)
        && ctx.env.sysnet.join(HS_TAP).is_dir()
}

#[must_use]
pub fn hs_guest_cmd(env: &Env, cmd: &str, timeout_sec: u64) -> bool {
    let host_dir = env.run_dir().join("guest/host");
    fsx::mkdirs_logged(env, &host_dir);
    let ts = unix_now();
    fsx::write_logged(env, host_dir.join("hotspot.cmd"), format!("{cmd} {ts}\n"));

    let mut parts = cmd.split_whitespace();
    let expect = match (parts.next(), parts.next()) {
        (Some(p1), Some(p2)) => format!("{p1} {p2}"),
        (Some(p1), None) => p1.to_owned(),
        _ => String::new(),
    };
    let guest_file = env.run_dir().join("guest/guest/hotspot");
    for _ in 0..timeout_sec {
        if fs::read_to_string(&guest_file).is_ok_and(|text| text.trim() == expect) {
            return true;
        }
        thread::sleep(Duration::from_secs(1));
    }
    false
}

pub fn hs_rules(ctx: &NetCtx<'_>, iface: &str) {
    ctx.ipt_run(&["-N", "XR_HS"]);
    ctx.ipt_run(&["-F", "XR_HS"]);
    ctx.ipt_run(&[
        "-A", "XR_HS", "-o", iface, "-p", "udp", "--sport", "67", "-j", "DROP",
    ]);
    if !ctx.ipt(&["-C", "OUTPUT", "-j", "XR_HS"]).success() {
        ctx.ipt_run(&["-I", "OUTPUT", "1", "-j", "XR_HS"]);
    }

    ctx.ip6t_run(&["-N", "XR_HS6"]);
    ctx.ip6t_run(&["-F", "XR_HS6"]);
    ctx.ip6t_run(&[
        "-A",
        "XR_HS6",
        "-o",
        iface,
        "-p",
        "icmpv6",
        "--icmpv6-type",
        "router-advertisement",
        "-j",
        "DROP",
    ]);
    ctx.ip6t_run(&[
        "-A", "XR_HS6", "-o", iface, "-p", "udp", "--sport", "547", "-j", "DROP",
    ]);
    if !ctx.ip6t(&["-C", "OUTPUT", "-j", "XR_HS6"]).success() {
        ctx.ip6t_run(&["-I", "OUTPUT", "1", "-j", "XR_HS6"]);
    }

    while ctx.ip6(&["rule", "del", "pref", HS_PREF]).success() {}
    ctx.ip6_run(&[
        "route", "replace", "default", "dev", XR_IF, "table", HS_TABLE,
    ]);
    ctx.ip6_run(&[
        "rule", "add", "pref", HS_PREF, "iif", HS_BR, "lookup", HS_TABLE,
    ]);
}

pub fn hs_rules_clear(ctx: &NetCtx<'_>) {
    while ctx.ip(&["rule", "del", "pref", HS_PREF4]).success() {}
    ctx.ip_run(&["route", "flush", "table", HS_TABLE]);
    while ctx.ipt(&["-D", "OUTPUT", "-j", "XR_HS"]).success() {}
    ctx.ipt_run(&["-F", "XR_HS"]);
    ctx.ipt_run(&["-X", "XR_HS"]);
    while ctx.ip6t(&["-D", "OUTPUT", "-j", "XR_HS6"]).success() {}
    ctx.ip6t_run(&["-F", "XR_HS6"]);
    ctx.ip6t_run(&["-X", "XR_HS6"]);
    while ctx.ip6(&["rule", "del", "pref", HS_PREF]).success() {}
    ctx.ip6_run(&["route", "flush", "table", HS_TABLE]);
}

#[must_use]
pub fn hs_gw_cidr(ctx: &NetCtx<'_>, iface: &str, android_cidr: &str) -> String {
    let hf = ctx.cfg.hotspot_subnet();
    let clash_file = ctx.env.run_dir().join("net.hs_clash");
    if !hf.is_empty() {
        if let Some(hc) = hs_subnet_clash(ctx.env, ctx.cfg.net_prefix(), hf, Some(iface)) {
            let expected = format!("{hf} {hc}");
            if fs::read_to_string(&clash_file).map_or(true, |text| text.trim() != expected) {
                log::err(
                    ctx.env,
                    "hotspot",
                    &format!("固定热点网段 {hf} 与 {hc} 冲突，本次热点网段跟随安卓"),
                );
                fsx::write_logged(ctx.env, &clash_file, format!("{expected}\n"));
            }
            return android_cidr.to_owned();
        }
        fsx::remove_logged(ctx.env, &clash_file);
        return hf.to_owned();
    }
    fsx::remove_logged(ctx.env, &clash_file);
    android_cidr.to_owned()
}

pub fn hs_route4(ctx: &NetCtx<'_>, gw: &str, android_cidr: &str) {
    while ctx.ip(&["rule", "del", "pref", HS_PREF4]).success() {}
    ctx.ip_run(&["route", "flush", "table", HS_TABLE]);
    if gw == android_cidr {
        return;
    }
    let Some(hn) = cidr::network(gw) else {
        return;
    };
    let via = format!("{}.254", ctx.cfg.net_prefix());
    ctx.ip_run(&[
        "route", "replace", &hn, "via", &via, "dev", XR_IF, "table", HS_TABLE,
    ]);
    ctx.ip_run(&[
        "rule", "add", "pref", HS_PREF4, "to", &hn, "lookup", HS_TABLE,
    ]);
}

pub fn hs_bridge(ctx: &NetCtx<'_>, iface: &str) -> bool {
    let out4 = ctx.ip(&["-4", "-o", "addr", "show", "dev", iface]);
    let cidr = out4
        .stdout
        .lines()
        .next()
        .and_then(|route_line| route_line.split_whitespace().nth(3))
        .unwrap_or("")
        .to_owned();
    if cidr.is_empty() {
        dlog(
            ctx.env,
            ctx.cfg,
            "hotspot",
            &format!("{iface} 还没有 IPv4 地址，暂不桥接"),
        );
        return false;
    }
    let gw = hs_gw_cidr(ctx, iface, &cidr);
    let alias = if gw != cidr && !cidr::overlap(&gw, &cidr) {
        format!("+{cidr}")
    } else {
        String::new()
    };
    let out6 = ctx.ip(&["-6", "-o", "addr", "show", "dev", iface, "scope", "link"]);
    let ll = out6
        .stdout
        .lines()
        .next()
        .and_then(|entry_line| entry_line.split_whitespace().nth(3))
        .unwrap_or("")
        .to_owned();
    let neigh_out = ctx.ip(&["-4", "neigh", "show", "dev", iface]);
    let clients: Vec<String> = neigh_out
        .stdout
        .lines()
        .filter(|row| !row.contains("FAILED") && !row.contains("INCOMPLETE"))
        .filter_map(|line| line.split_whitespace().next())
        .filter(|ip| !ip.is_empty() && ip.bytes().all(|byte| byte.is_ascii_digit() || byte == b'.'))
        .map(str::to_owned)
        .collect();
    let cl_str = clients.join(" ");
    let alias_note = if alias.is_empty() {
        String::new()
    } else {
        format!("（保留 {cidr} 给已连设备）")
    };
    dlog(
        ctx.env,
        ctx.cfg,
        "hotspot",
        &format!(
            "准备桥接 {iface}：安卓网关 {cidr}，OpenWrt 网关 {gw}{alias_note}，链路本地 {}，已知设备 {}",
            if ll.is_empty() { "无" } else { &ll },
            if cl_str.is_empty() { "无" } else { &cl_str }
        ),
    );

    let cmd = format!("on {gw} {alias} {cl_str}");
    let run = ctx.env.run_dir();
    if !hs_guest_cmd(ctx.env, &cmd, 15) {
        let resp = fs::read_to_string(run.join("guest/guest/hotspot"))
            .unwrap_or_else(|_| "无响应".to_owned());
        log::err(
            ctx.env,
            "hotspot",
            &format!(
                "OpenWrt 未接管热点网段 {gw}（{}），热点保持由安卓分配",
                resp.trim()
            ),
        );
        fsx::write_logged(
            ctx.env,
            run.join("net.hs_fail"),
            format!("{iface} {cidr} {gw}\n"),
        );
        return false;
    }

    if !ctx.ip(&["link", "show", HS_BR]).success()
        && !ctx.ip(&["link", "add", HS_BR, "type", "bridge"]).success()
    {
        log::err(ctx.env, "hotspot", "创建网桥失败");
        if !hs_guest_cmd(ctx.env, "off", 5) {
            dlog(ctx.env, ctx.cfg, "hotspot", "客机 5 秒内未确认关闭热点");
        }
        return false;
    }
    ctx.ip_run(&[
        "link",
        "set",
        HS_BR,
        "type",
        "bridge",
        "forward_delay",
        "0",
        "stp_state",
        "0",
    ]);
    let proc = &ctx.env.proc_dir;
    ctx.sysw_run(
        &proc.join(format!("sys/net/ipv4/conf/{HS_BR}/arp_ignore")),
        "1",
    );
    ctx.sysw_run(
        &proc.join(format!("sys/net/ipv4/conf/{HS_BR}/rp_filter")),
        "0",
    );
    ctx.sysw_run(
        &proc.join(format!("sys/net/ipv6/conf/{HS_BR}/accept_ra")),
        "0",
    );
    ctx.sysw_run(
        &proc.join(format!("sys/net/ipv6/conf/{HS_BR}/disable_ipv6")),
        "0",
    );
    ctx.ip_run(&["link", "set", HS_BR, "up"]);
    ctx.ip_run(&["link", "set", HS_TAP, "master", HS_BR]);
    if !ctx.ip(&["link", "set", iface, "master", HS_BR]).success() {
        log::err(ctx.env, "hotspot", &format!("{iface} 无法加入网桥"));
        hs_unbridge(ctx);
        return false;
    }
    ctx.sysw_run(
        &proc.join(format!("sys/net/ipv6/conf/{HS_BR}/forwarding")),
        "1",
    );
    if !ll.is_empty() {
        ctx.ip6_run(&["addr", "replace", &ll, "dev", HS_BR, "nodad"]);
    }
    hs_rules(ctx, iface);
    hs_route4(ctx, &gw, &cidr);
    ctx.ip6t_run(&[
        "-I",
        "XR_FWD6",
        "1",
        "-i",
        HS_BR,
        "-j",
        "REJECT",
        "--reject-with",
        "icmp6-adm-prohibited",
    ]);
    ctx.ip6t_run(&[
        "-I",
        "XR_FWD6",
        "1",
        "-i",
        HS_BR,
        "-p",
        "tcp",
        "-j",
        "REJECT",
        "--reject-with",
        "tcp-reset",
    ]);
    v6_tether_offload_off(ctx);
    fsx::write_logged(ctx.env, run.join("net.hs_if"), format!("{iface}\n"));
    fsx::write_logged(ctx.env, run.join("net.hs_cidr"), format!("{cidr}\n"));
    fsx::write_logged(ctx.env, run.join("net.hs_gw"), format!("{gw}\n"));
    fsx::remove_logged(ctx.env, run.join("net.hs_fail"));
    if gw == cidr {
        log::info(
            ctx.env,
            "hotspot",
            &format!("热点 {iface} 已桥接到 OpenWrt（网关 {cidr} 由 OpenWrt 接管）"),
        );
    } else {
        log::info(
            ctx.env,
            "hotspot",
            &format!(
                "热点 {iface} 已桥接到 OpenWrt（固定网段 {gw}，安卓原网段 {cidr} 保留到设备续租）"
            ),
        );
    }
    trigger_diag_snap_bg(ctx.env, ctx.cfg, &format!("热点桥接 {iface}"));
    true
}

pub fn hs_unbridge(ctx: &NetCtx<'_>) {
    let run = ctx.env.run_dir();
    let iface = fs::read_to_string(run.join("net.hs_if"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    let cidr = fs::read_to_string(run.join("net.hs_cidr"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    if iface.is_empty() && !ctx.env.sysnet.join(HS_BR).is_dir() {
        fsx::remove_logged(ctx.env, run.join("net.hs_fail"));
        return;
    }
    hs_rules_clear(ctx);
    while ctx
        .ip6t(&[
            "-D",
            "XR_FWD6",
            "-i",
            HS_BR,
            "-p",
            "tcp",
            "-j",
            "REJECT",
            "--reject-with",
            "tcp-reset",
        ])
        .success()
    {}
    while ctx
        .ip6t(&[
            "-D",
            "XR_FWD6",
            "-i",
            HS_BR,
            "-j",
            "REJECT",
            "--reject-with",
            "icmp6-adm-prohibited",
        ])
        .success()
    {}
    if !iface.is_empty() && hs_master(ctx.env, &iface) == HS_BR {
        ctx.ip_run(&["link", "set", &iface, "nomaster"]);
    }
    if hs_master(ctx.env, HS_TAP) == HS_BR {
        ctx.ip_run(&["link", "set", HS_TAP, "nomaster"]);
    }
    ctx.ip_run(&["link", "del", HS_BR]);
    if vm_alive(ctx.env) && hs_guest_ok(ctx.env) && !hs_guest_cmd(ctx.env, "off", 5) {
        dlog(
            ctx.env,
            ctx.cfg,
            "hotspot",
            "客机 5 秒内未确认关闭热点，继续拆桥",
        );
    }
    if !iface.is_empty()
        && !cidr.is_empty()
        && ctx.env.sysnet.join(&iface).is_dir()
        && is_executable(&ctx.env.bb)
    {
        let ip_only = cidr.split('/').next().unwrap_or(&cidr);
        let child = Command::new(&ctx.env.bb)
            .args([
                "arping", "-U", "-c", "2", "-I", &iface, "-s", ip_only, ip_only,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        note_spawn(ctx.env, "arping", child);
    }
    fsx::remove_logged(ctx.env, run.join("net.hs_if"));
    fsx::remove_logged(ctx.env, run.join("net.hs_cidr"));
    fsx::remove_logged(ctx.env, run.join("net.hs_gw"));
    fsx::remove_logged(ctx.env, run.join("net.hs_fail"));
    if !iface.is_empty() {
        log::info(ctx.env, "hotspot", &format!("热点 {iface} 已交还安卓"));
    }
    trigger_diag_snap_bg(ctx.env, ctx.cfg, &format!("热点拆桥 {iface}"));
}

pub fn hs_sync(ctx: &NetCtx<'_>) {
    let run = ctx.env.run_dir();
    let mut want = None;
    if hs_want(ctx) {
        if hs_guest_ok(ctx.env) {
            want = hs_pick(ctx);
        } else if !run.join("net.hs_old").is_file() {
            log::info(
                ctx.env,
                "hotspot",
                "当前系统盘不支持热点接管（请重置系统盘或使用新版完整包），热点保持由安卓分配",
            );
            fsx::touch_logged(ctx.env, run.join("net.hs_old"));
        }
    }
    let have = fs::read_to_string(run.join("net.hs_if"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    let Some(want_if) = want else {
        if !have.is_empty() || ctx.env.sysnet.join(HS_BR).is_dir() {
            let st = fs::read_to_string(run.join("state")).unwrap_or_else(|_| "未运行".to_owned());
            let tethers = tether_ifaces(ctx).join(" ");
            dlog(
                ctx.env,
                ctx.cfg,
                "hotspot",
                &format!(
                    "拆桥：选项={} 虚拟机={} 热点={tethers}",
                    i32::from(ctx.cfg.hotspot_bridge()),
                    st.trim()
                ),
            );
            hs_unbridge(ctx);
        }
        return;
    };

    let out4 = ctx.ip(&["-4", "-o", "addr", "show", "dev", &want_if]);
    let mut cidr = out4
        .stdout
        .lines()
        .next()
        .and_then(|row| row.split_whitespace().nth(3))
        .unwrap_or("")
        .to_owned();
    let hc = fs::read_to_string(run.join("net.hs_cidr"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    let hg = fs::read_to_string(run.join("net.hs_gw"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    if cidr.is_empty() && want_if == have {
        cidr.clone_from(&hc);
    }
    let gw = hs_gw_cidr(ctx, &want_if, &cidr);
    let hg_eff = if hg.is_empty() { &hc } else { &hg };
    if want_if == have
        && hs_master(ctx.env, &want_if) == HS_BR
        && hs_master(ctx.env, HS_TAP) == HS_BR
        && cidr == hc
        && gw == *hg_eff
    {
        if !ctx.ipt(&["-C", "OUTPUT", "-j", "XR_HS"]).success()
            || !ctx.ip6t(&["-C", "OUTPUT", "-j", "XR_HS6"]).success()
        {
            hs_rules(ctx, &want_if);
        }
        if gw != cidr
            && !ctx
                .ip(&["rule"])
                .stdout
                .lines()
                .any(|line| line.starts_with(&format!("{HS_PREF4}:")))
        {
            hs_route4(ctx, &gw, &cidr);
        }
        return;
    }

    let fail_sig = format!("{want_if} {cidr} {gw}");
    if fs::read_to_string(run.join("net.hs_fail")).is_ok_and(|text| text.trim() == fail_sig.trim())
    {
        return;
    }
    if !have.is_empty() {
        dlog(
            ctx.env,
            ctx.cfg,
            "hotspot",
            &format!("重新桥接：{have}({hc} -> {hg_eff}) -> {want_if}({cidr} -> {gw})"),
        );
        hs_unbridge(ctx);
    }
    if !hs_bridge(ctx, &want_if) {
        // 失败原因已由 hs_bridge 写入 module.log；下次 sync 会重新尝试
        dlog(
            ctx.env,
            ctx.cfg,
            "hotspot",
            &format!("{want_if} 本次未桥接"),
        );
    }
}

pub fn hs_fwd6_rules(ctx: &NetCtx<'_>) {
    if !ctx.env.run_dir().join("net.hs_if").is_file() {
        return;
    }
    ctx.ip6t_run(&[
        "-A",
        "XR_FWD6",
        "-i",
        HS_BR,
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
        HS_BR,
        "-j",
        "REJECT",
        "--reject-with",
        "icmp6-adm-prohibited",
    ]);
}
