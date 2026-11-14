//! 热点接管：把安卓热点接口桥接进虚拟机（`xr1` + `xrbr1`），由客机 `OpenWrt` 负责 DHCP 与路由，并维护相应的规则。

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

/// 热点固定网段与 `IPv6` 默认路由使用的路由表号。
const HS_TABLE: &str = "234";
/// 热点网桥 `IPv6` 规则（`iif xrbr1` 查热点表）的优先级。
const HS_PREF: &str = "19050";
/// 热点固定网段 `IPv4` 规则（目的为热点网段查热点表）的优先级。
const HS_PREF4: &str = "4790";

/// 虚拟机在运行时返回 true：`run/crosvm.pid` 的进程存在，且命令行（能读到时）含有 `crosvm`。
#[must_use]
pub(crate) fn vm_alive(env: &Env) -> bool {
    let pid = fs::read_to_string(env.run_dir().join("crosvm.pid")).unwrap_or_default();
    let trimmed = pid.trim();
    if !is_pid_running(env, trimmed) {
        return false;
    }
    proc_cmdline(env, trimmed).is_none_or(|cmdline| cmdline.contains("crosvm"))
}

/// 虚拟机在运行且状态为 `ready` 或 `stable` 时返回 true。
#[must_use]
pub(crate) fn vm_is_ready(env: &Env) -> bool {
    if !vm_alive(env) {
        return false;
    }
    let st = fs::read_to_string(env.run_dir().join("state")).unwrap_or_default();
    matches!(st.trim(), "ready" | "stable")
}

/// 创建（不存在时）并启用热点 tap 网卡 `xr1`，清空其地址并关闭其 `IPv6`；创建失败时记 ERROR 返回 false。
#[must_use]
pub(crate) fn hs_tap_up(ctx: &NetCtx<'_>) -> bool {
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

/// 撤销桥接并删除热点 tap 网卡 `xr1`。
pub(crate) fn hs_tap_down(ctx: &NetCtx<'_>) {
    hs_unbridge(ctx);
    ctx.ip_run(&["link", "del", HS_TAP]);
}

/// 接口所属网桥名（`/sys/class/net/<接口>/master` 的链接目标），不属于网桥时返回空串。
#[must_use]
pub(crate) fn hs_master(env: &Env, iface: &str) -> String {
    fs::read_link(env.sysnet.join(iface).join("master"))
        .ok()
        .and_then(|path| path.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_default()
}

/// 挑选要接管的热点接口：先看上次接管的接口，再看共享网络接口；要求存在、是 Wi-Fi（或设置了 `XR_HS_ANY`）且未处于 down。
#[must_use]
pub(crate) fn hs_pick(ctx: &NetCtx<'_>) -> Option<String> {
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

/// 客机已上报支持热点桥接（`guest/guest/hotspot` 存在）时返回 true。
#[must_use]
pub(crate) fn hs_guest_ok(env: &Env) -> bool {
    env.run_dir().join("guest/guest/hotspot").is_file()
}

/// 是否应接管热点：配置允许桥接、虚拟机有网卡、已就绪且 `xr1` 已存在。
#[must_use]
pub(crate) fn hs_want(ctx: &NetCtx<'_>) -> bool {
    ctx.cfg.hotspot_bridge()
        && ctx.cfg.vm_net()
        && vm_is_ready(ctx.env)
        && ctx.env.sysnet.join(HS_TAP).is_dir()
}

/// 向客机下发热点命令（写 `guest/host/hotspot.cmd`），在 `timeout_sec` 秒内等到客机回写同样的前两个词时返回 true。
#[must_use]
pub(crate) fn hs_guest_cmd(env: &Env, cmd: &str, timeout_sec: u64) -> bool {
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

/// 热点接管期间的防护规则：阻止手机自身向热点接口发 DHCP、RA、DHCPv6 应答，并让网桥入站的 `IPv6` 走 `xr0`。
pub(crate) fn hs_rules(ctx: &NetCtx<'_>, iface: &str) {
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

/// 删除热点相关的路由表、策略规则和 `XR_HS`/`XR_HS6` 链。
pub(crate) fn hs_rules_clear(ctx: &NetCtx<'_>) {
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

/// 决定热点网段：设置了固定网段且不冲突时用它；冲突时记一次 ERROR 并沿用安卓分配的网段。
#[must_use]
pub(crate) fn hs_gw_cidr(ctx: &NetCtx<'_>, iface: &str, android_cidr: &str) -> String {
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

/// 热点网段与安卓分配的不同时，加一条"去往热点网段经虚拟机"的路由和规则；相同时只清理旧规则。
pub(crate) fn hs_route4(ctx: &NetCtx<'_>, gw: &str, android_cidr: &str) {
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

/// `ip -o addr show` 输出第一行的第 4 列（地址/前缀），没有时为空串。
fn first_addr(stdout: &str) -> String {
    stdout
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(3))
        .unwrap_or("")
        .to_owned()
}

/// 热点接口当前的地址信息与桥接参数。
struct HsPlan {
    /// 安卓给热点分配的网关地址（`IPv4/前缀`）。
    cidr: String,
    /// `OpenWrt` 接管后使用的网关地址。
    gw: String,
    /// 需要为已连设备保留安卓原网段时为 `+<cidr>`，否则为空。
    alias: String,
    /// 热点接口的链路本地 `IPv6` 地址，没有时为空。
    ll: String,
    /// 邻居表里已知设备的 `IPv4` 地址，以空格分隔。
    clients: String,
}

/// 热点接口邻居表中可达设备的 `IPv4` 地址（排除 `FAILED`、`INCOMPLETE`），以空格连接。
fn hs_clients(ctx: &NetCtx<'_>, iface: &str) -> String {
    let neigh_out = ctx.ip(&["-4", "neigh", "show", "dev", iface]);
    let clients: Vec<&str> = neigh_out
        .stdout
        .lines()
        .filter(|row| !row.contains("FAILED") && !row.contains("INCOMPLETE"))
        .filter_map(|line| line.split_whitespace().next())
        .filter(|ip| !ip.is_empty() && ip.bytes().all(|byte| byte.is_ascii_digit() || byte == b'.'))
        .collect();
    clients.join(" ")
}

/// 读取热点接口的 `IPv4` 地址、计算 `OpenWrt` 网关、链路本地地址与已知设备并记调试日志；接口还没有 `IPv4` 地址时返回 `None`。
fn hs_probe(ctx: &NetCtx<'_>, iface: &str) -> Option<HsPlan> {
    let cidr = first_addr(&ctx.ip(&["-4", "-o", "addr", "show", "dev", iface]).stdout);
    if cidr.is_empty() {
        dlog(
            ctx.env,
            ctx.cfg,
            "hotspot",
            &format!("{iface} 还没有 IPv4 地址，暂不桥接"),
        );
        return None;
    }
    let gw = hs_gw_cidr(ctx, iface, &cidr);
    let alias = if gw != cidr && !cidr::overlap(&gw, &cidr) {
        format!("+{cidr}")
    } else {
        String::new()
    };
    let ll = first_addr(
        &ctx.ip(&["-6", "-o", "addr", "show", "dev", iface, "scope", "link"])
            .stdout,
    );
    let clients = hs_clients(ctx, iface);
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
            if clients.is_empty() { "无" } else { &clients }
        ),
    );
    Some(HsPlan {
        cidr,
        gw,
        alias,
        ll,
        clients,
    })
}

/// 通知客机接管热点网段；15 秒内未确认时记 ERROR、写 `run/net.hs_fail` 并返回 false。
fn hs_guest_on(ctx: &NetCtx<'_>, iface: &str, plan: &HsPlan) -> bool {
    let cmd = format!("on {} {} {}", plan.gw, plan.alias, plan.clients);
    if hs_guest_cmd(ctx.env, &cmd, 15) {
        return true;
    }
    let run = ctx.env.run_dir();
    let resp =
        fs::read_to_string(run.join("guest/guest/hotspot")).unwrap_or_else(|_| "无响应".to_owned());
    log::err(
        ctx.env,
        "hotspot",
        &format!(
            "OpenWrt 未接管热点网段 {}（{}），热点保持由安卓分配",
            plan.gw,
            resp.trim()
        ),
    );
    fsx::write_logged(
        ctx.env,
        run.join("net.hs_fail"),
        format!("{iface} {} {}\n", plan.cidr, plan.gw),
    );
    false
}

/// 确保网桥存在并设置参数、`sysctl` 后启用，把 `xr1` 挂上；网桥建不出来时记 ERROR、通知客机关闭并返回 false。
fn hs_make_bridge(ctx: &NetCtx<'_>) -> bool {
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
    for (family, name, value) in [
        ("ipv4", "arp_ignore", "1"),
        ("ipv4", "rp_filter", "0"),
        ("ipv6", "accept_ra", "0"),
        ("ipv6", "disable_ipv6", "0"),
    ] {
        ctx.sysw_run(
            &proc.join(format!("sys/net/{family}/conf/{HS_BR}/{name}")),
            value,
        );
    }
    ctx.ip_run(&["link", "set", HS_BR, "up"]);
    ctx.ip_run(&["link", "set", HS_TAP, "master", HS_BR]);
    true
}

/// 桥接成功后的收尾：开启网桥 `IPv6` 转发、补链路本地地址、装规则与路由、拒绝网桥 `IPv6` 转发、关闭网络共享卸载，写 `run/net.hs_*` 并记日志。
fn hs_bridge_done(ctx: &NetCtx<'_>, iface: &str, plan: &HsPlan) {
    let run = ctx.env.run_dir();
    let (cidr, gw) = (&plan.cidr, &plan.gw);
    ctx.sysw_run(
        &ctx.env
            .proc_dir
            .join(format!("sys/net/ipv6/conf/{HS_BR}/forwarding")),
        "1",
    );
    if !plan.ll.is_empty() {
        ctx.ip6_run(&["addr", "replace", &plan.ll, "dev", HS_BR, "nodad"]);
    }
    hs_rules(ctx, iface);
    hs_route4(ctx, gw, cidr);
    ctx.ip6t_run(&hs_reject6(&["-I", "XR_FWD6", "1"], false));
    ctx.ip6t_run(&hs_reject6(&["-I", "XR_FWD6", "1"], true));
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
}

/// 把热点接口 `iface` 桥接进虚拟机：读取安卓分配的地址、建网桥、交给客机接管 DHCP，成功后写 `run/net.hs_*` 并返回 true。
///
/// 任一步失败时记 ERROR、写 `run/net.hs_fail` 并回退，返回 false。
pub(crate) fn hs_bridge(ctx: &NetCtx<'_>, iface: &str) -> bool {
    let Some(plan) = hs_probe(ctx, iface) else {
        return false;
    };
    if !hs_guest_on(ctx, iface, &plan) || !hs_make_bridge(ctx) {
        return false;
    }
    if !ctx.ip(&["link", "set", iface, "master", HS_BR]).success() {
        log::err(ctx.env, "hotspot", &format!("{iface} 无法加入网桥"));
        hs_unbridge(ctx);
        return false;
    }
    hs_bridge_done(ctx, iface, &plan);
    true
}

/// 拼出热点网桥的 `IPv6` 拒绝规则参数：`head` 之后接 `-i xrbr1`，`tcp` 时回 `tcp-reset`，否则回 `icmp6-adm-prohibited`。
fn hs_reject6<'a>(head: &[&'a str], tcp: bool) -> Vec<&'a str> {
    let mut args = head.to_vec();
    args.extend(["-i", HS_BR]);
    if tcp {
        args.extend(["-p", "tcp", "-j", "REJECT", "--reject-with", "tcp-reset"]);
    } else {
        args.extend(["-j", "REJECT", "--reject-with", "icmp6-adm-prohibited"]);
    }
    args
}

/// 把热点接口 `iface`（非空时）和 `xr1` 移出网桥，再删除网桥。
fn hs_detach(ctx: &NetCtx<'_>, iface: &str) {
    if !iface.is_empty() && hs_master(ctx.env, iface) == HS_BR {
        ctx.ip_run(&["link", "set", iface, "nomaster"]);
    }
    if hs_master(ctx.env, HS_TAP) == HS_BR {
        ctx.ip_run(&["link", "set", HS_TAP, "nomaster"]);
    }
    ctx.ip_run(&["link", "del", HS_BR]);
}

/// 接口仍在且 `busybox` 可用时，后台发两次免费 `ARP`，让已连设备尽快刷新安卓原网关 `cidr` 的地址。
fn hs_arp_announce(ctx: &NetCtx<'_>, iface: &str, cidr: &str) {
    if iface.is_empty()
        || cidr.is_empty()
        || !ctx.env.sysnet.join(iface).is_dir()
        || !is_executable(&ctx.env.bb)
    {
        return;
    }
    let ip_only = cidr.split('/').next().unwrap_or(cidr);
    let child = Command::new(&ctx.env.bb)
        .args([
            "arping", "-U", "-c", "2", "-I", iface, "-s", ip_only, ip_only,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    note_spawn(ctx.env, "arping", child);
}

/// 删除 `run/net.hs_if`、`net.hs_cidr`、`net.hs_gw` 与 `net.hs_fail`。
fn hs_clear_state(ctx: &NetCtx<'_>) {
    let run = ctx.env.run_dir();
    for name in ["net.hs_if", "net.hs_cidr", "net.hs_gw", "net.hs_fail"] {
        fsx::remove_logged(ctx.env, run.join(name));
    }
}

/// 撤销热点桥接：通知客机停止、把接口移出网桥并恢复安卓原地址，删除规则与 `run/net.hs_*` 状态文件。
pub(crate) fn hs_unbridge(ctx: &NetCtx<'_>) {
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
    while ctx.ip6t(&hs_reject6(&["-D", "XR_FWD6"], true)).success() {}
    while ctx.ip6t(&hs_reject6(&["-D", "XR_FWD6"], false)).success() {}
    hs_detach(ctx, &iface);
    if vm_alive(ctx.env) && hs_guest_ok(ctx.env) && !hs_guest_cmd(ctx.env, "off", 5) {
        dlog(
            ctx.env,
            ctx.cfg,
            "hotspot",
            "客机 5 秒内未确认关闭热点，继续拆桥",
        );
    }
    hs_arp_announce(ctx, &iface, &cidr);
    hs_clear_state(ctx);
    if !iface.is_empty() {
        log::info(ctx.env, "hotspot", &format!("热点 {iface} 已交还安卓"));
    }
    trigger_diag_snap_bg(ctx.env, ctx.cfg, &format!("热点拆桥 {iface}"));
}

/// 当前应接管的热点接口：选项开启且条件满足时由 `hs_pick` 选出；客机不支持热点时只首次记一次 INFO，返回 `None`。
fn hs_target(ctx: &NetCtx<'_>) -> Option<String> {
    if !hs_want(ctx) {
        return None;
    }
    if hs_guest_ok(ctx.env) {
        return hs_pick(ctx);
    }
    let old_mark = ctx.env.run_dir().join("net.hs_old");
    if !old_mark.is_file() {
        log::info(
            ctx.env,
            "hotspot",
            "当前系统盘不支持热点接管（请重置系统盘或使用新版完整包），热点保持由安卓分配",
        );
        fsx::touch_logged(ctx.env, old_mark);
    }
    // 客体不支持且已提示过一次：不重复记日志
    None
}

/// 不再需要接管时：已接管或网桥仍在则记调试日志并拆桥。
fn hs_release(ctx: &NetCtx<'_>, have: &str) {
    if have.is_empty() && !ctx.env.sysnet.join(HS_BR).is_dir() {
        return;
    }
    let st =
        fs::read_to_string(ctx.env.run_dir().join("state")).unwrap_or_else(|_| "未运行".to_owned());
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

/// 已按当前参数桥接时只补规则：`XR_HS`/`XR_HS6` 钩子缺失则重装，固定网段时 `4790` 规则缺失则重装路由。
fn hs_reassert(ctx: &NetCtx<'_>, want_if: &str, cidr: &str, gw: &str) {
    if !ctx.ipt(&["-C", "OUTPUT", "-j", "XR_HS"]).success()
        || !ctx.ip6t(&["-C", "OUTPUT", "-j", "XR_HS6"]).success()
    {
        hs_rules(ctx, want_if);
    }
    if gw != cidr
        && !ctx
            .ip(&["rule"])
            .stdout
            .lines()
            .any(|line| line.starts_with(&format!("{HS_PREF4}:")))
    {
        hs_route4(ctx, gw, cidr);
    }
}

/// 热点对账：按当前条件决定应接管的接口，与已接管的不同时先撤销再桥接，必要时记日志。
pub(crate) fn hs_sync(ctx: &NetCtx<'_>) {
    let run = ctx.env.run_dir();
    let want = hs_target(ctx);
    let read = |name: &str| {
        fs::read_to_string(run.join(name))
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let have = read("net.hs_if");
    let Some(want_if) = want else {
        hs_release(ctx, &have);
        return;
    };

    let mut cidr = first_addr(
        &ctx.ip(&["-4", "-o", "addr", "show", "dev", &want_if])
            .stdout,
    );
    let hc = read("net.hs_cidr");
    let hg = read("net.hs_gw");
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
        hs_reassert(ctx, &want_if, &cidr, &gw);
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

/// 已接管热点时，在 `XR_FWD6` 中拒绝从热点网桥进入的 `IPv6` 转发（TCP 回 RST，其余回 `ICMPv6` 禁止）。
pub(crate) fn hs_fwd6_rules(ctx: &NetCtx<'_>) {
    if !ctx.env.run_dir().join("net.hs_if").is_file() {
        return;
    }
    ctx.ip6t_run(&hs_reject6(&["-A", "XR_FWD6"], true));
    ctx.ip6t_run(&hs_reject6(&["-A", "XR_FWD6"], false));
}
