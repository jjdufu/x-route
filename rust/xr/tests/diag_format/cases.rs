//! 用假的 `ip`/`iptables`/`ip6tables` 生成手机网络快照，与改动前录制的 fixture 逐行比较。

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use xr::diag;
use xr::env::Env;
use xr_test_support::Failure;

/// 改动前录制的快照（已做 [`normalize`]）。
const FIXTURE: &str = include_str!("../fixtures/diag_phone.txt");
/// 网桥、无线热点、热点接口与多个运行状态文件都存在时的快照（已做 [`normalize`] 与 [`sort_unordered`]）。
const FIXTURE_FULL: &str = include_str!("../fixtures/diag_phone_full.txt");

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

/// "## 网桥"与"## 运行状态"两节的条目来自目录遍历，顺序取决于文件系统；把这两节内的行排序后再比较。
#[must_use]
fn sort_unordered(text: &str) -> String {
    let mut out = String::new();
    let mut block: Vec<&str> = Vec::new();
    let mut sorting = false;
    for line in text.lines() {
        if line.starts_with("## ") || line.is_empty() {
            block.sort_unstable();
            for item in std::mem::take(&mut block) {
                out.push_str(item);
                out.push('\n');
            }
            sorting = line == "## 网桥" || line == "## 运行状态";
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if sorting {
            block.push(line);
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    block.sort_unstable();
    for item in block {
        out.push_str(item);
        out.push('\n');
    }
    out
}

/// 在 [`snapshot_env`] 基础上加两座网桥（一座有端口）、一个无线接口（假 `iw` 输出信息与连接设备）、
/// 热点接口记录，以及状态、客机文件和多个 `run/net.*` 文件（含超过 300 字节、带换行的内容与一个同名目录）。
///
/// # Errors
///
/// 准备目录或文件失败时返回错误。
fn full_env(dir: &Path) -> Result<Env, Failure> {
    let env = snapshot_env(dir)?;
    let mk = |path: &Path| fs::create_dir_all(path).map_err(|err| Failure::new(&err.to_string()));
    let put = |path: &Path, body: &str| {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| Failure::new(&err.to_string()))?;
        }
        fs::write(path, body).map_err(|err| Failure::new(&err.to_string()))
    };
    mk(&env.sysnet.join("xrbr1/brif/xr1"))?;
    mk(&env.sysnet.join("xrbr0"))?;
    mk(&env.sysnet.join("wlan1/wireless"))?;
    put(
        &env.sysbin.join("iw"),
        "#!/bin/sh\ncase \"$3\" in\n info) printf 'Interface %s\\n\\tifindex 5\\n\\ttype AP\\n\\tssid TestAP\\n\\tchannel 6 (2437 MHz)\\n' \"$2\" ;;\n station) printf 'Station aa:bb:cc:dd:ee:ff (on %s)\\n\\tinactive time:\\t10 ms\\n\\trx bytes:\\t5\\n\\tsignal:  \\t-40 dBm\\n\\tconnected time:\\t20 seconds\\n' \"$2\" ;;\nesac\n",
    )?;
    fs::set_permissions(env.sysbin.join("iw"), fs::Permissions::from_mode(0o755))
        .map_err(|err| Failure::new(&err.to_string()))?;
    let run = env.run_dir();
    let long = "x".repeat(320);
    for (rel, body) in [
        ("state", "stable\n"),
        ("guest/guest/ipv6", "ok\n多一行\n"),
        ("guest/guest/hotspot", "on 192.168.43.1/24\n"),
        ("net.hs_if", "wlan1\n"),
        ("net.gw", "on\n"),
        ("net.long", long.as_str()),
    ] {
        put(&run.join(rel), body)?;
    }
    mk(&run.join("net.dir"))?;
    Ok(env)
}

/// 网桥、热点与运行状态各节都有内容时，`diag::phone` 的输出（去掉时间与宿主机取值、无序节内排序后）与 fixture 一致；
/// `XR_GOLDEN_RECORD=1` 时录制。
///
/// # Errors
///
/// 准备失败、录制失败，或输出与 fixture 不一致（消息给出第一处不同的行号）时返回错误。
#[test]
fn diag_phone_full_matches_fixture() -> Result<(), Failure> {
    let dir = tempfile::TempDir::new().map_err(|err| Failure::new(&err.to_string()))?;
    let env = full_env(dir.path())?;
    let tmp = dir.path().to_string_lossy();
    let got = sort_unordered(&normalize(&diag::phone(&env), &tmp));
    if std::env::var_os("XR_GOLDEN_RECORD").is_some() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/diag_phone_full.txt");
        return fs::write(path, got).map_err(|err| Failure::new(&err.to_string()));
    }
    if got == FIXTURE_FULL {
        return Ok(());
    }
    let first_diff = got
        .lines()
        .zip(FIXTURE_FULL.lines())
        .position(|(left, right)| left != right);
    Err(Failure::new(&format!(
        "第一处不同在第 {first_diff:?} 行（从 0 计，None 表示行数不同）；实际输出：\n{got}"
    )))
}
