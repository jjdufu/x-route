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
use crate::cli::status::needs_restart;
use crate::cli::vpn_cfg::{list_dir, traffic_split, vpn_delete, vpn_import, vpn_set};
use crate::config;
use crate::config::{Config, KEYS, set_kv, valid_kv};
use crate::daemon;
use crate::daemon::watchdog;
use crate::daemon::{ensure_watchdog, vm_start, vm_stop};
use crate::diag;
use crate::diag::export_pack;
use crate::env::{Env, ProcRole};
use crate::log;
use crate::log::{blog, boot_clean_run, debug_arm, note_ok, note_rc, unix_now};
use crate::net::common::NetCtx;
use crate::net::run_net_cli;
use crate::out::{emit, emit_lines};
use crate::sys::exec::note_spawn;
use crate::sys::fsx;
use crate::sys::spawn;
use crate::sys::spawn::exec_crosvm_trampoline;
use crate::sys::{ce_ready, wait_boot_completed};
use crate::traffic::{acct_hooks, acct_json, acct_reset, acct_setup, acct_snapshot, acct_teardown};
use crate::uninstall;
use crate::vm;
use crate::vm::disk;
use crate::vm::{fuse_watch, run_net_hook};

/// 配置键写入后的联动：网络类键在虚拟机运行时触发规则同步，自启类键检查前置条件。
///
/// # Errors
///
/// 开启自启（`AUTOSTART`）时存在未确认的熔断，或当前配置还没有手动启动成功过，返回给用户看的原因。
fn after_set(env: &Env, key: &str) -> Result<(), String> {
    let cfg = Config::load(env);
    match key {
        "XR_EXPOSE" | "XR_MODE" | "XR_HOTSPOT_BRIDGE" | "XR_HOTSPOT_SUBNET" => {
            if vm::running(env) && cfg.vm_net() {
                let net_sh = env.moddir.join("scripts/net.sh");
                if net_sh.is_file() {
                    let net_sync = Command::new("sh")
                        .arg(&net_sh)
                        .arg("sync")
                        .env("MODDIR", &env.moddir)
                        .env("XR_DATA", &env.xr_data)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn();
                    note_spawn(env, "net.sh sync", net_sync);
                } else {
                    note_spawn(
                        env,
                        "xr internal net sync",
                        spawn::self_detached(env, &["internal", "net", "sync"]),
                    );
                }
            }
        }
        "XR_NOTIFY_TRAFFIC" => {
            fsx::write_logged(env, env.run_dir().join("notify.force"), "");
        }
        "XR_DEBUG" | "XR_DEBUG_HOURS" => {
            debug_arm(env);
            if key == "XR_DEBUG" {
                if cfg.debug_on() {
                    let hours = cfg.debug_hours();
                    let dur = if hours == 0 {
                        "不自动关闭".to_owned()
                    } else {
                        format!("{hours} 小时后自动关闭")
                    };
                    log::info(env, "debug", &format!("调试模式已开启（{dur}）"));
                    diag::snap_bg(env, "调试模式开启");
                } else {
                    log::info(env, "debug", "调试模式已关闭");
                }
            }
        }
        "AUTOSTART" if cfg.autostart() => {
            if env.tripped_file().is_file() {
                note_ok(env, "set_kv AUTOSTART=0", set_kv(env, "AUTOSTART", "0"));
                return Err("存在未确认的熔断，不能开启自启".to_owned());
            }
            let lg = fs::read_to_string(env.last_good_file()).unwrap_or_default();
            if lg.trim() != config::hash(env) {
                note_ok(env, "set_kv AUTOSTART=0", set_kv(env, "AUTOSTART", "0"));
                return Err("需先用当前配置手动启动成功一次".to_owned());
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
                "{\"ok\":false,\"error\":\"用法：xr ipv6 on|off\"}".to_owned(),
            );
        }
    };
    let v6_file = env.run_dir().join("guest/guest/ipv6");
    if !vm::running(env) || !v6_file.is_file() {
        return (1, "{\"ok\":false,\"error\":\"服务未就绪\"}".to_owned());
    }
    fsx::write_logged(
        env,
        env.run_dir().join("guest/host/ipv6.cmd"),
        format!("{arg} {}\n", unix_now()),
    );
    for _ in 0..env.ipv6_wait {
        if fs::read_to_string(&v6_file).is_ok_and(|raw| raw.trim() == want) {
            break;
        }
        thread::sleep(Duration::from_secs(1));
    }
    let cfg = Config::load(env);
    if cfg.vm_net() {
        note_rc(env, "run_net_hook", run_net_hook(env, "sync"));
    }
    if fs::read_to_string(&v6_file).is_ok_and(|text| text.trim() == want) {
        let label = if want == "1" { "开启" } else { "关闭" };
        log::info(env, "xr", &format!("服务 IPv6 已{label}"));
        (0, "{\"ok\":true}".to_owned())
    } else {
        (1, "{\"ok\":false,\"error\":\"服务未响应\"}".to_owned())
    }
}

fn bg_run(env: &Env, action: &str) -> i32 {
    let run = env.run_dir();
    fsx::remove_logged(env, run.join("bg.rc"));
    fsx::remove_logged(env, run.join("bg.rc.tmp"));
    if action == "start" && !vm::running(env) {
        fsx::write_logged(env, env.console_log(), "");
        fsx::write_logged(env, env.boot_log(), "");
        blog(env, "xr", "收到启动请求");
    }
    note_spawn(
        env,
        "xr internal bg-exec",
        spawn::self_detached(env, &["internal", "bg-exec", action]),
    );
    emit(env, OK_JSON, 0)
}

fn bg_exec(env: &Env, action: &str) -> i32 {
    let rc = match action {
        "start" => vm_start(env),
        "stop" => vm_stop(env),
        _ => 2_i32,
    };
    let run = env.run_dir();
    let tmp = run.join("bg.rc.tmp");
    let dst = run.join("bg.rc");
    if fs::write(&tmp, format!("{rc}\n")).is_ok() {
        fsx::rename_logged(env, &tmp, &dst);
    }
    rc
}

fn boot_service(env: &Env) -> i32 {
    note_ok(env, "boot_clean_run", boot_clean_run(env));
    if let Err(err) = env.ensure_dirs() {
        fsx::record_failure(env, "ensure_dirs", &env.xr_data, &err);
    }
    wait_boot_completed();
    if env.is_module_disabled() {
        return 0;
    }
    if env.fuse_file().is_file() {
        log::err(
            env,
            "service",
            "上次启动 VM 后整机复位，熔断：关闭自启，需在 WebUI 确认",
        );
        let now = Local::now().format("%Y-%m-%d %H:%M:%S");
        fsx::write_logged(env, env.tripped_file(), format!("{now}\n"));
        fsx::remove_logged(env, env.fuse_file());
        note_ok(env, "set_kv AUTOSTART=0", set_kv(env, "AUTOSTART", "0"));
    }
    let cfg = Config::load(env);
    if !daemon::ensure(env) {
        log::err(env, "service", "守护进程未能启动");
    }
    if !ensure_watchdog(env) {
        log::err(env, "service", "看门狗未能启动");
    }
    if !cfg.autostart() {
        log::info(env, "service", "AUTOSTART=0，不自启");
        return 0;
    }
    if env.tripped_file().is_file() {
        log::info(env, "service", "存在未确认的熔断，不自启");
        return 0;
    }
    let lg = fs::read_to_string(env.last_good_file()).unwrap_or_default();
    if lg.trim() != config::hash(env) {
        log::info(env, "service", "last_good 与当前配置不一致，跳过自启");
        return 0;
    }
    thread::sleep(Duration::from_secs(cfg.autostart_delay_sec()));
    while !ce_ready() {
        thread::sleep(Duration::from_secs(5));
    }
    vm_start(env)
}

fn action_toggle(env: &Env) -> i32 {
    if let Err(err) = env.ensure_dirs() {
        fsx::record_failure(env, "ensure_dirs", &env.xr_data, &err);
    }
    if vm::running(env) {
        note_rc(env, "vm_stop", vm_stop(env));
    } else {
        note_rc(env, "vm_start", vm_start(env));
    }
    emit(env, &status::line(env), 0)
}

fn run_traffic_cmd(env: &Env, sub: &str) -> i32 {
    let cfg = Config::load(env);
    let ctx = NetCtx::new(env, &cfg);
    match sub {
        "json" | "summary" | "" => emit(env, &acct_json(&ctx), 0),
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

/// 通用成功应答。
const OK_JSON: &str = "{\"ok\":true}";
/// `xr set` 参数个数不对时的应答。
const SET_USAGE_JSON: &str = "{\"ok\":false,\"error\":\"用法：xr set KEY VALUE\"}";

pub fn run(args: &[String]) -> i32 {
    let env = Env::from_process();
    let Some(cmd) = args.first().map(String::as_str) else {
        return emit(&env, "usage: xr <subcommand>", 2);
    };
    let rest = args.get(1..).unwrap_or(&[]);

    if cmd == "boot-tail" {
        let moff = rest.first().map_or("-1", String::as_str);
        let coff = rest.get(1).map_or("-1", String::as_str);
        return boot_tail::run(&env, moff, coff);
    }

    if let Err(err) = env.ensure_dirs() {
        fsx::record_failure(&env, "ensure_dirs", &env.xr_data, &err);
    }

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
            fsx::write_logged(&env, env.run_dir().join("notify.force"), "");
            if vm::running(&env) && env.run_dir().join("guest/host").is_dir() {
                fsx::write_logged(
                    &env,
                    env.run_dir().join("guest/host/acct.cmd"),
                    format!("reset {}{}\n", unix_now(), std::process::id()),
                );
            }
            log::info(&env, "xr", "流量统计已清零");
            emit(&env, OK_JSON, 0)
        }
        "traffic-split" => {
            let (rc, out) = traffic_split(&env, 4);
            emit(&env, &out, rc)
        }
        "running" => i32::from(!vm::running(&env)),
        "status-json" => emit(&env, &status::json(&env), 0),
        "status-line" => emit(&env, &status::line(&env), 0),
        "get" => {
            let cfg = Config::load(&env);
            emit_lines(
                &env,
                KEYS.iter().map(|key| format!("{key}={}", cfg.get_raw(key))),
                0,
            )
        }
        "set" => {
            let (Some(cfg_key), Some(item)) = (rest.first(), rest.get(1)) else {
                return emit(&env, SET_USAGE_JSON, 2);
            };
            if rest.len() != 2 {
                return emit(&env, SET_USAGE_JSON, 2);
            }
            let cfg = Config::load(&env);
            if cfg_key == "XR_HOTSPOT_SUBNET" && !item.is_empty() {
                if !hs_subnet_valid(item) {
                    let msg = json!({"ok": false, "error": "固定热点网段格式：私网网关地址/16~30，如 10.233.0.1/24"});
                    return emit(&env, &msg.to_string(), 1);
                }
                if let Some(clash) = hs_subnet_clash(&env, cfg.net_prefix(), item, None) {
                    let msg = json!({"ok": false, "error": format!("与 {clash} 冲突")});
                    return emit(&env, &msg.to_string(), 1);
                }
            }
            if !set_kv(&env, cfg_key, item) {
                let msg = json!({"ok": false, "error": format!("取值不合法：{cfg_key}")});
                return emit(&env, &msg.to_string(), 1);
            }
            if let Err(msg) = after_set(&env, cfg_key) {
                return emit(&env, &json!({"ok": false, "error": msg}).to_string(), 1);
            }
            let done = json!({"ok": true, "restart": needs_restart(&env, cfg_key)});
            emit(&env, &done.to_string(), 0)
        }
        "ipv6" => {
            let (rc, out) = ipv6_set(&env, rest.first().map_or("", String::as_str));
            emit(&env, &out, rc)
        }
        "ack-trip" => {
            fsx::remove_logged(&env, env.tripped_file());
            log::info(&env, "xr", "熔断已确认");
            0
        }
        "expand" => {
            let Some(mb) = rest.first() else {
                return emit(&env, "usage: xr expand MB", 2);
            };
            if mb.trim().parse::<u64>().is_err() {
                return emit(&env, "usage: xr expand MB", 2);
            }
            disk::expand(&env, mb)
        }
        "reset-disk" => disk::reset(&env),
        "sync" => {
            if vm::running(&env) {
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
            emit(&env, &out, rc)
        }
        "vpn-enable" => {
            let (rc, out) = vpn_set(&env, "enable", rest.first().map_or("", String::as_str));
            emit(&env, &out, rc)
        }
        "vpn-disable" => {
            let (rc, out) = vpn_set(&env, "disable", rest.first().map_or("", String::as_str));
            emit(&env, &out, rc)
        }
        "vpn-delete" => {
            let (rc, out) = vpn_delete(&env, rest.first().map_or("", String::as_str));
            emit(&env, &out, rc)
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
            st.map_or(1, |run_status| run_status.code().unwrap_or(1_i32))
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
            st.map_or(1, |exit_status| exit_status.code().unwrap_or(1_i32))
        }
        "ls" => {
            let (rc, items) = list_dir(
                &env,
                rest.first().map_or("", String::as_str),
                rest.get(1).map_or("dir", String::as_str),
            );
            emit_lines(&env, items, rc)
        }
        "log-export" => {
            let st_j = status::json(&env);
            let out = export_pack(&env, "xrouter-logs", false, &st_j);
            emit(&env, &out, i32::from(!out.contains("\"ok\":true")))
        }
        "diag-export" => {
            let st_j = status::json(&env);
            let out = export_pack(&env, "xrouter-diag", true, &st_j);
            emit(&env, &out, i32::from(!out.contains("\"ok\":true")))
        }
        "uninstall-now" => {
            let (rc, out) = uninstall::now(&env, rest.first().map_or("", String::as_str));
            emit(&env, &out, rc)
        }
        "clash-bridge" => crate::clash::run_clash_bridge(&env),
        "clash-ping" => crate::clash::run_clash_ping(&env),
        "clash-call" => crate::clash::run_clash_call(&env, rest.first().map_or("", String::as_str)),
        "clash-stream" => crate::clash::run_clash_stream(
            &env,
            rest.first().map_or("", String::as_str),
            rest.get(1).map_or("", String::as_str),
        ),
        "clash-ws-send" => crate::clash::run_clash_ws_send(
            &env,
            rest.first().map_or("", String::as_str),
            rest.get(1).map_or("text", String::as_str),
            rest.get(2).map_or("", String::as_str),
        ),
        "clash-ws-close" => {
            crate::clash::run_clash_ws_close(&env, rest.first().map_or("", String::as_str))
        }
        "internal" => match rest.first().map(String::as_str) {
            Some("dump-config-keys") => emit_lines(&env, KEYS, 0),
            Some("config-hash") => emit(&env, &config::hash(&env), 0),
            Some("valid-kv") => {
                let (Some(set_key), Some(val)) = (rest.get(1), rest.get(2)) else {
                    return 2;
                };
                let cfg = Config::load(&env);
                i32::from(!valid_kv(&env, &cfg, set_key, val))
            }
            Some("set-kv") => {
                let (Some(name_key), Some(value)) = (rest.get(1), rest.get(2)) else {
                    return 2;
                };
                i32::from(!set_kv(&env, name_key, value))
            }
            Some("net") => run_net_cli(&env, rest.get(1..).unwrap_or(&[])),
            Some("traffic") => run_traffic_cmd(&env, rest.get(1).map_or("json", String::as_str)),
            Some("vm") => match rest.get(1).map(String::as_str) {
                Some("start") => vm_start(&env),
                Some("stop") => vm_stop(&env),
                Some("status") => emit(&env, &vm::status_json(&env), 0),
                Some("expand") => {
                    let Some(mb) = rest.get(2) else {
                        return 2;
                    };
                    disk::expand(&env, mb)
                }
                Some("reset-disk") => disk::reset(&env),
                _ => 2,
            },
            Some("daemon") => {
                let mut daemon_env = env;
                daemon_env.role = ProcRole::Daemon;
                daemon::main_loop(&daemon_env)
            }
            Some("watchdog") => {
                let mut role_env = env;
                role_env.role = ProcRole::Watchdog;
                watchdog::main_loop(&role_env)
            }
            Some("fuse-watch") => {
                fuse_watch(&env);
                0
            }
            Some("clash-mux") => crate::clash::run_clash_mux(&env),
            Some("bg-exec") => bg_exec(&env, rest.get(1).map_or("", String::as_str)),
            Some("boot-service") => boot_service(&env),
            Some("action-toggle") => action_toggle(&env),
            Some("diag-snap") => {
                let reason = rest.get(1).map_or("manual", String::as_str);
                let cfg = Config::load(&env);
                if cfg.debug_on() {
                    diag::snap(&env, reason, 10);
                }
                0
            }
            Some("exec-crosvm") => exec_crosvm_trampoline(&env, rest.get(1..).unwrap_or(&[])),
            Some("uninstall") => uninstall::standard(&env),
            Some("uninstall-worker") => {
                let (Some(stage), Some(moddir), Some(upd)) =
                    (rest.get(1), rest.get(2), rest.get(3))
                else {
                    return 2;
                };
                uninstall::worker(&env, stage, moddir, upd)
            }
            _ => 2,
        },
        _ => emit(
            &env,
            "usage: xr start|stop|start-bg|stop-bg|boot-tail MOFF COFF|traffic|traffic-reset|traffic-split|running|status-json|status-line|get|set K V|ipv6 on|off|ack-trip|expand MB|reset-disk|sync|luci|project|vpn-import PATH [0|1]|vpn-enable [ID]|vpn-disable [ID]|vpn-delete ID|clash-bridge|clash-ping|ls PATH [dir|ovpn]|log-export|diag-export|uninstall-now confirm",
            2,
        ),
    }
}
