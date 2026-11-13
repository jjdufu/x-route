#![allow(clippy::format_push_string)]
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use chrono::Local;
use regex::Regex;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

use crate::config::Config;
use crate::env::Env;
use crate::log::{log_info, rotate_debug, unix_now};
use crate::sys::DirLock;

fn sys_cmd_output(bin: &Path, args: &[&str]) -> String {
    match Command::new(bin).args(args).output() {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            let err = String::from_utf8_lossy(&o.stderr);
            if !err.is_empty() {
                if !s.is_empty() && !s.ends_with('\n') {
                    s.push('\n');
                }
                s.push_str(&err);
            }
            s
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

pub fn diag_vpn_hosts(env: &Env) -> Vec<String> {
    let mut hosts = BTreeSet::new();
    let mut files = vec![
        env.xr_data.join("config/openvpn.ovpn"),
        env.run_dir().join("guest/host/openvpn.ovpn"),
    ];
    for dir in [
        env.xr_data.join("config/vpn"),
        env.run_dir().join("guest/host/vpn"),
    ] {
        if let Ok(rd) = fs::read_dir(&dir) {
            for ent in rd.flatten() {
                let p = ent.path();
                if p.extension().and_then(|s| s.to_str()) == Some("ovpn") {
                    files.push(p);
                }
            }
        }
    }
    for f in files {
        if let Ok(content) = fs::read_to_string(f) {
            for line in content.lines() {
                let mut parts = line.split_whitespace();
                if parts.next() == Some("remote") {
                    if let Some(host) = parts.next() {
                        if !host.is_empty() {
                            hosts.insert(host.to_string());
                        }
                    }
                }
            }
        }
    }
    hosts.into_iter().collect()
}

pub fn diag_redact(env: &Env, input: &str) -> String {
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
                out.push("<已隐去证书或密钥>".to_string());
            } else {
                in_pem = true;
            }
            continue;
        }
        if in_pem {
            if line.contains("-----END") {
                in_pem = false;
                out.push("<已隐去证书或密钥>".to_string());
            }
            continue;
        }

        if !in_xml_secret
            && xml_tags
                .iter()
                .any(|t| trimmed.starts_with(&format!("<{t}>")))
        {
            if xml_tags
                .iter()
                .any(|t| trimmed.contains(&format!("</{t}>")))
            {
                continue;
            }
            in_xml_secret = true;
            continue;
        }
        if in_xml_secret {
            if xml_tags
                .iter()
                .any(|t| trimmed.contains(&format!("</{t}>")))
            {
                in_xml_secret = false;
            }
            continue;
        }

        out.push(line.to_string());
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
    for host in diag_vpn_hosts(env) {
        if !host.is_empty() {
            text = text.replace(&host, "<VPN服务器>");
        }
    }
    text
}

pub fn diag_phone(env: &Env) -> String {
    let mut out = String::new();
    let ip_bin = env.sysbin_cmd("ip");
    let ipt_bin = env.sysbin_cmd("iptables");
    let ip6t_bin = env.sysbin_cmd("ip6tables");
    let iw_bin = env.sysbin_cmd("iw");
    let now = Local::now().format("%Y-%m-%d %H:%M:%S");
    out.push_str(&format!("### 手机网络快照 {now}\n"));

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
        .to_string();
    let mut tables = vec![
        "main".to_string(),
        "local_network".to_string(),
        "97".to_string(),
        "233".to_string(),
        "234".to_string(),
    ];
    for tok in up.split_whitespace() {
        tables.push(tok.to_string());
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

    let ipt_s = ipt_bin.to_string_lossy().into_owned();
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
            &format!("{ipt_s} {}", args.join(" ")),
            &ipt_bin,
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
                    for p in brd.flatten() {
                        ports.push(p.file_name().to_string_lossy().into_owned());
                    }
                }
                let ps = if ports.is_empty() {
                    "无".to_string()
                } else {
                    ports.join(" ")
                };
                out.push_str(&format!("{name}: 端口 {ps}\n"));
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
        out.push_str(&format!("{key}={}\n", val.trim()));
    }
    let mut ifaces = vec!["xr0".to_string(), "xr1".to_string(), "xrbr1".to_string()];
    for tok in up.split_whitespace() {
        ifaces.push(tok.to_string());
    }
    if let Ok(hs) = fs::read_to_string(env.run_dir().join("net.hs_if")) {
        for tok in hs.split_whitespace() {
            ifaces.push(tok.to_string());
        }
    }
    for iface in ifaces {
        for k in ["disable_ipv6", "forwarding", "proxy_ndp", "accept_ra"] {
            let p = format!("/proc/sys/net/ipv6/conf/{iface}/{k}");
            if let Ok(v) = fs::read_to_string(&p) {
                out.push_str(&format!("ipv6.{iface}.{k}={}\n", v.trim()));
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
                let info = sys_cmd_output(&iw_bin, &["dev", &n, "info"]);
                for l in info.lines() {
                    if ["Interface", "type", "ssid", "channel"]
                        .iter()
                        .any(|k| l.contains(k))
                    {
                        out.push_str(l);
                        out.push('\n');
                    }
                }
                let st = sys_cmd_output(&iw_bin, &["dev", &n, "station", "dump"]);
                for l in st.lines() {
                    if ["Station", "inactive time", "connected time", "signal:"]
                        .iter()
                        .any(|k| l.contains(k))
                    {
                        out.push_str(l);
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
    out.push_str(&format!("tether_offload_disabled={}\n", offload.trim()));
    let tether = sys_cmd_output(Path::new("dumpsys"), &["tethering"]);
    let mut tcount = 0_usize;
    for l in tether.lines() {
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
        .any(|k| l.contains(k))
        {
            out.push_str(l);
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
    for l in gp.lines() {
        let lower = l.to_ascii_lowercase();
        if lower.contains("net.dns") || lower.contains("dns") {
            out.push_str(l);
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
    out.push_str(&format!("private_dns_mode={}\n\n", pdns.trim()));

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
    for f in status_files {
        if f.is_file() {
            if let Ok(raw) = fs::read(&f) {
                let slice = raw.get(..raw.len().min(300)).unwrap_or(&[]);
                let val = String::from_utf8_lossy(slice).replace('\n', " ");
                let rel = f.strip_prefix(&run).map_or_else(
                    |_| f.to_string_lossy().into_owned(),
                    |p| p.to_string_lossy().into_owned(),
                );
                out.push_str(&format!("{rel}={val}\n"));
            }
        }
    }
    out.push('\n');
    out
}

pub fn diag_guest(env: &Env, wait_secs: u64) -> Result<String, String> {
    let pid_raw = fs::read_to_string(env.run_dir().join("crosvm.pid")).unwrap_or_default();
    let pid = pid_raw.trim();
    let host_dir = env.run_dir().join("guest/host");
    if pid.is_empty() || !crate::sys::is_pid_running(env, pid) || !host_dir.is_dir() {
        return Err("### OpenWrt 快照：服务未运行\n".to_string());
    }
    let ts = format!("{}{}", unix_now(), std::process::id());
    let _ = fs::write(host_dir.join("diag.cmd"), format!("snap {ts}\n"));
    let diag_txt = env.run_dir().join("guest/guest/diag.txt");
    let expected = format!("ts={ts}");
    let iters = wait_secs.saturating_mul(2);
    for _ in 0..iters {
        if let Ok(content) = fs::read_to_string(&diag_txt) {
            if content.lines().next().unwrap_or("") == expected {
                let now = Local::now().format("%Y-%m-%d %H:%M:%S");
                let rest: Vec<&str> = content.lines().skip(1).collect();
                return Ok(format!("### OpenWrt 快照 {now}\n{}\n", rest.join("\n")));
            }
        }
        thread::sleep(Duration::from_millis(500));
    }
    Err(format!(
        "### OpenWrt 快照：{wait_secs} 秒内无响应（旧版系统盘不支持，或服务未就绪）\n"
    ))
}

pub fn diag_snap(env: &Env, reason: &str, guest_wait_secs: u64) {
    let cfg = Config::load(env);
    if !cfg.debug_on() {
        return;
    }
    let lock_path = env.run_dir().join(".snap.lock");
    let lock = DirLock::acquire(lock_path, 600, 100, false);
    let mut raw = String::new();
    let now = Local::now().format("%Y-%m-%d %H:%M:%S");
    raw.push_str(&format!("======== {now} 快照：{reason}\n"));
    raw.push_str(&diag_phone(env));
    let state = fs::read_to_string(env.run_dir().join("state")).unwrap_or_default();
    if matches!(state.trim(), "ready" | "stable") {
        match diag_guest(env, guest_wait_secs) {
            Ok(s) | Err(s) => raw.push_str(&s),
        }
    }
    raw.push_str("======== 快照结束\n");
    let redacted = diag_redact(env, &raw);
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(env.debug_log())
    {
        let _ = writeln!(f, "{}", redacted.trim_end_matches('\n'));
    }
    drop(lock);
    rotate_debug(env);
}

pub fn diag_snap_bg(env: &Env, reason: &str) {
    let cfg = Config::load(env);
    if !cfg.debug_on() {
        return;
    }
    let bin = env.moddir.join("bin/xr");
    if bin.is_file() {
        let _ = Command::new(bin)
            .args(["internal", "diag-snap", reason])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    } else {
        diag_snap(env, reason, 10);
    }
}

pub fn diag_logcat() -> String {
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
    ];
    let filtered: Vec<&str> = out
        .lines()
        .filter(|l| {
            let low = l.to_ascii_lowercase();
            keywords.iter().any(|k| low.contains(k))
        })
        .collect();
    let skip = filtered.len().saturating_sub(4000);
    filtered.get(skip..).unwrap_or(&[]).join("\n")
}

pub fn download_dir(env: &Env) -> Option<PathBuf> {
    if let Some(d) = &env.download_override {
        if fs::create_dir_all(d).is_ok() {
            return Some(d.clone());
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
    let Ok(f) = File::create(out_zip) else {
        return false;
    };
    let mut zw = ZipWriter::new(f);
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let src_dir = root_dir.join(folder_name);
    let Ok(rd) = fs::read_dir(&src_dir) else {
        return false;
    };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for entry in entries {
        if !entry.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let zip_path = format!("{folder_name}/{name}");
        if zw.start_file(&zip_path, opts).is_err() {
            return false;
        }
        let Ok(mut rf) = File::open(&entry) else {
            return false;
        };
        let mut buf = Vec::new();
        if rf.read_to_end(&mut buf).is_err() || zw.write_all(&buf).is_err() {
            return false;
        }
    }
    zw.finish().is_ok()
}

pub fn export_pack(env: &Env, prefix: &str, is_diag: bool, status_json_str: &str) -> String {
    let Some(dest) = download_dir(env) else {
        return "{\"ok\":false,\"error\":\"no download dir\"}".to_string();
    };
    let stamp = Local::now().format("%Y%m%d-%H%M%S");
    let name = format!("{prefix}-{stamp}");
    let tmpd = env.run_dir().join(format!("export.{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmpd);
    let raw_dir = tmpd.join("raw");
    let pack_dir = tmpd.join(&name);
    if fs::create_dir_all(&raw_dir).is_err() || fs::create_dir_all(&pack_dir).is_err() {
        return "{\"ok\":false,\"error\":\"tmp\"}".to_string();
    }

    let ver = fs::read_to_string(env.moddir.join("module.prop"))
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix("version="))
        .unwrap_or("")
        .trim()
        .to_string();
    let model = sys_cmd_output(Path::new("getprop"), &["ro.product.model"]);
    let rel = sys_cmd_output(Path::new("getprop"), &["ro.build.version.release"]);
    let cfg = Config::load(env);
    let dbg_label = if cfg.debug_on() { "开" } else { "关" };
    let now = Local::now().format("%Y-%m-%d %H:%M:%S %z");
    let summary = format!(
        "XRouter {ver} / {} / Android {}\n导出时间 {now}\n调试模式 {dbg_label}\n{status_json_str}\n",
        model.trim(),
        rel.trim()
    );
    let _ = fs::write(raw_dir.join("summary.txt"), summary);

    let log_files = [
        env.module_log(),
        env.console_log(),
        env.ctl_log(),
        env.vmm_log(),
        env.xr_data.join("log/console.prev.log"),
    ];
    for f in log_files {
        if let Some(bn) = f.file_name().and_then(|s| s.to_str()) {
            let f1 = PathBuf::from(format!("{}.1", f.display()));
            if f1.is_file() {
                let _ = fs::copy(&f1, raw_dir.join(format!("{bn}.1")));
            }
            if f.is_file() {
                let _ = fs::copy(&f, raw_dir.join(bn));
            }
        }
    }

    if is_diag {
        let _ = fs::write(raw_dir.join("phone.txt"), diag_phone(env));
        let g = match diag_guest(env, 15) {
            Ok(s) | Err(s) => s,
        };
        let _ = fs::write(raw_dir.join("openwrt.txt"), g);
        let _ = fs::write(raw_dir.join("logcat.txt"), diag_logcat());
        let _ = fs::write(
            raw_dir.join("tethering.txt"),
            sys_cmd_output(Path::new("dumpsys"), &["tethering"]),
        );
        if let Ok(conf) = fs::read_to_string(env.conf_file()) {
            let filtered: Vec<&str> = conf.lines().filter(|l| !l.starts_with('#')).collect();
            let _ = fs::write(
                raw_dir.join("config.conf"),
                format!("{}\n", filtered.join("\n")),
            );
        }
        let dlog = env.debug_log();
        let dlog1 = PathBuf::from(format!("{}.1", dlog.display()));
        if dlog1.is_file() {
            let _ = fs::copy(&dlog1, raw_dir.join("debug.log.1"));
        }
        if dlog.is_file() {
            let _ = fs::copy(&dlog, raw_dir.join("debug.log"));
        }
        let nlog = env.run_dir().join("notifier.log");
        if nlog.is_file() {
            let _ = fs::copy(&nlog, raw_dir.join("notifier.log"));
        }
    }

    if let Ok(rd) = fs::read_dir(&raw_dir) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_file() {
                if let Some(bn) = p.file_name().and_then(|s| s.to_str()) {
                    let bytes = fs::read(&p).unwrap_or_default();
                    let raw_text = String::from_utf8_lossy(&bytes);
                    let red = diag_redact(env, &raw_text);
                    let _ = fs::write(pack_dir.join(bn), red);
                }
            }
        }
    }

    let out_zip = dest.join(format!("{name}.zip"));
    let _ = fs::remove_file(&out_zip);
    if !write_zip_from_dir(&out_zip, &tmpd, &name) {
        let _ = fs::remove_file(&out_zip);
        let _ = fs::remove_dir_all(&tmpd);
        return "{\"ok\":false,\"error\":\"zip\"}".to_string();
    }
    let _ = fs::remove_dir_all(&tmpd);
    let label = if is_diag { "诊断包" } else { "日志" };
    log_info(env, "xr", &format!("已导出{label} {name}.zip"));
    serde_json::json!({
        "ok": true,
        "path": out_zip.to_string_lossy(),
    })
    .to_string()
}
