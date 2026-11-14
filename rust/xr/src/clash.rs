//! `OpenClash` 控制通道：手机侧的复用进程（`clash-mux`）把客机 `hvc1` 控制口与多个本地 RPC 客户端相连，并提供 `xr clash-*` 子命令。
//!
//! 请求的 `id` 与 ws 的 `ws_id` 在复用进程内改写成不冲突的编号，应答再按登记表改回并发给原客户端。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead as _, BufReader, BufWriter, Write as _};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::Signal;
use serde::Serialize;
use serde_json::{Map, Value, json};
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::flag;

use crate::env::Env;
use crate::log;
use crate::log::{FAILURES, dlog_env, note_failure};
use crate::out::{self, emit};
use crate::sys::fsx;
use crate::sys::spawn;
use crate::sys::{is_pid_running, send_signal_logged};
use crate::vm;
use crate::vm::reap_in_background;

/// 客机上报的 `OpenClash` 状态（来自 `guest/guest/openclash.status`），用于 `xr status` 的 `clash` 字段。
#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct Status {
    /// `OpenClash` 是否已安装。
    pub(crate) installed: bool,
    /// `OpenClash` 是否在运行。
    pub(crate) running: bool,
    /// 控制面板端口，默认 9090。
    pub(crate) port: u16,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            installed: false,
            running: false,
            port: 9090,
        }
    }
}

/// 读取 `OpenClash` 状态；虚拟机不在（`alive` 为 false）或状态文件读不到时返回默认值。
#[must_use]
pub(crate) fn status(env: &Env, alive: bool) -> Status {
    if !alive {
        return Status::default();
    }
    let path = env.run_dir().join("guest/guest/openclash.status");
    let Ok(raw) = fs::read_to_string(path) else {
        return Status::default();
    };
    let mut st = Status::default();
    for line in raw.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let val = value.trim();
        match key.trim() {
            "installed" => st.installed = val == "1",
            "running" => st.running = val == "1",
            "port" => {
                if let Ok(port_num) = val.parse::<u16>()
                    && port_num > 0
                {
                    st.port = port_num;
                }
            }
            _ => {}
        }
    }
    st
}

/// 停止复用进程：先 `TERM`，1 秒内未退出再 `KILL`，然后删除其进程号、就绪标记与两个套接字。
pub(crate) fn stop_clash_mux(env: &Env) {
    let run = env.run_dir();
    if let Ok(pid_raw) = fs::read_to_string(run.join("clash_mux.pid")) {
        let pid = pid_raw.trim();
        if !pid.is_empty() && is_pid_running(env, pid) {
            send_signal_logged(env, pid, Signal::TERM);
            for _ in 0_i32..10_i32 {
                if !is_pid_running(env, pid) {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
            if is_pid_running(env, pid) {
                send_signal_logged(env, pid, Signal::KILL);
            }
        }
    }
    for name in [
        "clash_mux.pid",
        "clash_mux.ready",
        "clash-ctl.sock",
        "clash-rpc.sock",
    ] {
        fsx::remove_logged(env, run.join(name));
    }
}

/// 重启复用进程：先停止旧的，再后台启动 `xr internal clash-mux`，最多等 2 秒出现就绪标记（未就绪只记 INFO）。
pub(crate) fn start_clash_mux(env: &Env) {
    stop_clash_mux(env);
    let run = env.run_dir();
    let ready = run.join("clash_mux.ready");
    let Ok(mux) = spawn::self_detached(env, &["internal", "clash-mux"]) else {
        log::info(env, "clash", "clash-mux 启动失败");
        return;
    };
    if env.role.is_daemon() {
        reap_in_background(mux);
    }
    for _ in 0_i32..100_i32 {
        if ready.is_file() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    log::info(env, "clash", "clash-mux 未在预期时间内就绪");
}

/// 一个已连接的 RPC 客户端。
struct ClientHandle {
    /// 发往该客户端写线程的应答行。
    tx: mpsc::Sender<String>,
    /// 该客户端打开的 ws 在复用进程内的编号。
    ws_ids: HashSet<String>,
}

/// 按原 `ws_id` 查不到外部客户端 ws 归属的原因。
enum WsMiss {
    /// 登记表的锁已中毒：放弃本行且不回复（与原来 `Err(_) => None` 后直接返回相同）。
    Unavailable,
    /// 没有登记。
    Unknown,
}

/// 复用进程的共享状态。
struct MuxState {
    /// 客机控制口连接（未连接时为 `None`）。
    hvc_writer: Mutex<Option<UnixStream>>,
    /// 已连接的 RPC 客户端，键为客户端号。
    clients: Mutex<HashMap<u64, ClientHandle>>,
    /// 已转发、等待客机应答的请求：复用编号 -> (客户端号, 原 `id`)。
    pending_reqs: Mutex<HashMap<String, (u64, Value)>>,
    /// 已打开的 ws：复用编号 -> (客户端号, 原 `ws_id`)。
    active_ws: Mutex<HashMap<String, (u64, String)>>,
    /// 外部客户端打开的 ws：原 `ws_id` -> (客户端号, 复用编号)，供其它连接按原编号发送或关闭。
    ws_by_key: Mutex<HashMap<String, (u64, String)>>,
    /// 生成复用编号的递增序号。
    seq: AtomicU64,
}

impl MuxState {
    /// 空状态：客机未连接、没有客户端，序号从 1 开始。
    fn new() -> Self {
        Self {
            hvc_writer: Mutex::new(None),
            clients: Mutex::new(HashMap::new()),
            pending_reqs: Mutex::new(HashMap::new()),
            active_ws: Mutex::new(HashMap::new()),
            ws_by_key: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(1),
        }
    }

    /// 取下一个复用序号。
    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    /// 向客机控制口写一行并 flush；未连接或写失败时返回 false（写失败会断开该连接）。
    fn write_hvc_line(&self, line: &str) -> bool {
        let Ok(mut guard) = self.hvc_writer.lock() else {
            return false;
        };
        let Some(stream) = guard.as_mut() else {
            return false;
        };
        if stream.write_all(line.as_bytes()).is_err()
            || stream.write_all(b"\n").is_err()
            || stream.flush().is_err()
        {
            *guard = None;
            return false;
        }
        true
    }

    /// 把一行应答交给客户端的写线程；客户端已断开时只计数。
    fn send_to_client(&self, client_id: u64, line: String) {
        if let Ok(clients) = self.clients.lock()
            && let Some(ch) = clients.get(&client_id)
            && ch.tx.send(line).is_err()
        {
            // 该客户端的写线程已经结束（连接已断）：这一行无人接收，只计数
            note_failure(&FAILURES.log);
        }
    }

    /// 登记新客户端及其应答通道。
    fn register_client(&self, client_id: u64, tx: mpsc::Sender<String>) {
        if let Ok(mut clients) = self.clients.lock() {
            let replaced: Option<ClientHandle> = clients.insert(
                client_id,
                ClientHandle {
                    tx,
                    ws_ids: HashSet::new(),
                },
            );
            if replaced.is_some() {
                // 客户端编号单调递增，不会重复；万一重复，新连接覆盖旧连接（与原来相同），只计数
                note_failure(&FAILURES.log);
            }
        }
    }

    /// 注销客户端：清掉它的等待请求与 ws 登记，并通知客机关闭这些 ws。
    fn unregister_client(&self, client_id: u64) {
        let orphaned_ws: Vec<String> = self.clients.lock().map_or_else(
            |_| Vec::new(),
            |mut clients| {
                clients
                    .remove(&client_id)
                    .map_or_else(Vec::new, |client| client.ws_ids.into_iter().collect())
            },
        );
        if let Ok(mut pending) = self.pending_reqs.lock() {
            pending.retain(|_, (cid, _)| *cid != client_id);
        }
        if let Ok(mut active) = self.active_ws.lock() {
            active.retain(|mux_ws, _| !orphaned_ws.contains(mux_ws));
        }
        if let Ok(mut by_key) = self.ws_by_key.lock() {
            by_key.retain(|_, (cid, mux_ws)| *cid != client_id && !orphaned_ws.contains(mux_ws));
        }
        for mux_ws in orphaned_ws {
            let close_cmd = json!({"op": "ws_close", "ws_id": mux_ws}).to_string();
            if !self.write_hvc_line(&close_cmd) {
                // 客机通道未连接：客机侧的 ws 随通道一起失效，这条关闭通知无处可发，只计数
                note_failure(&FAILURES.log);
            }
        }
    }

    /// 按原 `ws_id` 查外部客户端打开的 ws 的归属（客户端号、mux 内 `ws_id`）；`close` 时同时移除登记。
    ///
    /// # Errors
    ///
    /// 登记表锁中毒返回 [`WsMiss::Unavailable`]，没有登记返回 [`WsMiss::Unknown`]。
    fn lookup_ws_key(&self, orig_ws: &str, close: bool) -> Result<(u64, String), WsMiss> {
        let Ok(mut by_key) = self.ws_by_key.lock() else {
            return Err(WsMiss::Unavailable);
        };
        let found = if close {
            by_key.remove(orig_ws)
        } else {
            by_key.get(orig_ws).cloned()
        };
        found.ok_or(WsMiss::Unknown)
    }

    /// 本连接没有映射时，按登记表找外部客户端打开的 ws。
    /// 未登记时，带 id 的请求直接回复（`ws_close` 回 ok=true，其它回 ok=false）并返回 `None`；
    /// 锁中毒时不回复、返回 `None`。
    fn resolve_external_ws(
        &self,
        client_id: u64,
        orig_id: Option<&Value>,
        op: &str,
        orig_ws: &str,
    ) -> Option<(u64, String)> {
        match self.lookup_ws_key(orig_ws, op == "ws_close") {
            Ok(found) => Some(found),
            Err(WsMiss::Unavailable) => None,
            Err(WsMiss::Unknown) => {
                if let Some(oid) = orig_id {
                    self.send_to_client(
                        client_id,
                        json!({"id": oid, "ok": op == "ws_close"}).to_string(),
                    );
                }
                None
            }
        }
    }

    /// 处理客机发来的一行：按登记表把复用 `id`/`ws_id` 改回原值并发给对应客户端，`ws_close` 时同时注销该 ws。
    fn handle_guest_line(&self, raw: &str) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return;
        }
        let Ok(mut val) = serde_json::from_str::<Value>(trimmed) else {
            return;
        };
        let Some(obj) = val.as_object_mut() else {
            return;
        };
        let mut target_client: Option<u64> = None;

        if let Some(mux_id) = obj.get("id").and_then(Value::as_str).map(str::to_owned)
            && let Ok(mut pending) = self.pending_reqs.lock()
            && let Some((cid, orig_id)) = pending.remove(&mux_id)
        {
            target_client = Some(cid);
            let _mux_id: Option<Value> = obj.insert("id".to_owned(), orig_id);
        }

        if let Some(mux_ws) = obj.get("ws_id").and_then(Value::as_str).map(str::to_owned) {
            let is_close = obj.get("op").and_then(Value::as_str) == Some("ws_close");
            if let Ok(mut active) = self.active_ws.lock() {
                let found = if is_close {
                    active.remove(&mux_ws)
                } else {
                    active.get(&mux_ws).cloned()
                };
                if let Some((cid, orig_ws)) = found {
                    if is_close {
                        if let Ok(mut by_key) = self.ws_by_key.lock() {
                            let _closed_key: Option<(u64, String)> = by_key.remove(&orig_ws);
                        }
                        if let Ok(mut clients) = self.clients.lock()
                            && let Some(ch) = clients.get_mut(&cid)
                        {
                            ch.ws_ids.retain(|owned| owned != &mux_ws);
                        }
                    }
                    if target_client.is_none() {
                        target_client = Some(cid);
                    }
                    let _mux_ws_id: Option<Value> =
                        obj.insert("ws_id".to_owned(), Value::String(orig_ws));
                }
            }
        }

        if let Some(cid) = target_client
            && let Ok(out) = serde_json::to_string(&val)
        {
            self.send_to_client(cid, out);
        }
    }

    /// 处理客户端发来的一行：改写 `id` 与 `ws_id` 并登记后转发给客机。
    ///
    /// 客机未连接时直接回复 503 错误；发送或关闭外部客户端打开的 ws 时由复用进程直接回复 ok。
    fn handle_client_line(&self, client_id: u64, ws_map: &mut HashMap<String, String>, raw: &str) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return;
        }
        let Ok(mut val) = serde_json::from_str::<Value>(trimmed) else {
            return;
        };
        let Some(obj) = val.as_object_mut() else {
            return;
        };

        let op = obj
            .get("op")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let orig_id = obj.get("id").cloned();

        let mut external_ws_owner: Option<(u64, String, String)> = None;

        match op.as_str() {
            "ws_open" => {
                let orig_ws = obj
                    .get("ws_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                if orig_ws.is_empty() {
                    return;
                }
                let mux_ws = format!("w{}", self.next_seq());
                let _reopened_local: Option<String> =
                    ws_map.insert(orig_ws.clone(), mux_ws.clone());
                if let Ok(mut active) = self.active_ws.lock() {
                    let _stale_active: Option<(u64, String)> =
                        active.insert(mux_ws.clone(), (client_id, orig_ws.clone()));
                }
                if let Ok(mut by_key) = self.ws_by_key.lock() {
                    let _reopened_key: Option<(u64, String)> =
                        by_key.insert(orig_ws, (client_id, mux_ws.clone()));
                }
                if let Ok(mut clients) = self.clients.lock()
                    && let Some(ch) = clients.get_mut(&client_id)
                {
                    ch.ws_ids.extend(std::iter::once(mux_ws.clone()));
                }
                let _orig_ws_id: Option<Value> =
                    obj.insert("ws_id".to_owned(), Value::String(mux_ws));
            }
            "ws_send" | "ws_close" => {
                let orig_ws = obj
                    .get("ws_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let local_mapped = if op == "ws_close" {
                    ws_map.remove(&orig_ws)
                } else {
                    ws_map.get(&orig_ws).cloned()
                };
                let is_local = local_mapped.is_some();
                let owner = local_mapped.map(|mapped| (client_id, mapped)).or_else(|| {
                    self.resolve_external_ws(client_id, orig_id.as_ref(), &op, &orig_ws)
                });
                let Some((owner_cid, mux_ws)) = owner else {
                    return;
                };
                if !is_local {
                    external_ws_owner = Some((owner_cid, orig_ws.clone(), mux_ws.clone()));
                }
                if op == "ws_close" {
                    if let Ok(mut by_key) = self.ws_by_key.lock() {
                        let _closed_key: Option<(u64, String)> = by_key.remove(&orig_ws);
                    }
                    if let Ok(mut active) = self.active_ws.lock() {
                        let _closed_active: Option<(u64, String)> = active.remove(&mux_ws);
                    }
                    if let Ok(mut clients) = self.clients.lock()
                        && let Some(ch) = clients.get_mut(&owner_cid)
                    {
                        ch.ws_ids.retain(|owned| owned != &mux_ws);
                    }
                    if external_ws_owner.is_some() {
                        self.send_to_client(
                            owner_cid,
                            json!({"op": "ws_close", "ws_id": orig_ws, "code": 1000_i32, "reason": "closed"})
                                .to_string(),
                        );
                    }
                }
                let _orig_ws_id: Option<Value> =
                    obj.insert("ws_id".to_owned(), Value::String(mux_ws));
            }
            _ => {}
        }

        let is_ext_ws_ctl = external_ws_owner.is_some() && (op == "ws_send" || op == "ws_close");
        let mux_req_id = if is_ext_ws_ctl {
            let _orig_req_id: Option<Value> = obj.remove("id");
            None
        } else if let Some(oid) = &orig_id {
            let mid = format!("r{}", self.next_seq());
            if let Ok(mut pending) = self.pending_reqs.lock() {
                let _reused_mid: Option<(u64, Value)> =
                    pending.insert(mid.clone(), (client_id, oid.clone()));
            }
            let _orig_req_id: Option<Value> =
                obj.insert("id".to_owned(), Value::String(mid.clone()));
            Some(mid)
        } else {
            None
        };

        let Ok(line) = serde_json::to_string(&val) else {
            return;
        };
        let written = self.write_hvc_line(&line);
        if is_ext_ws_ctl {
            if let Some(oid) = orig_id {
                self.send_to_client(
                    client_id,
                    json!({"id": oid, "ok": written || op == "ws_close"}).to_string(),
                );
            }
            return;
        }
        if !written {
            if let Some(mid) = mux_req_id
                && let Ok(mut pending) = self.pending_reqs.lock()
            {
                let _unsent: Option<(u64, Value)> = pending.remove(&mid);
            }
            if let Some(oid) = orig_id {
                let mut err = Map::new();
                err.extend([
                    ("id".to_owned(), oid),
                    ("ok".to_owned(), Value::Bool(false)),
                    ("status".to_owned(), Value::from(503_i32)),
                    (
                        "error".to_owned(),
                        Value::String("hvc1 channel not connected".to_owned()),
                    ),
                ]);
                self.send_to_client(client_id, Value::Object(err).to_string());
            }
        }
    }
}

/// 服务一个 RPC 客户端连接：后台线程写应答，当前线程逐行读请求并交给 [`MuxState::handle_client_line`]，断开后注销。
fn serve_rpc_client(env: &Env, state: &MuxState, stream: UnixStream, client_id: u64) {
    let Ok(writer_stream) = stream.try_clone() else {
        return;
    };
    let (tx, rx) = mpsc::channel::<String>();
    state.register_client(client_id, tx);

    let writer_handle = thread::spawn(move || {
        let mut bw = BufWriter::new(writer_stream);
        while let Ok(line) = rx.recv() {
            if bw.write_all(line.as_bytes()).is_err()
                || bw.write_all(b"\n").is_err()
                || bw.flush().is_err()
            {
                break;
            }
        }
    });

    let mut br = BufReader::new(stream);
    let mut line = String::new();
    let mut ws_map: HashMap<String, String> = HashMap::new();
    loop {
        line.clear();
        match br.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => state.handle_client_line(client_id, &mut ws_map, &line),
        }
    }

    state.unregister_client(client_id);
    note_join(env, "clash 客户端写线程", &writer_handle.join());
}

/// 在分离线程里服务一个 RPC 客户端连接：显式丢弃句柄即 detach，与原来不 join 的语义相同。
fn spawn_rpc_client(env: Env, state: Arc<MuxState>, stream: UnixStream, client_id: u64) {
    drop(thread::spawn(move || {
        serve_rpc_client(&env, &state, stream, client_id);
    }));
}

/// 等待线程结束后的结果：线程异常结束时（release 构建为 `panic = "abort"`，只在测试构建中可能出现）
/// 计数并在调试开关打开时写调试日志，调用方继续。
fn note_join(env: &Env, what: &str, res: &thread::Result<()>) {
    if res.is_err() {
        note_failure(&FAILURES.exec);
        dlog_env(env, "clash", &format!("{what}异常结束"));
    }
}

/// 套接字选项设置（超时、阻塞模式）或关闭的结果：失败时在调试开关打开时写调试日志，按原逻辑继续。
fn note_sock(env: &Env, what: &str, res: std::io::Result<()>) {
    if let Err(err) = res {
        dlog_env(env, "clash", &format!("{what} 失败：{err}"));
    }
}

/// `xr internal clash-mux`：监听 `clash-ctl.sock`（客机控制口）和 `clash-rpc.sock`（RPC 客户端），转发双方消息直到收到 `TERM`/`INT`；返回进程退出码。
#[must_use]
pub(crate) fn run_clash_mux(env: &Env) -> i32 {
    let run = env.run_dir();
    let ctl_sock = run.join("clash-ctl.sock");
    let rpc_sock = run.join("clash-rpc.sock");
    let pid_file = run.join("clash_mux.pid");
    let ready_file = run.join("clash_mux.ready");

    fsx::remove_logged(env, &ctl_sock);
    fsx::remove_logged(env, &rpc_sock);
    fsx::remove_logged(env, &ready_file);

    let Ok(ctl_listener) = UnixListener::bind(&ctl_sock) else {
        return 1;
    };
    let Ok(rpc_listener) = UnixListener::bind(&rpc_sock) else {
        fsx::remove_logged(env, &ctl_sock);
        return 1;
    };
    fsx::chmod_logged(env, &ctl_sock, 0o600);
    fsx::chmod_logged(env, &rpc_sock, 0o600);
    note_sock(
        env,
        "clash-ctl.sock 设为非阻塞",
        ctl_listener.set_nonblocking(true),
    );
    note_sock(
        env,
        "clash-rpc.sock 设为非阻塞",
        rpc_listener.set_nonblocking(true),
    );

    let stop = Arc::new(AtomicBool::new(false));
    if let Err(err) = flag::register(SIGTERM, Arc::clone(&stop)) {
        log::err(env, "clash", &format!("信号注册失败 SIGTERM: {err}"));
    }
    if let Err(err) = flag::register(SIGINT, Arc::clone(&stop)) {
        log::err(env, "clash", &format!("信号注册失败 SIGINT: {err}"));
    }

    fsx::write_logged(env, &pid_file, format!("{}\n", std::process::id()));
    fsx::write_logged(env, &ready_file, "ready\n");

    let state = Arc::new(MuxState::new());

    let s_ctl = Arc::clone(&state);
    let stop_ctl = Arc::clone(&stop);
    let env_ctl = env.clone();
    let ctl_thread = thread::spawn(move || {
        while !stop_ctl.load(Ordering::Relaxed) {
            let accepted = ctl_listener.accept();
            match accepted {
                Ok((stream, _)) => {
                    note_sock(&env_ctl, "客机连接设为阻塞", stream.set_nonblocking(false));
                    let Ok(reader_stream) = stream.try_clone() else {
                        continue;
                    };
                    if let Ok(mut guard) = s_ctl.hvc_writer.lock() {
                        *guard = Some(stream);
                    }
                    let mut br = BufReader::new(reader_stream);
                    let mut line = String::new();
                    while !stop_ctl.load(Ordering::Relaxed) {
                        line.clear();
                        match br.read_line(&mut line) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => s_ctl.handle_guest_line(&line),
                        }
                    }
                    drop(br);
                    if let Ok(mut guard) = s_ctl.hvc_writer.lock() {
                        *guard = None;
                    }
                }
                Err(read_err) if read_err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(_) => break,
            }
        }
    });

    let s_rpc = Arc::clone(&state);
    let stop_rpc = Arc::clone(&stop);
    let env_rpc = env.clone();
    let rpc_thread = thread::spawn(move || {
        let mut next_cid = 1_u64;
        while !stop_rpc.load(Ordering::Relaxed) {
            let accepted = rpc_listener.accept();
            match accepted {
                Ok((stream, _)) => {
                    note_sock(&env_rpc, "RPC 连接设为阻塞", stream.set_nonblocking(false));
                    let cid = next_cid;
                    next_cid = next_cid.saturating_add(1);
                    spawn_rpc_client(env_rpc.clone(), Arc::clone(&s_rpc), stream, cid);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(_) => break,
            }
        }
    });

    let started = Instant::now();
    let mut saw_vm = false;
    while !stop.load(Ordering::Relaxed) {
        if vm::running(env) {
            saw_vm = true;
        } else if (saw_vm || started.elapsed() > Duration::from_secs(10))
            && !env.fuse_file().is_file()
        {
            break;
        } else {
            // 虚拟机还没起来且仍在 10 秒宽限期内，或 fuse 文件还在：继续等待
        }
        thread::sleep(Duration::from_millis(300));
    }

    stop.store(true, Ordering::Relaxed);
    note_join(env, "clash 客机通道线程", &ctl_thread.join());
    note_join(env, "clash RPC 监听线程", &rpc_thread.join());
    for name in [
        "clash_mux.pid",
        "clash_mux.ready",
        "clash-ctl.sock",
        "clash-rpc.sock",
    ] {
        fsx::remove_logged(env, run.join(name));
    }
    0
}

/// `xr clash-bridge`：把标准输入的请求行转发给复用进程，并把应答逐行写到标准输出；通道不可用时输出错误 JSON 并返回 1。
#[must_use]
pub(crate) fn run_clash_bridge(env: &Env) -> i32 {
    let rpc_sock = env.run_dir().join("clash-rpc.sock");
    let Ok(stream) = UnixStream::connect(&rpc_sock) else {
        return emit(
            env,
            "{\"ok\":false,\"error\":\"clash channel unavailable\"}",
            1,
        );
    };
    let Ok(sock_in) = stream.try_clone() else {
        return 1;
    };
    let Ok(mut sock_out) = stream.try_clone() else {
        return 1;
    };

    let reader_handle = thread::spawn(move || {
        let mut br = BufReader::new(sock_in);
        let mut stdout = BufWriter::new(std::io::stdout().lock());
        let mut line = String::new();
        loop {
            line.clear();
            match br.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if stdout.write_all(line.as_bytes()).is_err() || stdout.flush().is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut stdin = BufReader::new(std::io::stdin().lock());
    let mut line = String::new();
    loop {
        line.clear();
        match stdin.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if sock_out.write_all(line.as_bytes()).is_err() || sock_out.flush().is_err() {
                    break;
                }
            }
        }
    }
    note_sock(env, "clash 桥接写端关闭", sock_out.shutdown(Shutdown::Both));
    note_sock(env, "clash 桥接连接关闭", stream.shutdown(Shutdown::Both));
    note_join(env, "clash 桥接读线程", &reader_handle.join());
    0
}

/// `xr clash-ping`：向客机发 `ping` 并输出应答；应答含 `"ok":true` 时返回 0，否则返回 1。
#[must_use]
pub(crate) fn run_clash_ping(env: &Env) -> i32 {
    let rpc_sock = env.run_dir().join("clash-rpc.sock");
    let Ok(mut stream) = UnixStream::connect(&rpc_sock) else {
        return emit(
            env,
            "{\"ok\":false,\"error\":\"clash channel unavailable\"}",
            1,
        );
    };
    note_sock(
        env,
        "clash-ping 读超时",
        stream.set_read_timeout(Some(Duration::from_secs(3))),
    );
    note_sock(
        env,
        "clash-ping 写超时",
        stream.set_write_timeout(Some(Duration::from_secs(2))),
    );
    if stream.write_all(b"{\"id\":1,\"op\":\"ping\"}\n").is_err() || stream.flush().is_err() {
        return emit(env, "{\"ok\":false,\"error\":\"write failed\"}", 1);
    }
    let mut br = BufReader::new(stream);
    let mut line = String::new();
    match br.read_line(&mut line) {
        Ok(0) | Err(_) => emit(env, "{\"ok\":false,\"error\":\"timeout or closed\"}", 1),
        Ok(_) => {
            let reply = line.trim();
            emit(env, reply, i32::from(!reply.contains("\"ok\":true")))
        }
    }
}

/// 把一行 RPC 应答写到 stdout 并立即 flush（WebUI 逐行读取）。
///
/// # Errors
///
/// stdout 写入或 flush 失败时返回对应的 `io::Error`。
fn emit_rpc_line(line: &str) -> std::io::Result<()> {
    out::raw(&render_rpc_line(line))
}

/// 生成一行 RPC 应答在 stdout 上的文本：不超过 48 KiB 时原样加换行；更长时先输出
/// `{"_chunks":N}`，再逐块输出 `@<块>`（按字符边界切分），每行一个换行。
fn render_rpc_line(line: &str) -> String {
    const MAX_CHUNK: usize = 49_152;
    let trimmed = line.trim_end_matches(['\r', '\n']);
    if trimmed.len() <= MAX_CHUNK {
        return out::join_lines([trimmed]);
    }
    let mut chunks: Vec<&str> = Vec::new();
    let mut start = 0;
    while start < trimmed.len() {
        let mut end = start.saturating_add(MAX_CHUNK).min(trimmed.len());
        while end > start && !trimmed.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        if end == start {
            end = trimmed.len();
        }
        // start 与 end 都在字符边界上（start 是上一轮的 end），get 恒为 Some；None 时结束循环
        let Some(part) = trimmed.get(start..end) else {
            break;
        };
        chunks.push(part);
        start = end;
    }
    let header = format!("{{\"_chunks\":{}}}", chunks.len());
    out::join_lines(
        std::iter::once(header).chain(chunks.into_iter().map(|part| format!("@{part}"))),
    )
}

/// `xr clash-call <json>`：发送一个 RPC 请求（缺 `id` 时补 `c1`）并输出应答，最多等 20 秒。
///
/// 参数不是 JSON 对象时返回 2，通道不可用或超时返回 1。
#[must_use]
pub(crate) fn run_clash_call(env: &Env, req_json: &str) -> i32 {
    let Ok(mut val) = serde_json::from_str::<Value>(req_json) else {
        return emit(
            env,
            "{\"ok\":false,\"status\":400,\"error\":\"invalid json\"}",
            2,
        );
    };
    let Some(obj) = val.as_object_mut() else {
        return emit(
            env,
            "{\"ok\":false,\"status\":400,\"error\":\"json object required\"}",
            2,
        );
    };
    if !obj.contains_key("id") {
        let _absent_id: Option<Value> = obj.insert("id".to_owned(), Value::String("c1".to_owned()));
    }
    let rpc_sock = env.run_dir().join("clash-rpc.sock");
    let Ok(mut stream) = UnixStream::connect(&rpc_sock) else {
        return emit(
            env,
            "{\"ok\":false,\"status\":503,\"error\":\"clash channel unavailable\"}",
            1,
        );
    };
    note_sock(
        env,
        "clash-call 读超时",
        stream.set_read_timeout(Some(Duration::from_secs(20))),
    );
    note_sock(
        env,
        "clash-call 写超时",
        stream.set_write_timeout(Some(Duration::from_secs(5))),
    );
    let wire = val.to_string();
    if stream.write_all(wire.as_bytes()).is_err()
        || stream.write_all(b"\n").is_err()
        || stream.flush().is_err()
    {
        return emit(
            env,
            "{\"ok\":false,\"status\":502,\"error\":\"write failed\"}",
            1,
        );
    }
    let mut br = BufReader::new(stream);
    let mut line = String::new();
    match br.read_line(&mut line) {
        Ok(0) | Err(_) => emit(
            env,
            "{\"ok\":false,\"status\":504,\"error\":\"timeout or closed\"}",
            1,
        ),
        Ok(_) => out::finish(env, emit_rpc_line(&line), 0),
    }
}

/// `xr clash-stream <ws_id> <path>`：打开一个到 `OpenClash` 接口的 ws，并把收到的消息逐行输出直到关闭。
///
/// 参数非法时返回 2。
pub(crate) fn run_clash_stream(env: &Env, ws_id: &str, path: &str) -> i32 {
    if ws_id.is_empty() || !path.starts_with('/') {
        return emit(
            env,
            "{\"op\":\"ws_close\",\"ok\":false,\"error\":\"invalid args\"}",
            2,
        );
    }
    let rpc_sock = env.run_dir().join("clash-rpc.sock");
    let Ok(mut stream) = UnixStream::connect(&rpc_sock) else {
        let closed = format!(
            "{{\"op\":\"ws_close\",\"ws_id\":\"{ws_id}\",\"code\":1006,\"reason\":\"channel unavailable\"}}"
        );
        return emit(env, &closed, 1);
    };
    note_sock(
        env,
        "clash-stream 写超时",
        stream.set_write_timeout(Some(Duration::from_secs(5))),
    );
    let open_cmd = json!({"id": "open", "op": "ws_open", "ws_id": ws_id, "path": path}).to_string();
    if stream.write_all(open_cmd.as_bytes()).is_err()
        || stream.write_all(b"\n").is_err()
        || stream.flush().is_err()
    {
        let closed = format!(
            "{{\"op\":\"ws_close\",\"ws_id\":\"{ws_id}\",\"code\":1006,\"reason\":\"write failed\"}}"
        );
        return emit(env, &closed, 1);
    }
    let mut br = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match br.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if let Err(err) = emit_rpc_line(&line) {
                    // WebUI 已关闭读端：结束流（原来的打印宏在此 panic 中止）
                    return out::finish(env, Err(err), 0);
                }
                if let Ok(value) = serde_json::from_str::<Value>(line.trim()) {
                    let is_close = value.get("op").and_then(Value::as_str) == Some("ws_close");
                    let open_fail = value.get("id").and_then(Value::as_str) == Some("open")
                        && value.get("ok").and_then(Value::as_bool) == Some(false);
                    if is_close || open_fail {
                        break;
                    }
                }
            }
        }
    }
    0
}

/// `xr clash-ws-send <ws_id> <mode> <payload>`：向已打开的 ws 发送一条文本或 base64 二进制消息并输出应答。
///
/// `ws_id` 为空时返回 2。
#[must_use]
pub(crate) fn run_clash_ws_send(env: &Env, ws_id: &str, mode: &str, payload: &str) -> i32 {
    if ws_id.is_empty() {
        return 2;
    }
    let rpc_sock = env.run_dir().join("clash-rpc.sock");
    let Ok(mut stream) = UnixStream::connect(&rpc_sock) else {
        return emit(
            env,
            "{\"ok\":false,\"error\":\"clash channel unavailable\"}",
            1,
        );
    };
    note_sock(
        env,
        "clash-ws-send 读超时",
        stream.set_read_timeout(Some(Duration::from_secs(5))),
    );
    note_sock(
        env,
        "clash-ws-send 写超时",
        stream.set_write_timeout(Some(Duration::from_secs(5))),
    );
    let cmd = if mode == "bin" {
        json!({"id": "s1", "op": "ws_send", "ws_id": ws_id, "binary": true, "data_b64": payload})
    } else {
        json!({"id": "s1", "op": "ws_send", "ws_id": ws_id, "binary": false, "data": payload})
    }
    .to_string();
    if stream.write_all(cmd.as_bytes()).is_err()
        || stream.write_all(b"\n").is_err()
        || stream.flush().is_err()
    {
        return emit(env, "{\"ok\":false,\"error\":\"write failed\"}", 1);
    }
    let mut br = BufReader::new(stream);
    let mut line = String::new();
    match br.read_line(&mut line) {
        Ok(0) | Err(_) => 1,
        Ok(_) => {
            let reply = line.trim();
            emit(env, reply, i32::from(!reply.contains("\"ok\":true")))
        }
    }
}

/// `xr clash-ws-close <ws_id>`：关闭已打开的 ws；通道不可用时视为已关闭，输出 `{"ok":true}`。
///
/// `ws_id` 为空时返回 2。
#[must_use]
pub(crate) fn run_clash_ws_close(env: &Env, ws_id: &str) -> i32 {
    if ws_id.is_empty() {
        return 2;
    }
    let rpc_sock = env.run_dir().join("clash-rpc.sock");
    let Ok(mut stream) = UnixStream::connect(&rpc_sock) else {
        return emit(env, "{\"ok\":true}", 0);
    };
    note_sock(
        env,
        "clash-ws-close 读超时",
        stream.set_read_timeout(Some(Duration::from_secs(3))),
    );
    note_sock(
        env,
        "clash-ws-close 写超时",
        stream.set_write_timeout(Some(Duration::from_secs(3))),
    );
    let cmd = json!({"id": "c1", "op": "ws_close", "ws_id": ws_id}).to_string();
    if stream.write_all(cmd.as_bytes()).is_err()
        || stream.write_all(b"\n").is_err()
        || stream.flush().is_err()
    {
        return 0;
    }
    let mut br = BufReader::new(stream);
    let mut line = String::new();
    if let Err(err) = br.read_line(&mut line) {
        // 读失败与读到空行同样按"已关闭"回答，与原来相同
        dlog_env(env, "clash", &format!("clash-ws-close 读应答失败：{err}"));
    }
    let reply = line.trim();
    emit(
        env,
        if reply.is_empty() {
            "{\"ok\":true}"
        } else {
            reply
        },
        0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_clash_status_parsing() {
        let td = TempDir::new().ok();
        assert!(td.is_some());
        if let Some(tmp) = td {
            let mut env = Env::from_process();
            env.xr_data = tmp.path().to_path_buf();
            let st_dir = env.run_dir().join("guest/guest");
            assert!(fs::create_dir_all(&st_dir).is_ok());
            assert!(
                fs::write(
                    st_dir.join("openclash.status"),
                    "installed=1\nrunning=1\nport=9095\n"
                )
                .is_ok()
            );

            let off = status(&env, false);
            assert!(!off.installed);
            assert!(!off.running);
            assert_eq!(off.port, 9090);

            let on = status(&env, true);
            assert!(on.installed);
            assert!(on.running);
            assert_eq!(on.port, 9095);
        }
    }

    #[test]
    fn test_mux_multiplex_and_cleanup() {
        let pair = UnixStream::pair().ok();
        assert!(pair.is_some());
        if let Some((guest_end, mux_end)) = pair {
            let state = MuxState::new();
            if let Ok(mut guard) = state.hvc_writer.lock() {
                *guard = Some(mux_end);
            }
            let (tx1, rx1) = mpsc::channel::<String>();
            let (tx2, rx2) = mpsc::channel::<String>();
            state.register_client(1, tx1);
            state.register_client(2, tx2);

            let mut ws1 = HashMap::new();
            let mut ws2 = HashMap::new();
            state.handle_client_line(1, &mut ws1, "{\"id\":10,\"op\":\"ping\"}");
            state.handle_client_line(
                2,
                &mut ws2,
                "{\"id\":\"abc\",\"op\":\"ws_open\",\"ws_id\":\"s1\",\"path\":\"/traffic\"}",
            );

            let mut br = BufReader::new(guest_end);
            let mut l1 = String::new();
            let mut l2 = String::new();
            assert!(br.read_line(&mut l1).is_ok());
            assert!(br.read_line(&mut l2).is_ok());

            let v1: Value = serde_json::from_str(&l1).unwrap_or(Value::Null);
            let v2: Value = serde_json::from_str(&l2).unwrap_or(Value::Null);
            let mid1 = v1.get("id").and_then(Value::as_str).unwrap_or("");
            let mid2 = v2.get("id").and_then(Value::as_str).unwrap_or("");
            let mws2 = v2.get("ws_id").and_then(Value::as_str).unwrap_or("");
            assert!(!mid1.is_empty() && !mid2.is_empty() && !mws2.is_empty());

            state.handle_guest_line(&format!("{{\"id\":\"{mid1}\",\"ok\":true}}"));
            state.handle_guest_line(&format!(
                "{{\"id\":\"{mid2}\",\"op\":\"ws_open_ok\",\"ok\":true,\"ws_id\":\"{mws2}\"}}"
            ));
            state.handle_guest_line(&format!(
                "{{\"op\":\"ws_msg\",\"ws_id\":\"{mws2}\",\"binary\":false,\"data\":\"hi\"}}"
            ));

            let r1: Value =
                serde_json::from_str(&rx1.recv_timeout(Duration::from_secs(1)).unwrap_or_default())
                    .unwrap_or(Value::Null);
            assert_eq!(r1.get("id"), Some(&Value::from(10_i32)));

            let r2a: Value =
                serde_json::from_str(&rx2.recv_timeout(Duration::from_secs(1)).unwrap_or_default())
                    .unwrap_or(Value::Null);
            assert_eq!(r2a.get("id"), Some(&Value::from("abc")));
            assert_eq!(r2a.get("ws_id"), Some(&Value::from("s1")));

            let r2b: Value =
                serde_json::from_str(&rx2.recv_timeout(Duration::from_secs(1)).unwrap_or_default())
                    .unwrap_or(Value::Null);
            assert_eq!(r2b.get("ws_id"), Some(&Value::from("s1")));
            assert_eq!(r2b.get("data"), Some(&Value::from("hi")));

            // Client 2 disconnect sends ws_close to guest
            state.unregister_client(2);
            let mut l3 = String::new();
            assert!(br.read_line(&mut l3).is_ok());
            let v3: Value = serde_json::from_str(&l3).unwrap_or(Value::Null);
            assert_eq!(v3.get("op"), Some(&Value::from("ws_close")));
            assert_eq!(v3.get("ws_id"), Some(&Value::from(mws2)));
        }
    }

    #[test]
    fn test_cross_client_ws_control() {
        let pair = UnixStream::pair().ok();
        assert!(pair.is_some());
        if let Some((guest_end, mux_end)) = pair {
            let state = MuxState::new();
            if let Ok(mut guard) = state.hvc_writer.lock() {
                *guard = Some(mux_end);
            }
            let (tx_stream, rx_stream) = mpsc::channel::<String>();
            let (tx_ctl, rx_ctl) = mpsc::channel::<String>();
            state.register_client(10, tx_stream);
            state.register_client(11, tx_ctl);

            let mut ws_stream = HashMap::new();
            let mut ws_ctl = HashMap::new();
            state.handle_client_line(
                10,
                &mut ws_stream,
                "{\"id\":\"open\",\"op\":\"ws_open\",\"ws_id\":\"w_ext_1\",\"path\":\"/logs\"}",
            );

            let mut br = BufReader::new(guest_end);
            let mut l1 = String::new();
            assert!(br.read_line(&mut l1).is_ok());
            let v1: Value = serde_json::from_str(&l1).unwrap_or(Value::Null);
            let mws = v1.get("ws_id").and_then(Value::as_str).unwrap_or("");
            assert_ne!(mws, "");

            // Client 11 sends ws_send targeting client 10's ws_id
            state.handle_client_line(
                11,
                &mut ws_ctl,
                "{\"id\":\"s1\",\"op\":\"ws_send\",\"ws_id\":\"w_ext_1\",\"binary\":false,\"data\":\"ping\"}",
            );
            let mut l2 = String::new();
            assert!(br.read_line(&mut l2).is_ok());
            let v2: Value = serde_json::from_str(&l2).unwrap_or(Value::Null);
            assert_eq!(v2.get("ws_id"), Some(&Value::from(mws)));
            assert_eq!(v2.get("data"), Some(&Value::from("ping")));
            let r_send: Value = serde_json::from_str(
                &rx_ctl
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap_or_default(),
            )
            .unwrap_or(Value::Null);
            assert_eq!(r_send.get("ok"), Some(&Value::from(true)));

            // Client 11 sends ws_close targeting client 10's ws_id
            state.handle_client_line(
                11,
                &mut ws_ctl,
                "{\"id\":\"c1\",\"op\":\"ws_close\",\"ws_id\":\"w_ext_1\"}",
            );
            let mut l3 = String::new();
            assert!(br.read_line(&mut l3).is_ok());
            let v3: Value = serde_json::from_str(&l3).unwrap_or(Value::Null);
            assert_eq!(v3.get("op"), Some(&Value::from("ws_close")));
            assert_eq!(v3.get("ws_id"), Some(&Value::from(mws)));

            let close_stream: Value = serde_json::from_str(
                &rx_stream
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap_or_default(),
            )
            .unwrap_or(Value::Null);
            assert_eq!(close_stream.get("op"), Some(&Value::from("ws_close")));
            assert_eq!(close_stream.get("ws_id"), Some(&Value::from("w_ext_1")));
        }
    }

    #[test]
    fn test_mux_without_guest_returns_503() {
        let state = MuxState::new();
        let (tx, rx) = mpsc::channel::<String>();
        state.register_client(1, tx);
        let mut ws = HashMap::new();
        state.handle_client_line(1, &mut ws, "{\"id\":99,\"op\":\"ping\"}");
        let resp: Value =
            serde_json::from_str(&rx.recv_timeout(Duration::from_secs(1)).unwrap_or_default())
                .unwrap_or(Value::Null);
        assert_eq!(resp.get("id"), Some(&Value::from(99_i32)));
        assert_eq!(resp.get("ok"), Some(&Value::from(false)));
        assert_eq!(resp.get("status"), Some(&Value::from(503_i32)));
    }
}

#[cfg(test)]
mod edition_tests;

#[cfg(test)]
mod render_tests;
