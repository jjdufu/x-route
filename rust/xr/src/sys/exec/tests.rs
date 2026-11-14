//! 调试日志的预期失败判定：删除到不存在、新建已存在的链可忽略，其余失败照常记录。

use proptest::prelude::*;
use xr_test_support::{Failure, ensure};

use super::expected_noise;

/// 删除到不存在与重复建链判为预期；`add` 失败、权限错误与成功的命令都不算。
///
/// # Errors
///
/// 任一判定与期望不同时返回错误。
#[test]
fn noise_cases() -> Result<(), Failure> {
    ensure(
        expected_noise(
            &["rule", "del", "pref", "19100"],
            2,
            "RTNETLINK answers: No such file or directory",
        ),
        "删规则到不存在",
    )?;
    ensure(
        expected_noise(
            &["-w", "-N", "XR_ACCT_UP"],
            1,
            "iptables: Chain already exists.",
        ),
        "重复建链",
    )?;
    ensure(
        expected_noise(
            &["-w", "-D", "FORWARD", "-j", "XR_FWD"],
            1,
            "iptables: Bad rule (does a matching rule exist in that chain?).",
        ),
        "删不存在的规则",
    )?;
    ensure(
        !expected_noise(
            &["route", "add", "10.8.0.0/24", "dev", "xr0"],
            2,
            "RTNETLINK answers: File exists",
        ),
        "add 失败照常记录",
    )?;
    ensure(
        !expected_noise(
            &["rule", "del", "pref", "19100"],
            2,
            "RTNETLINK answers: Permission denied",
        ),
        "权限错误照常记录",
    )?;
    ensure(
        !expected_noise(
            &["rule", "del", "pref", "19100"],
            0,
            "RTNETLINK answers: No such file or directory",
        ),
        "成功的命令不算",
    )?;
    ensure(
        !expected_noise(
            &["-w", "-N", "XR_ACCT_UP"],
            1,
            "iptables: No chain/target/match by that name.",
        ),
        "建链遇到其它错误照常记录",
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_000))]

    /// 任意参数与错误输出都不 panic；退出码为 0 时恒为 false。
    #[test]
    fn zero_code_never_noise(args in prop::collection::vec(".{0,12}", 0..8), stderr in ".{0,80}", code in any::<i32>()) {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let noisy = expected_noise(&refs, code, &stderr);
        prop_assert!(code != 0_i32 || !noisy);
        prop_assert!(!expected_noise(&refs, 0, &stderr));
    }
}
