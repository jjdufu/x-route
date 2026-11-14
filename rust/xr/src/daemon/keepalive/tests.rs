//! 常驻通知节流：间隔、强制刷新、内容不变与熄屏判定（状态在内存，不写盘）。

use std::cell::Cell;

use xr_test_support::{Failure, ensure};

use super::{NotifyMem, notify_throttled, notify_unchanged};

/// 非稳态（未显示或强制刷新）时不节流；初始状态（`ts == 0`）立即刷新；不足间隔节流，到间隔放行。
///
/// # Errors
///
/// 任一判定与期望不同时返回错误。
#[test]
fn throttle_by_interval() -> Result<(), Failure> {
    let fresh = NotifyMem::default();
    ensure(
        !notify_throttled(true, 1_700_000_000, &fresh, 10),
        "ts 为 0 时立即刷新",
    )?;
    let mem = NotifyMem {
        ts: 100,
        last: "a".to_owned(),
    };
    ensure(notify_throttled(true, 105, &mem, 10), "不足间隔节流")?;
    ensure(!notify_throttled(true, 110, &mem, 10), "到间隔放行")?;
    ensure(!notify_throttled(false, 105, &mem, 10), "强制刷新不节流")?;
    ensure(!notify_throttled(false, 105, &mem, 10), "未显示不节流")
}

/// 负载与上次相同不重发且不查亮屏；负载变化时亮屏才重发；强制刷新或未显示总是重发。
///
/// # Errors
///
/// 任一判定与期望不同时返回错误。
#[test]
fn unchanged_payload_not_reposted() -> Result<(), Failure> {
    let mem = NotifyMem {
        ts: 100,
        last: "up 1".to_owned(),
    };
    let asked = Cell::new(0_u32);
    let screen = |on: bool| {
        asked.set(asked.get().saturating_add(1));
        on
    };
    ensure(
        notify_unchanged(true, &mem, "up 1", || screen(true)),
        "相同负载不重发",
    )?;
    ensure(asked.get() == 0, "相同负载时不查询亮屏")?;
    ensure(
        !notify_unchanged(true, &mem, "up 2", || screen(true)),
        "负载变化且亮屏重发",
    )?;
    ensure(
        notify_unchanged(true, &mem, "up 2", || screen(false)),
        "熄屏不重发",
    )?;
    ensure(
        !notify_unchanged(false, &mem, "up 1", || screen(true)),
        "强制刷新重发",
    )?;
    ensure(
        !notify_unchanged(false, &mem, "up 1", || screen(true)),
        "未显示时重发",
    )
}
