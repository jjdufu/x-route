//! `RESTART_KEYS`：需要重启虚拟机才能生效的配置键，锁定其内容与顺序。

use super::{KEYS, RESTART_KEYS};
use xr_test_support::{Failure, ensure};

/// 与 05a 合并前 `cli/status.rs` 中那份副本逐项一致（11 项，顺序相同）。
///
/// # Errors
///
/// 内容或顺序与期望不同时返回错误。
#[test]
fn restart_keys_exact() -> Result<(), Failure> {
    let want = [
        "XR_NET_PREFIX",
        "XR_VM_MAC",
        "XR_DNS",
        "VM_CPUS",
        "VM_MEM_MB",
        "VM_SWIOTLB_MB",
        "VM_NET",
        "VM_DISK_RW",
        "VM_SHARE",
        "VM_SHARE_MODE",
        "VM_SHARE_PATH",
    ];
    ensure(RESTART_KEYS == want, "RESTART_KEYS 内容或顺序变化")
}

/// 每个需要重启的键都是合法配置键。
///
/// # Errors
///
/// 某个键不在 `KEYS` 中时返回错误。
#[test]
fn restart_keys_are_config_keys() -> Result<(), Failure> {
    ensure(
        RESTART_KEYS.iter().all(|key| KEYS.contains(key)),
        "RESTART_KEYS 含有非配置键",
    )
}
