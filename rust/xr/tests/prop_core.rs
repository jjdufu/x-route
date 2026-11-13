use std::fs;
use std::os::unix::fs::PermissionsExt;

use proptest::prelude::*;
use tempfile::TempDir;
use xr::cidr::{
    cidr_aligned, cidr_net, cidr_overlap, cidr_private, hs_mac, hs_subnet_clash, hs_subnet_valid,
    ip2int, ip6_expand, ip6_format, ip6_mask, mac_eui64, mask2len, norm_route, v6_compose,
    v6_is_ll, v6_is_ula, v6_pick_addr, valid_cidr, valid_ip4,
};
use xr::config::{config_hash, set_kv, valid_kv, Config, KEYS};
use xr::env::Env;
use xr::log::{blog, boot_clean_run, log_info, xr_now};
use xr::sys::exec::xr_exec;
use xr::sys::{cgroup_escape, cgroup_in_apps};

fn make_test_env(tmp: &TempDir) -> Env {
    let mut env = Env::from_process();
    env.xr_data = tmp.path().join("data");
    env.moddir = tmp.path().join("mod");
    env.media = tmp.path().join("media");
    let _ = fs::create_dir_all(env.media.join("DCIM"));
    let _ = env.ensure_dirs();
    let repo_conf =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../module/config/config.conf");
    if repo_conf.is_file() {
        let _ = fs::copy(repo_conf, env.conf_file());
    }
    env
}

#[test]
fn test_common_parity_valid_kv_and_cidr() {
    let tmp = TempDir::new().unwrap_or_else(|_| std::process::abort());
    let mut env = make_test_env(&tmp);
    let cfg = Config::load(&env);

    assert_eq!(KEYS.len(), 25);
    assert!(valid_kv(&env, &cfg, "XR_CLASH_UI", "0"));
    assert!(valid_kv(&env, &cfg, "XR_CLASH_UI", "1"));
    assert!(!valid_kv(&env, &cfg, "XR_CLASH_UI", "2"));
    assert!(valid_kv(&env, &cfg, "XR_NET_PREFIX", "172.29.233"));
    assert!(!valid_kv(&env, &cfg, "XR_NET_PREFIX", "8.8.8"));
    assert!(!valid_kv(&env, &cfg, "XR_NET_PREFIX", "172.29.256"));
    assert!(!valid_kv(&env, &cfg, "XR_NET_PREFIX", "172.32.1"));
    assert!(valid_kv(&env, &cfg, "XR_NET_PREFIX", "192.168.7"));
    assert!(valid_kv(&env, &cfg, "XR_NET_PREFIX", "10.1.2"));

    assert!(valid_kv(&env, &cfg, "XR_VM_MAC", "52:54:00:1d:e9:fe"));
    assert!(!valid_kv(&env, &cfg, "XR_VM_MAC", "52:54:00:1D:e9:fe"));

    assert!(valid_kv(&env, &cfg, "XR_DNS", "223.5.5.5 119.29.29.29"));
    assert!(!valid_kv(&env, &cfg, "XR_DNS", ""));
    assert!(!valid_kv(
        &env,
        &cfg,
        "XR_DNS",
        "1.1.1.1 2.2.2.2 3.3.3.3 4.4.4.4"
    ));
    assert!(!valid_kv(&env, &cfg, "XR_DNS", "1.1.1.300"));
    assert!(!valid_ip4("010.0.0.1"));
    assert!(!valid_ip4("192.168.008.1"));
    assert!(!valid_kv(&env, &cfg, "XR_DNS", "192.168.01.1"));
    assert_eq!(ip2int("010.000.000.008"), Some((10 << 24) | 8));

    assert!(valid_kv(&env, &cfg, "XR_EXPOSE", "lan"));
    assert!(valid_kv(&env, &cfg, "XR_EXPOSE", "local"));
    assert!(!valid_kv(&env, &cfg, "XR_EXPOSE", "hotspot"));
    assert!(valid_kv(&env, &cfg, "XR_MODE", "global"));
    assert!(valid_kv(&env, &cfg, "XR_MODE", "bypass"));
    assert!(!valid_kv(&env, &cfg, "XR_MODE", "site"));
    assert!(valid_kv(&env, &cfg, "VM_CPUS", "4"));
    assert!(!valid_kv(&env, &cfg, "VM_CPUS", "5"));
    assert!(valid_kv(&env, &cfg, "VM_MEM_MB", "512"));
    assert!(!valid_kv(&env, &cfg, "VM_MEM_MB", "256"));
    assert!(!valid_kv(&env, &cfg, "VM_MEM_MB", "99999999"));
    assert!(valid_kv(&env, &cfg, "VM_SWIOTLB_MB", "64"));
    assert!(!valid_kv(&env, &cfg, "VM_SWIOTLB_MB", "48"));
    assert!(valid_kv(&env, &cfg, "VM_SHARE_MODE", "ro"));
    assert!(!valid_kv(&env, &cfg, "VM_SHARE_MODE", "rw"));

    env.rw_map_opt = "uidmap=0 1023 1".to_string();
    assert!(valid_kv(&env, &cfg, "VM_SHARE_MODE", "rw"));
    env.rw_map_opt.clear();

    assert!(!valid_kv(&env, &cfg, "VM_SHARE_PATH", "/data/adb"));
    assert!(!valid_kv(&env, &cfg, "VM_SHARE_PATH", "/sdcard/../../adb"));
    assert!(valid_kv(&env, &cfg, "VM_SHARE_PATH", "/sdcard/DCIM"));
    assert!(!valid_kv(&env, &cfg, "UNKNOWN", "1"));

    assert!(cidr_private("10.20.0.0/16"));
    assert!(cidr_private("172.16.0.0/12"));
    assert!(cidr_private("192.168.50.0/24"));
    assert!(cidr_private("100.64.0.0/10"));
    assert!(!cidr_private("8.8.8.0/24"));
    assert!(!cidr_private("172.32.0.0/16"));
    assert!(!cidr_private("192.0.0.0/8"));
    assert!(!cidr_private("10.0.0.0/7"));

    assert!(cidr_aligned("192.168.50.0/24"));
    assert!(!cidr_aligned("192.168.50.1/24"));
    assert!(cidr_overlap("192.168.0.0/16", "192.168.50.0/24"));
    assert!(!cidr_overlap("192.168.50.0/24", "192.168.51.0/24"));
    assert_eq!(
        cidr_net("192.168.250.254/30").as_deref(),
        Some("192.168.250.252/30")
    );

    assert_eq!(norm_route("10.58.88.2").as_deref(), Some("10.58.88.2/32"));
    assert_eq!(
        norm_route("10.8.0.0/255.255.255.0").as_deref(),
        Some("10.8.0.0/24")
    );
    assert_eq!(
        norm_route("2001:db8::1").as_deref(),
        Some("2001:db8::1/128")
    );
    assert_eq!(
        ip6_expand("2001:db8::1").as_deref(),
        Some("20010db8000000000000000000000001")
    );
    assert!(!valid_cidr("10.0.0.0/0"));
    assert!(!valid_cidr("2001:db8::/129"));
    assert!(!valid_cidr("2001:db8:::1"));
    assert!(!valid_cidr("10.8.0.0/255.0.255.0"));
}

#[test]
fn test_ipv6_eui64_and_hotspot_clash() {
    let tmp = TempDir::new().unwrap_or_else(|_| std::process::abort());
    let mut env = make_test_env(&tmp);

    assert_eq!(
        mac_eui64("52:54:00:1d:e9:fe").as_deref(),
        Some("505400fffe1de9fe")
    );
    assert_eq!(
        hs_mac("52:54:00:1d:e9:fe").as_deref(),
        Some("52:54:00:1d:e9:ff")
    );
    assert_eq!(
        v6_compose("2409:8a00:1:2::/64", "505400fffe1de9fe").as_deref(),
        Some("2409:8a00:0001:0002:5054:00ff:fe1d:e9fe")
    );
    let pick1 = v6_pick_addr(
        "2409:8a00:1:2::/64",
        &["2409:8a00:1:2:1111:2222:3333:4444/64".to_string()],
        "52:54:00:1d:e9:fe",
    );
    assert_eq!(
        pick1.as_deref(),
        Some("2409:8a00:0001:0002:5054:00ff:fe1d:e9fe/64")
    );
    let pick2 = v6_pick_addr(
        "2409:8a00:1:2::/64",
        &["2409:8a00:0001:0002:5054:00ff:fe1d:e9fe/64".to_string()],
        "52:54:00:1d:e9:fe",
    );
    assert_eq!(
        pick2.as_deref(),
        Some("2409:8a00:0001:0002:7872:00e9:0000:0254/64")
    );
    assert!(v6_is_ll("fe80::1"));
    assert!(!v6_is_ll("2409:8a00:1:2::1"));
    assert!(v6_is_ula("fd7a:7872:e9::1"));

    let sb = tmp.path().join("sb");
    let _ = fs::create_dir_all(&sb);
    let ip_mock = sb.join("ip");
    let script = r#"#!/bin/sh
case "$*" in
  "-4 -o addr show")
    printf '%s\n' \
      "1: lo    inet 127.0.0.1/8 scope host lo" \
      "20: wlan0    inet 192.168.31.88/24 brd 192.168.31.255 scope global wlan0" \
      "21: rmnet_data1    inet 10.120.5.6/30 scope global rmnet_data1" \
      "30: wlan2    inet 10.233.0.47/24 scope global wlan2" \
      "40: xr0    inet 172.29.233.1/24 scope global xr0"
    ;;
  "route show table local_network")
    echo "10.233.0.0/24 dev wlan2 proto static scope link"
    ;;
esac
"#;
    let _ = fs::write(&ip_mock, script);
    let _ = fs::set_permissions(&ip_mock, fs::Permissions::from_mode(0o755));
    env.sysbin = sb;

    assert!(hs_subnet_valid("10.234.0.1/24"));
    assert!(!hs_subnet_valid("100.64.0.1/24"));
    assert_eq!(
        hs_subnet_clash(&env, "172.29.233", "192.168.31.1/24", None).as_deref(),
        Some("wlan0 192.168.31.88/24")
    );
    assert!(hs_subnet_clash(&env, "172.29.233", "10.233.0.1/24", Some("wlan2")).is_none());
}

#[test]
fn test_set_kv_log_boot_clean_and_cgroup() {
    let tmp = TempDir::new().unwrap_or_else(|_| std::process::abort());
    let mut env = make_test_env(&tmp);

    let h0 = config_hash(&env);
    assert_eq!(h0.len(), 16);
    assert!(set_kv(&env, "XR_EXPOSE", "local"));
    assert!(!set_kv(&env, "XR_EXPOSE", "invalid"));
    assert_eq!(Config::load(&env).expose(), "local");
    assert!(set_kv(&env, "VM_MEM_MB", "1024"));
    assert_ne!(h0, config_hash(&env));

    env.fake_date_fail = true;
    env.bb = tmp.path().join("nonexistent-bb");
    env.proc_uptime = tmp.path().join("uptime");
    let _ = fs::write(&env.proc_uptime, "123.45 99.0\n");
    assert_eq!(xr_now(&env), "uptime+123.45s");
    log_info(&env, "test", "nodate");
    let log_s = fs::read_to_string(env.module_log()).unwrap_or_default();
    assert!(log_s.contains("uptime+123.45s [INFO] [test] nodate"));
    env.fake_date_fail = false;

    let _ = fs::write(env.console_log(), "serial\n");
    let _ = fs::write(env.run_dir().join(".net.lock"), "old");
    env.proc_mounts = tmp.path().join("mounts");
    let _ = fs::write(&env.proc_mounts, "");
    assert!(boot_clean_run(&env));
    assert!(env.log_dir().join("console.prev.log").is_file());
    assert!(!env.run_dir().join(".net.lock").exists());

    let _ = fs::File::create(env.boot_log());
    blog(&env, "vm", "step 1");
    let cfg = Config::load(&env);
    let _ = xr_exec(
        &env,
        &cfg,
        std::path::Path::new("/bin/true"),
        &["add", "x"],
        false,
    );
    let _ = xr_exec(
        &env,
        &cfg,
        std::path::Path::new("/bin/true"),
        &["-C", "x"],
        false,
    );
    let b_s = fs::read_to_string(env.boot_log()).unwrap_or_default();
    assert!(b_s.contains("[vm] step 1"));
    assert!(b_s.contains("  $ /bin/true add x"));
    assert!(!b_s.contains("-C x"));

    env.proc_base = tmp.path().join("proc");
    env.cgroup_root = tmp.path().join("cg");
    let _ = fs::create_dir_all(env.proc_base.join("1234"));
    let _ = fs::create_dir_all(env.cgroup_root.join("system"));
    let _ = fs::create_dir_all(env.cgroup_root.join("uid_0"));
    let _ = fs::write(
        env.proc_base.join("1234/cgroup"),
        "0::/apps/uid_10383/pid_16139\n",
    );
    let _ = fs::write(env.proc_base.join("1234/oom_score_adj"), "0\n");
    let _ = fs::write(env.cgroup_root.join("cgroup.procs"), "");
    let _ = fs::write(env.cgroup_root.join("system/cgroup.procs"), "");
    let _ = fs::write(env.cgroup_root.join("uid_0/cgroup.procs"), "");
    assert!(cgroup_in_apps(&env, "1234"));
    assert!(cgroup_escape(&env, "1234"));
    assert_eq!(
        fs::read_to_string(env.cgroup_root.join("cgroup.procs")).unwrap_or_default(),
        "1234\n"
    );
    assert_eq!(
        fs::read_to_string(env.cgroup_root.join("uid_0/cgroup.procs")).unwrap_or_default(),
        ""
    );
    assert_eq!(
        fs::read_to_string(env.proc_base.join("1234/oom_score_adj")).unwrap_or_default(),
        "-1000\n"
    );
}

proptest! {
    #[test]
    fn prop_valid_ip4_roundtrips_and_never_panics(
        a in 0_u8..=255,
        b in 0_u8..=255,
        c in 0_u8..=255,
        d in 0_u8..=255,
        plen in 1_u8..=32,
    ) {
        let s = format!("{a}.{b}.{c}.{d}");
        prop_assert!(valid_ip4(&s));
        let expected = (u32::from(a) << 24) | (u32::from(b) << 16) | (u32::from(c) << 8) | u32::from(d);
        prop_assert_eq!(ip2int(&s), Some(expected));

        let cidr = format!("{s}/{plen}");
        prop_assert!(valid_cidr(&cidr));
        let net = cidr_net(&cidr).unwrap_or_default();
        prop_assert!(cidr_aligned(&net));
        prop_assert!(cidr_overlap(&cidr, &net));
    }

    #[test]
    fn prop_mask2len_only_contiguous(plen in 0_u8..=32) {
        let m = if plen == 0 { 0_u32 } else { (!0_u32) << 32_u8.saturating_sub(plen) };
        let s = format!("{}.{}.{}.{}", (m >> 24) & 255, (m >> 16) & 255, (m >> 8) & 255, m & 255);
        prop_assert_eq!(mask2len(&s), Some(plen));
    }

    #[test]
    fn prop_ip6_expand_mask_format_no_panic(
        g0 in 0_u16..=0xFFFF,
        g1 in 0_u16..=0xFFFF,
        g2 in 0_u16..=0xFFFF,
        g3 in 0_u16..=0xFFFF,
        plen in 1_u8..=128,
    ) {
        let s = format!("{g0:x}:{g1:x}:{g2:x}:{g3:x}::1");
        let exp = ip6_expand(&s).unwrap_or_default();
        prop_assert_eq!(exp.len(), 32);
        let masked = ip6_mask(&exp, plen).unwrap_or_default();
        let formatted = ip6_format(&masked).unwrap_or_default();
        let cidr = format!("{formatted}/{plen}");
        prop_assert!(cidr_aligned(&cidr));
        prop_assert!(cidr_overlap(&s, &cidr));
    }

    #[test]
    fn prop_arbitrary_strings_never_panic(raw in ".*") {
        let _ = valid_ip4(&raw);
        let _ = ip2int(&raw);
        let _ = mask2len(&raw);
        let _ = ip6_expand(&raw);
        let _ = norm_route(&raw);
        let _ = valid_cidr(&raw);
        let _ = cidr_private(&raw);
        let _ = cidr_aligned(&raw);
        let _ = hs_subnet_valid(&raw);
        let _ = mac_eui64(&raw);
    }
}
