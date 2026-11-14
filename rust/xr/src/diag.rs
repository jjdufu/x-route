//! 诊断与日志导出：采集手机与客机的网络快照、脱敏后写入调试日志，以及打包导出日志或诊断包。

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use regex::Regex;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use crate::config::Config;
use crate::env::Env;
use crate::log;
use crate::log::FAILURES;
use crate::log::note_failure;
use crate::log::{local_now, rotate_debug, unix_now};
use crate::sys::exec::note_spawn;
use crate::sys::fsx;
use crate::sys::{LockWait, PathLock};

/// 运行命令并返回标准输出加标准错误的文本；无法运行时返回空串。
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

/// 向 `out` 追加一节 `## 标题` 加命令输出。
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

/// 从 `VPN` 配置中收集 `remote` 主机名（含旧版单文件与下发给客机的副本），供脱敏时替换。
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

/// 视为密钥的 `OpenVPN` 内联标签：`<ca>…</ca>` 等整段隐去。
const SECRET_TAGS: [&str; 9] = [
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

/// 逐行跟踪是否处在证书/密钥块或内联密钥段中。
#[derive(Default)]
struct SecretBlocks {
    /// 处在 `-----BEGIN` … `-----END` 之间。
    in_pem: bool,
    /// 处在 `<标签>` … `</标签>` 之间。
    in_xml_secret: bool,
}

impl SecretBlocks {
    /// 证书与密钥块：块开始或在块内时返回 true（该行不保留）；单行块与块结束时在 `out` 放一个占位行。
    fn pem_line(&mut self, line: &str, out: &mut Vec<String>) -> bool {
        if !self.in_pem && line.contains("-----BEGIN") {
            if line.contains("-----END") {
                out.push("<已隐去证书或密钥>".to_owned());
            } else {
                self.in_pem = true;
            }
            return true;
        }
        if !self.in_pem {
            return false;
        }
        if line.contains("-----END") {
            self.in_pem = false;
            out.push("<已隐去证书或密钥>".to_owned());
        }
        true
    }

    /// 内联密钥段：段开始或在段内时返回 true（该行不保留，也不放占位行）。
    fn xml_line(&mut self, trimmed: &str) -> bool {
        let closes = || {
            SECRET_TAGS
                .iter()
                .any(|tag| trimmed.contains(&format!("</{tag}>")))
        };
        if !self.in_xml_secret
            && SECRET_TAGS
                .iter()
                .any(|open_tag| trimmed.starts_with(&format!("<{open_tag}>")))
        {
            if !closes() {
                self.in_xml_secret = true;
            }
            return true;
        }
        if !self.in_xml_secret {
            return false;
        }
        if closes() {
            self.in_xml_secret = false;
        }
        true
    }
}

/// 去掉证书与密钥块（换成占位行）和 `OpenVPN` 内联密钥段，输入以换行结尾时输出也保留末尾换行。
fn strip_secret_blocks(input: &str) -> String {
    let mut out = Vec::new();
    let mut state = SecretBlocks::default();
    for line in input.lines() {
        if state.pem_line(line, &mut out) || state.xml_line(line.trim()) {
            continue;
        }
        out.push(line.to_owned());
    }
    let mut text = out.join("\n");
    if input.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// 按正则隐去网址、密码类键值、`UCI` 密码选项，并把 `[AF_INET]`/`[AF_INET6]` 后的地址换成占位符。
fn mask_patterns(mut text: String) -> String {
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
    text
}

/// 把 `VPN` 配置里的服务器地址换成占位符。
fn mask_vpn_hosts(env: &Env, mut text: String) -> String {
    for host in vpn_hosts(env) {
        if !host.is_empty() {
            text = text.replace(&host, "<VPN服务器>");
        }
    }
    text
}

/// 脱敏：隐去证书与密钥块、`OpenVPN` 内联密钥段、网址、密码类键值和 `VPN` 服务器地址。
#[must_use]
pub fn redact(env: &Env, input: &str) -> String {
    mask_vpn_hosts(env, mask_patterns(strip_secret_blocks(input)))
}

/// 网桥目录 `br` 下 `brif` 中的端口名，以空格连接；没有端口时为"无"。
fn bridge_ports(br: &Path) -> String {
    let mut ports = Vec::new();
    if let Ok(brd) = fs::read_dir(br.join("brif")) {
        for entry in brd.flatten() {
            ports.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    if ports.is_empty() {
        "无".to_owned()
    } else {
        ports.join(" ")
    }
}

/// 把 `text` 中含任一 `needles` 的行依次追加到 `out`。
fn push_lines_with(out: &mut String, text: &str, needles: &[&str]) {
    for line in text.lines() {
        if needles.iter().any(|needle| line.contains(needle)) {
            out.push_str(line);
            out.push('\n');
        }
    }
}

/// 无线接口 `n` 的 `iw dev info` 摘要（接口、类型、`SSID`、信道）与 `station dump` 摘要（设备、空闲与连接时长、信号）。
fn diag_wireless(out: &mut String, wifi_bin: &Path, n: &str) {
    let info = sys_cmd_output(wifi_bin, &["dev", n, "info"]);
    push_lines_with(out, &info, &["Interface", "type", "ssid", "channel"]);
    let st = sys_cmd_output(wifi_bin, &["dev", n, "station", "dump"]);
    push_lines_with(
        out,
        &st,
        &["Station", "inactive time", "connected time", "signal:"],
    );
}

/// 快照的路由部分：`ip -4/-6 rule`、固定表与上游表的 `IPv4`/`IPv6` 路由、接口地址与链路、邻居与代理邻居。
fn diag_routes(out: &mut String, ip_bin: &Path, up: &str) {
    let ip_s = ip_bin.to_string_lossy().into_owned();
    dsec(out, &format!("{ip_s} -4 rule"), ip_bin, &["-4", "rule"]);
    dsec(out, &format!("{ip_s} -6 rule"), ip_bin, &["-6", "rule"]);

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
            out,
            &format!("{ip_s} -4 route show table {tbl}"),
            ip_bin,
            &["-4", "route", "show", "table", tbl],
        );
        dsec(
            out,
            &format!("{ip_s} -6 route show table {tbl}"),
            ip_bin,
            &["-6", "route", "show", "table", tbl],
        );
    }
    dsec(out, &format!("{ip_s} -br addr"), ip_bin, &["-br", "addr"]);
    dsec(out, &format!("{ip_s} -br link"), ip_bin, &["-br", "link"]);
    dsec(
        out,
        &format!("{ip_s} -6 neigh show proxy"),
        ip_bin,
        &["-6", "neigh", "show", "proxy"],
    );
    dsec(out, &format!("{ip_s} neigh"), ip_bin, &["neigh"]);
}

/// 快照的防火墙部分：`iptables` 与 `ip6tables` 中本模块相关的链。
fn diag_iptables(out: &mut String, iptables_bin: &Path, ip6t_bin: &Path) {
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
            out,
            &format!("{iptables_path} {}", args.join(" ")),
            iptables_bin,
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
        dsec(out, &format!("{ip6t_s} {}", args.join(" ")), ip6t_bin, args);
    }
}

/// 快照的"网桥"一节：每座 `xrbr*` 网桥及其端口。
fn diag_bridges(out: &mut String, env: &Env) {
    out.push_str("## 网桥\n");
    if let Ok(rd) = fs::read_dir(&env.sysnet) {
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().into_owned();
            if !name.starts_with("xrbr") || !ent.path().is_dir() {
                continue;
            }
            out.push_str(&name);
            out.push_str(": 端口 ");
            out.push_str(&bridge_ports(&ent.path()));
            out.push('\n');
        }
    }
    out.push('\n');
}

/// 快照的"sysctl"一节：转发开关，以及本模块接口、上游与热点接口的 `IPv6` 参数。
fn diag_sysctl(out: &mut String, env: &Env, up: &str) {
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
}

/// 快照的"热点"一节：无线接口信息与已连设备、网络共享卸载开关与 `dumpsys tethering` 摘要（最多 60 行）。
fn diag_hotspot(out: &mut String, env: &Env, wifi_bin: &Path) {
    out.push_str("## 热点\n");
    if let Ok(rd) = fs::read_dir(&env.sysnet) {
        for ent in rd.flatten() {
            let path = ent.path();
            if path.join("wireless").is_dir() || path.join("phy80211").is_dir() {
                let n = ent.file_name().to_string_lossy().into_owned();
                diag_wireless(out, wifi_bin, &n);
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
}

/// 快照的"DNS"一节：含 `dns` 的系统属性（最多 20 行）与私人 DNS 模式。
fn diag_dns(out: &mut String) {
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
}

/// 快照的"运行状态"一节：状态、客机文件与 `run/net.*`，每个最多 300 字节、换行换成空格。
fn diag_run_state(out: &mut String, env: &Env) {
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
}

/// 手机网络快照：路由规则与路由表、`iptables`/`ip6tables` 规则、接口与无线状态等命令输出。
#[must_use]
pub fn phone(env: &Env) -> String {
    let mut out = String::new();
    let ip_bin = env.sysbin_cmd("ip");
    let iptables_bin = env.sysbin_cmd("iptables");
    let ip6t_bin = env.sysbin_cmd("ip6tables");
    let wifi_bin = env.sysbin_cmd("iw");
    let now = local_now().format("%Y-%m-%d %H:%M:%S");
    out.push_str("### 手机网络快照 ");
    out.push_str(&now.to_string());
    out.push('\n');

    let up = fs::read_to_string(env.run_dir().join("net.up_if"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    diag_routes(&mut out, &ip_bin, &up);
    diag_iptables(&mut out, &iptables_bin, &ip6t_bin);
    diag_bridges(&mut out, env);
    diag_sysctl(&mut out, env, &up);
    diag_hotspot(&mut out, env, &wifi_bin);
    diag_dns(&mut out);
    diag_run_state(&mut out, env);
    out
}

/// 向 `OpenWrt` 客体请求一次诊断快照，最多等待 `wait_secs` 秒。
///
/// # Errors
///
/// 虚拟机未运行或共享目录不存在、或在等待时间内客体没有回应时，返回写入诊断包的说明文字。
pub(crate) fn guest(env: &Env, wait_secs: u64) -> Result<String, String> {
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
            let now = local_now().format("%Y-%m-%d %H:%M:%S");
            let rest: Vec<&str> = content.lines().skip(1).collect();
            return Ok(format!("### OpenWrt 快照 {now}\n{}\n", rest.join("\n")));
        }
        thread::sleep(Duration::from_millis(500));
    }
    Err(format!(
        "### OpenWrt 快照：{wait_secs} 秒内无响应（旧版系统盘不支持，或服务未就绪）\n"
    ))
}

/// 调试模式开启时采集一次快照（手机，以及就绪时的客机）并脱敏追加到 `debug.log`，然后按大小轮转。
///
/// `guest_wait_secs` 为等待客机应答的最长秒数；同一时刻只有一个快照在写。
pub(crate) fn snap(env: &Env, reason: &str, guest_wait_secs: u64) {
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
    let now = local_now().format("%Y-%m-%d %H:%M:%S");
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

/// 调试模式开启时在后台采集快照（`xr internal diag-snap`）；找不到 `bin/xr` 时在当前进程直接采集。
pub(crate) fn snap_bg(env: &Env, reason: &str) {
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

/// 与网络、热点、本模块和界面相关的 `logcat` 行（关键字过滤，最多最后 4000 行）。
#[must_use]
pub(crate) fn logcat() -> String {
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

/// 导出目录：测试覆盖目录，或 `/data/media/0/Download`、`/sdcard/Download`，都没有时尝试创建前者。
#[must_use]
pub(crate) fn download_dir(env: &Env) -> Option<PathBuf> {
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

/// 把 `root_dir/folder_name` 下的文件打包为 `out_zip`（压缩包内保留 `folder_name/` 目录），成功返回 true。
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

/// 模块版本：`module.prop` 中 `version=` 的值，读不到时为空串。
#[must_use]
pub(crate) fn module_version(env: &Env) -> String {
    fs::read_to_string(env.moddir.join("module.prop"))
        .unwrap_or_default()
        .lines()
        .find_map(|row| row.strip_prefix("version="))
        .unwrap_or("")
        .trim()
        .to_owned()
}

/// 写导出包的 `summary.txt`：版本、机型与安卓版本，导出时间，调试模式，状态 JSON 与失败计数。
fn write_summary(env: &Env, raw_dir: &Path, status_json_str: &str) {
    let ver = module_version(env);
    let model = sys_cmd_output(Path::new("getprop"), &["ro.product.model"]);
    let rel = sys_cmd_output(Path::new("getprop"), &["ro.build.version.release"]);
    let cfg = Config::load(env);
    let dbg_label = if cfg.debug_on() { "开" } else { "关" };
    let now = local_now().format("%Y-%m-%d %H:%M:%S %z");
    let mut summary = format!(
        "XRouter {ver} / {} / Android {}\n导出时间 {now}\n调试模式 {dbg_label}\n{status_json_str}\n",
        model.trim(),
        rel.trim()
    );
    summary.push_str(&log::failures_summary_line());
    fsx::write_logged(env, raw_dir.join("summary.txt"), summary);
}

/// 复制各日志（及其 `.1` 轮转份）到 `raw_dir`。
fn copy_logs(env: &Env, raw_dir: &Path) {
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
}

/// 诊断包额外内容：手机与客机快照、`logcat`、热点状态、去掉注释的配置、调试日志与通知器日志。
fn add_diag_extras(env: &Env, raw_dir: &Path) {
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

/// 把 `raw_dir` 中每个文件脱敏后写到 `pack_dir`。
fn redact_into_pack(env: &Env, raw_dir: &Path, pack_dir: &Path) {
    if let Ok(rd) = fs::read_dir(raw_dir) {
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
}

/// 把 `tmpd/name` 打包为 `dest/name.zip` 并清理临时目录；成功返回压缩包路径，失败时删掉半成品并返回 `None`。
fn finalize_zip(env: &Env, dest: &Path, tmpd: &Path, name: &str) -> Option<PathBuf> {
    let out_zip = dest.join(format!("{name}.zip"));
    fsx::remove_logged(env, &out_zip);
    if !write_zip_from_dir(&out_zip, tmpd, name) {
        fsx::remove_logged(env, &out_zip);
        fsx::remove_tree_logged(env, tmpd);
        return None;
    }
    fsx::remove_tree_logged(env, tmpd);
    Some(out_zip)
}

/// 导出日志（`is_diag` 为 false）或诊断包到下载目录，返回 JSON 结果：成功时含压缩包路径。
///
/// 内容含摘要（版本、机型、状态 `status_json_str` 与失败计数）和各日志；诊断包另含手机与客机快照、`logcat`、热点状态、去掉注释的配置、调试日志与通知器日志；文本均先脱敏。
#[must_use]
pub(crate) fn export_pack(env: &Env, prefix: &str, is_diag: bool, status_json_str: &str) -> String {
    let Some(dest) = download_dir(env) else {
        return "{\"ok\":false,\"error\":\"no download dir\"}".to_owned();
    };
    let stamp = local_now().format("%Y%m%d-%H%M%S");
    let name = format!("{prefix}-{stamp}");
    let tmpd = env.run_dir().join(format!("export.{}", std::process::id()));
    fsx::remove_tree_logged(env, &tmpd);
    let raw_dir = tmpd.join("raw");
    let pack_dir = tmpd.join(&name);
    if fs::create_dir_all(&raw_dir).is_err() || fs::create_dir_all(&pack_dir).is_err() {
        return "{\"ok\":false,\"error\":\"tmp\"}".to_owned();
    }

    write_summary(env, &raw_dir, status_json_str);
    copy_logs(env, &raw_dir);
    if is_diag {
        add_diag_extras(env, &raw_dir);
    }
    redact_into_pack(env, &raw_dir, &pack_dir);

    let Some(out_zip) = finalize_zip(env, &dest, &tmpd, &name) else {
        return "{\"ok\":false,\"error\":\"zip\"}".to_owned();
    };
    let label = if is_diag { "诊断包" } else { "日志" };
    log::info(env, "xr", &format!("已导出{label} {name}.zip"));
    serde_json::json!({
        "ok": true,
        "path": out_zip.to_string_lossy(),
    })
    .to_string()
}
