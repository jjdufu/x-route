//! 从 VPN 配置中收集服务器地址，并在诊断文本中脱敏。

use std::fs;
use std::os::unix::fs::PermissionsExt as _;

use xr::diag;
use xr::env::Env;
use xr_test_support::{Failure, ensure};

/// 收集 `config/vpn` 与 `run/guest/host/vpn` 中所有 `.ovpn` 的 `remote`，排序去重；不可读目录和非 ovpn 文件被跳过。
///
/// # Errors
///
/// 准备文件失败或断言不成立时返回错误。
#[test]
fn collects_remote_hosts() -> Result<(), Failure> {
    let dir = tempfile::TempDir::new().map_err(|err| Failure::new(&err.to_string()))?;
    let mut env = Env::from_process();
    env.xr_data = dir.path().to_path_buf();
    let cfg_vpn = env.xr_data.join("config/vpn");
    let guest_vpn = env.run_dir().join("guest/host/vpn");
    fs::create_dir_all(&cfg_vpn).map_err(|err| Failure::new(&err.to_string()))?;
    fs::create_dir_all(&guest_vpn).map_err(|err| Failure::new(&err.to_string()))?;
    fs::write(
        cfg_vpn.join("a.ovpn"),
        "client\nremote vpn.example.com 1194\nremote 1.2.3.4\n",
    )
    .map_err(|err| Failure::new(&err.to_string()))?;
    fs::write(guest_vpn.join("b.ovpn"), "remote  b.example.org\n")
        .map_err(|err| Failure::new(&err.to_string()))?;
    fs::write(cfg_vpn.join("notes.txt"), "remote ignored.example.net\n")
        .map_err(|err| Failure::new(&err.to_string()))?;
    let locked = cfg_vpn.join("locked");
    fs::create_dir_all(&locked).map_err(|err| Failure::new(&err.to_string()))?;
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000))
        .map_err(|err| Failure::new(&err.to_string()))?;

    let hosts = diag::vpn_hosts(&env);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755))
        .map_err(|err| Failure::new(&err.to_string()))?;
    ensure(
        hosts == ["1.2.3.4", "b.example.org", "vpn.example.com"],
        &format!("hosts 不符：{hosts:?}"),
    )?;

    let redacted = diag::redact(
        &env,
        "连接 vpn.example.com 与 b.example.org、1.2.3.4 正常\n",
    );
    ensure(
        !redacted.contains("vpn.example.com"),
        "vpn.example.com 未脱敏",
    )?;
    ensure(!redacted.contains("b.example.org"), "b.example.org 未脱敏")?;
    ensure(!redacted.contains("1.2.3.4"), "1.2.3.4 未脱敏")?;
    ensure(redacted.contains("<VPN服务器>"), "缺少占位符")
}
