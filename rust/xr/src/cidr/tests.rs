//! `ip6_format`：按 4 个十六进制字符分组的输出与逐字符参照实现一致（锁定 `as_chunks` 改写）。

use super::ip6_format;
use xr_test_support::{Failure, ensure};

/// 参照实现：只接受 32 个十六进制字符，每 4 个字符之间插入 `:`。
fn reference(hex32: &str) -> Option<String> {
    if hex32.len() != 32 || !hex32.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = String::with_capacity(39);
    for (pos, ch) in hex32.chars().enumerate() {
        if pos != 0 && pos.is_multiple_of(4) {
            out.push(':');
        }
        out.push(ch);
    }
    Some(out)
}

/// 由字母表循环取字符、从 `offset` 开始，生成长度为 `len` 的字符串。
fn cyclic(alphabet: &str, offset: usize, len: usize) -> String {
    alphabet.chars().cycle().skip(offset).take(len).collect()
}

/// 长度 0–40、不同起点的十六进制串（含大小写）及混入非十六进制字符的串，输出均与参照实现一致。
///
/// # Errors
///
/// 任一输入的输出与参照实现不同时返回错误。
#[test]
fn ip6_format_matches_reference_for_all_lengths() -> Result<(), Failure> {
    let alphabets = [
        "0123456789abcdefABCDEF",
        "fedcba9876543210",
        "0123456789abcdeg",
        "00中0",
    ];
    for alphabet in alphabets {
        for offset in 0_usize..16 {
            for len in 0_usize..=40 {
                let input = cyclic(alphabet, offset, len);
                ensure(
                    ip6_format(&input) == reference(&input),
                    "输出应与参照实现一致",
                )?;
            }
        }
    }
    Ok(())
}

/// 已知地址的格式化结果。
///
/// # Errors
///
/// 输出与期望不同时返回错误。
#[test]
fn ip6_format_known_values() -> Result<(), Failure> {
    ensure(
        ip6_format("20010db8000000000000000000000001").as_deref()
            == Some("2001:0db8:0000:0000:0000:0000:0000:0001"),
        "2001:db8::1 的完整分组",
    )?;
    ensure(
        ip6_format("20010db800000000000000000000000").is_none(),
        "31 位应拒绝",
    )?;
    ensure(
        ip6_format("20010db8000000000000000000000001f").is_none(),
        "33 位应拒绝",
    )?;
    ensure(
        ip6_format("20010db800000000000000000000000g").is_none(),
        "非十六进制应拒绝",
    )
}
