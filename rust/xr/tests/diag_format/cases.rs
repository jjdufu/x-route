//! 用假的 `ip`/`iptables`/`ip6tables` 生成手机网络快照，与改动前录制的 fixture 逐行比较。

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use xr::diag;
use xr::env::Env;
use xr_test_support::Failure;

/// 改动前录制的快照（已做 [`normalize`]）。
const FIXTURE: &str = include_str!("../fixtures/diag_phone.txt");

/// 去掉首行时间戳，把临时目录路径换成 `<临时目录>`，并把依赖宿主机 `/proc/sys` 的取值替换为 `<值>`，只保留格式。
#[must_use]
pub(crate) fn normalize(raw: &str, tmp: &str) -> String {
    let mut out = String::new();
    let replaced = raw.replace(tmp, "<临时目录>");
    for line in replaced.lines().skip(1) {
        let host_dependent = line.starts_with("net/ipv4/ip_forward=")
            || line.starts_with("net/ipv6/conf/all/forwarding=")
            || line.starts_with("ipv6.");
        match line.split_once('=') {
            Some((key, _value)) if host_dependent => {
                out.push_str(key);
                out.push_str("=<值>");
            }
            _ => out.push_str(line),
        }
        out.push('\n');
    }
    out
}

/// 在 `dir` 下写一个可执行的假命令，输出 `name` 与参数。
///
/// # Errors
///
/// 写文件或改权限失败时返回错误。
fn fake_bin(dir: &Path, name: &str) -> Result<(), Failure> {
    let path = dir.join(name);
    let mut body = String::from("#!/bin/sh\necho \"");
    body.push_str(name);
    body.push_str(" $*\"\n");
    fs::write(&path, body).map_err(|err| Failure::new(&err.to_string()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
        .map_err(|err| Failure::new(&err.to_string()))
}

/// 准备隔离的 `Env`：数据目录、`sysnet`、`sysbin` 都在临时目录中。
///
/// # Errors
///
/// 准备目录或假命令失败时返回错误。
pub(crate) fn snapshot_env(dir: &Path) -> Result<Env, Failure> {
    let mut env = Env::from_process();
    env.xr_data = dir.join("data");
    env.sysnet = dir.join("sysnet");
    env.sysbin = dir.join("bin");
    for sub in [env.run_dir(), env.sysnet.clone(), env.sysbin.clone()] {
        fs::create_dir_all(sub).map_err(|err| Failure::new(&err.to_string()))?;
    }
    for name in ["ip", "iptables", "ip6tables", "iw"] {
        fake_bin(&env.sysbin, name)?;
    }
    fs::write(env.run_dir().join("net.up_if"), "wlan0\n")
        .map_err(|err| Failure::new(&err.to_string()))?;
    Ok(env)
}

/// `diag::phone` 的输出（去掉时间与宿主机取值后）与 fixture 逐行一致。
///
/// # Errors
///
/// 准备失败，或输出与 fixture 不一致（消息给出第一处不同的行号）时返回错误。
#[test]
fn diag_phone_matches_fixture() -> Result<(), Failure> {
    let dir = tempfile::TempDir::new().map_err(|err| Failure::new(&err.to_string()))?;
    let env = snapshot_env(dir.path())?;
    let tmp = dir.path().to_string_lossy();
    let got = normalize(&diag::phone(&env), &tmp);
    if got == FIXTURE {
        return Ok(());
    }
    let first_diff = got
        .lines()
        .zip(FIXTURE.lines())
        .position(|(left, right)| left != right);
    Err(Failure::new(&format!(
        "第一处不同在第 {first_diff:?} 行（从 0 计，None 表示行数不同）；实际输出：\n{got}"
    )))
}
