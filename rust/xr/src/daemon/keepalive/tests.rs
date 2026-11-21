//! 常驻通知节流（间隔、强制刷新、内容不变与熄屏，状态在内存）与网关残留的去抖决策。

use std::cell::Cell;
use std::fs;

use proptest::prelude::*;
use tempfile::TempDir;
use xr_test_support::{Failure, ensure, ensure_eq, ensure_ok};

use super::{
    CARRIER_GRACE, NotifyMem, RESET_GRACE, ResetState, Stale, StaleAction, StaleGuard, StaleObs,
    cgroup_escape_tree, notify_throttled, notify_unchanged, stale_decide,
};
use crate::log::tests::env_at;

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

/// 一轮观察的简写。
const fn obs(kind: Stale, reset: ResetState, now: u64) -> StaleObs {
    StaleObs {
        kind,
        reset,
        daemon_up: true,
        now,
    }
}

/// 默认宽限的去抖状态。
const fn guard() -> StaleGuard {
    StaleGuard::with_grace(CARRIER_GRACE, RESET_GRACE)
}

/// `VmDead`：守护进程在且复位待处理时交给重启流程，其余立即撤路由；与是否在观察无关。
///
/// # Errors
///
/// 任一判定与期望不同时返回错误。
#[test]
fn vm_dead_is_immediate() -> Result<(), Failure> {
    let mut gd = guard();
    ensure_eq(
        &stale_decide(obs(Stale::CarrierDown, ResetState::NotSeen, 0), &mut gd),
        &StaleAction::Wait,
        "先进入观察",
    )?;
    ensure_eq(
        &stale_decide(obs(Stale::VmDead, ResetState::Pending, 1), &mut gd),
        &StaleAction::GuestReboot,
        "复位待处理交给重启流程",
    )?;
    let mut fresh = guard();
    ensure_eq(
        &stale_decide(obs(Stale::VmDead, ResetState::NotSeen, 1), &mut fresh),
        &StaleAction::Teardown,
        "无复位立即撤路由",
    )?;
    ensure_eq(
        &stale_decide(obs(Stale::VmDead, ResetState::Marked, 1), &mut fresh),
        &StaleAction::Teardown,
        "仅有标记也撤路由",
    )?;
    let down = StaleObs {
        daemon_up: false,
        ..obs(Stale::VmDead, ResetState::Pending, 1)
    };
    ensure_eq(
        &stale_decide(down, &mut fresh),
        &StaleAction::Teardown,
        "守护进程不在时撤路由",
    )
}

/// `CarrierDown` 无复位标记：0、5、10、14 秒观察，15 秒撤路由。
///
/// # Errors
///
/// 任一判定与期望不同时返回错误。
#[test]
fn carrier_down_grace() -> Result<(), Failure> {
    let mut gd = guard();
    for now in [0_u64, 5, 10, 14] {
        ensure_eq(
            &stale_decide(obs(Stale::CarrierDown, ResetState::NotSeen, now), &mut gd),
            &StaleAction::Wait,
            &format!("t={now} 观察"),
        )?;
    }
    ensure_eq(
        &stale_decide(obs(Stale::CarrierDown, ResetState::NotSeen, 15), &mut gd),
        &StaleAction::Teardown,
        "t=15 撤路由",
    )
}

/// `CarrierDown` 有复位标记：0、10、29 秒观察，30 秒撤路由。
///
/// # Errors
///
/// 任一判定与期望不同时返回错误。
#[test]
fn carrier_down_reset_grace() -> Result<(), Failure> {
    let mut gd = guard();
    for now in [0_u64, 10, 29] {
        ensure_eq(
            &stale_decide(obs(Stale::CarrierDown, ResetState::Marked, now), &mut gd),
            &StaleAction::Wait,
            &format!("t={now} 观察"),
        )?;
    }
    ensure_eq(
        &stale_decide(obs(Stale::CarrierDown, ResetState::Marked, 30), &mut gd),
        &StaleAction::Teardown,
        "t=30 撤路由",
    )
}

/// 观察中恢复（`Clean`）后再次无载波，从新的时刻重新计时。
///
/// # Errors
///
/// 任一判定与期望不同时返回错误。
#[test]
fn recovery_resets_timer() -> Result<(), Failure> {
    let mut gd = guard();
    ensure_eq(
        &stale_decide(obs(Stale::CarrierDown, ResetState::NotSeen, 0), &mut gd),
        &StaleAction::Wait,
        "t=0",
    )?;
    ensure_eq(
        &stale_decide(obs(Stale::CarrierDown, ResetState::NotSeen, 5), &mut gd),
        &StaleAction::Wait,
        "t=5",
    )?;
    ensure_eq(
        &stale_decide(obs(Stale::Clean, ResetState::NotSeen, 6), &mut gd),
        &StaleAction::Wait,
        "恢复",
    )?;
    ensure_eq(
        &stale_decide(obs(Stale::CarrierDown, ResetState::NotSeen, 10), &mut gd),
        &StaleAction::Wait,
        "重新开始观察",
    )?;
    ensure_eq(
        &stale_decide(obs(Stale::CarrierDown, ResetState::NotSeen, 24), &mut gd),
        &StaleAction::Wait,
        "距新起点 14 秒",
    )?;
    ensure_eq(
        &stale_decide(obs(Stale::CarrierDown, ResetState::NotSeen, 25), &mut gd),
        &StaleAction::Teardown,
        "距新起点 15 秒",
    )
}

/// 属性测试用的观察：残留种类与时间步长。
fn kind_of(code: u8) -> Stale {
    match code % 3 {
        0 => Stale::Clean,
        1 => Stale::CarrierDown,
        _ => Stale::VmDead,
    }
}

/// `run/fuse_watch.pid` 残留已退出的死 `PID` 时，`cgroup_escape_tree` 只迁移在跑进程，不写 `cgroup_escape 未成功` 调试噪音。
///
/// # Errors
///
/// 目录创建或断言不满足时返回 [`Failure`]。
#[test]
fn cgroup_escape_tree_skips_dead_pid() -> Result<(), Failure> {
    let tmp = ensure_ok(TempDir::new(), "临时目录")?;
    let mut env = env_at(&tmp.path().join("data"));
    env.proc_base = tmp.path().join("proc");
    env.cgroup_root = tmp.path().join("cg");
    for dir in [
        env.run_dir(),
        env.xr_data.join("config"),
        env.xr_data.join("log"),
        env.proc_base.join("1234"),
        env.cgroup_root.clone(),
    ] {
        ensure_ok(fs::create_dir_all(&dir), "建测试目录")?;
    }
    ensure_ok(
        fs::write(env.xr_data.join("config/config.conf"), "XR_DEBUG=1\n"),
        "开启调试日志",
    )?;
    ensure_ok(
        fs::write(env.run_dir().join("fuse_watch.pid"), "2147483640\n"),
        "写入死 PID",
    )?;
    cgroup_escape_tree(&env, 1234_u32);
    let procs = ensure_ok(
        fs::read_to_string(env.cgroup_root.join("cgroup.procs")),
        "读 cgroup.procs",
    )?;
    ensure_eq(&procs.trim(), &"1234", "本进程已写入 cgroup.procs")?;
    let dbg = fs::read_to_string(env.xr_data.join("log/debug.log")).unwrap_or_default();
    ensure(
        !dbg.contains("cgroup_escape 未成功"),
        "死 PID 不产生 cgroup_escape 未成功日志",
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_000))]

    /// 任意观察序列（时间单调递增，起点可接近 `u64::MAX`）：`CarrierDown` 时出现 `Teardown`，必然已连续无载波至少 `CARRIER_GRACE` 秒。
    #[test]
    fn teardown_only_after_grace(start in any::<u64>(), steps in prop::collection::vec((any::<u8>(), 0_u64..20), 1..60)) {
        let mut gd = guard();
        let mut now = start;
        let mut run_start: Option<u64> = None;
        for (code, step) in steps {
            now = now.saturating_add(step);
            let kind = kind_of(code);
            let action = stale_decide(obs(kind, ResetState::NotSeen, now), &mut gd);
            if kind == Stale::CarrierDown {
                let began = *run_start.get_or_insert(now);
                if action == StaleAction::Teardown {
                    prop_assert!(now.saturating_sub(began) >= CARRIER_GRACE);
                }
            } else {
                run_start = None;
            }
        }
    }
}
