pub mod boot_tail;
pub mod status;
pub mod vpn_cfg;

use std::fs;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use chrono::Local;
use serde_json::json;

use crate::cidr::{hs_subnet_clash, hs_subnet_valid};
use crate::cli::boot_tail::run_boot_tail;
use crate::cli::status::{needs_restart, status_json, status_line};
use crate::cli::vpn_cfg::{list_dir, traffic_split, vpn_delete, vpn_import, vpn_set};
use crate::config::{config_hash, set_kv, valid_kv, Config, KEYS};
use crate::daemon::watchdog::watchdog_loop;
use crate::daemon::{daemon_loop, ensure_daemon, ensure_watchdog, vm_start, vm_stop};
use crate::diag::{diag_snap, diag_snap_bg, export_pack};
use crate::env::Env;
use crate::log::{blog, boot_clean_run, debug_arm, log_err, log_info, unix_now};
use crate::net::common::NetCtx;
use crate::net::run_net_cli;
use crate::sys::spawn::{exec_crosvm_trampoline, spawn_self_detached};
use crate::sys::{ce_ready, wait_boot_completed};
use crate::traffic::{acct_hooks, acct_json, acct_reset, acct_setup, acct_snapshot, acct_teardown};
use crate::vm::disk::{disk_expand, disk_reset};
use crate::vm::{fuse_watch, run_net_hook, vm_running};

fn after_set(env: &Env, key: &str) -> Result<(), String> {
    let cfg = Config::load(env);
    match key {
        "XR_EXPOSE" | "XR_MODE" | "XR_HOTSPOT_BRIDGE" | "XR_HOTSPOT_SUBNET" => {
            if vm_running(env) && cfg.vm_net() {
                let net_sh = env.moddir.join("scripts/net.sh");
                if net_sh.is_file() {
                    let _ = Command::new("sh")
                        .arg(&net_sh)
                        .arg("sync")
                        .env("MODDIR", &env.moddir)
                        .env("XR_DATA", &env.xr_data)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn();
                } else {
                    let _ = spawn_self_detached(env, &["internal", "net", "sync"]);
                }
            }
        }
        "XR_NOTIFY_TRAFFIC" => {
            let _ = fs::write(env.run_dir().join("notify.force"), "");
        }
        "XR_DEBUG" | "XR_DEBUG_HOURS" => {
            debug_arm(env);
            if key == "XR_DEBUG" {
                if cfg.debug_on() {
                    let h = cfg.debug_hours();
                    let dur = if h == 0 {
                        "不自动关闭".to_string()
                    } else {
                        format!("{h} 小时后自动关闭")
                    };
                    log_info(env, "debug", &format!("调试模式已开启（{dur}）"));
                    diag_snap_bg(env, "调试模式开启");
                } else {
                    log_info(env, "debug", "调试模式已关闭");
                }
            }
        }
        "AUTOSTART" if cfg.autostart() => {
            if env.tripped_file().is_file() {
                let _ = set_kv(env, "AUTOSTART", "0");
                return Err("存在未确认的熔断，不能开启自启".to_string());
            }
            let lg = fs::read_to_string(env.last_good_file()).unwrap_or_default();
            if lg.trim() != config_hash(env) {
                let _ = set_kv(env, "AUTOSTART", "0");
                return Err("需先用当前配置手动启动成功一次".to_string());
            }
        }
        _ => {}
    }
    Ok(())
}

fn ipv6_set(env: &Env, arg: &str) -> (i32, String) {
    let want = match arg {
        "on" => "1",
        "off" => "0",
        _ => {
            return (
                2,
                "{\"ok\":false,\"error\":\"用法：xr ipv6 on|off\"}".to_string(),
            )
        }
    };
    let v6_file = env.run_dir().join("guest/guest/ipv6");
    if !vm_running(env) || !v6_file.is_file() {
        return (1, "{\"ok\":false,\"error\":\"服务未就绪\"}".to_string());
    }
    let _ = fs::write(
        env.run_dir().join("guest/host/ipv6.cmd"),
        format!("{arg} {}\n", unix_now()),
    );
    for _ in 0..env.ipv6_wait {
        if fs::read_to_string(&v6_file).is_ok_and(|s| s.trim() == want) {
            break;
        }
        thread::sleep(Duration::from_secs(1));
    }
    let cfg = Config::load(env);
    if cfg.vm_net() {
        let _ = run_net_hook(env, "sync");
    }
    if fs::read_to_string(&v6_file).is_ok_and(|s| s.trim() == want) {
        let label = if want == "1" { "开启" } else { "关闭" };
        log_info(env, "xr", &format!("服务 IPv6 已{label}"));
        (0, "{\"ok\":true}".to_string())
    } else {
        (1, "{\"ok\":false,\"error\":\"服务未响应\"}".to_string())
    }
}

fn bg_run(env: &Env, action: &str) -> i32 {
    let run = env.run_dir();
    let _ = fs::remove_file(run.join("bg.rc"));
    let _ = fs::remove_file(run.join("bg.rc.tmp"));
    if action == "start" && !vm_running(env) {
        let _ = fs::write(env.console_log(), "");
        let _ = fs::write(env.boot_log(), "");
        blog(env, "xr", "收到启动请求");
    }
    let _ = spawn_self_detached(env, &["internal", "bg-exec", action]);
    println!("{{\"ok\":true}}");
    0
}

fn bg_exec(env: &Env, action: &str) -> i32 {
    let rc = match action {
        "start" => vm_start(env),
        "stop" => vm_stop(env),
        _ => 2,
    };
    let run = env.run_dir();
    let tmp = run.join("bg.rc.tmp");
    let dst = run.join("bg.rc");
    if fs::write(&tmp, format!("{rc}\n")).is_ok() {
        let _ = fs::rename(&tmp, &dst);
    }
    rc
}

fn boot_service(env: &Env) -> i32 {
    boot_clean_run(env);
    let _ = env.ensure_dirs();
    wait_boot_completed();
    if env.is_module_disabled() {
        return 0;
    }
    if env.fuse_file().is_file() {
        log_err(
            env,
            "service",
            "上次启动 VM 后整机复位，熔断：关闭自启，需在 WebUI 确认",
        );
        let now = Local::now().format("%Y-%m-%d %H:%M:%S");
        let _ = fs::write(env.tripped_file(), format!("{now}\n"));
        let _ = fs::remove_file(env.fuse_file());
        let _ = set_kv(env, "AUTOSTART", "0");
    }
    let cfg = Config::load(env);
    if !ensure_daemon(env) {
        log_err(env, "service", "守护进程未能启动");
    }
    if !ensure_watchdog(env) {
        log_err(env, "service", "看门狗未能启动");
    }
    if !cfg.autostart() {
        log_info(env, "service", "AUTOSTART=0，不自启");
        return 0;
    }
    if env.tripped_file().is_file() {
        log_info(env, "service", "存在未确认的熔断，不自启");
        return 0;
    }
    let lg = fs::read_to_string(env.last_good_file()).unwrap_or_default();
    if lg.trim() != config_hash(env) {
        log_info(env, "service", "last_good 与当前配置不一致，跳过自启");
        return 0;
    }
    thread::sleep(Duration::from_secs(cfg.autostart_delay_sec()));
    while !ce_ready() {
        thread::sleep(Duration::from_secs(5));
    }
    vm_start(env)
}

fn action_toggle(env: &Env) -> i32 {
    let _ = env.ensure_dirs();
    if vm_running(env) {
        let _ = vm_stop(env);
    } else {
        let _ = vm_start(env);
    }
    println!("{}", status_line(env));
    0
}

fn run_traffic_cmd(env: &Env, sub: &str) -> i32 {
    let cfg = Config::load(env);
    let ctx = NetCtx::new(env, &cfg);
    match sub {
        "json" | "summary" | "" => {
            println!("{}", acct_json(&ctx));
            0
        }
        "reset" => {
            acct_reset(&ctx);
            0
        }
        "setup" => {
            acct_setup(&ctx, env.run_dir().join("net.fwd6").is_file());
            0
        }
        "hooks" => {
            acct_hooks(&ctx);
            0
        }
        "snapshot" => {
            acct_snapshot(&ctx);
            0
        }
        "teardown" => {
            acct_teardown(&ctx);
            0
        }
        _ => 2,
    }
}

pub fn run_cli(args: &[String]) -> i32 {
    let env = Env::from_process();
    let Some(cmd) = args.first().map(String::as_str) else {
        println!("usage: xr <subcommand>");
        return 2;
    };
    let rest = args.get(1..).unwrap_or(&[]);

    if cmd == "boot-tail" {
        let moff = rest.first().map_or("-1", String::as_str);
        let coff = rest.get(1).map_or("-1", String::as_str);
        return run_boot_tail(&env, moff, coff);
    }

    let _ = env.ensure_dirs();

    match cmd {
        "start" => vm_start(&env),
        "stop" => vm_stop(&env),
        "start-bg" => bg_run(&env, "start"),
        "stop-bg" => bg_run(&env, "stop"),
        "traffic" => run_traffic_cmd(&env, rest.first().map_or("json", String::as_str)),
        "traffic-reset" => {
            let cfg = Config::load(&env);
            let ctx = NetCtx::new(&env, &cfg);
            acct_reset(&ctx);
            let _ = fs::write(env.run_dir().join("notify.force"), "");
            if vm_running(&env) && env.run_dir().join("guest/host").is_dir() {
                let _ = fs::write(
                    env.run_dir().join("guest/host/acct.cmd"),
                    format!("reset {}{}\n", unix_now(), std::process::id()),
                );
            }
            log_info(&env, "xr", "流量统计已清零");
            println!("{{\"ok\":true}}");
            0
        }
        "traffic-split" => {
            let (rc, out) = traffic_split(&env, 4);
            println!("{out}");
            rc
        }
        "running" => i32::from(!vm_running(&env)),
        "status-json" => {
            println!("{}", status_json(&env));
            0
        }
        "status-line" => {
            println!("{}", status_line(&env));
            0
        }
        "get" => {
            let cfg = Config::load(&env);
            for k in KEYS {
                println!("{k}={}", cfg.get_raw(k));
            }
            0
        }
        "set" => {
            let (Some(k), Some(v)) = (rest.first(), rest.get(1)) else {
                println!("{{\"ok\":false,\"error\":\"用法：xr set KEY VALUE\"}}");
                return 2;
            };
            if rest.len() != 2 {
                println!("{{\"ok\":false,\"error\":\"用法：xr set KEY VALUE\"}}");
                return 2;
            }
            let cfg = Config::load(&env);
            if k == "XR_HOTSPOT_SUBNET" && !v.is_empty() {
                if !hs_subnet_valid(v) {
                    println!(
                        "{}",
                        json!({"ok": false, "error": "固定热点网段格式：私网网关地址/16~30，如 10.233.0.1/24"})
                    );
                    return 1;
                }
                if let Some(clash) = hs_subnet_clash(&env, cfg.net_prefix(), v, None) {
                    println!(
                        "{}",
                        json!({"ok": false, "error": format!("与 {clash} 冲突")})
                    );
                    return 1;
                }
            }
            if !set_kv(&env, k, v) {
                println!(
                    "{}",
                    json!({"ok": false, "error": format!("取值不合法：{k}")})
                );
                return 1;
            }
            if let Err(msg) = after_set(&env, k) {
                println!("{}", json!({"ok": false, "error": msg}));
                return 1;
            }
            println!("{}", json!({"ok": true, "restart": needs_restart(&env, k)}));
            0
        }
        "ipv6" => {
            let (rc, out) = ipv6_set(&env, rest.first().map_or("", String::as_str));
            println!("{out}");
            rc
        }
        "ack-trip" => {
            let _ = fs::remove_file(env.tripped_file());
            log_info(&env, "xr", "熔断已确认");
            0
        }
        "expand" => {
            let Some(mb) = rest.first() else {
                println!("usage: xr expand MB");
                return 2;
            };
            if mb.trim().parse::<u64>().is_err() {
                println!("usage: xr expand MB");
                return 2;
            }
            disk_expand(&env, mb)
        }
        "reset-disk" => disk_reset(&env),
        "sync" => {
            if vm_running(&env) {
                run_net_hook(&env, "sync")
            } else {
                1
            }
        }
        "vpn-import" => {
            let (rc, out) = vpn_import(
                &env,
                rest.first().map_or("", String::as_str),
                rest.get(1).map_or("1", String::as_str),
            );
            println!("{out}");
            rc
        }
        "vpn-enable" => {
            let (rc, out) = vpn_set(&env, "enable", rest.first().map_or("", String::as_str));
            println!("{out}");
            rc
        }
        "vpn-disable" => {
            let (rc, out) = vpn_set(&env, "disable", rest.first().map_or("", String::as_str));
            println!("{out}");
            rc
        }
        "vpn-delete" => {
            let (rc, out) = vpn_delete(&env, rest.first().map_or("", String::as_str));
            println!("{out}");
            rc
        }
        "project" => {
            let st = Command::new("am")
                .args([
                    "start",
                    "-a",
                    "android.intent.action.VIEW",
                    "-d",
                    "https://github.com/jjdufu/x-route",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            st.map_or(1, |s| s.code().unwrap_or(1))
        }
        "luci" => {
            let cfg = Config::load(&env);
            let url = format!("http://{}.254", cfg.net_prefix());
            let st = Command::new("am")
                .args(["start", "-a", "android.intent.action.VIEW", "-d", &url])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            st.map_or(1, |s| s.code().unwrap_or(1))
        }
        "ls" => {
            let (rc, items) = list_dir(
                &env,
                rest.first().map_or("", String::as_str),
                rest.get(1).map_or("dir", String::as_str),
            );
            for it in items {
                println!("{it}");
            }
            rc
        }
        "log-export" => {
            let st_j = status_json(&env);
            let out = export_pack(&env, "xrouter-logs", false, &st_j);
            println!("{out}");
            i32::from(!out.contains("\"ok\":true"))
        }
        "diag-export" => {
            let st_j = status_json(&env);
            let out = export_pack(&env, "xrouter-diag", true, &st_j);
            println!("{out}");
            i32::from(!out.contains("\"ok\":true"))
        }
        "internal" => match rest.first().map(String::as_str) {
            Some("dump-config-keys") => {
                for k in KEYS {
                    println!("{k}");
                }
                0
            }
            Some("config-hash") => {
                println!("{}", config_hash(&env));
                0
            }
            Some("valid-kv") => {
                let (Some(k), Some(v)) = (rest.get(1), rest.get(2)) else {
                    return 2;
                };
                let cfg = Config::load(&env);
                i32::from(!valid_kv(&env, &cfg, k, v))
            }
            Some("set-kv") => {
                let (Some(k), Some(v)) = (rest.get(1), rest.get(2)) else {
                    return 2;
                };
                i32::from(!set_kv(&env, k, v))
            }
            Some("net") => run_net_cli(&env, rest.get(1..).unwrap_or(&[])),
            Some("traffic") => run_traffic_cmd(&env, rest.get(1).map_or("json", String::as_str)),
            Some("vm") => match rest.get(1).map(String::as_str) {
                Some("start") => vm_start(&env),
                Some("stop") => vm_stop(&env),
                Some("status") => {
                    println!("{}", crate::vm::vm_status_json(&env));
                    0
                }
                Some("expand") => {
                    let Some(mb) = rest.get(2) else {
                        return 2;
                    };
                    disk_expand(&env, mb)
                }
                Some("reset-disk") => disk_reset(&env),
                _ => 2,
            },
            Some("daemon") => {
                let mut e = env;
                e.in_daemon = true;
                daemon_loop(&e)
            }
            Some("watchdog") => {
                let mut e = env;
                e.in_watchdog = true;
                watchdog_loop(&e)
            }
            Some("fuse-watch") => {
                fuse_watch(&env);
                0
            }
            Some("bg-exec") => bg_exec(&env, rest.get(1).map_or("", String::as_str)),
            Some("boot-service") => boot_service(&env),
            Some("action-toggle") => action_toggle(&env),
            Some("diag-snap") => {
                let reason = rest.get(1).map_or("manual", String::as_str);
                let cfg = Config::load(&env);
                if cfg.debug_on() {
                    diag_snap(&env, reason, 10);
                }
                0
            }
            Some("exec-crosvm") => exec_crosvm_trampoline(rest.get(1..).unwrap_or(&[])),
            _ => 2,
        },
        _ => {
            println!("usage: xr start|stop|start-bg|stop-bg|boot-tail MOFF COFF|traffic|traffic-reset|traffic-split|running|status-json|status-line|get|set K V|ipv6 on|off|ack-trip|expand MB|reset-disk|sync|luci|project|vpn-import PATH [0|1]|vpn-enable [ID]|vpn-disable [ID]|vpn-delete ID|ls PATH [dir|ovpn]|log-export|diag-export|uninstall-now confirm");
            2
        }
    }
}
