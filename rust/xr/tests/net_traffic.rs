use std::fs;
use std::path::PathBuf;
use xr::config::Config;
use xr::env::Env;
use xr::net::common::{combine_quad_max, human_bytes, read_quad, write_quad, NetCtx};
use xr::net::net_status_json;
use xr::net::vpn_routes::{is_default_route, routes_valid};
use xr::traffic::{acct_lan4, acct_lan6};

fn temp_env(name: &str) -> Env {
    let base: PathBuf = std::env::temp_dir().join(format!("xr-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let moddir = base.join("mod");
    let data = base.join("data");
    let tmp = base.join("tmp");
    let _ = fs::create_dir_all(moddir.join("config"));
    let _ = fs::create_dir_all(moddir.join("run"));
    let _ = fs::create_dir_all(data.join("run"));
    let _ = fs::create_dir_all(&tmp);
    let mut env = Env::from_process();
    env.moddir = moddir;
    env.xr_data = data;
    env.sysbin = base.join("sysbin");
    env.tmpdir = Some(tmp);
    let _ = fs::create_dir_all(&env.sysbin);
    env
}

#[test]
fn test_human_bytes_and_quads() {
    assert_eq!(human_bytes(0), "0 B");
    assert_eq!(human_bytes(1023), "1023 B");
    assert_eq!(human_bytes(1024), "1.0 KB");
    assert_eq!(human_bytes(1024 * 1024), "1.0 MB");

    let a = [10, 20, 30, 40];
    let b = [15, 10, 50, 40];
    assert_eq!(combine_quad_max(a, b), [15, 20, 50, 40]);

    let env = temp_env("quad");
    let qfile = env.run_dir().join("quad.test");
    write_quad(&qfile, [1, 2, 3, 4]);
    assert_eq!(read_quad(&qfile), [1, 2, 3, 4]);
}

#[test]
fn test_routes_valid_and_default_route() {
    assert!(is_default_route("0.0.0.0/0"));
    assert!(is_default_route("default"));
    assert!(is_default_route("::/0"));
    assert!(!is_default_route("10.0.0.0/8"));

    let env = temp_env("routes");
    let cfg = Config::load(&env);
    let ctx = NetCtx::new(&env, &cfg);
    let gf = xr::net::vpn_routes::guest_routes_file(&ctx);
    if let Some(p) = gf.parent() {
        let _ = fs::create_dir_all(p);
    }
    let _ = fs::write(
        &gf,
        "1.1.1.1\n10.0.0.0/8\n# comment\n2001:db8::/32\nnot-a-cidr\n",
    );
    let valid = routes_valid(&ctx);
    assert_eq!(valid, vec!["1.1.1.1/32", "10.0.0.0/8", "2001:db8::/32"]);
}

#[test]
fn test_acct_lan_and_status_json() {
    let env = temp_env("status");
    let cfg = Config::load(&env);
    let ctx = NetCtx::new(&env, &cfg);

    let _ = fs::write(env.run_dir().join("net.expose_if"), "wlan0 ap0 \n");
    let _ = fs::write(env.run_dir().join("net.hs_if"), "wlan1\n");
    let _ = fs::write(
        env.run_dir().join("routes.active"),
        "172.16.99.0/24\n2001:db8:1::/64\n",
    );

    let lan4 = acct_lan4(&ctx);
    assert!(lan4.contains(&"10.0.0.0/8".to_string()));
    assert!(lan4.contains(&"192.168.0.0/16".to_string()));
    assert!(lan4.contains(&"172.29.233.0/24".to_string()));
    assert!(lan4.contains(&"172.16.99.0/24".to_string()));

    let lan6 = acct_lan6(&ctx);
    assert!(lan6.contains(&"fe80::/10".to_string()));
    assert!(lan6.contains(&"2001:db8:1::/64".to_string()));

    let json = net_status_json(&env);
    assert!(json.contains("\"mode\":\"down\""));
    assert!(json.contains("\"hotspot\":\"wlan1\""));
}
