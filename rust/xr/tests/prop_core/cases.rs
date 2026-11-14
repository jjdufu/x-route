//! 核心纯函数与文件副作用的回归测试，以及 `CIDR`/`IPv6` 解析的属性测试。

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use proptest::prelude::*;
use tempfile::TempDir;
use xr::cidr;
use xr::cidr::{
    hs_mac, hs_subnet_clash, hs_subnet_valid, ip2int, ip6_expand, ip6_format, ip6_mask, mac_eui64,
    mask2len, norm_route, v6_compose, v6_is_ll, v6_is_ula, v6_pick_addr, valid_ip4,
};
use xr::config;
use xr::config::{Config, KEYS, set_kv, valid_kv};
use xr::daemon::keepalive::{
    VMM_RUN_MARKER, vm_exit_reason, vm_guest_reset_pending, vmm_has_guest_reset, vmm_last_run_slice,
};
use xr::env::Env;
use xr::log;
use xr::log::{blog, boot_clean_run, xr_now};
use xr::sys::exec;
use xr::sys::{cgroup_escape, cgroup_in_apps};
use xr_test_support::{Failure, ensure, ensure_eq, ensure_ne, ensure_ok};

/// 一条 `valid_kv` 检查：键、值、期望结果。
type KvCase = (&'static str, &'static str, bool);

/// 一条字符串结果检查：说明、实际结果、期望值。
type StrCase = (&'static str, Option<String>, &'static str);

/// 建临时目录和指向它的 `Env`：数据、模块、内部存储目录都在临时目录下，并复制仓库自带的默认配置。
///
/// # Errors
///
/// 建目录或复制配置失败时返回 [`Failure`]。
fn setup_env() -> Result<(TempDir, Env), Failure> {
    let tmp = ensure_ok(TempDir::new(), "建临时目录")?;
    let mut env = Env::from_process();
    env.xr_data = tmp.path().join("data");
    env.moddir = tmp.path().join("mod");
    env.media = tmp.path().join("media");
    ensure_ok(
        fs::create_dir_all(env.media.join("DCIM")),
        "准备 media/DCIM",
    )?;
    ensure_ok(env.ensure_dirs(), "准备数据目录")?;
    let repo_conf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../module/config/config.conf");
    if repo_conf.is_file() {
        let _copied: u64 = ensure_ok(fs::copy(repo_conf, env.conf_file()), "复制默认配置")?;
    }
    Ok((tmp, env))
}

/// 拆分前这些检查跑在"取不到日期、没有 busybox"的环境之后：让 `bb` 指向不存在的文件并写入假的 uptime，保持条件一致。
///
/// # Errors
///
/// 写 uptime 文件失败时返回 [`Failure`]。
fn no_busybox(env: &mut Env, tmp: &TempDir) -> Result<(), Failure> {
    env.bb = tmp.path().join("nonexistent-bb");
    env.proc_uptime = tmp.path().join("uptime");
    ensure_ok(fs::write(&env.proc_uptime, "123.45 99.0\n"), "准备 uptime")
}

/// 逐条检查 `valid_kv` 的结果。
///
/// # Errors
///
/// 任一条结果与期望不同时返回 [`Failure`]。
fn check_kv(env: &Env, cfg: &Config, cases: &[KvCase]) -> Result<(), Failure> {
    for &(key, val, want) in cases {
        ensure_eq(
            &valid_kv(env, cfg, key, val),
            &want,
            &format!("valid_kv({key}, {val:?})"),
        )?;
    }
    Ok(())
}

/// 开关、网段前缀、MAC 与 DNS 的 `valid_kv` 检查。
const KV_FORMAT: [KvCase; 15] = [
    ("XR_CLASH_UI", "0", true),
    ("XR_CLASH_UI", "1", true),
    ("XR_CLASH_UI", "2", false),
    ("XR_NET_PREFIX", "172.29.233", true),
    ("XR_NET_PREFIX", "8.8.8", false),
    ("XR_NET_PREFIX", "172.29.256", false),
    ("XR_NET_PREFIX", "172.32.1", false),
    ("XR_NET_PREFIX", "192.168.7", true),
    ("XR_NET_PREFIX", "10.1.2", true),
    ("XR_VM_MAC", "52:54:00:1d:e9:fe", true),
    ("XR_VM_MAC", "52:54:00:1D:e9:fe", false),
    ("XR_DNS", "223.5.5.5 119.29.29.29", true),
    ("XR_DNS", "", false),
    ("XR_DNS", "1.1.1.1 2.2.2.2 3.3.3.3 4.4.4.4", false),
    ("XR_DNS", "1.1.1.300", false),
];

/// 暴露方式、模式、资源上下限与共享模式的 `valid_kv` 检查（未设置属主映射时）。
const KV_LIMITS: [KvCase; 15] = [
    ("XR_EXPOSE", "lan", true),
    ("XR_EXPOSE", "local", true),
    ("XR_EXPOSE", "hotspot", false),
    ("XR_MODE", "global", true),
    ("XR_MODE", "bypass", true),
    ("XR_MODE", "site", false),
    ("VM_CPUS", "4", true),
    ("VM_CPUS", "5", false),
    ("VM_MEM_MB", "512", true),
    ("VM_MEM_MB", "256", false),
    ("VM_MEM_MB", "99999999", false),
    ("VM_SWIOTLB_MB", "64", true),
    ("VM_SWIOTLB_MB", "48", false),
    ("VM_SHARE_MODE", "ro", true),
    ("VM_SHARE_MODE", "rw", false),
];

/// 共享目录与未知键的 `valid_kv` 检查。
const KV_SHARE_PATH: [KvCase; 4] = [
    ("VM_SHARE_PATH", "/data/adb", false),
    ("VM_SHARE_PATH", "/sdcard/../../adb", false),
    ("VM_SHARE_PATH", "/sdcard/DCIM", true),
    ("UNKNOWN", "1", false),
];

/// 配置项校验与 shell 版一致：开关、网段前缀、MAC、DNS、暴露方式、模式、资源上下限、共享目录与未知键。
///
/// # Errors
///
/// 任一检查不成立时返回 [`Failure`]。
#[test]
fn valid_kv_parity() -> Result<(), Failure> {
    let (_tmp, mut env) = setup_env()?;
    let cfg = Config::load(&env);

    ensure_eq(&KEYS.len(), &25, "配置键数量")?;
    check_kv(&env, &cfg, &KV_FORMAT)?;
    ensure(!valid_ip4("010.0.0.1"), "前导 0 的 IPv4 无效")?;
    ensure(!valid_ip4("192.168.008.1"), "前导 0 的 IPv4 无效")?;
    check_kv(&env, &cfg, &[("XR_DNS", "192.168.01.1", false)])?;
    ensure_eq(
        &ip2int("010.000.000.008"),
        &Some((10_u32 << 24_u32) | 8_u32),
        "ip2int 前导 0",
    )?;
    check_kv(&env, &cfg, &KV_LIMITS)?;

    "uidmap=0 1023 1".clone_into(&mut env.rw_map_opt);
    check_kv(&env, &cfg, &[("VM_SHARE_MODE", "rw", true)])?;
    env.rw_map_opt.clear();

    check_kv(&env, &cfg, &KV_SHARE_PATH)
}

/// `cidr::is_private` 的检查：网段与期望结果（私网、`CGNAT` 为真）。
const PRIVATE_CASES: [(&str, bool); 8] = [
    ("10.20.0.0/16", true),
    ("172.16.0.0/12", true),
    ("192.168.50.0/24", true),
    ("100.64.0.0/10", true),
    ("8.8.8.0/24", false),
    ("172.32.0.0/16", false),
    ("192.0.0.0/8", false),
    ("10.0.0.0/7", false),
];

/// `CIDR` 工具与 shell 版一致：私网判断、对齐、重叠、网络号、路由规范化、`IPv6` 展开与非法输入。
///
/// # Errors
///
/// 任一检查不成立时返回 [`Failure`]。
#[test]
fn cidr_parity() -> Result<(), Failure> {
    for (block, want) in PRIVATE_CASES {
        ensure_eq(
            &cidr::is_private(block),
            &want,
            &format!("is_private({block})"),
        )?;
    }
    ensure(cidr::is_aligned("192.168.50.0/24"), "192.168.50.0/24 对齐")?;
    ensure(
        !cidr::is_aligned("192.168.50.1/24"),
        "192.168.50.1/24 不对齐",
    )?;
    ensure(
        cidr::overlap("192.168.0.0/16", "192.168.50.0/24"),
        "包含关系视为重叠",
    )?;
    ensure(
        !cidr::overlap("192.168.50.0/24", "192.168.51.0/24"),
        "相邻网段不重叠",
    )?;
    let strings: [StrCase; 5] = [
        (
            "network",
            cidr::network("192.168.250.254/30"),
            "192.168.250.252/30",
        ),
        (
            "norm_route v4 主机",
            norm_route("10.58.88.2"),
            "10.58.88.2/32",
        ),
        (
            "norm_route 点分掩码",
            norm_route("10.8.0.0/255.255.255.0"),
            "10.8.0.0/24",
        ),
        (
            "norm_route v6 主机",
            norm_route("2001:db8::1"),
            "2001:db8::1/128",
        ),
        (
            "ip6_expand",
            ip6_expand("2001:db8::1"),
            "20010db8000000000000000000000001",
        ),
    ];
    for (what, got, want) in strings {
        ensure_eq(&got.as_deref(), &Some(want), what)?;
    }
    for bad in [
        "10.0.0.0/0",
        "2001:db8::/129",
        "2001:db8:::1",
        "10.8.0.0/255.0.255.0",
    ] {
        ensure(!cidr::is_valid(bad), &format!("{bad} 应无效"))?;
    }
    Ok(())
}

/// `EUI-64` 接口标识、热点 MAC、`IPv6` 地址拼接与选址、链路本地与 `ULA` 判断。
///
/// # Errors
///
/// 任一检查不成立时返回 [`Failure`]。
#[test]
fn ipv6_eui64() -> Result<(), Failure> {
    let mac = "52:54:00:1d:e9:fe";
    let pick1 = v6_pick_addr(
        "2409:8a00:1:2::/64",
        &["2409:8a00:1:2:1111:2222:3333:4444/64".to_owned()],
        mac,
    );
    let pick2 = v6_pick_addr(
        "2409:8a00:1:2::/64",
        &["2409:8a00:0001:0002:5054:00ff:fe1d:e9fe/64".to_owned()],
        mac,
    );
    let strings: [StrCase; 5] = [
        ("mac_eui64", mac_eui64(mac), "505400fffe1de9fe"),
        ("hs_mac", hs_mac(mac), "52:54:00:1d:e9:ff"),
        (
            "v6_compose",
            v6_compose("2409:8a00:1:2::/64", "505400fffe1de9fe"),
            "2409:8a00:0001:0002:5054:00ff:fe1d:e9fe",
        ),
        (
            "v6_pick_addr 无冲突",
            pick1,
            "2409:8a00:0001:0002:5054:00ff:fe1d:e9fe/64",
        ),
        (
            "v6_pick_addr 冲突",
            pick2,
            "2409:8a00:0001:0002:7872:00e9:0000:0254/64",
        ),
    ];
    for (what, got, want) in strings {
        ensure_eq(&got.as_deref(), &Some(want), what)?;
    }
    ensure(v6_is_ll("fe80::1"), "fe80::1 是链路本地")?;
    ensure(!v6_is_ll("2409:8a00:1:2::1"), "公网地址不是链路本地")?;
    ensure(v6_is_ula("fd7a:7872:e9::1"), "fd7a::/16 是 ULA")
}

/// 热点网段校验与冲突检测：与上游接口网段冲突时报出接口，热点自身接口的网段不算冲突。
///
/// # Errors
///
/// 准备模拟 `ip` 失败或任一检查不成立时返回 [`Failure`]。
#[test]
fn hotspot_clash() -> Result<(), Failure> {
    let (tmp, mut env) = setup_env()?;
    let sb = tmp.path().join("sb");
    ensure_ok(fs::create_dir_all(&sb), "准备 sb 目录")?;
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
    ensure_ok(fs::write(&ip_mock, script), "写模拟 ip")?;
    ensure_ok(
        fs::set_permissions(&ip_mock, fs::Permissions::from_mode(0o755)),
        "模拟 ip 可执行",
    )?;
    env.sysbin = sb;

    ensure(
        hs_subnet_valid("10.234.0.1/24"),
        "10.234.0.1/24 可作热点网段",
    )?;
    ensure(
        !hs_subnet_valid("100.64.0.1/24"),
        "100.64.0.0/10 不可作热点网段",
    )?;
    ensure_eq(
        &hs_subnet_clash(&env, "172.29.233", "192.168.31.1/24", None).as_deref(),
        &Some("wlan0 192.168.31.88/24"),
        "与 wlan0 冲突",
    )?;
    ensure(
        hs_subnet_clash(&env, "172.29.233", "10.233.0.1/24", Some("wlan2")).is_none(),
        "热点自身接口不算冲突",
    )
}

/// `set_kv` 只接受合法值并写回配置，改动参与指纹的键后配置指纹变化。
///
/// # Errors
///
/// 任一检查不成立时返回 [`Failure`]。
#[test]
fn set_kv_roundtrip() -> Result<(), Failure> {
    let (_tmp, env) = setup_env()?;
    let h0 = config::hash(&env);
    ensure_eq(&h0.len(), &16, "指纹长度")?;
    ensure(set_kv(&env, "XR_EXPOSE", "local"), "写入合法值")?;
    ensure(!set_kv(&env, "XR_EXPOSE", "invalid"), "拒绝非法值")?;
    ensure_eq(Config::load(&env).expose(), "local", "写回后读出")?;
    ensure(set_kv(&env, "VM_MEM_MB", "1024"), "写入内存大小")?;
    ensure_ne(&h0, &config::hash(&env), "指纹随配置变化")
}

/// 取不到日期时日志时间戳退回 `uptime+秒数s`，日志行照常写入。
///
/// # Errors
///
/// 任一检查不成立时返回 [`Failure`]。
#[test]
fn log_without_date() -> Result<(), Failure> {
    let (tmp, mut env) = setup_env()?;
    env.fake_date_fail = true;
    no_busybox(&mut env, &tmp)?;
    ensure_eq(
        xr_now(&env).as_str(),
        "uptime+123.45s",
        "退回 uptime 时间戳",
    )?;
    log::info(&env, "test", "nodate");
    let log_s = fs::read_to_string(env.module_log()).unwrap_or_default();
    ensure(
        log_s.contains("uptime+123.45s [INFO] [test] nodate"),
        "日志行带 uptime 时间戳",
    )
}

/// 开机清理：串口日志轮转为 `console.prev.log`，残留的网络锁被删除。
///
/// # Errors
///
/// 任一检查不成立时返回 [`Failure`]。
#[test]
fn boot_clean_rotates_console() -> Result<(), Failure> {
    let (tmp, mut env) = setup_env()?;
    no_busybox(&mut env, &tmp)?;
    ensure_ok(fs::write(env.console_log(), "serial\n"), "准备串口日志")?;
    ensure_ok(
        fs::write(env.run_dir().join(".net.lock"), "old"),
        "准备残留锁",
    )?;
    env.proc_mounts = tmp.path().join("mounts");
    ensure_ok(fs::write(&env.proc_mounts, ""), "准备 mounts")?;
    ensure(boot_clean_run(&env), "开机清理成功")?;
    ensure(
        env.log_dir().join("console.prev.log").is_file(),
        "串口日志已轮转",
    )?;
    ensure(!env.run_dir().join(".net.lock").exists(), "残留锁已删除")
}

/// 开机日志记录步骤与会改动状态的命令，只读命令（`-C` 检查）不记录。
///
/// # Errors
///
/// 任一检查不成立时返回 [`Failure`]。
#[test]
fn boot_log_records_mutating_cmds() -> Result<(), Failure> {
    let (tmp, mut env) = setup_env()?;
    no_busybox(&mut env, &tmp)?;
    let _boot_log: fs::File = ensure_ok(fs::File::create(env.boot_log()), "准备开机日志")?;
    blog(&env, "vm", "step 1");
    let cfg = Config::load(&env);
    let _add_out = exec::run_xr(&env, &cfg, Path::new("/bin/true"), &["add", "x"], false);
    let _check_out = exec::run_xr(&env, &cfg, Path::new("/bin/true"), &["-C", "x"], false);
    let b_s = fs::read_to_string(env.boot_log()).unwrap_or_default();
    ensure(b_s.contains("[vm] step 1"), "记录步骤")?;
    ensure(b_s.contains("  $ /bin/true add x"), "记录改动命令")?;
    ensure(!b_s.contains("-C x"), "不记录只读命令")
}

/// cgroup 迁移：识别应用 cgroup，写入 v2 根组（不写 v1 `uid_0`），并把 OOM 分数设为 -1000。
///
/// # Errors
///
/// 准备文件失败或任一检查不成立时返回 [`Failure`]。
#[test]
fn cgroup_escape_paths() -> Result<(), Failure> {
    let (tmp, mut env) = setup_env()?;
    no_busybox(&mut env, &tmp)?;
    env.proc_base = tmp.path().join("proc");
    env.cgroup_root = tmp.path().join("cg");
    ensure_ok(
        fs::create_dir_all(env.proc_base.join("1234")),
        "准备 proc/1234",
    )?;
    ensure_ok(
        fs::create_dir_all(env.cgroup_root.join("system")),
        "准备 cg/system",
    )?;
    ensure_ok(
        fs::create_dir_all(env.cgroup_root.join("uid_0")),
        "准备 cg/uid_0",
    )?;
    ensure_ok(
        fs::write(
            env.proc_base.join("1234/cgroup"),
            "0::/apps/uid_10383/pid_16139\n",
        ),
        "准备 cgroup",
    )?;
    ensure_ok(
        fs::write(env.proc_base.join("1234/oom_score_adj"), "0\n"),
        "准备 oom_score_adj",
    )?;
    for name in ["cgroup.procs", "system/cgroup.procs", "uid_0/cgroup.procs"] {
        ensure_ok(
            fs::write(env.cgroup_root.join(name), ""),
            "准备 cgroup.procs",
        )?;
    }
    ensure(cgroup_in_apps(&env, "1234"), "识别为应用 cgroup")?;
    ensure(cgroup_escape(&env, "1234"), "迁移成功")?;
    let read = |path: &Path| fs::read_to_string(path).unwrap_or_default();
    ensure_eq(
        read(&env.cgroup_root.join("cgroup.procs")).as_str(),
        "1234\n",
        "写入 v2 根组",
    )?;
    ensure_eq(
        read(&env.cgroup_root.join("uid_0/cgroup.procs")).as_str(),
        "",
        "不写 v1 uid_0",
    )?;
    ensure_eq(
        read(&env.proc_base.join("1234/oom_score_adj")).as_str(),
        "-1000\n",
        "OOM 分数",
    )
}

/// 僵尸态 `crosvm` 不算仍在运行；退出原因只取最后一次启动的日志；客机重启只在就绪后识别为待重启。
///
/// # Errors
///
/// 准备文件失败或任一检查不成立时返回 [`Failure`]。
#[test]
fn vm_exit_reason_zombie_and_guest_reset() -> Result<(), Failure> {
    let (tmp, mut env) = setup_env()?;
    env.proc_base = tmp.path().join("proc");

    let zdir = env.proc_base.join("4321");
    ensure_ok(fs::create_dir_all(&zdir), "准备 proc/4321")?;
    ensure_ok(
        fs::write(zdir.join("cmdline"), b"crosvm\0run\0"),
        "准备 cmdline",
    )?;
    ensure_ok(
        fs::write(
            zdir.join("stat"),
            "4321 (crosvm) Z 1 4321 4321 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 100 0 0\n",
        ),
        "准备 stat",
    )?;
    ensure_ok(
        fs::write(env.run_dir().join("crosvm.pid"), "4321\n"),
        "准备 crosvm.pid",
    )?;
    let vmm1 = format!(
        "old run fatal\n{VMM_RUN_MARKER}1000 ===\n[INFO] system reset event\n[INFO] vcpu requested reset\n[INFO] exiting with success\n"
    );
    ensure_ok(fs::write(env.vmm_log(), &vmm1), "准备 vmm.log")?;

    // 僵尸态 crosvm 不得误判为“仍在运行”，且应只截取最后一次 VMM_RUN_MARKER 之后的日志
    let reason = vm_exit_reason(&env);
    ensure(!reason.contains("仍在运行"), &format!("got: {reason}"))?;
    ensure(
        reason.contains("vcpu requested reset"),
        &format!("got: {reason}"),
    )?;
    ensure(!reason.contains("old run fatal"), &format!("got: {reason}"))?;
    ensure(vmm_has_guest_reset(&vmm1), "识别客机重启")?;

    // 启动早期（state 尚未进入 ready/stable）不视作正常客机重启，防止内核 panic=5 死循环
    ensure_ok(
        fs::write(env.run_dir().join("state"), "booting\n"),
        "写 state",
    )?;
    ensure(!vm_guest_reset_pending(&env), "启动早期不算待重启")?;

    // 已就绪或稳定运行后收到客机 reset，应识别为待重启
    ensure_ok(
        fs::write(env.run_dir().join("state"), "stable\n"),
        "写 state",
    )?;
    ensure(vm_guest_reset_pending(&env), "稳定后识别为待重启")?;

    // 新一轮启动写入新 marker 后，上一轮的 reset 记录不再串味
    let vmm2 = format!("{vmm1}{VMM_RUN_MARKER}1005 ===\n[ERROR] disk io error\n");
    ensure_ok(fs::write(env.vmm_log(), &vmm2), "追加新一轮日志")?;
    ensure_eq(
        vmm_last_run_slice(&vmm2),
        "[ERROR] disk io error\n",
        "只取最后一轮",
    )?;
    ensure(!vmm_has_guest_reset(&vmm2), "新一轮无重启记录")?;
    ensure(!vm_guest_reset_pending(&env), "新一轮不算待重启")
}

proptest! {
    /// 任意合法点分 `IPv4` 都有效、`ip2int` 往返正确；加任意前缀长度后网络号对齐且与原网段重叠。
    #[test]
    fn prop_valid_ip4_roundtrips_and_never_panics(
        oct_a in 0_u8..=255,
        oct_b in 0_u8..=255,
        oct_c in 0_u8..=255,
        oct_d in 0_u8..=255,
        plen in 1_u8..=32,
    ) {
        let addr = format!("{oct_a}.{oct_b}.{oct_c}.{oct_d}");
        prop_assert!(valid_ip4(&addr));
        let expected = (u32::from(oct_a) << 24_u32)
            | (u32::from(oct_b) << 16_u32)
            | (u32::from(oct_c) << 8_u32)
            | u32::from(oct_d);
        prop_assert_eq!(ip2int(&addr), Some(expected));

        let block = format!("{addr}/{plen}");
        prop_assert!(cidr::is_valid(&block));
        let net = cidr::network(&block).unwrap_or_default();
        prop_assert!(cidr::is_aligned(&net));
        prop_assert!(cidr::overlap(&block, &net));
    }

    /// 连续掩码都能换算回前缀长度。
    #[test]
    fn prop_mask2len_only_contiguous(plen in 0_u8..=32) {
        let mask = if plen == 0 { 0_u32 } else { (!0_u32) << 32_u8.saturating_sub(plen) };
        let dotted = format!(
            "{}.{}.{}.{}",
            (mask >> 24_u32) & 255,
            (mask >> 16_u32) & 255,
            (mask >> 8_u32) & 255,
            mask & 255
        );
        prop_assert_eq!(mask2len(&dotted), Some(plen));
    }

    /// `IPv6` 展开为 32 位十六进制，按前缀掩码后格式化得到对齐网段，且与原地址重叠。
    #[test]
    fn prop_ip6_expand_mask_format_no_panic(
        g0 in 0_u16..=0xFFFF,
        g1 in 0_u16..=0xFFFF,
        g2 in 0_u16..=0xFFFF,
        g3 in 0_u16..=0xFFFF,
        plen in 1_u8..=128,
    ) {
        let addr = format!("{g0:x}:{g1:x}:{g2:x}:{g3:x}::1");
        let exp = ip6_expand(&addr).unwrap_or_default();
        prop_assert_eq!(exp.len(), 32);
        let masked = ip6_mask(&exp, plen).unwrap_or_default();
        let formatted = ip6_format(&masked).unwrap_or_default();
        let block = format!("{formatted}/{plen}");
        prop_assert!(cidr::is_aligned(&block));
        prop_assert!(cidr::overlap(&addr, &block));
    }

    /// 任意字符串交给各解析函数都不会 panic。
    #[test]
    fn prop_arbitrary_strings_never_panic(raw in ".*") {
        let _ip4_ok = valid_ip4(&raw);
        let _ip4_int = ip2int(&raw);
        let _mask_len = mask2len(&raw);
        let _ip6_hex = ip6_expand(&raw);
        let _route = norm_route(&raw);
        let _cidr_ok = cidr::is_valid(&raw);
        let _private = cidr::is_private(&raw);
        let _aligned = cidr::is_aligned(&raw);
        let _hs_ok = hs_subnet_valid(&raw);
        let _eui64 = mac_eui64(&raw);
    }
}
