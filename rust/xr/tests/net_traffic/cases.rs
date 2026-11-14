//! 网络与流量统计的公共函数：字节数显示、四元计数读写与合并、`VPN` 路由过滤、局域网网段和状态 JSON。

use std::fs;

use tempfile::TempDir;
use xr::config::Config;
use xr::env::Env;
use xr::net;
use xr::net::common::{NetCtx, combine_quad_max, human_bytes, read_quad, write_quad};
use xr::net::vpn_routes::{guest_routes_file, is_default_route, routes_valid};
use xr::traffic::{acct_lan4, acct_lan6};
use xr_test_support::{Failure, ensure, ensure_eq, ensure_ok};

/// 建临时目录和指向它的 `Env`：模块、数据、临时目录与模拟 `sysbin` 都在临时目录下。
///
/// # Errors
///
/// 建目录失败时返回 [`Failure`]。
fn temp_env() -> Result<(TempDir, Env), Failure> {
    let base = ensure_ok(TempDir::new(), "建临时目录")?;
    let moddir = base.path().join("mod");
    let data = base.path().join("data");
    let tmp = base.path().join("tmp");
    for dir in [
        moddir.join("config"),
        moddir.join("run"),
        data.join("run"),
        tmp.clone(),
    ] {
        ensure_ok(fs::create_dir_all(&dir), &format!("准备 {}", dir.display()))?;
    }
    let mut env = Env::from_process();
    env.moddir = moddir;
    env.xr_data = data;
    env.sysbin = base.path().join("sysbin");
    env.tmpdir = Some(tmp);
    ensure_ok(fs::create_dir_all(&env.sysbin), "准备 sysbin")?;
    Ok((base, env))
}

/// 字节数按 1024 进位显示；四元计数逐项取较大值；写入的四元计数能原样读回。
///
/// # Errors
///
/// 任一检查不成立时返回 [`Failure`]。
#[test]
fn human_bytes_and_quads() -> Result<(), Failure> {
    ensure_eq(human_bytes(0).as_str(), "0 B", "0 字节")?;
    ensure_eq(human_bytes(1023).as_str(), "1023 B", "1023 字节")?;
    ensure_eq(human_bytes(1024).as_str(), "1.0 KB", "1 KiB")?;
    ensure_eq(human_bytes(1024 * 1024).as_str(), "1.0 MB", "1 MiB")?;

    let left = [10_u64, 20, 30, 40];
    let right = [15_u64, 10, 50, 40];
    ensure_eq(
        &combine_quad_max(left, right),
        &[15_u64, 20, 50, 40],
        "逐项取较大值",
    )?;

    let (_base, env) = temp_env()?;
    let qfile = env.run_dir().join("quad.test");
    write_quad(&env, &qfile, [1, 2, 3, 4]);
    ensure_eq(&read_quad(&qfile), &[1_u64, 2, 3, 4], "四元计数读回写入值")
}

/// 默认路由的各种写法都能识别；客机推送的路由只保留合法 `CIDR`（主机地址补 `/32`），跳过注释和非法行。
///
/// # Errors
///
/// 准备文件失败或任一检查不成立时返回 [`Failure`]。
#[test]
fn routes_valid_and_default_route() -> Result<(), Failure> {
    ensure(is_default_route("0.0.0.0/0"), "0.0.0.0/0 是默认路由")?;
    ensure(is_default_route("default"), "default 是默认路由")?;
    ensure(is_default_route("::/0"), "::/0 是默认路由")?;
    ensure(!is_default_route("10.0.0.0/8"), "10.0.0.0/8 不是默认路由")?;

    let (_base, env) = temp_env()?;
    let cfg = Config::load(&env);
    let ctx = NetCtx::new(&env, &cfg);
    let gf = guest_routes_file(&ctx);
    if let Some(parent) = gf.parent() {
        ensure_ok(fs::create_dir_all(parent), "准备路由文件目录")?;
    }
    ensure_ok(
        fs::write(
            &gf,
            "1.1.1.1\n10.0.0.0/8\n# comment\n2001:db8::/32\nnot-a-cidr\n",
        ),
        "写客机路由文件",
    )?;
    let valid = routes_valid(&ctx);
    let valid_strs: Vec<&str> = valid.iter().map(String::as_str).collect();
    ensure_eq(
        &valid_strs,
        &vec!["1.1.1.1/32", "10.0.0.0/8", "2001:db8::/32"],
        "合法路由",
    )
}

/// 局域网网段包含私网、管理网段与已生效的 `VPN` 路由；没有网络时状态为 `down`，并报出热点接口。
///
/// # Errors
///
/// 准备文件失败或任一检查不成立时返回 [`Failure`]。
#[test]
fn acct_lan_and_status_json() -> Result<(), Failure> {
    let (_base, env) = temp_env()?;
    let cfg = Config::load(&env);
    let ctx = NetCtx::new(&env, &cfg);

    ensure_ok(
        fs::write(env.run_dir().join("net.expose_if"), "wlan0 ap0 \n"),
        "写 net.expose_if",
    )?;
    ensure_ok(
        fs::write(env.run_dir().join("net.hs_if"), "wlan1\n"),
        "写 net.hs_if",
    )?;
    ensure_ok(
        fs::write(
            env.run_dir().join("routes.active"),
            "172.16.99.0/24\n2001:db8:1::/64\n",
        ),
        "写 routes.active",
    )?;

    let lan4 = acct_lan4(&ctx);
    for block in [
        "10.0.0.0/8",
        "192.168.0.0/16",
        "172.29.233.0/24",
        "172.16.99.0/24",
    ] {
        ensure(
            lan4.contains(&block.to_owned()),
            &format!("IPv4 局域网含 {block}"),
        )?;
    }

    let lan6 = acct_lan6(&ctx);
    for block in ["fe80::/10", "2001:db8:1::/64"] {
        ensure(
            lan6.contains(&block.to_owned()),
            &format!("IPv6 局域网含 {block}"),
        )?;
    }

    let json = net::status_json(&env);
    ensure(json.contains("\"mode\":\"down\""), "状态为 down")?;
    ensure(json.contains("\"hotspot\":\"wlan1\""), "报出热点接口")
}
