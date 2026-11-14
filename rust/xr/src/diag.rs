use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use chrono::Local;
use regex::Regex;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use crate::config::Config;
use crate::env::Env;
use crate::log::FAILURES;
use crate::log::note_failure;
use crate::log::{log_info, rotate_debug, unix_now};
use crate::sys::exec::note_spawn;
use crate::sys::fsx;
use crate::sys::{LockWait, PathLock};

fn sys_cmd_output(bin: &Path, args: &[&str]) -> String {
    match Command::new(bin).args(args).output() {
        Ok(result) => {
            let mut text = String::from_utf8_lossy(&result.stdout).into_owned();
            let err = String::from_utf8_lossy(&result.stderr);
            if !err.is_empty() {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&err);
            }
            text
        }
        Err(_) => String::new(),
    }
}

fn dsec(out: &mut String, title: &str, bin: &Path, args: &[&str]) {
    out.push_str("## ");
    out.push_str(title);
    out.push('\n');
    let body = sys_cmd_output(bin, args);
    out.push_str(&body);
    if !body.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
}

#[must_use]
pub fn vpn_hosts(env: &Env) -> Vec<String> {
    let mut hosts = BTreeSet::new();
    let mut files = vec![
        env.xr_data.join("config/openvpn.ovpn"),
        env.run_dir().join("guest/host/openvpn.ovpn"),
    ];
    for dir in [
        env.xr_data.join("config/vpn"),
        env.run_dir().join("guest/host/vpn"),
    ] {
        let Ok(rd) = fs::read_dir(&dir) else {
            continue;
        };
        for ent in rd.flatten() {
            let file_path = ent.path();
            if file_path.extension().and_then(|os_str| os_str.to_str()) == Some("ovpn") {
                files.push(file_path);
            }
        }
    }
    for file in files {
        let Ok(content) = fs::read_to_string(file) else {
            continue;
        };
        for line in content.lines() {
            let mut parts = line.split_whitespace();
            if parts.next() == Some("remote")
                && let Some(host) = parts.next()
                && !host.is_empty()
            {
                hosts.extend(std::iter::once(host.to_owned()));
            }
        }
    }
    hosts.into_iter().collect()
}

#[must_use]
pub fn redact(env: &Env, input: &str) -> String {
    let mut out = Vec::new();
    let mut in_pem = false;
    let mut in_xml_secret = false;

    let xml_tags = [
        "ca",
        "cert",
        "key",
        "tls-auth",
        "tls-crypt",
        "tls-crypt-v2",
        "secret",
        "pkcs12",
        "extra-certs",
    ];

    for line in input.lines() {
        let trimmed = line.trim();
        if !in_pem && line.contains("-----BEGIN") {
            if line.contains("-----END") {
                out.push("<已隐去证书或密钥>".to_owned());
            } else {
                in_pem = true;
            }
            continue;
        }
        if in_pem {
            if line.contains("-----END") {
                in_pem = false;
                out.push("<已隐去证书或密钥>".to_owned());
            }
            continue;
        }

        if !in_xml_secret
            && xml_tags
                .iter()
                .any(|open_tag| trimmed.starts_with(&format!("<{open_tag}>")))
        {
            if xml_tags
                .iter()
                .any(|close_tag| trimmed.contains(&format!("</{close_tag}>")))
            {
                continue;
            }
            in_xml_secret = true;
            continue;
        }
        if in_xml_secret {
            if xml_tags
                .iter()
                .any(|tag| trimmed.contains(&format!("</{tag}>")))
            {
                in_xml_secret = false;
            }
            continue;
        }

        out.push(line.to_owned());
    }

    let mut text = out.join("\n");
    if input.ends_with('\n') {
        text.push('\n');
    }

    if let Ok(re_url) = Regex::new(r#"[a-zA-Z][a-zA-Z0-9+.-]*://[^\s"<>]*"#) {
        text = re_url.replace_all(&text, "<URL>").into_owned();
    }
    if let Ok(re_kv) = Regex::new(
        r#"(?i)((?:password|passwd|passphrase|pass|secret|token|psk|private_?key|preshared_?key|auth-user-pass)["']?\s*[=:]\s*)\S+"#,
    ) {
        text = re_kv.replace_all(&text, "${1}<已隐去>").into_owned();
    }
    if let Ok(re_uci) = Regex::new(r"(option\s+(?:password|passwd|key|secret|token|psk)\s+)\S+") {
        text = re_uci.replace_all(&text, "${1}<已隐去>").into_owned();
    }
    if let Ok(re_af) = Regex::new(r"\[AF_INET6?\]\S*") {
        text = re_af
            .replace_all(&text, "[AF_INET]<VPN服务器>")
            .into_owned();
    }
    for host in vpn_hosts(env) {
        if !host.is_empty() {
            text = text.replace(&host, "<VPN服务器>");
        }
    }
    text
}

#[must_use]
pub fn phone(env: &Env) -> String {
    let mut out = String::new();
    let ip_bin = env.sysbin_cmd("ip");
    let iptables_bin = env.sysbin_cmd("iptables");
    let ip6t_bin = env.sysbin_cmd("ip6tables");
    let wifi_bin = env.sysbin_cmd("iw");
    let now = Local::now().format("%Y-%m-%d %H:%M:%S");
    out.push_str("### 手机网络快照 ");
    out.push_str(&now.to_string());
    out.push('\n');

    let ip_s = ip_bin.to_string_lossy().into_owned();
    dsec(
        &mut out,
        &format!("{ip_s} -4 rule"),
        &ip_bin,
        &["-4", "rule"],
    );
    dsec(
        &mut out,
        &format!("{ip_s} -6 rule"),
        &ip_bin,
        &["-6", "rule"],
    );

    let up = fs::read_to_string(env.run_dir().join("net.up_if"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    let mut tables = vec![
        "main".to_owned(),
        "local_network".to_owned(),
        "97".to_owned(),
        "233".to_owned(),
        "234".to_owned(),
    ];
    for tok in up.split_whitespace() {
        tables.push(tok.to_owned());
    }
    for tbl in &tables {
        dsec(
            &mut out,
            &format!("{ip_s} -4 route show table {tbl}"),
            &ip_bin,
            &["-4", "route", "show", "table", tbl],
        );
        dsec(
            &mut out,
            &format!("{ip_s} -6 route show table {tbl}"),
            &ip_bin,
            &["-6", "route", "show", "table", tbl],
        );
    }
    dsec(
        &mut out,
        &format!("{ip_s} -br addr"),
        &ip_bin,
        &["-br", "addr"],
    );
    dsec(
        &mut out,
        &format!("{ip_s} -br link"),
        &ip_bin,
        &["-br", "link"],
    );
    dsec(
        &mut out,
        &format!("{ip_s} -6 neigh show proxy"),
        &ip_bin,
        &["-6", "neigh", "show", "proxy"],
    );
    dsec(&mut out, &format!("{ip_s} neigh"), &ip_bin, &["neigh"]);

    let iptables_path = iptables_bin.to_string_lossy().into_owned();
    for args in [
        &["-w", "-S", "FORWARD"][..],
        &["-w", "-S", "XR_FWD"],
        &["-w", "-nvxL", "XR_FWD"],
        &["-w", "-t", "nat", "-S", "POSTROUTING"],
        &["-w", "-t", "nat", "-S", "XR_NAT"],
        &["-w", "-S", "OUTPUT"],
        &["-w", "-S", "XR_HS"],
    ] {
        dsec(
            &mut out,
            &format!("{iptables_path} {}", args.join(" ")),
            &iptables_bin,
            args,
        );
    }

    let ip6t_s = ip6t_bin.to_string_lossy().into_owned();
    for args in [
        &["-w", "-S", "FORWARD"][..],
        &["-w", "-S", "XR_FWD6"],
        &["-w", "-nvxL", "XR_FWD6"],
        &["-w", "-S", "OUTPUT"],
        &["-w", "-S", "XR_HS6"],
    ] {
        dsec(
            &mut out,
            &format!("{ip6t_s} {}", args.join(" ")),
            &ip6t_bin,
            args,
        );
    }

    out.push_str("## 网桥\n");
    if let Ok(rd) = fs::read_dir(&env.sysnet) {
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().into_owned();
            if name.starts_with("xrbr") && ent.path().is_dir() {
                let mut ports = Vec::new();
                if let Ok(brd) = fs::read_dir(ent.path().join("brif")) {
                    for entry in brd.flatten() {
                        ports.push(entry.file_name().to_string_lossy().into_owned());
                    }
                }
                let ps = if ports.is_empty() {
                    "无".to_owned()
                } else {
                    ports.join(" ")
                };
                out.push_str(&name);
                out.push_str(": 端口 ");
                out.push_str(&ps);
                out.push('\n');
            }
        }
    }
    out.push('\n');

    out.push_str("## sysctl\n");
    for path in [
        "/proc/sys/net/ipv4/ip_forward",
        "/proc/sys/net/ipv6/conf/all/forwarding",
    ] {
        let val = fs::read_to_string(path).unwrap_or_default();
        let key = path.strip_prefix("/proc/sys/").unwrap_or(path);
        out.push_str(key);
        out.push('=');
        out.push_str(val.trim());
        out.push('\n');
    }
    let mut ifaces = vec!["xr0".to_owned(), "xr1".to_owned(), "xrbr1".to_owned()];
    for tok in up.split_whitespace() {
        ifaces.push(tok.to_owned());
    }
    if let Ok(hs) = fs::read_to_string(env.run_dir().join("net.hs_if")) {
        for tok in hs.split_whitespace() {
            ifaces.push(tok.to_owned());
        }
    }
    for iface in ifaces {
        for sysctl_key in ["disable_ipv6", "forwarding", "proxy_ndp", "accept_ra"] {
            let target = format!("/proc/sys/net/ipv6/conf/{iface}/{sysctl_key}");
            if let Ok(value) = fs::read_to_string(&target) {
                out.push_str("ipv6.");
                out.push_str(&iface);
                out.push('.');
                out.push_str(sysctl_key);
                out.push('=');
                out.push_str(value.trim());
                out.push('\n');
            }
        }
    }
    out.push('\n');

    out.push_str("## 热点\n");
    if let Ok(rd) = fs::read_dir(&env.sysnet) {
        for ent in rd.flatten() {
            let path = ent.path();
            if path.join("wireless").is_dir() || path.join("phy80211").is_dir() {
                let n = ent.file_name().to_string_lossy().into_owned();
                let info = sys_cmd_output(&wifi_bin, &["dev", &n, "info"]);
                for info_line in info.lines() {
                    if ["Interface", "type", "ssid", "channel"]
                        .iter()
                        .any(|needle| info_line.contains(needle))
                    {
                        out.push_str(info_line);
                        out.push('\n');
                    }
                }
                let st = sys_cmd_output(&wifi_bin, &["dev", &n, "station", "dump"]);
                for entry_line in st.lines() {
                    if ["Station", "inactive time", "connected time", "signal:"]
                        .iter()
                        .any(|needle_key| entry_line.contains(needle_key))
                    {
                        out.push_str(entry_line);
                        out.push('\n');
                    }
                }
            }
        }
    }
    let offload = sys_cmd_output(
        Path::new("settings"),
        &["get", "global", "tether_offload_disabled"],
    );
    out.push_str("tether_offload_disabled=");
    out.push_str(offload.trim());
    out.push('\n');
    let tether = sys_cmd_output(Path::new("dumpsys"), &["tethering"]);
    let mut tcount = 0_usize;
    for row in tether.lines() {
        if [
            " - ",
            "Tethering",
            "Upstream",
            "upstream",
            "offload",
            "Offload",
            "state",
            "State",
        ]
        .iter()
        .any(|name_key| row.contains(name_key))
        {
            out.push_str(row);
            out.push('\n');
            tcount = tcount.saturating_add(1);
            if tcount >= 60 {
                break;
            }
        }
    }
    out.push('\n');

    out.push_str("## DNS\n");
    let gp = sys_cmd_output(Path::new("getprop"), &[]);
    let mut dcount = 0_usize;
    for line in gp.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("net.dns") || lower.contains("dns") {
            out.push_str(line);
            out.push('\n');
            dcount = dcount.saturating_add(1);
            if dcount >= 20 {
                break;
            }
        }
    }
    let pdns = sys_cmd_output(
        Path::new("settings"),
        &["get", "global", "private_dns_mode"],
    );
    out.push_str("private_dns_mode=");
    out.push_str(pdns.trim());
    out.push('\n');
    out.push('\n');

    out.push_str("## 运行状态\n");
    let run = env.run_dir();
    let mut status_files = vec![
        run.join("state"),
        run.join("guest/guest/ipv6"),
        run.join("guest/guest/openvpn.status"),
        run.join("guest/guest/hotspot"),
    ];
    if let Ok(rd) = fs::read_dir(&run) {
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().into_owned();
            if name.starts_with("net.") && ent.path().is_file() {
                status_files.push(ent.path());
            }
        }
    }
    for file in status_files {
        if file.is_file()
            && let Ok(raw) = fs::read(&file)
        {
            let slice = raw.get(..raw.len().min(300)).unwrap_or(&[]);
            let val = String::from_utf8_lossy(slice).replace('\n', " ");
            let rel = file.strip_prefix(&run).map_or_else(
                |_| file.to_string_lossy().into_owned(),
                |file_path| file_path.to_string_lossy().into_owned(),
            );
            out.push_str(&rel);
            out.push('=');
            out.push_str(&val);
            out.push('\n');
        }
    }
    out.push('\n');
    out
}

pub fn guest(env: &Env, wait_secs: u64) -> Result<String, String> {
    let pid_raw = fs::read_to_string(env.run_dir().join("crosvm.pid")).unwrap_or_default();
    let pid = pid_raw.trim();
    let host_dir = env.run_dir().join("guest/host");
    if pid.is_empty() || !crate::sys::is_pid_running(env, pid) || !host_dir.is_dir() {
        return Err("### OpenWrt 快照：服务未运行\n".to_owned());
    }
    let ts = format!("{}{}", unix_now(), std::process::id());
    fsx::write_logged(env, host_dir.join("diag.cmd"), format!("snap {ts}\n"));
    let diag_txt = env.run_dir().join("guest/guest/diag.txt");
    let expected = format!("ts={ts}");
    let iters = wait_secs.saturating_mul(2);
    for _ in 0..iters {
        if let Ok(content) = fs::read_to_string(&diag_txt)
            && content.lines().next().unwrap_or("") == expected
        {
            let now = Local::now().format("%Y-%m-%d %H:%M:%S");
            let rest: Vec<&str> = content.lines().skip(1).collect();
            return Ok(format!("### OpenWrt 快照 {now}\n{}\n", rest.join("\n")));
        }
        thread::sleep(Duration::from_millis(500));
    }
    Err(format!(
        "### OpenWrt 快照：{wait_secs} 秒内无响应（旧版系统盘不支持，或服务未就绪）\n"
    ))
}

pub fn snap(env: &Env, reason: &str, guest_wait_secs: u64) {
    let cfg = Config::load(env);
    if !cfg.debug_on() {
        return;
    }
    let lock_path = env.run_dir().join(".snap.lock");
    let lock = PathLock::acquire(
        env,
        lock_path,
        LockWait {
            max_iters: 600,
            sleep_ms: 100,
            check_pid: false,
        },
    );
    let mut raw = String::new();
    let now = Local::now().format("%Y-%m-%d %H:%M:%S");
    raw.push_str("======== ");
    raw.push_str(&now.to_string());
    raw.push_str(" 快照：");
    raw.push_str(reason);
    raw.push('\n');
    raw.push_str(&phone(env));
    let state = fs::read_to_string(env.run_dir().join("state")).unwrap_or_default();
    if matches!(state.trim(), "ready" | "stable") {
        match guest(env, guest_wait_secs) {
            Ok(text) | Err(text) => raw.push_str(&text),
        }
    }
    raw.push_str("======== 快照结束\n");
    let redacted = redact(env, &raw);
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(env.debug_log())
        && writeln!(file, "{}", redacted.trim_end_matches('\n')).is_err()
    {
        // 写的就是调试日志本身：失败只计数，不再写日志
        note_failure(&FAILURES.log);
    }
    drop(lock);
    rotate_debug(env);
}

pub fn snap_bg(env: &Env, reason: &str) {
    let cfg = Config::load(env);
    if !cfg.debug_on() {
        return;
    }
    let bin = env.moddir.join("bin/xr");
    if bin.is_file() {
        let child = Command::new(bin)
            .args(["internal", "diag-snap", reason])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        note_spawn(env, "diag-snap", child);
    } else {
        snap(env, reason, 10);
    }
}

#[must_use]
pub fn logcat() -> String {
    let out = sys_cmd_output(
        Path::new("logcat"),
        &["-d", "-v", "threadtime", "-t", "30000"],
    );
    let keywords = [
        "tethering",
        "ipserver",
        "hostapd",
        "softap",
        "netd",
        "networkstack",
        "dhcpserver",
        "bpfcoordinator",
        "offloadcontroller",
        "routeradvert",
        "connectivityservice",
        "wifiservice",
        "xrouter",
        "xrnotify",
        "chromium",
        "webview",
        "console",
        "sukisu",
    ];
    let filtered: Vec<&str> = out
        .lines()
        .filter(|line| {
            let low = line.to_ascii_lowercase();
            keywords.iter().any(|key| low.contains(key))
        })
        .collect();
    let skip = filtered.len().saturating_sub(4000);
    filtered.get(skip..).unwrap_or(&[]).join("\n")
}

#[must_use]
pub fn download_dir(env: &Env) -> Option<PathBuf> {
    if let Some(dir) = &env.download_override {
        if fs::create_dir_all(dir).is_ok() {
            return Some(dir.clone());
        }
        return None;
    }
    let p1 = Path::new("/data/media/0/Download");
    if p1.is_dir() {
        return Some(p1.to_path_buf());
    }
    let p2 = Path::new("/sdcard/Download");
    if p2.is_dir() {
        return Some(p2.to_path_buf());
    }
    if fs::create_dir_all(p1).is_ok() {
        return Some(p1.to_path_buf());
    }
    None
}

fn write_zip_from_dir(out_zip: &Path, root_dir: &Path, folder_name: &str) -> bool {
    let Ok(file) = File::create(out_zip) else {
        return false;
    };
    let mut zw = ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let src_dir = root_dir.join(folder_name);
    let Ok(rd) = fs::read_dir(&src_dir) else {
        return false;
    };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|dir_entry| dir_entry.path()).collect();
    entries.sort();
    for entry in entries {
        if !entry.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().and_then(|os_str| os_str.to_str()) else {
            continue;
        };
        let zip_path = format!("{folder_name}/{name}");
        if zw.start_file(&zip_path, opts).is_err() {
            return false;
        }
        let Ok(buf) = fs::read(&entry) else {
            return false;
        };
        if zw.write_all(&buf).is_err() {
            return false;
        }
    }
    zw.finish().is_ok()
}

#[must_use]
pub fn export_pack(env: &Env, prefix: &str, is_diag: bool, status_json_str: &str) -> String {
    let Some(dest) = download_dir(env) else {
        return "{\"ok\":false,\"error\":\"no download dir\"}".to_owned();
    };
    let stamp = Local::now().format("%Y%m%d-%H%M%S");
    let name = format!("{prefix}-{stamp}");
    let tmpd = env.run_dir().join(format!("export.{}", std::process::id()));
    fsx::remove_tree_logged(env, &tmpd);
    let raw_dir = tmpd.join("raw");
    let pack_dir = tmpd.join(&name);
    if fs::create_dir_all(&raw_dir).is_err() || fs::create_dir_all(&pack_dir).is_err() {
        return "{\"ok\":false,\"error\":\"tmp\"}".to_owned();
    }

    let ver = fs::read_to_string(env.moddir.join("module.prop"))
        .unwrap_or_default()
        .lines()
        .find_map(|row| row.strip_prefix("version="))
        .unwrap_or("")
        .trim()
        .to_owned();
    let model = sys_cmd_output(Path::new("getprop"), &["ro.product.model"]);
    let rel = sys_cmd_output(Path::new("getprop"), &["ro.build.version.release"]);
    let cfg = Config::load(env);
    let dbg_label = if cfg.debug_on() { "开" } else { "关" };
    let now = Local::now().format("%Y-%m-%d %H:%M:%S %z");
    let mut summary = format!(
        "XRouter {ver} / {} / Android {}\n导出时间 {now}\n调试模式 {dbg_label}\n{status_json_str}\n",
        model.trim(),
        rel.trim()
    );
    summary.push_str(&crate::log::failures_summary_line());
    fsx::write_logged(env, raw_dir.join("summary.txt"), summary);

    let log_files = [
        env.module_log(),
        env.console_log(),
        env.ctl_log(),
        env.vmm_log(),
        env.xr_data.join("log/console.prev.log"),
    ];
    for file in log_files {
        if let Some(bn) = file.file_name().and_then(|raw_name| raw_name.to_str()) {
            let f1 = PathBuf::from(format!("{}.1", file.display()));
            if f1.is_file() {
                fsx::copy_logged(env, &f1, raw_dir.join(format!("{bn}.1")));
            }
            if file.is_file() {
                fsx::copy_logged(env, &file, raw_dir.join(bn));
            }
        }
    }

    if is_diag {
        fsx::write_logged(env, raw_dir.join("phone.txt"), phone(env));
        let guest = match guest(env, 15) {
            Ok(text) | Err(text) => text,
        };
        fsx::write_logged(env, raw_dir.join("openwrt.txt"), guest);
        fsx::write_logged(env, raw_dir.join("logcat.txt"), logcat());
        fsx::write_logged(
            env,
            raw_dir.join("tethering.txt"),
            sys_cmd_output(Path::new("dumpsys"), &["tethering"]),
        );
        if let Ok(conf) = fs::read_to_string(env.conf_file()) {
            let filtered: Vec<&str> = conf.lines().filter(|line| !line.starts_with('#')).collect();
            fsx::write_logged(
                env,
                raw_dir.join("config.conf"),
                format!("{}\n", filtered.join("\n")),
            );
        }
        let dlog = env.debug_log();
        let dlog1 = PathBuf::from(format!("{}.1", dlog.display()));
        if dlog1.is_file() {
            fsx::copy_logged(env, &dlog1, raw_dir.join("debug.log.1"));
        }
        if dlog.is_file() {
            fsx::copy_logged(env, &dlog, raw_dir.join("debug.log"));
        }
        let nlog = env.run_dir().join("notifier.log");
        if nlog.is_file() {
            fsx::copy_logged(env, &nlog, raw_dir.join("notifier.log"));
        }
    }

    if let Ok(rd) = fs::read_dir(&raw_dir) {
        for ent in rd.flatten() {
            let file_path = ent.path();
            if file_path.is_file()
                && let Some(bn) = file_path.file_name().and_then(|os_str| os_str.to_str())
            {
                let bytes = fs::read(&file_path).unwrap_or_default();
                let raw_text = String::from_utf8_lossy(&bytes);
                let red = redact(env, &raw_text);
                fsx::write_logged(env, pack_dir.join(bn), red);
            }
        }
    }

    let out_zip = dest.join(format!("{name}.zip"));
    fsx::remove_logged(env, &out_zip);
    if !write_zip_from_dir(&out_zip, &tmpd, &name) {
        fsx::remove_logged(env, &out_zip);
        fsx::remove_tree_logged(env, &tmpd);
        return "{\"ok\":false,\"error\":\"zip\"}".to_owned();
    }
    fsx::remove_tree_logged(env, &tmpd);
    let label = if is_diag { "诊断包" } else { "日志" };
    log_info(env, "xr", &format!("已导出{label} {name}.zip"));
    serde_json::json!({
        "ok": true,
        "path": out_zip.to_string_lossy(),
    })
    .to_string()
}
