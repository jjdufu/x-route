//! 流量快照落盘策略：阈值、最长间隔、计数重置、溢出，以及守护进程周期快照的首次初始化与按阈值写入。

use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;

use proptest::prelude::*;
use tempfile::TempDir;
use xr_test_support::{Failure, ensure, ensure_eq, ensure_ok};

use super::{SNAP_DEFAULT, SnapMem, SnapPolicy, acct_snapshot_if, snap_due, sum_delta};
use crate::config::Config;
use crate::log::tests::env_at;
use crate::net::common::{NetCtx, read_quad, write_quad};

/// 测试用策略：1000 字节或 100 秒。
const POLICY: SnapPolicy = SnapPolicy {
    min_bytes: 1_000,
    max_age: 100,
};

/// 已初始化、上次在 1000 秒写过全 0 的内存状态。
const MEM0: SnapMem = SnapMem {
    quad: [0; 4],
    at: 1_000,
};

/// 计数相同不写；增量差 1 字节不写、达到阈值写；有变化且到最长间隔写，没变化到间隔也不写。
///
/// # Errors
///
/// 任一判定与期望不同时返回错误。
#[test]
fn due_by_bytes_and_age() -> Result<(), Failure> {
    ensure(!snap_due([0; 4], &MEM0, POLICY, 5_000), "没变化不写")?;
    ensure(
        !snap_due([999, 0, 0, 0], &MEM0, POLICY, 1_010),
        "差 1 字节不写",
    )?;
    ensure(
        snap_due([500, 0, 250, 250], &MEM0, POLICY, 1_010),
        "四项合计达到阈值写",
    )?;
    ensure(
        snap_due([1, 0, 0, 0], &MEM0, POLICY, 1_100),
        "到最长间隔且有变化写",
    )?;
    ensure(
        !snap_due([1, 0, 0, 0], &MEM0, POLICY, 1_099),
        "未到间隔且增量小不写",
    )?;
    ensure(
        snap_due([1, 0, 0, 0], &SnapMem::default(), POLICY, 1),
        "未初始化写",
    )
}

/// 计数变小（清零后）按差的绝对值计，同样触发写入；默认策略为 64 MiB / 30 分钟。
///
/// # Errors
///
/// 任一判定与期望不同时返回错误。
#[test]
fn reset_counts_trigger_and_defaults() -> Result<(), Failure> {
    let big = SnapMem {
        quad: [5_000, 0, 0, 0],
        at: 1_000,
    };
    ensure(snap_due([0; 4], &big, POLICY, 1_001), "计数变小触发写入")?;
    ensure_eq(&SNAP_DEFAULT.min_bytes, &(64 * 1024 * 1024), "默认 64 MiB")?;
    ensure_eq(&SNAP_DEFAULT.max_age, &1_800_u64, "默认 30 分钟")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_000))]

    /// 任意计数（含接近 `u64::MAX`）求增量不溢出，且与参数顺序无关；判定不 panic。
    #[test]
    fn delta_symmetric(left in any::<[u64; 4]>(), right in any::<[u64; 4]>(), now in any::<u64>(), at in any::<u64>()) {
        prop_assert_eq!(sum_delta(left, right), sum_delta(right, left));
        let mem = SnapMem { quad: right, at };
        let due = snap_due(left, &mem, SNAP_DEFAULT, now);
        prop_assert!(!due || left != right);
    }
}

/// 假 `iptables`（忽略开头的 `-w`）：`-S XR_ACCT_UP` 成功；`-nvxL 链` 输出两行表头和 `cnt.<链>` 中每行一条规则的字节数。
const FAKE_IPT: &str = "#!/bin/sh\nd=$(dirname \"$0\")\n[ \"$1\" = -w ] && shift\ncase \"$1\" in\n  -S) exit 0 ;;\n  -nvxL) echo h1; echo h2; while read -r b; do echo \"0 $b RETURN\"; done < \"$d/cnt.$2\" ;;\nesac\nexit 0\n";

/// 等刚写好的脚本可以执行（并行测试里可能短暂 `ETXTBSY`）。
///
/// # Errors
///
/// 重试 100 次仍不可执行或出现其它错误时返回 [`Failure`]。
fn wait_exec_ready(path: &Path) -> Result<(), Failure> {
    for _ in 0_u32..100 {
        match Command::new(path).output() {
            Ok(_) => return Ok(()),
            Err(err) if err.kind() == ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(err) => {
                return Err(Failure::new(&format!(
                    "执行 {} 失败：{err}",
                    path.display()
                )));
            }
        }
    }
    Err(Failure::new("假 iptables 一直处于 ETXTBSY"))
}

/// 设置上行链的内网与公网字节数（下行链为 0）。
///
/// # Errors
///
/// 写文件失败时返回 [`Failure`]。
fn set_counts(sysbin: &Path, lan: u64, wan: u64) -> Result<(), Failure> {
    ensure_ok(
        fs::write(sysbin.join("cnt.XR_ACCT_UP"), format!("{lan}\n{wan}\n")),
        "写上行计数",
    )?;
    ensure_ok(
        fs::write(sysbin.join("cnt.XR_ACCT_DN"), "0\n0\n"),
        "写下行计数",
    )
}

/// 首次调用用快照文件初始化、不立刻写；增量不足阈值不写；达到阈值写入并更新内存；文件已等于总量时不写。
///
/// # Errors
///
/// 准备环境失败或任一检查不成立时返回 [`Failure`]。
#[test]
fn snapshot_if_follows_policy() -> Result<(), Failure> {
    let tmp = ensure_ok(TempDir::new(), "临时目录")?;
    let mut env = env_at(&tmp.path().join("data"));
    env.sysbin = tmp.path().join("sysbin");
    ensure_ok(fs::create_dir_all(&env.sysbin), "建 sysbin")?;
    ensure_ok(fs::create_dir_all(env.run_dir()), "建 run")?;
    let ipt = env.sysbin.join("iptables");
    ensure_ok(fs::write(&ipt, FAKE_IPT), "写假 iptables")?;
    ensure_ok(
        fs::set_permissions(&ipt, fs::Permissions::from_mode(0o755)),
        "chmod 假 iptables",
    )?;
    wait_exec_ready(&ipt)?;
    let cfg = Config::load(&env);
    let ctx = NetCtx::new(&env, &cfg);
    let snap = env.acct_snap_file();
    write_quad(&env, &snap, [100, 0, 0, 0]);
    let mut mem = SnapMem::default();

    set_counts(&env.sysbin, 0, 600)?;
    ensure(
        !acct_snapshot_if(&ctx, POLICY, &mut mem, 1_000),
        "首次只初始化",
    )?;
    ensure_eq(
        &mem,
        &SnapMem {
            quad: [100, 0, 0, 0],
            at: 1_000,
        },
        "用文件内容初始化",
    )?;
    ensure_eq(&read_quad(&snap), &[100_u64, 0, 0, 0], "文件未改")?;

    set_counts(&env.sysbin, 0, 1_100)?;
    ensure(
        acct_snapshot_if(&ctx, POLICY, &mut mem, 1_010),
        "增量 1000 达到阈值",
    )?;
    ensure_eq(&read_quad(&snap), &[1_100_u64, 0, 0, 0], "写入总量")?;
    ensure_eq(
        &mem,
        &SnapMem {
            quad: [1_100, 0, 0, 0],
            at: 1_010,
        },
        "内存同步",
    )?;

    set_counts(&env.sysbin, 0, 1_600)?;
    ensure(
        !acct_snapshot_if(&ctx, POLICY, &mut mem, 1_020),
        "增量 500 不写",
    )?;
    ensure_eq(&read_quad(&snap), &[1_100_u64, 0, 0, 0], "文件保持")?;

    write_quad(&env, &snap, [1_600, 0, 0, 0]);
    ensure(
        !acct_snapshot_if(&ctx, POLICY, &mut mem, 2_000),
        "文件已等于总量时不写",
    )?;
    ensure_eq(&mem.quad, &[1_600_u64, 0, 0, 0], "内存跟上文件")
}
