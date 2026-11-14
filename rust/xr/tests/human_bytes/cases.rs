//! 保留旧的浮点实现作对照，要求整数实现对任意 `u64` 输出完全一致。

use proptest::prelude::*;
use xr::net::common::human_bytes;

/// 失败信息。
type Failure = String;

/// 旧实现（`common.rs:196` 改动前）：`bytes` 转 `f64` 后按 1024 逐级缩小，一位小数。
///
/// 转换不用 `as`：拆成高低 32 位，`hi * 2^32 + lo` 用 `mul_add`，只有一次舍入，与原来的 `bytes as f64` 相同。
///
/// # Errors
///
/// 拆分失败（不可能发生）时返回错误。
fn human_bytes_f64(bytes: u64) -> Result<String, Failure> {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let hi = u32::try_from(bytes >> 32_u32).map_err(|err| err.to_string())?;
    let lo = u32::try_from(bytes & 0xFFFF_FFFF_u64).map_err(|err| err.to_string())?;
    let mut val = f64::from(hi).mul_add(4_294_967_296.0_f64, f64::from(lo));
    let mut idx = 0_usize;
    while val >= 1024.0_f64 && idx < 4_usize {
        val /= 1024.0_f64;
        idx = idx.saturating_add(1);
    }
    if idx == 0 {
        return Ok(format!("{bytes} B"));
    }
    let unit = UNITS.get(idx).copied().unwrap_or("TB");
    Ok(format!("{val:.1} {unit}"))
}

/// 比较一个样本的新旧输出。
///
/// # Errors
///
/// 输出不一致时返回带样本值的错误。
fn same(bytes: u64) -> Result<(), Failure> {
    let old = human_bytes_f64(bytes)?;
    let new = human_bytes(bytes);
    if old == new {
        Ok(())
    } else {
        Err(format!("{bytes}: 旧 {old}，新 {new}"))
    }
}

/// 固定边界：0、1023、1024、1535、1536、1048575、1048576、`u64::MAX`，以及每个单位的
/// `k*1024^n ± 1` 与 `(k+0.05)*1024^n ± 2`（十分位舍入的临界点）。
///
/// # Errors
///
/// 任一样本新旧输出不一致时返回错误。
#[test]
fn boundaries_match_float_version() -> Result<(), Failure> {
    for bytes in [0, 1023, 1024, 1535, 1536, 1_048_575, 1_048_576, u64::MAX] {
        same(bytes)?;
    }
    let mut scale = 1_u64;
    for _unit in 0_u32..5_u32 {
        for whole in [1_u64, 2, 9, 10, 99, 100, 512, 1023] {
            let base = whole.saturating_mul(scale);
            let tie = base.saturating_add(scale.checked_div(20).unwrap_or(0));
            for bytes in [
                base.saturating_sub(1),
                base,
                base.saturating_add(1),
                tie.saturating_sub(2),
                tie.saturating_sub(1),
                tie,
                tie.saturating_add(1),
                tie.saturating_add(2),
            ] {
                same(bytes)?;
            }
        }
        scale = scale.saturating_mul(1024);
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    /// 随机 `u64`（均匀分布与按位宽分布各一组）新旧输出完全一致。
    #[test]
    fn random_values_match_float_version(bytes in any::<u64>(), shift in 0_u32..64_u32) {
        let narrow = bytes.checked_shr(shift).unwrap_or(0);
        prop_assert_eq!(human_bytes_f64(bytes).unwrap_or_default(), human_bytes(bytes));
        prop_assert_eq!(human_bytes_f64(narrow).unwrap_or_default(), human_bytes(narrow));
    }
}
