//! 配置备份：配置文本解析、清单与 zip 往返、恶意包拒绝、逐键模拟校验与客机回执解析。

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::path::Path;

use proptest::prelude::*;
use serde_json::json;
use tempfile::TempDir;
use xr_test_support::{Failure, ensure, ensure_eq, ensure_ok};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

use super::{
    Fault, LIMITS, Limits, allowed_entry, check_manifest, parse_ack, phone_conf, plan_config,
    read_package, sha_hex, write_zip_tree,
};
use crate::config::{Config, KEYS};
use crate::env::Env;
use crate::log::tests::env_at;

/// 包条目：名字与内容。
type Entries = Vec<(&'static str, Vec<u8>)>;

/// 带引号、注释与未知键的配置文本。
const CONF: &str = "# 注释\nXR_DNS=\"223.5.5.5 119.29.29.29\"\nVM_CPUS='3'\nVM_MEM_MB=1024\nUNKNOWN_KEY=1\n  # 缩进注释\nVM_DISK_SIZE_MB=\"4096\"\nXR_MODE=\"bypass\"\n";

/// 临时环境：数据目录、内部存储目录与不存在的 `ip`（热点网段检查不读系统网卡）。
///
/// # Errors
///
/// 建目录失败时返回 [`Failure`]。
fn setup() -> Result<(TempDir, Env), Failure> {
    let tmp = ensure_ok(TempDir::new(), "临时目录")?;
    let mut env = env_at(&tmp.path().join("data"));
    env.media = tmp.path().join("media");
    env.sysbin = tmp.path().join("nobin");
    ensure_ok(fs::create_dir_all(&env.media), "建内部存储")?;
    ensure_ok(fs::create_dir_all(env.xr_data.join("config")), "建配置目录")?;
    Ok((tmp, env))
}

/// 按给定条目写一个 zip。
///
/// # Errors
///
/// 写入失败时返回 [`Failure`]。
fn make_zip(path: &Path, entries: &[(&str, Vec<u8>)]) -> Result<(), Failure> {
    let mut zw = ZipWriter::new(ensure_ok(File::create(path), "建 zip")?);
    for (name, body) in entries {
        ensure_ok(
            zw.start_file(*name, SimpleFileOptions::default()),
            "写条目头",
        )?;
        ensure_ok(zw.write_all(body), "写条目")?;
    }
    ensure_ok(zw.finish(), "收尾").map(|_file| ())
}

/// 合法包的条目：手机配置、一份 `VPN`、`OpenWrt` 备份与对应清单；`format` 为清单中的格式版本。
fn good_entries(format: u64) -> Vec<(&'static str, Vec<u8>)> {
    let files: Vec<(&'static str, Vec<u8>)> = vec![
        ("phone/config.conf", b"XR_DNS=\"1.1.1.1\"\n".to_vec()),
        ("phone/vpn/home.ovpn", b"client\n".to_vec()),
        ("phone/vpn/home.enabled", b"1\n".to_vec()),
        ("openwrt/backup.tar.gz", b"tar".to_vec()),
    ];
    let sums: serde_json::Map<String, serde_json::Value> = files
        .iter()
        .map(|(name, body)| ((*name).to_owned(), json!(sha_hex(body))))
        .collect();
    let manifest = json!({"kind": "xrouter-config", "format": format, "files": sums});
    let mut all = files;
    all.push(("manifest.json", manifest.to_string().into_bytes()));
    all
}

/// 写包并读取、检查清单。
///
/// # Errors
///
/// 写包失败时返回 [`Failure`]；内层结果为读取或检查的结果。
fn load(
    dir: &Path,
    entries: &[(&str, Vec<u8>)],
    limits: Limits,
) -> Result<Result<(), Fault>, Failure> {
    let path = dir.join("pkg.zip");
    make_zip(&path, entries)?;
    Ok(read_package(&path, limits).and_then(|files| check_manifest(&files)))
}

/// `from_conf_str` 与 `Config::load` 对同一文本逐键一致（进程环境里已设置的键除外）。
///
/// # Errors
///
/// 准备文件失败或任一键不一致时返回 [`Failure`]。
#[test]
fn conf_str_matches_load() -> Result<(), Failure> {
    let (_tmp, env) = setup()?;
    ensure_ok(fs::write(env.conf_file(), CONF), "写配置")?;
    let loaded = Config::load(&env);
    let parsed = Config::from_conf_str(CONF);
    for key in KEYS.iter().filter(|key| std::env::var(key).is_err()) {
        ensure_eq(&loaded.get_raw(key), &parsed.get_raw(key), key)?;
    }
    ensure_eq(&parsed.get("VM_CPUS"), &Some("3"), "单引号")?;
    ensure_eq(
        &parsed.get("XR_DNS"),
        &Some("223.5.5.5 119.29.29.29"),
        "双引号",
    )?;
    ensure(parsed.get("UNKNOWN_KEY").is_none(), "未知键忽略")
}

/// 导出的手机配置按 `KEYS` 顺序、去掉 `VM_DISK_SIZE_MB`，只含出现过的键。
///
/// # Errors
///
/// 输出不符时返回 [`Failure`]。
#[test]
fn phone_conf_follows_keys_order() -> Result<(), Failure> {
    ensure_eq(
        &phone_conf(CONF).as_str(),
        &"XR_DNS=\"223.5.5.5 119.29.29.29\"\nXR_MODE=\"bypass\"\nVM_CPUS=\"3\"\nVM_MEM_MB=\"1024\"\n",
        "导出配置",
    )
}

/// `write_zip_tree` 写出的包读回后文件集合与内容逐字节一致，路径分隔符为 `/`。
///
/// # Errors
///
/// 读写失败或内容不一致时返回 [`Failure`]。
#[test]
fn zip_tree_roundtrip() -> Result<(), Failure> {
    let tmp = ensure_ok(TempDir::new(), "临时目录")?;
    let root = tmp.path().join("stage");
    let want: BTreeMap<String, Vec<u8>> = good_entries(1)
        .into_iter()
        .map(|(name, body)| (name.to_owned(), body))
        .collect();
    for (name, body) in &want {
        let path = root.join(name);
        ensure_ok(fs::create_dir_all(path.parent().unwrap_or(&root)), "建目录")?;
        ensure_ok(fs::write(&path, body), "写文件")?;
    }
    let out = tmp.path().join("out.zip");
    ensure_ok(write_zip_tree(&out, &root), "打包")?;
    let mut archive = ensure_ok(
        ZipArchive::new(ensure_ok(File::open(&out), "打开")?),
        "读包",
    )?;
    let mut got = BTreeMap::new();
    for idx in 0..archive.len() {
        let mut entry = ensure_ok(archive.by_index(idx), "读条目")?;
        let mut body = Vec::new();
        let _read: usize = ensure_ok(entry.read_to_end(&mut body), "解压")?;
        ensure(
            got.insert(entry.name().to_owned(), body).is_none(),
            "条目不重复",
        )?;
    }
    ensure_eq(&got, &want, "往返一致")?;
    let read_back = read_package(&out, LIMITS).and_then(|files| check_manifest(&files));
    ensure_eq(&read_back, &Ok(()), "合法包通过检查")
}

/// 恶意或不合法的包逐一被拒绝，并给出对应文案。
///
/// # Errors
///
/// 任一包未被拒绝或文案不符时返回 [`Failure`]。
#[test]
fn rejects_bad_packages() -> Result<(), Failure> {
    let tmp = ensure_ok(TempDir::new(), "临时目录")?;
    let dir = tmp.path();
    let with = |extra: (&'static str, Vec<u8>)| {
        let mut all = good_entries(1);
        all.push(extra);
        all
    };
    let cases: Vec<(Entries, Fault)> = vec![
        (
            with(("../x", b"x".to_vec())),
            Fault::Package("条目路径越界"),
        ),
        (
            with(("/etc/passwd", b"x".to_vec())),
            Fault::Package("条目路径越界"),
        ),
        (
            with(("phone/evil.sh", b"x".to_vec())),
            Fault::Package("包含未知条目"),
        ),
        (
            with(("phone/vpn/a-b.ovpn", b"x".to_vec())),
            Fault::Package("包含未知条目"),
        ),
        (
            good_entries(1)
                .into_iter()
                .map(|(name, body)| {
                    if name == "phone/vpn/home.ovpn" {
                        (name, b"tampered\n".to_vec())
                    } else {
                        (name, body)
                    }
                })
                .collect(),
            Fault::Checksum,
        ),
        (
            good_entries(1)
                .into_iter()
                .filter(|(name, _)| *name != "openwrt/backup.tar.gz")
                .collect(),
            Fault::Package("缺少必需条目"),
        ),
        (good_entries(2), Fault::Package("格式版本不受支持")),
    ];
    for (entries, want) in cases {
        let got = load(dir, &entries, LIMITS)?;
        ensure_eq(&got, &Err(want.clone()), &want.to_string())?;
    }
    Ok(())
}

/// 条目数超过上限、解压后总量超过上限（高压缩比条目）都被拒绝。
///
/// # Errors
///
/// 未被拒绝时返回 [`Failure`]。
#[test]
fn rejects_over_limits() -> Result<(), Failure> {
    let tmp = ensure_ok(TempDir::new(), "临时目录")?;
    let many: Vec<(String, Vec<u8>)> = (0_u32..513)
        .map(|idx| (format!("phone/vpn/p{idx}.ovpn"), b"x".to_vec()))
        .collect();
    let refs: Vec<(&str, Vec<u8>)> = many
        .iter()
        .map(|(name, body)| (name.as_str(), body.clone()))
        .collect();
    ensure_eq(
        &load(tmp.path(), &refs, LIMITS)?,
        &Err(Fault::Package("条目过多")),
        "条目过多",
    )?;
    let mut big = good_entries(1);
    big.push(("phone/vpn/big.ovpn", vec![0_u8; 64 * 1024]));
    let small = Limits {
        entries: 512,
        unpacked: 16 * 1024,
    };
    ensure_eq(
        &load(tmp.path(), &big, small)?,
        &Err(Fault::Package("解压后体积超限")),
        "体积超限",
    )
}

/// 条目名白名单。
///
/// # Errors
///
/// 判断不符时返回 [`Failure`]。
#[test]
fn entry_whitelist() -> Result<(), Failure> {
    for name in [
        "manifest.json",
        "phone/config.conf",
        "openwrt/backup.tar.gz",
        "phone/vpn/home_2.ovpn",
        "phone/vpn/x.enabled",
        "phone/vpn/x.name",
    ] {
        ensure(allowed_entry(name), name)?;
    }
    for name in [
        "phone/vpn/.ovpn",
        "phone/vpn/a.b.ovpn",
        "phone/vpn/x.conf",
        "phone/vpn/sub/x.ovpn",
        "openwrt/other.tar.gz",
        "manifest.json.bak",
    ] {
        ensure(!allowed_entry(name), name)?;
    }
    Ok(())
}

/// 按 `KEYS` 顺序校验：新前缀先并入，热点网段与新前缀组合才合法时也能通过；不存在的共享目录跳过；`VM_DISK_SIZE_MB` 忽略。
///
/// # Errors
///
/// 结果不符时返回 [`Failure`]。
#[test]
fn plan_follows_keys_order() -> Result<(), Failure> {
    let (_tmp, env) = setup()?;
    let base = Config::from_conf_str("XR_NET_PREFIX=\"10.10.10\"\n");
    let missing = env.media.join("nope");
    let text = format!(
        "XR_HOTSPOT_SUBNET=\"10.10.10.1/24\"\nXR_NET_PREFIX=\"172.29.233\"\nVM_SHARE_PATH=\"{}\"\nVM_DISK_SIZE_MB=\"2048\"\nVM_CPUS=\"9\"\n",
        missing.display()
    );
    let plan = plan_config(&env, &base, &text);
    let applied: Vec<&str> = plan.applied.iter().map(|(key, _)| key.as_str()).collect();
    ensure_eq(
        &applied,
        &vec!["XR_NET_PREFIX", "XR_HOTSPOT_SUBNET"],
        "通过的键",
    )?;
    let skipped: Vec<&str> = plan.skipped.iter().map(|(key, _)| key.as_str()).collect();
    ensure_eq(&skipped, &vec!["VM_CPUS", "VM_SHARE_PATH"], "跳过的键")?;
    let alone = plan_config(&env, &base, "XR_HOTSPOT_SUBNET=\"10.10.10.1/24\"\n");
    ensure_eq(&alone.skipped.len(), &1_usize, "只有热点网段时与旧前缀冲突")
}

/// 客机回执：ts 不符或缺状态行时继续等待；ok 带其余行；err 带原因。
///
/// # Errors
///
/// 解析不符时返回 [`Failure`]。
#[test]
fn ack_parsing() -> Result<(), Failure> {
    ensure_eq(
        &parse_ack("ts=1\nok\nfw=24.10 r1\nsize=3\n", "1"),
        &Some(Ok(vec!["fw=24.10 r1".to_owned(), "size=3".to_owned()])),
        "ok",
    )?;
    ensure_eq(
        &parse_ack("ts=1\nerr 归档损坏\n", "1"),
        &Some(Err("归档损坏".to_owned())),
        "err",
    )?;
    ensure_eq(&parse_ack("ts=1\nok\n", "2"), &None, "ts 不符")?;
    ensure_eq(&parse_ack("ts=1\n", "1"), &None, "缺状态行")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_000))]

    /// 任意文本解析配置都不 panic，且只含 `KEYS` 中的键。
    #[test]
    fn conf_str_never_panics(text in "(?s).{0,400}") {
        let cfg = Config::from_conf_str(&text);
        prop_assert!(cfg.get("UNKNOWN").is_none());
        prop_assert!(KEYS.iter().all(|key| cfg.get(key).is_none_or(|val| !val.contains('\n'))));
    }
}
