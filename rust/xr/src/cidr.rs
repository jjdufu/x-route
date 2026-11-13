use std::process::Command;

use crate::env::{Env, HS_BR, HS_TAP, XR_IF};

pub fn is_uint(s: &str) -> bool {
    !s.is_empty() && s.len() <= 6 && s.bytes().all(|b| b.is_ascii_digit())
}

pub fn parse_uint(s: &str) -> Option<u32> {
    if !is_uint(s) {
        return None;
    }
    s.parse::<u32>().ok()
}

pub fn in_range(s: &str, min: u32, max: u32) -> bool {
    parse_uint(s).is_some_and(|v| v >= min && v <= max)
}

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
        out = (out << 8) | val;
    }
    Some(out)
}

pub fn prefix_mask(plen: u8) -> Option<u32> {
    if plen == 0 {
        Some(0)
    } else if plen <= 32 {
        let shift = 32_u8.saturating_sub(plen);
        Some((!0_u32) << shift)
    } else {
        None
    }
}

pub fn mask2len(s: &str) -> Option<u8> {
    if !valid_ip4(s) {
        return None;
    }
    let m = ip2int(s)?;
    (0_u8..=32_u8).find(|&p| prefix_mask(p) == Some(m))
}

fn collect_v6_groups(s: &str) -> Option<Vec<String>> {
    if s.is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for g in s.split(':') {
        if g.is_empty() || g.len() > 4 || !g.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        out.push(format!("{g:0>4}"));
    }
    Some(out)
}

pub fn ip6_expand(s: &str) -> Option<String> {
    if s.is_empty() {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    if !lower.bytes().all(|b| b.is_ascii_hexdigit() || b == b':') {
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
    for g in left {
        out.push_str(&g);
    }
    for _ in 0..miss {
        out.push_str("0000");
    }
    for g in right {
        out.push_str(&g);
    }
    Some(out)
}

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

pub fn ip6_format(hex32: &str) -> Option<String> {
    if hex32.len() != 32 || !hex32.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut groups = Vec::with_capacity(8);
    let bytes = hex32.as_bytes();
    for chunk in bytes.chunks_exact(4) {
        let g = std::str::from_utf8(chunk).ok()?;
        groups.push(g);
    }
    Some(groups.join(":"))
}

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
            ip6_expand(addr)?;
            return Some(format!("{addr}/{plen}"));
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
        ip6_expand(raw)?;
        return Some(format!("{raw}/128"));
    }
    None
}

pub fn valid_cidr(raw: &str) -> bool {
    norm_route(raw).is_some()
}

pub fn cidr_private(raw: &str) -> bool {
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
    let a = (ip >> 24) & 0xFF;
    let b = (ip >> 16) & 0xFF;
    if a == 10 {
        plen >= 8
    } else if a == 172 && (16..=31).contains(&b) {
        plen >= 12
    } else if a == 192 && b == 168 {
        plen >= 16
    } else if a == 100 && (64..=127).contains(&b) {
        plen >= 10
    } else {
        false
    }
}

pub fn cidr_aligned(raw: &str) -> bool {
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

pub fn cidr_overlap(a: &str, b: &str) -> bool {
    let (Some(ca), Some(cb)) = (norm_route(a), norm_route(b)) else {
        return false;
    };
    let (Some((aa, pa_s)), Some((ab, pb_s))) = (ca.split_once('/'), cb.split_once('/')) else {
        return false;
    };
    if ca.contains(':') != cb.contains(':') {
        return false;
    }
    let (Ok(pa), Ok(pb)) = (pa_s.parse::<u8>(), pb_s.parse::<u8>()) else {
        return false;
    };
    let min_p = pa.min(pb);
    if ca.contains(':') {
        let (Some(xa), Some(xb)) = (ip6_expand(aa), ip6_expand(ab)) else {
            return false;
        };
        ip6_mask(&xa, min_p) == ip6_mask(&xb, min_p)
    } else {
        let (Some(xa), Some(xb), Some(mask)) = (ip2int(aa), ip2int(ab), prefix_mask(min_p)) else {
            return false;
        };
        (xa & mask) == (xb & mask)
    }
}

pub fn cidr_net(raw: &str) -> Option<String> {
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

pub fn mac_eui64(mac: &str) -> Option<String> {
    let lower = mac.to_ascii_lowercase();
    let parts: Vec<&str> = lower.split(':').collect();
    if parts.len() != 6 {
        return None;
    }
    let mut bytes = [0_u8; 6];
    for (dst, s) in bytes.iter_mut().zip(parts.iter()) {
        if s.len() != 2 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        *dst = u8::from_str_radix(s, 16).ok()?;
    }
    let [b0, b1, b2, b3, b4, b5] = bytes;
    let first = b0 ^ 0x02;
    Some(format!(
        "{first:02x}{b1:02x}{b2:02x}fffe{b3:02x}{b4:02x}{b5:02x}"
    ))
}

pub fn hs_mac(vm_mac: &str) -> Option<String> {
    let (prefix, last) = vm_mac.rsplit_once(':')?;
    let byte = u8::from_str_radix(last, 16).ok()?;
    Some(format!("{prefix}:{:02x}", byte ^ 0x01))
}

pub fn v6_is_ll(addr: &str) -> bool {
    let bare = addr.split('/').next().unwrap_or(addr);
    ip6_expand(bare).is_some_and(|h| {
        h.starts_with("fe8") || h.starts_with("fe9") || h.starts_with("fea") || h.starts_with("feb")
    })
}

pub fn v6_is_ula(addr: &str) -> bool {
    let bare = addr.split('/').next().unwrap_or(addr);
    ip6_expand(bare).is_some_and(|h| h.starts_with("fc") || h.starts_with("fd"))
}

pub fn v6_compose(pfx: &str, iid16: &str) -> Option<String> {
    let (addr, plen_s) = pfx.split_once('/')?;
    let plen: u8 = plen_s.parse().ok()?;
    if plen > 64 || iid16.len() != 16 || !iid16.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let expanded = ip6_expand(addr)?;
    let net = ip6_mask(&expanded, plen)?;
    let prefix16 = net.get(..16)?;
    let full = format!("{prefix16}{}", iid16.to_ascii_lowercase());
    ip6_format(&full)
}

pub fn v6_addr_eq(a: &str, b: &str) -> bool {
    let aa = a.split('/').next().unwrap_or(a);
    let ba = b.split('/').next().unwrap_or(b);
    let (Some(ea), Some(eb)) = (ip6_expand(aa), ip6_expand(ba)) else {
        return false;
    };
    ea == eb
}

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
    cands.push("787200e900000254".to_string());
    for i in 1_u16..=8_u16 {
        cands.push(format!("787200e90000{i:04x}"));
    }
    for iid in cands {
        let Some(cand) = v6_compose(pfx, &iid) else {
            continue;
        };
        if !busy.iter().any(|b| !b.is_empty() && v6_addr_eq(&cand, b)) {
            return Some(format!("{cand}/{plen}"));
        }
    }
    None
}

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
    if addr.starts_with("100.") || !cidr_private(raw) {
        return false;
    }
    let (Some(ip), Some(mask)) = (ip2int(addr), prefix_mask(plen_u8)) else {
        return false;
    };
    let host = ip & !mask;
    let bcast = !mask;
    host != 0 && host != bcast
}

pub fn hs_subnet_clash(
    env: &Env,
    net_prefix: &str,
    subnet: &str,
    skip_if: Option<&str>,
) -> Option<String> {
    let xr_net = format!("{net_prefix}.0/24");
    if cidr_overlap(subnet, &xr_net) {
        return Some(format!("{XR_IF} {xr_net}"));
    }
    let ip_bin = env.sysbin_cmd("ip");
    let mut skip = vec![
        "lo".to_string(),
        XR_IF.to_string(),
        HS_TAP.to_string(),
        HS_BR.to_string(),
    ];
    if let Some(s) = skip_if.filter(|s| !s.is_empty()) {
        skip.push(s.to_string());
    }
    if let Ok(out) = Command::new(&ip_bin)
        .args(["route", "show", "table", "local_network"])
        .output()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            for (idx, tok) in tokens.iter().enumerate() {
                if *tok == "dev" {
                    if let Some(dev) = tokens.get(idx.saturating_add(1)) {
                        skip.push((*dev).to_string());
                    }
                }
            }
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
            if skip.iter().any(|s| s == dev) {
                continue;
            }
            if cidr_overlap(subnet, addr_cidr) {
                return Some(format!("{dev} {addr_cidr}"));
            }
        }
    }
    None
}
