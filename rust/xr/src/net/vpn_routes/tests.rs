//! `VPN` 路由检查：分类、去重、本地网段随网络切换、拒绝记录落盘与日志去重。

use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use xr_test_support::{Failure, ensure, ensure_eq, ensure_ok};

use super::{
    RejectKind, Rejected, guest_routes_file, parse_rejected, rejected_body, routes_check,
    sync_rejected,
};
use crate::config::Config;
use crate::env::Env;
use crate::log::tests::env_at;
use crate::net::common::NetCtx;

/// 假 `ip`：`addr show dev X` 读同目录下的 `addr.X`，`route show table local_network` 读 `tether`。
const FAKE_IP: &str = "#!/bin/sh\nd=$(dirname \"$0\")\ncase \"$*\" in\n  \"-4 -o addr show dev \"*) cat \"$d/addr.$6\" 2>/dev/null ;;\n  \"route show table local_network\") cat \"$d/tether\" 2>/dev/null ;;\nesac\nexit 0\n";

/// 建临时环境：假 `ip`、`run` 与客机路由目录。
///
/// # Errors
///
/// 准备文件失败时返回 [`Failure`]。
fn setup() -> Result<(TempDir, Env), Failure> {
    let tmp = ensure_ok(TempDir::new(), "临时目录")?;
    let mut env = env_at(&tmp.path().join("data"));
    env.sysbin = tmp.path().join("sysbin");
    for dir in [
        env.sysbin.clone(),
        env.run_dir().join("guest/guest"),
        env.moddir.join("config"),
    ] {
        ensure_ok(fs::create_dir_all(&dir), "建目录")?;
    }
    let ip = env.sysbin.join("ip");
    ensure_ok(fs::write(&ip, FAKE_IP), "写假 ip")?;
    ensure_ok(
        fs::set_permissions(&ip, fs::Permissions::from_mode(0o755)),
        "chmod 假 ip",
    )?;
    wait_exec_ready(&ip)?;
    Ok((tmp, env))
}

/// 等刚写好的脚本可以执行：并行测试里别的线程 `fork` 时可能短暂继承写句柄，此时执行会得到 `ETXTBSY`，稍等重试。
///
/// # Errors
///
/// 重试 100 次仍不可执行，或出现其它错误时返回 [`Failure`]。
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
    Err(Failure::new("假 ip 一直处于 ETXTBSY"))
}

/// 写文件的简写。
///
/// # Errors
///
/// 写入失败时返回 [`Failure`]。
fn put(path: &Path, body: &str) -> Result<(), Failure> {
    ensure_ok(fs::write(path, body), &format!("写 {}", path.display()))
}

/// 一条拒绝记录。
fn rej(route: &str, kind: RejectKind, with: &str) -> Rejected {
    Rejected {
        route: route.to_owned(),
        kind,
        with: with.to_owned(),
    }
}

/// 去重后分类：本地网段、保留网段、格式错误、主机位各一条；切换上游后原本地网段的路由恢复可下发。
///
/// # Errors
///
/// 准备文件失败或任一检查不成立时返回 [`Failure`]。
#[test]
fn check_classifies_and_follows_network() -> Result<(), Failure> {
    let (_tmp, env) = setup()?;
    let cfg = Config::load(&env);
    let ctx = NetCtx::new(&env, &cfg);
    put(
        &guest_routes_file(&ctx),
        "10.58.88.0/24 10.58.88.0/24,192.168.31.0/24 172.29.233.0/24 bad 10.0.0.1/24 0.0.0.0/0\n",
    )?;
    put(
        &env.sysbin.join("addr.wlan0"),
        "3: wlan0    inet 192.168.31.205/24 brd 192.168.31.255 scope global wlan0\n",
    )?;
    put(&env.run_dir().join("net.up_if"), "wlan0\n")?;
    let check = routes_check(&ctx);
    ensure_eq(&check.valid, &vec!["10.58.88.0/24".to_owned()], "可下发")?;
    ensure_eq(
        &check.rejected,
        &vec![
            rej("192.168.31.0/24", RejectKind::Local, "192.168.31.205/24"),
            rej("172.29.233.0/24", RejectKind::Reserved, "172.29.233.0/24"),
            rej("bad", RejectKind::Format, ""),
            rej("10.0.0.1/24", RejectKind::HostBits, ""),
        ],
        "拒绝",
    )?;

    put(
        &env.sysbin.join("addr.rmnet_data4"),
        "5: rmnet_data4    inet 100.70.1.2/30 scope global rmnet_data4\n",
    )?;
    put(&env.run_dir().join("net.up_if"), "rmnet_data4\n")?;
    let moved = routes_check(&ctx);
    ensure_eq(
        &moved.valid,
        &vec!["10.58.88.0/24".to_owned(), "192.168.31.0/24".to_owned()],
        "切到移动网络后恢复",
    )?;
    ensure(
        moved
            .rejected
            .iter()
            .all(|item| item.kind != RejectKind::Local),
        "不再有本地直连",
    )
}

/// 共享接口（`local_network` 表里的设备）的网段同样算本地网段。
///
/// # Errors
///
/// 准备文件失败或任一检查不成立时返回 [`Failure`]。
#[test]
fn tether_iface_counts_as_local() -> Result<(), Failure> {
    let (_tmp, env) = setup()?;
    let cfg = Config::load(&env);
    let ctx = NetCtx::new(&env, &cfg);
    put(&guest_routes_file(&ctx), "192.168.43.0/24\n")?;
    put(
        &env.sysbin.join("tether"),
        "192.168.43.0/24 dev wlan2 proto static scope link\n",
    )?;
    put(
        &env.sysbin.join("addr.wlan2"),
        "9: wlan2    inet 192.168.43.1/24 scope global wlan2\n",
    )?;
    let check = routes_check(&ctx);
    ensure(check.valid.is_empty(), "无可下发")?;
    ensure_eq(
        &check.rejected,
        &vec![rej("192.168.43.0/24", RejectKind::Local, "192.168.43.1/24")],
        "共享接口网段",
    )
}

/// 可下发的超过 64 条：前 64 条下发，其余记为 `Limit`；重复项不占名额。
///
/// # Errors
///
/// 准备文件失败或任一检查不成立时返回 [`Failure`]。
#[test]
fn limit_after_64_unique() -> Result<(), Failure> {
    let (_tmp, env) = setup()?;
    let cfg = Config::load(&env);
    let ctx = NetCtx::new(&env, &cfg);
    let mut lines = vec!["10.1.0.0/24 10.1.0.0/24".to_owned()];
    lines.extend((0_u32..70).map(|idx| format!("10.2.{idx}.0/24")));
    let body = lines.join("\n");
    put(&guest_routes_file(&ctx), &body)?;
    let check = routes_check(&ctx);
    ensure_eq(&check.valid.len(), &64_usize, "64 条")?;
    ensure(
        check.valid.iter().any(|route| route == "10.1.0.0/24"),
        "重复项只算一次",
    )?;
    ensure_eq(&check.rejected.len(), &7_usize, "其余 7 条超限")?;
    ensure(
        check
            .rejected
            .iter()
            .all(|item| item.kind == RejectKind::Limit),
        "均为 Limit",
    )
}

/// 落盘按 route 排序、能原样解析；无法识别的行跳过。
///
/// # Errors
///
/// 任一检查不成立时返回 [`Failure`]。
#[test]
fn rejected_body_roundtrip() -> Result<(), Failure> {
    let items = vec![
        rej("bad", RejectKind::Format, ""),
        rej("192.168.31.0/24", RejectKind::Local, "192.168.31.205/24"),
    ];
    let body = rejected_body(&items);
    ensure_eq(
        body.as_str(),
        "192.168.31.0/24\tlocal\t192.168.31.205/24\nbad\tformat\t\n",
        "排序与格式",
    )?;
    let mut parsed = parse_rejected(&format!("{body}junk\nx\tnope\t\n"));
    parsed.reverse();
    ensure_eq(&parsed, &items, "解析回读")
}

/// 两次同样的拒绝结果只写一次文件、只记一次日志；本地直连消失时记"已恢复经 VPN"。
///
/// # Errors
///
/// 准备文件失败或任一检查不成立时返回 [`Failure`]。
#[test]
fn sync_rejected_logs_once() -> Result<(), Failure> {
    let (_tmp, env) = setup()?;
    let cfg = Config::load(&env);
    let ctx = NetCtx::new(&env, &cfg);
    let items = vec![
        rej("192.168.31.0/24", RejectKind::Local, "192.168.31.205/24"),
        rej("bad", RejectKind::Format, ""),
    ];
    sync_rejected(&ctx, &items);
    let file = env.run_dir().join("routes.rejected");
    let mtime = |path: &Path| fs::metadata(path).and_then(|meta| meta.modified()).ok();
    let first = mtime(&file);
    std::thread::sleep(std::time::Duration::from_millis(20));
    sync_rejected(&ctx, &items);
    ensure(first.is_some() && first == mtime(&file), "内容不变不重写")?;
    let log = fs::read_to_string(env.module_log()).unwrap_or_default();
    ensure_eq(
        &log.matches("本地直连优先").count(),
        &1_usize,
        "本地直连 INFO 只一次",
    )?;
    ensure_eq(
        &log.matches("[ERROR] [vpn] 格式错误 bad").count(),
        &1_usize,
        "格式错误 ERROR 只一次",
    )?;
    ensure(
        log.contains("[INFO] [vpn] VPN 路由 192.168.31.0/24 与当前网络 192.168.31.205/24 同网段"),
        "INFO 文案",
    )?;

    sync_rejected(&ctx, items.get(1..).unwrap_or_default());
    let after = fs::read_to_string(env.module_log()).unwrap_or_default();
    ensure(
        after.contains("VPN 路由 192.168.31.0/24 已恢复经 VPN"),
        "恢复 INFO",
    )?;
    ensure_eq(
        &after.matches("格式错误 bad").count(),
        &1_usize,
        "格式错误不重复",
    )
}
