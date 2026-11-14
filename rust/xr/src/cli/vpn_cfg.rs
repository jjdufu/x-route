//! `VPN` 配置管理：多配置的导入、启用停用与删除，隧道和配置列表，旧版单文件配置的迁移与镜像，以及分流流量查询和目录浏览。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use crate::cidr::norm_route;
use crate::env::Env;
use crate::log;
use crate::log::{dlog_env, unix_now};
use crate::net::vpn_routes::is_default_route;
use crate::sys::fsx;
use crate::vm;
use crate::vm::vpn_sync_host;

/// 由显示名生成配置编号：只保留字母、数字和下划线（连续的其它字符合并为一个下划线），最长 32 字节；结果为空时用名字的哈希生成 `p_<数字>`。
#[must_use]
pub(crate) fn vpn_slug(name: &str) -> String {
    let mut out = String::new();
    let mut last_us = false;
    for octet in name.bytes() {
        if octet.is_ascii_alphanumeric() || octet == b'_' {
            out.push(char::from(octet));
            last_us = false;
        } else if !last_us && !out.is_empty() {
            out.push('_');
            last_us = true;
        } else {
            // 连续的非法字符只保留一个下划线；开头的非法字符直接丢弃
        }
        if out.len() >= 32 {
            break;
        }
    }
    let trimmed = out.trim_matches('_').to_owned();
    if trimmed.is_empty() {
        let mut hash = 0_u32;
        for byte in name.as_bytes() {
            hash = hash.wrapping_mul(31).wrapping_add(u32::from(*byte));
        }
        format!("p_{hash}")
    } else {
        trimmed
    }
}

/// 没有多配置目录但有旧版单文件 `config/openvpn.ovpn` 时，把它迁移为多配置目录里的一份配置（保留名字与启用状态）。
pub(crate) fn vpn_migrate_legacy(env: &Env) {
    let vpn_dir = env.vpn_dir();
    let legacy = env.xr_data.join("config/openvpn.ovpn");
    if !vpn_dir.is_dir() && fs::metadata(&legacy).is_ok_and(|meta| meta.len() > 0) {
        fsx::mkdirs_logged(env, &vpn_dir);
        let mn = fs::read_to_string(env.xr_data.join("config/openvpn.name"))
            .unwrap_or_default()
            .trim()
            .to_owned();
        let disp = if mn.is_empty() {
            "OpenVPN".to_owned()
        } else {
            mn
        };
        let mid = vpn_slug(&disp);
        let dst_ovpn = vpn_dir.join(format!("{mid}.ovpn"));
        fsx::copy_logged(env, &legacy, &dst_ovpn);
        #[cfg(unix)]
        {
            fsx::chmod_logged(env, &dst_ovpn, 0o600);
        }
        fsx::write_logged(
            env,
            vpn_dir.join(format!("{mid}.name")),
            format!("{disp}\n"),
        );
        let men = fs::read_to_string(env.xr_data.join("config/openvpn.enabled"))
            .map_or("1\n", |text| if text.trim() == "0" { "0\n" } else { "1\n" });
        fsx::write_logged(env, vpn_dir.join(format!("{mid}.enabled")), men);
    }
}

/// 把多配置目录中已启用（没有则第一个）的配置镜像回旧版单文件 `config/openvpn.*`，没有配置时删除旧版文件。
pub(crate) fn vpn_mirror_legacy(env: &Env) {
    let vpn_dir = env.vpn_dir();
    if !vpn_dir.is_dir() {
        return;
    }
    let mut first_id: Option<String> = None;
    let mut en_id: Option<String> = None;
    if let Ok(rd) = fs::read_dir(&vpn_dir) {
        let mut paths: Vec<_> = rd.flatten().map(|entry| entry.path()).collect();
        paths.sort();
        for file_path in paths {
            if file_path.extension().and_then(|raw_name| raw_name.to_str()) != Some("ovpn") {
                continue;
            }
            if fs::metadata(&file_path).map_or(true, |meta| meta.len() == 0) {
                continue;
            }
            let Some(pid) = file_path.file_stem().and_then(|os_str| os_str.to_str()) else {
                continue;
            };
            if first_id.is_none() {
                first_id = Some(pid.to_owned());
            }
            if en_id.is_none()
                && fs::read_to_string(vpn_dir.join(format!("{pid}.enabled")))
                    .is_ok_and(|text| text.trim() == "1")
            {
                en_id = Some(pid.to_owned());
            }
        }
    }
    let pick = en_id.clone().or(first_id);
    let cfg_dir = env.xr_data.join("config");
    if let Some(pid) = pick {
        fsx::mkdirs_logged(env, &cfg_dir);
        let dst = cfg_dir.join("openvpn.ovpn");
        fsx::copy_logged(env, vpn_dir.join(format!("{pid}.ovpn")), &dst);
        #[cfg(unix)]
        {
            fsx::chmod_logged(env, &dst, 0o600);
        }
        let nm = fs::read_to_string(vpn_dir.join(format!("{pid}.name"))).unwrap_or(pid);
        fsx::write_logged(
            env,
            cfg_dir.join("openvpn.name"),
            format!("{}\n", nm.trim()),
        );
        fsx::write_logged(
            env,
            cfg_dir.join("openvpn.enabled"),
            if en_id.is_some() { "1\n" } else { "0\n" },
        );
    } else {
        fsx::remove_logged(env, cfg_dir.join("openvpn.ovpn"));
        fsx::remove_logged(env, cfg_dir.join("openvpn.name"));
        fsx::remove_logged(env, cfg_dir.join("openvpn.enabled"));
    }
}

/// 客机推送了但手机侧未生效的 `VPN` 路由（规范化后不在 `routes.active` 中，忽略默认路由）；虚拟机不在时为空。
pub(crate) fn vpn_rejected(env: &Env) -> Vec<String> {
    if !vm::running(env) {
        return Vec::new();
    }
    let act_raw = fs::read_to_string(env.run_dir().join("routes.active")).unwrap_or_default();
    let act_set: BTreeSet<&str> = act_raw.lines().map(str::trim).collect();
    let gf = env.run_dir().join("guest/guest/openvpn.routes");
    let Ok(raw) = fs::read_to_string(gf) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for tok in raw.split(|ch: char| ch == ',' || ch.is_whitespace()) {
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
            None => out.push(tok.to_owned()),
        }
    }
    out
}

/// 隧道显示名：旧版单份配置的 `xrouter` 隧道用 `openvpn.name`（非空时），其余用 `config/vpn/<名>.name`，都没有时用原名。
fn tunnel_name(env: &Env, raw_nm: &str, vn: &str) -> String {
    if raw_nm == "xrouter" && !vn.is_empty() {
        return vn.to_owned();
    }
    let nf = env.vpn_dir().join(format!("{raw_nm}.name"));
    if nf.is_file() {
        fs::read_to_string(nf)
            .unwrap_or_else(|_| raw_nm.to_owned())
            .trim()
            .to_owned()
    } else {
        raw_nm.to_owned()
    }
}

/// `openvpn.tunnels` 的一行（`设备|名字|地址|MTU|路由`）。
#[derive(Debug, Clone, Copy)]
struct TunLine<'line> {
    /// 隧道设备名。
    dev: &'line str,
    /// 隧道所属配置名（旧版单份配置为 `xrouter`）。
    name: &'line str,
    /// 隧道地址。
    addr: &'line str,
    /// `MTU`，解析失败为 0。
    mtu: u64,
    /// 路由原文（逗号或空白分隔）。
    routes: &'line str,
}

/// 解析 `openvpn.tunnels` 的一行；设备为空时返回 `None`。
fn parse_tun_line(line: &str) -> Option<TunLine<'_>> {
    let mut parts = line.splitn(5, '|');
    let dev = parts.next().unwrap_or("");
    if dev.is_empty() {
        return None;
    }
    let name = parts.next().unwrap_or("");
    let addr = parts.next().unwrap_or("");
    let mtu = parts
        .next()
        .unwrap_or("0")
        .trim()
        .parse::<u64>()
        .unwrap_or(0);
    let routes = parts.next().unwrap_or("");
    Some(TunLine {
        dev,
        name,
        addr,
        mtu,
        routes,
    })
}

/// 把路由原文按逗号或空白拆成网段列表，丢弃空项。
fn split_routes(raw: &str) -> Vec<&str> {
    raw.split(|ch: char| ch == ',' || ch.is_whitespace())
        .filter(|text| !text.is_empty())
        .collect()
}

/// `openvpn.tunnels` 的一行转成隧道 JSON；设备为空时返回 `None`。
fn tunnel_entry(env: &Env, line: &str, vn: &str) -> Option<Value> {
    let tun = parse_tun_line(line)?;
    let nm = tunnel_name(env, tun.name, vn);
    Some(json!({
        "dev": tun.dev,
        "name": nm,
        "status": "up",
        "addr": tun.addr,
        "mtu": tun.mtu,
        "routes": split_routes(tun.routes),
    }))
}

/// 没有已连接隧道时旧版单份配置的占位项：虚拟机未运行为 `down`，汇总状态为 `up` 时为 `connecting`，否则沿用汇总状态。
fn tunnel_placeholder(alive: bool, summary_st: &str, vn: &str) -> Value {
    let st = if !alive {
        "down"
    } else if summary_st == "up" {
        "connecting"
    } else {
        summary_st
    };
    let nm = if vn.is_empty() { "OpenVPN" } else { vn };
    json!({
        "dev": "",
        "name": nm,
        "status": st,
        "addr": "",
        "mtu": 0_i32,
        "routes": [],
    })
}

/// `VPN` 隧道列表（JSON）：取自客机的 `openvpn.tunnels`；没有已连接隧道但有配置时给出一条按 `summary_st` 推断状态的占位项。
#[must_use]
pub(crate) fn vpn_tunnels(env: &Env, summary_st: &str) -> Vec<Value> {
    let vn = fs::read_to_string(env.xr_data.join("config/openvpn.name"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    let mut out = Vec::new();
    let tun_file = env.run_dir().join("guest/guest/openvpn.tunnels");
    let alive = vm::running(env);
    if alive
        && fs::metadata(&tun_file).is_ok_and(|meta| meta.len() > 0)
        && let Ok(content) = fs::read_to_string(&tun_file)
    {
        out.extend(
            content
                .lines()
                .filter_map(|line| tunnel_entry(env, line, &vn)),
        );
    }
    if out.is_empty() && env.xr_data.join("config/openvpn.ovpn").is_file() {
        out.push(tunnel_placeholder(alive, summary_st, &vn));
    }
    out
}

/// 推断各份配置连接状态所需的客机上报信息。
#[derive(Debug)]
struct ProfileCtx<'sum> {
    /// 虚拟机是否在运行。
    alive: bool,
    /// `openvpn.tunnels` 原文。
    tun_raw: String,
    /// 客机处于多隧道模式（存在 `openvpn.multi`）。
    multi: bool,
    /// `openvpn.connecting` 原文（每行一个配置编号）。
    conn_raw: String,
    /// 汇总连接状态。
    summary_st: &'sum str,
}

/// 配置 `pid` 的连接状态与所用隧道：匹配到隧道（同名，或启用且非多隧道时的旧版 `xrouter`）为 `up`；
/// 否则在连接中列表里、或启用且非多隧道且汇总状态为 `up`/`connecting` 时为 `connecting`；其余为 `down`。
fn profile_link<'ctx>(
    ctx: &'ctx ProfileCtx<'_>,
    pid: &str,
    pen: bool,
) -> (&'static str, Option<TunLine<'ctx>>) {
    if !ctx.alive {
        return ("down", None);
    }
    let hit = ctx
        .tun_raw
        .lines()
        .filter_map(parse_tun_line)
        .find(|tun| tun.name == pid || (tun.name == "xrouter" && pen && !ctx.multi));
    if hit.is_some() {
        return ("up", hit);
    }
    if ctx.conn_raw.lines().any(|row| row.trim() == pid)
        || (pen && !ctx.multi && matches!(ctx.summary_st, "up" | "connecting"))
    {
        ("connecting", None)
    } else {
        ("down", None)
    }
}

/// `config/vpn/` 里的一份配置转成 JSON；不是非空 `.ovpn` 或文件名不是 UTF-8 时返回 `None`。
fn profile_entry(vpn_dir: &Path, ctx: &ProfileCtx<'_>, file_path: &Path) -> Option<Value> {
    if file_path.extension().and_then(|raw_name| raw_name.to_str()) != Some("ovpn") {
        return None;
    }
    if fs::metadata(file_path).map_or(true, |meta| meta.len() == 0) {
        return None;
    }
    let pid = file_path.file_stem().and_then(|os_str| os_str.to_str())?;
    let pnm_raw = fs::read_to_string(vpn_dir.join(format!("{pid}.name")))
        .unwrap_or_default()
        .trim()
        .to_owned();
    let pnm = if pnm_raw.is_empty() {
        pid.to_owned()
    } else {
        pnm_raw
    };
    let pen = fs::read_to_string(vpn_dir.join(format!("{pid}.enabled")))
        .is_ok_and(|raw| raw.trim() == "1");
    let (pst, tun) = profile_link(ctx, pid, pen);
    Some(json!({
        "id": pid,
        "name": pnm,
        "enabled": pen,
        "status": pst,
        "dev": tun.map_or("", |line| line.dev),
        "addr": tun.map_or("", |line| line.addr),
        "mtu": tun.map_or(0, |line| line.mtu),
        "routes": tun.map_or_else(Vec::new, |line| split_routes(line.routes)),
    }))
}

/// `VPN` 配置列表（JSON）：每份配置的编号、名字、启用状态、连接状态与对应隧道的地址、`MTU` 和路由。
#[must_use]
pub(crate) fn vpn_profiles(env: &Env, summary_st: &str) -> Vec<Value> {
    vpn_migrate_legacy(env);
    let vpn_dir = env.vpn_dir();
    if !vpn_dir.is_dir() {
        return Vec::new();
    }
    let guest = env.run_dir().join("guest/guest");
    let ctx = ProfileCtx {
        alive: vm::running(env),
        tun_raw: fs::read_to_string(guest.join("openvpn.tunnels")).unwrap_or_default(),
        multi: guest.join("openvpn.multi").is_file(),
        conn_raw: fs::read_to_string(guest.join("openvpn.connecting")).unwrap_or_default(),
        summary_st,
    };
    let Ok(rd) = fs::read_dir(&vpn_dir) else {
        return Vec::new();
    };
    let mut paths: Vec<_> = rd.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    paths
        .iter()
        .filter_map(|file_path| profile_entry(&vpn_dir, &ctx, file_path))
        .collect()
}

/// 生成下发给客机的 `openvpn.cmd` 一行（`动作 时间戳`），保证与当前内容不同，客机才会再次执行。
fn vpn_next_cmd(env: &Env, act: &str) -> String {
    let mut ts = unix_now().to_string();
    let cur = fs::read_to_string(env.run_dir().join("guest/host/openvpn.cmd")).unwrap_or_default();
    let cand = format!("{act} {ts}");
    if cur.trim() == cand {
        ts = format!("{ts}{}", std::process::id());
    }
    format!("{act} {ts}\n")
}

/// 配置编号非空且只含字母、数字和下划线时返回 true。
fn valid_profile_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// 导入前检查：路径须在内部存储下且不含 `..`、文件存在、大小在 8 字节到 256 KiB 之间、内容只含可见 `ASCII` 与空白。
///
/// # Errors
///
/// 不满足时返回给用户看的 JSON 错误应答。
fn import_source(env: &Env, path_arg: &str) -> Result<PathBuf, &'static str> {
    let src_s = env.canon_share_path(path_arg);
    let media_s = env.media.to_string_lossy();
    if !src_s.starts_with(&format!("{media_s}/")) || src_s.contains("..") {
        return Err("{\"ok\":false,\"error\":\"路径无效\"}");
    }
    let src = PathBuf::from(&src_s);
    if !src.is_file() {
        return Err("{\"ok\":false,\"error\":\"文件不存在\"}");
    }
    let sz = fs::metadata(&src).map_or(0, |meta| meta.len());
    if !(8..=262_144).contains(&sz) {
        return Err("{\"ok\":false,\"error\":\"文件大小不符\"}");
    }
    let Ok(bytes) = fs::read(&src) else {
        return Err("{\"ok\":false,\"error\":\"文件不存在\"}");
    };
    if bytes
        .iter()
        .any(|&byte| !byte.is_ascii_graphic() && !byte.is_ascii_whitespace())
    {
        return Err("{\"ok\":false,\"error\":\"不是文本配置\"}");
    }
    Ok(src)
}

/// 导入配置的编号：已有同名（`.name` 与 `bn` 相同）配置时沿用其编号，否则由名字生成编号并在重名时追加 `_2`、`_3`…。
fn import_id(vpn_dir: &Path, bn: &str) -> String {
    if let Ok(rd) = fs::read_dir(vpn_dir) {
        for ent in rd.flatten() {
            let file_path = ent.path();
            if file_path.extension().and_then(|raw_name| raw_name.to_str()) != Some("ovpn") {
                continue;
            }
            let Some(eid) = file_path.file_stem().and_then(|os_str| os_str.to_str()) else {
                continue;
            };
            let enm = fs::read_to_string(vpn_dir.join(format!("{eid}.name")))
                .unwrap_or_default()
                .trim()
                .to_owned();
            if enm == bn {
                return eid.to_owned();
            }
        }
    }
    let base = vpn_slug(bn);
    let mut cand = base.clone();
    let mut n = 2_u32;
    while vpn_dir.join(format!("{cand}.ovpn")).exists() {
        cand = format!("{base}_{n}");
        n = n.saturating_add(1);
    }
    cand
}

/// 导入配置的落盘位置。
#[derive(Debug, Clone, Copy)]
struct ImportTarget<'tgt> {
    /// 配置目录 `config/vpn/`。
    vpn_dir: &'tgt Path,
    /// 配置编号。
    id: &'tgt str,
    /// 显示名。
    bn: &'tgt str,
}

/// 写入导入的配置（`.ovpn` 权限 600、`.name`、`.enabled`）并同步旧版镜像；`enable_flag` 为 `0` 时保留原启用状态，否则启用。
///
/// 返回是否启用。
fn import_write(env: &Env, src: &Path, tgt: ImportTarget<'_>, enable_flag: &str) -> bool {
    let ImportTarget { vpn_dir, id, bn } = tgt;
    let dst_ovpn = vpn_dir.join(format!("{id}.ovpn"));
    fsx::copy_logged(env, src, &dst_ovpn);
    #[cfg(unix)]
    {
        fsx::chmod_logged(env, &dst_ovpn, 0o600);
    }
    fsx::write_logged(env, vpn_dir.join(format!("{id}.name")), format!("{bn}\n"));
    let en = if enable_flag == "0" {
        fs::read_to_string(vpn_dir.join(format!("{id}.enabled")))
            .is_ok_and(|text| text.trim() == "1")
    } else {
        true
    };
    fsx::write_logged(
        env,
        vpn_dir.join(format!("{id}.enabled")),
        if en { "1\n" } else { "0\n" },
    );
    vpn_mirror_legacy(env);
    en
}

/// `xr vpn-import <路径> [0|1]`：从内部存储导入一个 `OpenVPN` 文本配置（同名覆盖），按参数或原状态决定是否启用，运行中时同步给客机。
///
/// 返回（退出码, JSON 结果）；路径不在内部存储下、文件不存在、大小不在 8 字节到 256 KiB 之间或不是文本时返回 1。
#[must_use]
pub(crate) fn vpn_import(env: &Env, path_arg: &str, enable_flag: &str) -> (i32, String) {
    let src = match import_source(env, path_arg) {
        Ok(path) => path,
        Err(msg) => return (1, msg.to_owned()),
    };

    vpn_migrate_legacy(env);
    let vpn_dir = env.vpn_dir();
    fsx::mkdirs_logged(env, env.xr_data.join("config"));
    fsx::mkdirs_logged(env, &vpn_dir);

    let raw_bn = src
        .file_stem()
        .and_then(|ext_os| ext_os.to_str())
        .unwrap_or("OpenVPN")
        .replace(['"', '\\'], "");
    let bn = if raw_bn.is_empty() {
        "OpenVPN".to_owned()
    } else {
        raw_bn
    };
    let id = import_id(&vpn_dir, &bn);
    let tgt = ImportTarget {
        vpn_dir: &vpn_dir,
        id: &id,
        bn: &bn,
    };
    let en = import_write(env, &src, tgt, enable_flag);

    if vm::running(env) {
        if vpn_sync_host(env).is_none() {
            // 刚写入的配置目录不存在（创建失败）：客机侧本轮不更新，只记调试日志
            dlog_env(env, "vpn", "导入后同步客机配置：配置目录不存在");
        }
        if en {
            let host = env.run_dir().join("guest/host");
            fsx::copy_logged(env, &src, host.join("openvpn.ovpn"));
            fsx::write_logged(env, host.join("openvpn.cmd"), vpn_next_cmd(env, "enable"));
        }
    }
    log::info(env, "xr", "已导入 OpenVPN 配置");
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

/// `xr vpn-enable|vpn-disable [ID]`：启用或停用一份配置，同步给客机并等待其应答；不指定编号时停用全部配置，或在没有已启用配置时启用第一份。
///
/// 客机不支持多隧道时启用一份会停用其它配置；返回（退出码, JSON 结果），参数非法返回 2，配置无效、不存在、服务未运行或客机未响应返回 1。
#[must_use]
pub(crate) fn vpn_set(env: &Env, act: &str, tid: &str) -> (i32, String) {
    let want = match act {
        "enable" => true,
        "disable" => false,
        _ => {
            return (
                2,
                "{\"ok\":false,\"error\":\"用法：xr vpn-enable|vpn-disable [ID]\"}".to_owned(),
            );
        }
    };
    let vpn_dir = env.vpn_dir();
    if !tid.is_empty() {
        vpn_migrate_legacy(env);
        if !valid_profile_id(tid) {
            return (1, "{\"ok\":false,\"error\":\"配置无效\"}".to_owned());
        }
        let ovpn = vpn_dir.join(format!("{tid}.ovpn"));
        if !fs::metadata(&ovpn).is_ok_and(|ovpn_meta| ovpn_meta.len() > 0) {
            return (1, "{\"ok\":false,\"error\":\"配置不存在\"}".to_owned());
        }
        if !vm::running(env) {
            if want {
                return (1, "{\"ok\":false,\"error\":\"服务未运行\"}".to_owned());
            }
            fsx::write_logged(env, vpn_dir.join(format!("{tid}.enabled")), "0\n");
            vpn_mirror_legacy(env);
            return (0, "{\"ok\":true}".to_owned());
        }
        let multi = env.run_dir().join("guest/guest/openvpn.multi").is_file();
        if want
            && !multi
            && let Ok(rd) = fs::read_dir(&vpn_dir)
        {
            for ent in rd.flatten() {
                let target = ent.path();
                if target.extension().and_then(|ext_os| ext_os.to_str()) == Some("ovpn")
                    && let Some(oid) = target.file_stem().and_then(|stem_os| stem_os.to_str())
                {
                    fsx::write_logged(env, vpn_dir.join(format!("{oid}.enabled")), "0\n");
                }
            }
        }
        fsx::write_logged(
            env,
            vpn_dir.join(format!("{tid}.enabled")),
            if want { "1\n" } else { "0\n" },
        );
        vpn_mirror_legacy(env);
        let any = vpn_sync_host(env).unwrap_or(false);
        let host = env.run_dir().join("guest/host");
        if want {
            fsx::copy_logged(env, &ovpn, host.join("openvpn.ovpn"));
        }
        let cmd_act = if any { "enable" } else { "disable" };
        let cmd_line = vpn_next_cmd(env, cmd_act);
        fsx::write_logged(env, host.join("openvpn.cmd"), &cmd_line);
        let cmd_trim = cmd_line.trim();
        let guest = env.run_dir().join("guest/guest");
        for _ in 0..env.vpn_wait {
            let ack = fs::read_to_string(guest.join("openvpn.ack")).unwrap_or_default();
            if ack.trim() == cmd_trim {
                return (0, "{\"ok\":true}".to_owned());
            }
            let vst = fs::read_to_string(guest.join("openvpn.status"))
                .unwrap_or_default()
                .trim()
                .to_owned();
            let tun = fs::read_to_string(guest.join("openvpn.tunnels")).unwrap_or_default();
            let conn = fs::read_to_string(guest.join("openvpn.connecting")).unwrap_or_default();
            let has_tun = tun.contains(&format!("|{tid}|"));
            let has_conn = conn.lines().any(|line| line.trim() == tid);
            let is_multi = guest.join("openvpn.multi").is_file();
            if want {
                if has_tun || has_conn || (!is_multi && matches!(vst.as_str(), "up" | "connecting"))
                {
                    return (0, "{\"ok\":true}".to_owned());
                }
            } else if !has_tun && !has_conn && (is_multi || vst == "down" || vst.is_empty()) {
                return (0, "{\"ok\":true}".to_owned());
            } else {
                // 状态尚未到达目标：继续轮询
            }
            thread::sleep(Duration::from_secs(1));
        }
        return (1, "{\"ok\":false,\"error\":\"未响应\"}".to_owned());
    }

    let legacy = env.xr_data.join("config/openvpn.ovpn");
    if !legacy.is_file() {
        return (1, "{\"ok\":false,\"error\":\"未导入配置\"}".to_owned());
    }
    fsx::mkdirs_logged(env, env.xr_data.join("config"));
    fsx::write_logged(
        env,
        env.xr_data.join("config/openvpn.enabled"),
        if want { "1\n" } else { "0\n" },
    );
    if vpn_dir.is_dir()
        && let Ok(rd) = fs::read_dir(&vpn_dir)
    {
        let mut paths: Vec<_> = rd.flatten().map(|entry| entry.path()).collect();
        paths.sort();
        let mut has_en = false;
        let mut first_id: Option<String> = None;
        for file_path in &paths {
            if file_path.extension().and_then(|raw_name| raw_name.to_str()) == Some("ovpn")
                && fs::metadata(file_path).is_ok_and(|meta| meta.len() > 0)
                && let Some(vid) = file_path.file_stem().and_then(|os_str| os_str.to_str())
            {
                if first_id.is_none() {
                    first_id = Some(vid.to_owned());
                }
                if !want {
                    fsx::write_logged(env, vpn_dir.join(format!("{vid}.enabled")), "0\n");
                } else if fs::read_to_string(vpn_dir.join(format!("{vid}.enabled")))
                    .is_ok_and(|text| text.trim() == "1")
                {
                    has_en = true;
                } else {
                    // 要开启但该配置未启用：先不改，循环结束后再决定是否启用第一个配置
                }
            }
        }
        if want
            && !has_en
            && let Some(fid) = first_id
        {
            fsx::write_logged(env, vpn_dir.join(format!("{fid}.enabled")), "1\n");
        }
    }
    if !vm::running(env) {
        if want {
            return (1, "{\"ok\":false,\"error\":\"服务未运行\"}".to_owned());
        }
        return (0, "{\"ok\":true}".to_owned());
    }
    let host = env.run_dir().join("guest/host");
    fsx::mkdirs_logged(env, &host);
    if vpn_dir.is_dir() && vpn_sync_host(env).is_none() {
        // 检查与同步之间目录被删除：客机侧本轮不更新，只记调试日志
        dlog_env(env, "vpn", "切换单配置时同步客机配置：配置目录不存在");
    }
    if want {
        fsx::copy_logged(env, &legacy, host.join("openvpn.ovpn"));
    }
    fsx::write_logged(env, host.join("openvpn.cmd"), vpn_next_cmd(env, act));
    let guest = env.run_dir().join("guest/guest");
    for _ in 0..env.vpn_wait {
        let vst = fs::read_to_string(guest.join("openvpn.status"))
            .unwrap_or_default()
            .trim()
            .to_owned();
        if want {
            if matches!(vst.as_str(), "up" | "connecting") {
                return (0, "{\"ok\":true}".to_owned());
            }
        } else if vst == "down" || vst.is_empty() {
            return (0, "{\"ok\":true}".to_owned());
        } else {
            // 状态尚未到达目标：继续轮询
        }
        thread::sleep(Duration::from_secs(1));
    }
    (1, "{\"ok\":false,\"error\":\"未响应\"}".to_owned())
}

/// `xr vpn-delete <ID>`：删除一份配置，原来启用时同步给客机；返回（退出码, JSON 结果），配置无效或不存在时返回 1。
#[must_use]
pub(crate) fn vpn_delete(env: &Env, did: &str) -> (i32, String) {
    vpn_migrate_legacy(env);
    if !valid_profile_id(did) {
        return (1, "{\"ok\":false,\"error\":\"配置无效\"}".to_owned());
    }
    let vpn_dir = env.vpn_dir();
    let ovpn = vpn_dir.join(format!("{did}.ovpn"));
    if !ovpn.is_file() {
        return (1, "{\"ok\":false,\"error\":\"配置不存在\"}".to_owned());
    }
    let was_en = fs::read_to_string(vpn_dir.join(format!("{did}.enabled")))
        .is_ok_and(|text| text.trim() == "1");
    fsx::remove_logged(env, &ovpn);
    fsx::remove_logged(env, vpn_dir.join(format!("{did}.name")));
    fsx::remove_logged(env, vpn_dir.join(format!("{did}.enabled")));
    vpn_mirror_legacy(env);
    if vm::running(env) {
        let any = vpn_sync_host(env).unwrap_or(false);
        if was_en || !any {
            let host = env.run_dir().join("guest/host");
            if !any {
                fsx::remove_logged(env, host.join("openvpn.ovpn"));
            }
            let act = if any { "enable" } else { "disable" };
            fsx::write_logged(env, host.join("openvpn.cmd"), vpn_next_cmd(env, act));
        }
    }
    log::info(env, "xr", "已删除 OpenVPN 配置");
    (0, "{\"ok\":true}".to_owned())
}

/// `xr traffic-split`：请客机统计一次分流流量，最多等 `wait_secs` 秒，返回手机与热点两部分的出网、局域网与合计流量。
///
/// 服务未运行或客机未响应时返回 1。
#[must_use]
pub(crate) fn traffic_split(env: &Env, wait_secs: u64) -> (i32, String) {
    if !vm::running(env) {
        return (1, "{\"ok\":false,\"error\":\"服务未运行\"}".to_owned());
    }
    let host = env.run_dir().join("guest/host");
    fsx::mkdirs_logged(env, &host);
    let ts = format!("{}{}", unix_now(), std::process::id());
    fsx::write_logged(env, host.join("acct.cmd"), format!("snap {ts}\n"));
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
                    .map(|text| text.parse::<u64>().unwrap_or(0))
                    .collect();
                let at = |idx: usize| vals.get(idx).copied().unwrap_or(0);
                let (pwu, pwd, plu, pld) = (at(0), at(1), at(2), at(3));
                let (hwu, hwd, hlu, hld) = (at(4), at(5), at(6), at(7));
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
    (1, "{\"ok\":false,\"error\":\"服务未响应\"}".to_owned())
}

/// `xr ls <路径> [dir|ovpn]`：列出内部存储下某个目录的子目录（`d/名字`）与文件（`f/名字`，`ovpn` 模式只列 `.ovpn`/`.conf`），按名字排序。
///
/// 路径不在内部存储下或不是目录时返回 1 和空列表。
#[must_use]
pub(crate) fn list_dir(env: &Env, path_arg: &str, mode: &str) -> (i32, Vec<String>) {
    let raw = if path_arg.is_empty() {
        env.media.to_string_lossy().into_owned()
    } else {
        path_arg.to_owned()
    };
    let p_str = env.canon_share_path(&raw);
    let media_s = env.media.to_string_lossy();
    if (p_str != media_s.as_ref() && !p_str.starts_with(&format!("{media_s}/")))
        || p_str.contains("..")
    {
        return (1, Vec::new());
    }
    let file_path = Path::new(&p_str);
    if !file_path.is_dir() {
        return (1, Vec::new());
    }
    let mut items = Vec::new();
    if let Ok(rd) = fs::read_dir(file_path) {
        for ent in rd.flatten() {
            let ep = ent.path();
            let Some(name) = ep.file_name().and_then(|raw_name| raw_name.to_str()) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            if ep.is_dir() {
                items.push(format!("d/{name}"));
            } else if mode == "ovpn"
                && ep
                    .extension()
                    .and_then(|os_str| os_str.to_str())
                    .is_some_and(|ext| {
                        ext.eq_ignore_ascii_case("ovpn") || ext.eq_ignore_ascii_case("conf")
                    })
            {
                items.push(format!("f/{name}"));
            } else {
                // 其他文件不列出
            }
        }
    }
    items.sort();
    (0, items)
}
