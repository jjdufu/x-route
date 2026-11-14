//! 负载与设备快照：解析、邻居计数、请求与等待流程。

use std::fs;
use std::time::{Duration, Instant};

use proptest::prelude::*;
use serde_json::Value;
use tempfile::TempDir;
use xr_test_support::{Failure, ensure, ensure_eq, ensure_ok};

use super::{Reply, Snapshot, count_neigh, parse, probe, section};
use crate::env::Env;
use crate::log::tests::env_at;

/// 完整回执样例。
const FULL: &str = "ts=111\ncpu_pct=12\nmem_pct=43\nload1=0.15\nuptime=19967\nconntrack=123\ndev_hotspot=2\nhs=on\n";

/// 完整回执逐项解析，`hs=on` 时取客机热点计数。
///
/// # Errors
///
/// 准备文件失败或任一检查不成立时返回 [`Failure`]。
#[test]
fn parses_full_reply() -> Result<(), Failure> {
    let got = parse(FULL, Some("111"));
    let want = Reply {
        ts: "111".to_owned(),
        snap: Snapshot {
            cpu_pct: Some(12),
            mem_pct: Some(43),
            load1: Some("0.15".to_owned()),
            hotspot: 2,
            hs_on: true,
        },
    };
    ensure_eq(&got, &Some(want), "完整回执")
}

/// 缺少的字段为 `None` 或 0。
///
/// # Errors
///
/// 准备文件失败或任一检查不成立时返回 [`Failure`]。
#[test]
fn missing_fields_are_none_or_zero() -> Result<(), Failure> {
    let got = parse("ts=5\nuptime=3\n", None).map(|reply| reply.snap);
    ensure_eq(&got, &Some(Snapshot::default()), "缺字段")
}

/// 首行 ts 不匹配、缺失或为空时不认这份回执。
///
/// # Errors
///
/// 准备文件失败或任一检查不成立时返回 [`Failure`]。
#[test]
fn ts_mismatch_or_missing_is_none() -> Result<(), Failure> {
    ensure(parse(FULL, Some("222")).is_none(), "ts 不匹配")?;
    ensure(parse("cpu_pct=1\n", None).is_none(), "首行不是 ts")?;
    ensure(parse("ts=\ncpu_pct=1\n", None).is_none(), "ts 为空")?;
    ensure(parse("", None).is_none(), "空文件")
}

/// 非整数（含小数、负数）与非法平均负载被忽略。
///
/// # Errors
///
/// 准备文件失败或任一检查不成立时返回 [`Failure`]。
#[test]
fn non_numeric_values_are_ignored() -> Result<(), Failure> {
    let body = "ts=1\ncpu_pct=1.5\nmem_pct=-3\nload1=abc\ndev_hotspot=x\nhs=off\n";
    let got = parse(body, Some("1")).map(|reply| reply.snap);
    ensure_eq(&got, &Some(Snapshot::default()), "非数字值")
}

/// 邻居表按 MAC 去重，只数 `REACHABLE`/`STALE`/`DELAY`/`PROBE`。
///
/// # Errors
///
/// 准备文件失败或任一检查不成立时返回 [`Failure`]。
#[test]
fn neigh_counts_distinct_live_macs() -> Result<(), Failure> {
    let text = "192.168.9.2 lladdr aa:bb:cc:dd:ee:01 REACHABLE\n192.168.9.3 lladdr aa:bb:cc:dd:ee:02 FAILED\nfe80::1 lladdr aa:bb:cc:dd:ee:01 STALE\n192.168.9.4 lladdr aa:bb:cc:dd:ee:03 DELAY\n192.168.9.5 INCOMPLETE\n";
    ensure_eq(&count_neigh(text), &2_u32, "去重且 FAILED/INCOMPLETE 不计")
}

/// JSON 指针处的字符串。
fn text<'a>(value: &'a Value, ptr: &str) -> Option<&'a str> {
    value.pointer(ptr).and_then(Value::as_str)
}

/// 建临时环境：`run/guest/guest` 与 `run/guest/host`，`cap` 为 true 时写能力标记。
///
/// # Errors
///
/// 准备文件失败时返回 [`Failure`]。
fn setup(cap: bool) -> Result<(TempDir, Env), Failure> {
    let tmp = ensure_ok(TempDir::new(), "临时目录")?;
    let env = env_at(&tmp.path().join("data"));
    ensure_ok(
        fs::create_dir_all(env.run_dir().join("guest/guest")),
        "建目录",
    )?;
    ensure_ok(
        fs::create_dir_all(env.run_dir().join("guest/host")),
        "建目录",
    )?;
    if cap {
        ensure_ok(
            fs::write(env.run_dir().join("guest/guest/stats.cap"), "1\n"),
            "写 cap",
        )?;
    }
    Ok((tmp, env))
}

/// 虚拟机未运行或旧系统盘（无 `stats.cap`）时直接返回原因，不写 `host/stats.cmd`。
///
/// # Errors
///
/// 准备文件失败或任一检查不成立时返回 [`Failure`]。
#[test]
fn stopped_and_unsupported_send_nothing() -> Result<(), Failure> {
    let (_tmp, env) = setup(false)?;
    let cmd = env.run_dir().join("guest/host/stats.cmd");
    ensure_eq(
        &text(&section(&env, false, ""), "/reason"),
        &Some("stopped"),
        "未运行",
    )?;
    ensure_eq(
        &text(&section(&env, true, ""), "/reason"),
        &Some("unsupported"),
        "旧系统盘",
    )?;
    ensure(!cmd.exists(), "不发请求")
}

/// 已有 15 s 内的回执时直接返回、不等待，同时发出下一次请求。
///
/// # Errors
///
/// 准备文件失败或任一检查不成立时返回 [`Failure`]。
#[test]
fn fresh_data_returns_without_waiting() -> Result<(), Failure> {
    let (_tmp, env) = setup(true)?;
    ensure_ok(
        fs::write(env.run_dir().join("guest/guest/stats"), FULL),
        "写回执",
    )?;
    let start = Instant::now();
    let value = section(&env, true, "");
    ensure(start.elapsed() < Duration::from_millis(500), "不等待")?;
    ensure_eq(
        &value.pointer("/ok").and_then(Value::as_bool),
        &Some(true),
        "ok",
    )?;
    ensure_eq(
        &value.pointer("/cpu_pct").and_then(Value::as_u64),
        &Some(12),
        "cpu",
    )?;
    ensure_eq(&text(&value, "/load1"), &Some("0.15"), "load1")?;
    ensure_eq(
        &value.pointer("/devices/total").and_then(Value::as_u64),
        &Some(3),
        "设备总数",
    )?;
    ensure_eq(&text(&value, "/devices/source"), &Some("openwrt"), "来源")?;
    let cmd = ensure_ok(
        fs::read_to_string(env.run_dir().join("guest/host/stats.cmd")),
        "读请求",
    )?;
    ensure(!cmd.trim().is_empty(), "已发下一次请求")
}

/// 有能力标记但客机不应答：等待到期后返回 `timeout`，退出码 1。
///
/// # Errors
///
/// 准备文件失败或任一检查不成立时返回 [`Failure`]。
#[test]
fn no_reply_times_out() -> Result<(), Failure> {
    let (_tmp, env) = setup(true)?;
    let (rc, value) = probe(&env, true, "", 0);
    ensure_eq(&rc, &1_i32, "退出码")?;
    ensure_eq(&text(&value, "/reason"), &Some("timeout"), "超时")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_000))]

    /// 任意字节串（按 UTF-8 宽松解码）都不 panic；给出期望 ts 时结果的 ts 必与之相等。
    #[test]
    fn parse_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..256), want in "[0-9]{0,4}") {
        let body = String::from_utf8_lossy(&bytes);
        let loose = parse(&body, None);
        let strict = parse(&body, Some(&want));
        prop_assert!(strict.is_none() || strict.as_ref().map(|reply| reply.ts.as_str()) == Some(want.as_str()));
        prop_assert!(strict.is_none() || loose.is_some());
    }
}
