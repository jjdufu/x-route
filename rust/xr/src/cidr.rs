//! `IPv4`/`IPv6` 地址、前缀与 MAC 的解析和校验，以及热点子网冲突检测。
//!
//! 纯字符串运算为主，与原 shell 实现逐条对齐（见集成测试 `prop_core`）。

use std::process::Command;

use crate::env::{Env, HS_BR, HS_TAP, XR_IF};

#[cfg(test)]
mod tests;

/// 判断 `text` 是否为 1–6 位十进制数字（不含符号、空串返回 false）。
#[must_use]
pub(crate) fn is_uint(text: &str) -> bool {
    !text.is_empty() && text.len() <= 6 && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// 把 1–6 位十进制数字串解析为 `u32`；格式不符时返回 `None`。
#[must_use]
pub(crate) fn parse_uint(text: &str) -> Option<u32> {
    if !is_uint(text) {
        return None;
    }
    text.parse::<u32>().ok()
}

/// `text` 是十进制整数且落在闭区间 `[min, max]` 内时返回 true。
#[must_use]
pub(crate) fn in_range(text: &str, min: u32, max: u32) -> bool {
    parse_uint(text).is_some_and(|value| value >= min && value <= max)
}

/// 判断 `raw` 是否为点分十进制 `IPv4` 地址：恰好 4 段，每段 0–255、无前导零。
#[must_use]
pub fn valid_ip4(raw: &str) -> bool {
    let mut parts = raw.split('.');
    let (Some(o1), Some(o2), Some(o3), Some(o4), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return false;
    };
    for oct in [o1, o2, o3, o4] {
        if oct.is_empty() || oct.len() > 3 || !oct.bytes().all(|ch| ch.is_ascii_digit()) {
            return false;
        }
        if oct.len() > 1 && oct.starts_with('0') {
            return false;
        }
        let Ok(val) = oct.parse::<u16>() else {
            return false;
        };
        if val > 255 {
            return false;
        }
    }
    true
}

/// 把点分十进制 `IPv4` 地址转成大端序 `u32`；不是合法地址时返回 `None`。
#[must_use]
pub fn ip2int(raw: &str) -> Option<u32> {
    let mut parts = raw.split('.');
    let (Some(o1), Some(o2), Some(o3), Some(o4), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return None;
    };
    let mut out: u32 = 0;
    for oct in [o1, o2, o3, o4] {
        if oct.is_empty() || oct.len() > 3 || !oct.bytes().all(|ch| ch.is_ascii_digit()) {
            return None;
        }
        let trimmed = oct.trim_start_matches('0');
        let val: u32 = if trimmed.is_empty() {
            0
        } else {
            trimmed.parse::<u32>().ok()?
        };
        if val > 255 {
            return None;
        }
        out = (out << 8_u32) | val;
    }
    Some(out)
}

/// 前缀长度 `plen`（0–32）对应的 `IPv4` 网络掩码；超过 32 返回 `None`。
#[must_use]
pub(crate) const fn prefix_mask(plen: u8) -> Option<u32> {
    if plen == 0 {
        Some(0)
    } else if plen <= 32 {
        let shift = 32_u8.saturating_sub(plen);
        Some((!0_u32) << shift)
    } else {
        None
    }
}

/// 把点分掩码（如 `255.255.255.0`）换算成前缀长度；不是连续掩码时返回 `None`。
#[must_use]
pub fn mask2len(text: &str) -> Option<u8> {
    if !valid_ip4(text) {
        return None;
    }
    let mask_val = ip2int(text)?;
    (0_u8..=32_u8).find(|&plen| prefix_mask(plen) == Some(mask_val))
}

/// 把用 `:` 分隔的 `IPv6` 片段补零成 4 位一组；空串得到空列表，任一组非法时返回 `None`。
fn collect_v6_groups(text: &str) -> Option<Vec<String>> {
    if text.is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for group in text.split(':') {
        if group.is_empty()
            || group.len() > 4
            || !group.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return None;
        }
        out.push(format!("{group:0>4}"));
    }
    Some(out)
}

/// 把 `IPv6` 地址（可含 `::`）展开成 32 个小写十六进制字符；格式非法时返回 `None`。
#[must_use]
pub fn ip6_expand(text: &str) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    if !lower
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() || byte == b':')
    {
        return None;
    }
    let parts: Vec<&str> = lower.split("::").collect();
    if parts.len() > 2 {
        return None;
    }
    if parts.len() == 1 {
        if lower.starts_with(':') || lower.ends_with(':') {
            return None;
        }
        let groups = collect_v6_groups(&lower)?;
        if groups.len() != 8 {
            return None;
        }
        return Some(groups.concat());
    }
    let left_str = parts.first().copied()?;
    let right_str = parts.get(1).copied()?;
    let left = collect_v6_groups(left_str)?;
    let right = collect_v6_groups(right_str)?;
    let used = left.len().saturating_add(right.len());
    if used >= 8 {
        return None;
    }
    let miss = 8_usize.saturating_sub(used);
    let mut out = String::with_capacity(32);
    for hex_group in left {
        out.push_str(&hex_group);
    }
    for _ in 0..miss {
        out.push_str("0000");
    }
    for group in right {
        out.push_str(&group);
    }
    Some(out)
}

/// 对 32 位十六进制形式的 `IPv6` 地址按前缀长度 `plen` 清零主机位，结果仍为 32 个十六进制字符。
#[must_use]
pub fn ip6_mask(hex32: &str, plen: u8) -> Option<String> {
    if hex32.len() != 32 || plen > 128 {
        return None;
    }
    let val = u128::from_str_radix(hex32, 16).ok()?;
    let masked = if plen == 0 {
        0_u128
    } else if plen == 128 {
        val
    } else {
        let shift = 128_u8.saturating_sub(plen);
        val & ((!0_u128) << shift)
    };
    Some(format!("{masked:032x}"))
}

/// 把 32 个十六进制字符按 4 个一组用 `:` 连接（不压缩零）；长度或字符不符时返回 `None`。
#[must_use]
pub fn ip6_format(hex32: &str) -> Option<String> {
    if hex32.len() != 32 || !hex32.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut groups = Vec::with_capacity(8);
    let bytes = hex32.as_bytes();
    for chunk in bytes.as_chunks::<4>().0 {
        let group = std::str::from_utf8(chunk).ok()?;
        groups.push(group);
    }
    Some(groups.join(":"))
}

/// 把路由写法规范成 `地址/前缀长度`：支持 `IPv4` 点分掩码、前缀长度、裸地址（补 `/32`、`/128`）与 `IPv6`。
///
/// `default`、空串和任何非法写法返回 `None`。
#[must_use]
pub fn norm_route(raw: &str) -> Option<String> {
    if raw.is_empty() || raw == "default" {
        return None;
    }
    if let Some((addr, mask_or_len)) = raw.split_once('/') {
        if addr.contains(':') || mask_or_len.contains(':') {
            if addr.is_empty() {
                return None;
            }
            let plen = parse_uint(mask_or_len)?;
            if !(1..=128).contains(&plen) {
                return None;
            }
            return ip6_expand(addr).map(|_expanded| format!("{addr}/{plen}"));
        }
        if mask_or_len.contains('.') {
            if !valid_ip4(addr) {
                return None;
            }
            let plen = mask2len(mask_or_len)?;
            if !(1..=32).contains(&plen) {
                return None;
            }
            return Some(format!("{addr}/{plen}"));
        }
        if !valid_ip4(addr) {
            return None;
        }
        if mask_or_len.starts_with('0') {
            return None;
        }
        let plen = parse_uint(mask_or_len)?;
        if !(1..=32).contains(&plen) {
            return None;
        }
        return Some(format!("{addr}/{plen}"));
    }
    if valid_ip4(raw) {
        return Some(format!("{raw}/32"));
    }
    if raw.contains(':') {
        return ip6_expand(raw).map(|_expanded| format!("{raw}/128"));
    }
    None
}

/// `raw` 能被 [`norm_route`] 规范化时返回 true。
#[must_use]
pub fn is_valid(raw: &str) -> bool {
    norm_route(raw).is_some()
}

/// `raw`（`地址/前缀`）完全落在 `10/8`、`172.16/12`、`192.168/16` 或 `100.64/10` 内时返回 true。
#[must_use]
pub fn is_private(raw: &str) -> bool {
    let Some((addr, plen_s)) = raw.split_once('/') else {
        return false;
    };
    let Some(plen) = parse_uint(plen_s) else {
        return false;
    };
    if plen > 32 || !valid_ip4(addr) {
        return false;
    }
    let Some(ip) = ip2int(addr) else {
        return false;
    };
    let oct1 = (ip >> 24_u32) & 0xFF;
    let oct2 = (ip >> 16_u32) & 0xFF;
    if oct1 == 10 {
        plen >= 8
    } else if oct1 == 172 && (16..=31).contains(&oct2) {
        plen >= 12
    } else if oct1 == 192 && oct2 == 168 {
        plen >= 16
    } else if oct1 == 100 && (64..=127).contains(&oct2) {
        plen >= 10
    } else {
        false
    }
}

/// 规范化后的路由地址没有主机位（地址恰为网络号）时返回 true，`IPv4`、`IPv6` 都支持。
#[must_use]
pub fn is_aligned(raw: &str) -> bool {
    let Some(norm) = norm_route(raw) else {
        return false;
    };
    let Some((addr, plen_s)) = norm.split_once('/') else {
        return false;
    };
    let Ok(plen) = plen_s.parse::<u8>() else {
        return false;
    };
    if norm.contains(':') {
        let Some(expanded) = ip6_expand(addr) else {
            return false;
        };
        let Some(masked) = ip6_mask(&expanded, plen) else {
            return false;
        };
        expanded == masked
    } else {
        let Some(ip) = ip2int(addr) else {
            return false;
        };
        let Some(mask) = prefix_mask(plen) else {
            return false;
        };
        (ip & mask) == ip
    }
}

/// 两条路由（同为 `IPv4` 或同为 `IPv6`）按较短前缀比较网络号相同时返回 true，即二者有交集。
#[must_use]
pub fn overlap(lhs: &str, rhs: &str) -> bool {
    let (Some(ca), Some(cb)) = (norm_route(lhs), norm_route(rhs)) else {
        return false;
    };
    let (Some((lhs_addr, lhs_plen)), Some((rhs_addr, rhs_plen))) =
        (ca.split_once('/'), cb.split_once('/'))
    else {
        return false;
    };
    if ca.contains(':') != cb.contains(':') {
        return false;
    }
    let (Ok(pa), Ok(pb)) = (lhs_plen.parse::<u8>(), rhs_plen.parse::<u8>()) else {
        return false;
    };
    let min_p = pa.min(pb);
    if ca.contains(':') {
        let (Some(xa), Some(xb)) = (ip6_expand(lhs_addr), ip6_expand(rhs_addr)) else {
            return false;
        };
        ip6_mask(&xa, min_p) == ip6_mask(&xb, min_p)
    } else {
        let (Some(xa), Some(xb), Some(mask)) =
            (ip2int(lhs_addr), ip2int(rhs_addr), prefix_mask(min_p))
        else {
            return false;
        };
        (xa & mask) == (xb & mask)
    }
}

/// 求 `IPv4` 路由 `地址/前缀` 的网络号，格式为 `a.b.c.d/前缀`。
#[must_use]
pub fn network(raw: &str) -> Option<String> {
    let (addr, plen_s) = raw.split_once('/')?;
    let plen: u8 = plen_s.parse().ok()?;
    let ip = ip2int(addr)?;
    let mask = prefix_mask(plen)?;
    let net = ip & mask;
    Some(format!(
        "{}.{}.{}.{}/{plen}",
        (net >> 24) & 255,
        (net >> 16) & 255,
        (net >> 8) & 255,
        net & 255
    ))
}

/// 由 MAC 地址生成 EUI-64 接口标识（16 个十六进制字符，翻转 U/L 位并插入 `fffe`）。
#[must_use]
pub fn mac_eui64(mac: &str) -> Option<String> {
    let lower = mac.to_ascii_lowercase();
    let parts: Vec<&str> = lower.split(':').collect();
    if parts.len() != 6 {
        return None;
    }
    let mut bytes = [0_u8; 6];
    for (dst, text) in bytes.iter_mut().zip(parts.iter()) {
        if text.len() != 2 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        *dst = u8::from_str_radix(text, 16).ok()?;
    }
    let [b0, b1, b2, b3, b4, b5] = bytes;
    let first = b0 ^ 0x02;
    Some(format!(
        "{first:02x}{b1:02x}{b2:02x}fffe{b3:02x}{b4:02x}{b5:02x}"
    ))
}

/// 热点桥接口使用的 MAC：取虚拟机 MAC，把最后一个字节的最低位取反。
#[must_use]
pub fn hs_mac(vm_mac: &str) -> Option<String> {
    let (prefix, last) = vm_mac.rsplit_once(':')?;
    let byte = u8::from_str_radix(last, 16).ok()?;
    Some(format!("{prefix}:{:02x}", byte ^ 0x01))
}

/// 地址（可带前缀）属于链路本地段 `fe80::/10` 时返回 true。
#[must_use]
pub fn v6_is_ll(addr: &str) -> bool {
    let bare = addr.split('/').next().unwrap_or(addr);
    ip6_expand(bare).is_some_and(|hex| {
        hex.starts_with("fe8")
            || hex.starts_with("fe9")
            || hex.starts_with("fea")
            || hex.starts_with("feb")
    })
}

/// 地址（可带前缀）属于唯一本地段 `fc00::/7` 时返回 true。
#[must_use]
pub fn v6_is_ula(addr: &str) -> bool {
    let bare = addr.split('/').next().unwrap_or(addr);
    ip6_expand(bare).is_some_and(|hex| hex.starts_with("fc") || hex.starts_with("fd"))
}

/// 用前缀 `pfx`（长度不超过 64）的前 64 位与 16 位十六进制接口标识 `iid16` 拼出完整 `IPv6` 地址。
#[must_use]
pub fn v6_compose(pfx: &str, iid16: &str) -> Option<String> {
    let (addr, plen_s) = pfx.split_once('/')?;
    let plen: u8 = plen_s.parse().ok()?;
    if plen > 64 || iid16.len() != 16 || !iid16.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let expanded = ip6_expand(addr)?;
    let net = ip6_mask(&expanded, plen)?;
    let prefix16 = net.get(..16)?;
    let full = format!("{prefix16}{}", iid16.to_ascii_lowercase());
    ip6_format(&full)
}

/// 两个 `IPv6` 地址（忽略前缀部分）展开后相同时返回 true。
#[must_use]
pub(crate) fn v6_addr_eq(lhs: &str, rhs: &str) -> bool {
    let aa = lhs.split('/').next().unwrap_or(lhs);
    let ba = rhs.split('/').next().unwrap_or(rhs);
    let (Some(ea), Some(eb)) = (ip6_expand(aa), ip6_expand(ba)) else {
        return false;
    };
    ea == eb
}

/// 在前缀 `pfx`（48–64 位）内挑一个未被 `busy` 占用的地址，带前缀长度返回。
///
/// 候选依次是 MAC 的 EUI-64、固定接口标识 `787200e900000254`、`787200e90000{1..8}`；全部被占用时返回 `None`。
#[must_use]
pub fn v6_pick_addr(pfx: &str, busy: &[String], mac: &str) -> Option<String> {
    let (_, plen_s) = pfx.split_once('/')?;
    let plen: u8 = plen_s.parse().ok()?;
    if !(48..=64).contains(&plen) {
        return None;
    }
    let mut cands = Vec::with_capacity(10);
    if let Some(eui) = mac_eui64(mac) {
        cands.push(eui);
    }
    cands.push("787200e900000254".to_owned());
    for i in 1_u16..=8_u16 {
        cands.push(format!("787200e90000{i:04x}"));
    }
    for iid in cands {
        let Some(cand) = v6_compose(pfx, &iid) else {
            continue;
        };
        if !busy
            .iter()
            .any(|used| !used.is_empty() && v6_addr_eq(&cand, used))
        {
            return Some(format!("{cand}/{plen}"));
        }
    }
    None
}

/// 判断热点子网设置是否可用：私有 `IPv4`、前缀 16–30、不在 `100.64/10`，且地址不是网络号或广播地址。
#[must_use]
pub fn hs_subnet_valid(raw: &str) -> bool {
    let Some((addr, plen_s)) = raw.split_once('/') else {
        return false;
    };
    if plen_s.starts_with('0') || !valid_ip4(addr) {
        return false;
    }
    let Some(plen) = parse_uint(plen_s) else {
        return false;
    };
    if !(16..=30).contains(&plen) {
        return false;
    }
    let Ok(plen_u8) = u8::try_from(plen) else {
        return false;
    };
    if addr.starts_with("100.") || !is_private(raw) {
        return false;
    }
    let (Some(ip), Some(mask)) = (ip2int(addr), prefix_mask(plen_u8)) else {
        return false;
    };
    let host = ip & !mask;
    let bcast = !mask;
    host != 0 && host != bcast
}

/// 一行路由输出中每个 `dev` 之后的接口名，按出现顺序返回。
fn dev_values(line: &str) -> Vec<String> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let mut devs = Vec::new();
    for pair in tokens.windows(2) {
        if let [tok, dev] = *pair
            && tok == "dev"
        {
            devs.push(dev.to_owned());
        }
    }
    devs
}

/// 检查热点子网 `subnet` 是否与 `xr0` 的 `/24` 或手机上其他接口的 `IPv4` 地址冲突。
///
/// 跳过 `lo`、`xr0`、`xr1`、`xrbr1`、`local_network` 表里的接口以及 `skip_if`；冲突时返回"接口名 地址段"，无冲突返回 `None`。
#[must_use]
pub fn hs_subnet_clash(
    env: &Env,
    net_prefix: &str,
    subnet: &str,
    skip_if: Option<&str>,
) -> Option<String> {
    let xr_net = format!("{net_prefix}.0/24");
    if overlap(subnet, &xr_net) {
        return Some(format!("{XR_IF} {xr_net}"));
    }
    let ip_bin = env.sysbin_cmd("ip");
    let mut skip = vec![
        "lo".to_owned(),
        XR_IF.to_owned(),
        HS_TAP.to_owned(),
        HS_BR.to_owned(),
    ];
    if let Some(skip_name) = skip_if.filter(|content| !content.is_empty()) {
        skip.push(skip_name.to_owned());
    }
    if let Ok(out) = Command::new(&ip_bin)
        .args(["route", "show", "table", "local_network"])
        .output()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            skip.extend(dev_values(line));
        }
    }
    if let Ok(out) = Command::new(&ip_bin)
        .args(["-4", "-o", "addr", "show"])
        .output()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            let (Some(raw_dev), Some(addr_cidr)) = (tokens.get(1), tokens.get(3)) else {
                continue;
            };
            let dev = raw_dev.split('@').next().unwrap_or(raw_dev);
            if skip.iter().any(|raw| raw == dev) {
                continue;
            }
            if overlap(subnet, addr_cidr) {
                return Some(format!("{dev} {addr_cidr}"));
            }
        }
    }
    None
}
