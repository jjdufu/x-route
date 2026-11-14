//! 实时速率计算：正常速率、计数回退、抖动、溢出与冷启动，以及速率公式的属性测试。

use proptest::prelude::*;
use xr_test_support::{Failure, ensure, ensure_eq, ensure_some};

use super::{MAX_INTERVAL_MS, MIN_INTERVAL_MS, Pair, RateState};

/// 首次采样不出速率；隔 2 秒后按增量算出各组速率，合计为公网加内网。
///
/// # Errors
///
/// 速率与期望不同时返回错误。
#[test]
fn first_sample_then_rate() -> Result<(), Failure> {
    let mut state = RateState::default();
    ensure_eq(
        &state.update_at(10_000, [100, 200, 300, 400]),
        &None,
        "首次只记基线",
    )?;
    let rates = ensure_some(
        state.update_at(12_000, [4_100, 2_200, 1_300, 400]),
        "第二次应出速率",
    )?;
    ensure_eq(
        &rates.wan,
        &Pair {
            up: 2_000,
            down: 1_000,
        },
        "公网",
    )?;
    ensure_eq(&rates.lan, &Pair { up: 500, down: 0 }, "内网")?;
    ensure_eq(
        &rates.total,
        &Pair {
            up: 2_500,
            down: 1_000,
        },
        "合计",
    )?;
    ensure_eq(&rates.interval_ms, &2_000, "间隔")
}

/// 某个分量回退时返回 `None` 并以当前值为新基线，下一次正常出速率。
///
/// # Errors
///
/// 回退处理与期望不同时返回错误。
#[test]
fn rollback_resets_baseline() -> Result<(), Failure> {
    let mut state = RateState::default();
    ensure(
        state.update_at(0, [5_000, 5_000, 5_000, 5_000]).is_none(),
        "首次",
    )?;
    ensure(
        state.update_at(2_000, [5_000, 10, 5_000, 5_000]).is_none(),
        "回退返回 None",
    )?;
    let rates = ensure_some(
        state.update_at(4_000, [5_000, 2_010, 5_000, 5_000]),
        "回退后下一次应出速率",
    )?;
    ensure_eq(&rates.wan.down, &1_000, "以回退后的值为基线")
}

/// 间隔不足最小间隔时返回上次结果，基线不变。
///
/// # Errors
///
/// 抖动处理与期望不同时返回错误。
#[test]
fn jitter_returns_previous() -> Result<(), Failure> {
    let mut state = RateState::default();
    ensure(state.update_at(0, [0; 4]).is_none(), "首次")?;
    ensure(
        state.update_at(MIN_INTERVAL_MS - 1, [9_999; 4]).is_none(),
        "首次后过快：仍无结果",
    )?;
    let first = state.update_at(2_000, [2_000, 0, 0, 0]);
    ensure_eq(
        &first.map(|rates| rates.wan.up),
        &Some(1_000),
        "基线未被过快的查询改动",
    )?;
    let again = state.update_at(2_500, [9_000, 0, 0, 0]);
    ensure_eq(&again, &first, "过快的查询返回上次结果")?;
    let later = state.update_at(4_000, [4_000, 0, 0, 0]);
    ensure_eq(
        &later.map(|rates| rates.wan.up),
        &Some(1_000),
        "基线仍为 2000 ms 时的值",
    )
}

/// 增量接近 `u64::MAX` 时不 panic，结果饱和。
///
/// # Errors
///
/// 饱和结果与期望不同时返回错误。
#[test]
fn overflow_saturates() -> Result<(), Failure> {
    let mut state = RateState::default();
    ensure(state.update_at(0, [0; 4]).is_none(), "首次")?;
    let rates = ensure_some(state.update_at(MIN_INTERVAL_MS, [u64::MAX; 4]), "应出速率")?;
    let each = u64::MAX.checked_div(1_000).unwrap_or(0);
    ensure_eq(&rates.wan.up, &each, "乘法饱和后除以 1000 ms")?;
    ensure_eq(&rates.total.up, &each.saturating_mul(2), "合计为两组之和")
}

/// 间隔超过最大间隔时视为冷启动，返回 `None`，下一次以新基线出速率。
///
/// # Errors
///
/// 冷启动处理与期望不同时返回错误。
#[test]
fn cold_start_after_long_gap() -> Result<(), Failure> {
    let mut state = RateState::default();
    ensure(state.update_at(0, [0; 4]).is_none(), "首次")?;
    ensure(
        state
            .update_at(MAX_INTERVAL_MS + 1, [1_000_000; 4])
            .is_none(),
        "间隔过长",
    )?;
    let rates = state.update_at(MAX_INTERVAL_MS + 2_001, [1_002_000; 4]);
    ensure_eq(&rates.map(|got| got.lan.down), &Some(1_000), "新基线")
}

/// 属性测试基线的上限：`u64::MAX` 的一半，保证加上增量不溢出。
const HALF: u64 = u64::MAX >> 1_u32;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_000))]

    /// 任意基线、增量与合法间隔：各分量速率等于 增量×1000/间隔（u128 参考值），合计等于公网加内网。
    #[test]
    fn prop_rate_matches_formula(
        base in prop::array::uniform4(0_u64..=HALF),
        delta in prop::array::uniform4(0_u64..=1_000_000_000_000),
        ms in MIN_INTERVAL_MS..=MAX_INTERVAL_MS,
    ) {
        let mut state = RateState::default();
        prop_assert!(state.update_at(0, base).is_none());
        let quad = core::array::from_fn::<u64, 4, _>(|idx| {
            base.get(idx).copied().unwrap_or(0).saturating_add(delta.get(idx).copied().unwrap_or(0))
        });
        let rates = state.update_at(ms, quad);
        prop_assert!(rates.is_some());
        let got = rates.unwrap_or_default();
        let expect = delta.map(|bytes| {
            u128::from(bytes).saturating_mul(1_000).checked_div(u128::from(ms)).and_then(|val| u64::try_from(val).ok()).unwrap_or(u64::MAX)
        });
        prop_assert_eq!([got.wan.up, got.wan.down, got.lan.up, got.lan.down], expect);
        prop_assert_eq!(got.total.up, got.wan.up.saturating_add(got.lan.up));
        prop_assert_eq!(got.total.down, got.wan.down.saturating_add(got.lan.down));
        prop_assert_eq!(got.interval_ms, ms);
    }
}
