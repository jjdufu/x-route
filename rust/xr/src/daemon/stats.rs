//! 实时速率服务：守护进程内监听 `run/stats.sock`，收到请求时才采样手机侧计数链并计算速率（只在内存中，不写文件）。

use std::fs;
use std::io::{BufRead as _, BufReader, ErrorKind, Write as _};
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::config::Config;
use crate::env::Env;
use crate::log;
use crate::log::dlog_env;
use crate::net::common::NetCtx;
use crate::rate::RateState;
use crate::sys::fsx;
use crate::traffic::{acct_present, acct_total_locked};

/// 单个连接读请求的超时。
const READ_TIMEOUT: Duration = Duration::from_millis(500);
/// 没有连接时两次 `accept` 之间的休眠。
const IDLE_SLEEP: Duration = Duration::from_millis(20);

/// 运行中的速率服务：停止标志与服务线程。
#[derive(Debug)]
pub(crate) struct StatsServer {
    /// 置位后服务线程在下一轮退出。
    stop: Arc<AtomicBool>,
    /// 服务线程。
    handle: JoinHandle<()>,
    /// 本进程绑定的套接字节点（设备号、inode），退出时只删除自己的节点。
    node: Option<(u64, u64)>,
}

/// 套接字节点的（设备号、inode）；不存在时为 `None`。
fn sock_node(env: &Env) -> Option<(u64, u64)> {
    fs::symlink_metadata(env.stats_sock())
        .ok()
        .map(|meta| (meta.dev(), meta.ino()))
}

/// 启动失败时写一行 WARN 并删除套接字节点；守护进程照常运行，`status-json` 的 `traffic` 降级为 `source:"none"`。
fn start_failed(env: &Env, what: &str, err: &std::io::Error) {
    log::msg(
        env,
        "WARN",
        "daemon",
        &format!("实时速率服务未启动：{what}失败（{err}）"),
    );
    fsx::remove_logged(env, env.stats_sock());
}

/// 绑定 `run/stats.sock`（权限 0600、非阻塞）并起服务线程；任一步失败返回 `None`。
pub(crate) fn start(env: &Env) -> Option<StatsServer> {
    let sock = env.stats_sock();
    fsx::remove_logged(env, &sock);
    let listener = match UnixListener::bind(&sock) {
        Ok(listener) => listener,
        Err(err) => {
            start_failed(env, "绑定 stats.sock ", &err);
            return None;
        }
    };
    fsx::chmod_logged(env, &sock, 0o600);
    if let Err(err) = listener.set_nonblocking(true) {
        start_failed(env, "设置非阻塞", &err);
        return None;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread_env = env.clone();
    let spawned = thread::Builder::new()
        .name("xr-stats".to_owned())
        .spawn(move || serve(&thread_env, &listener, &thread_stop));
    match spawned {
        Ok(handle) => Some(StatsServer {
            stop,
            handle,
            node: sock_node(env),
        }),
        Err(err) => {
            start_failed(env, "创建线程", &err);
            None
        }
    }
}

/// 停止服务线程并等待其结束；套接字仍是本进程绑定的那个节点时才删除（新守护进程可能已重新绑定）。线程异常结束时写调试日志。
pub(crate) fn shutdown(env: &Env, server: Option<StatsServer>) {
    if let Some(srv) = server {
        srv.stop.store(true, Ordering::SeqCst);
        if srv.handle.join().is_err() {
            dlog_env(env, "daemon", "实时速率服务线程异常结束");
        }
        if srv.node.is_some() && sock_node(env) == srv.node {
            fsx::remove_logged(env, env.stats_sock());
        }
    }
}

/// 服务线程：串行处理连接，没有连接时休眠，`stop` 置位后退出。
fn serve(env: &Env, listener: &UnixListener, stop: &AtomicBool) {
    let origin = Instant::now();
    let mut state = RateState::default();
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _peer)) => handle_conn(env, &stream, origin, &mut state),
            Err(err) if err.kind() == ErrorKind::WouldBlock => thread::sleep(IDLE_SLEEP),
            Err(err) => {
                dlog_env(env, "daemon", &format!("stats.sock accept 失败：{err}"));
                thread::sleep(IDLE_SLEEP);
            }
        }
    }
}

/// 处理一个连接：读一行请求、回一行 JSON；任何一步失败写调试日志并结束本次连接。
fn handle_conn(env: &Env, stream: &UnixStream, origin: Instant, state: &mut RateState) {
    let prepared = stream
        .set_nonblocking(false)
        .and_then(|()| stream.set_read_timeout(Some(READ_TIMEOUT)))
        .and_then(|()| stream.set_write_timeout(Some(READ_TIMEOUT)));
    if let Err(err) = prepared {
        dlog_env(env, "daemon", &format!("stats.sock 连接设置失败：{err}"));
        return;
    }
    let mut request = String::new();
    if let Err(err) = BufReader::new(stream).read_line(&mut request) {
        dlog_env(env, "daemon", &format!("stats.sock 读请求失败：{err}"));
        return;
    }
    let reply = reply_for(env, request.trim(), origin, state);
    let mut writer = stream;
    let sent = writer
        .write_all(format!("{reply}\n").as_bytes())
        .and_then(|()| writer.flush());
    if let Err(err) = sent {
        dlog_env(env, "daemon", &format!("stats.sock 回复失败：{err}"));
    }
}

/// 一组上下行累计值的 JSON。
fn pair_json(up: u64, down: u64) -> Value {
    json!({ "up": up, "down": down })
}

/// 按请求生成回复：`rate` 返回速率与累计总量；计数链不在时 `counting` 为 false；其他请求为 `unknown`。
fn reply_for(env: &Env, request: &str, origin: Instant, state: &mut RateState) -> Value {
    if request != "rate" {
        return json!({ "ok": false, "error": "unknown" });
    }
    let cfg = Config::load(env);
    let ctx = NetCtx::new(env, &cfg);
    if !acct_present(&ctx) {
        return json!({ "ok": true, "counting": false, "rate": null, "total": null });
    }
    let quad = acct_total_locked(&ctx);
    let now_ms = u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX);
    let rate = state
        .update_at(now_ms, quad)
        .and_then(|rates| serde_json::to_value(rates).ok())
        .unwrap_or(Value::Null);
    let [wan_up, wan_down, lan_up, lan_down] = quad;
    json!({
        "ok": true,
        "counting": true,
        "rate": rate,
        "total": {
            "wan": pair_json(wan_up, wan_down),
            "lan": pair_json(lan_up, lan_down),
            "total": pair_json(wan_up.saturating_add(lan_up), wan_down.saturating_add(lan_down)),
        },
    })
}
