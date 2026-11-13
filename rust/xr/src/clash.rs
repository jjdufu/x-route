use std::collections::{HashMap, HashSet};
use std::fs::{self, Permissions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::Signal;
use serde::Serialize;
use serde_json::{json, Map, Value};
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::flag;

use crate::env::Env;
use crate::log::log_info;
use crate::sys::spawn::spawn_self_detached;
use crate::sys::{is_pid_running, send_signal};
use crate::vm::vm_running;

#[derive(Debug, Clone, Serialize)]
pub struct ClashStatus {
    pub installed: bool,
    pub running: bool,
    pub port: u16,
}

impl Default for ClashStatus {
    fn default() -> Self {
        Self {
            installed: false,
            running: false,
            port: 9090,
        }
    }
}

pub fn clash_status(env: &Env, alive: bool) -> ClashStatus {
    if !alive {
        return ClashStatus::default();
    }
    let path = env.run_dir().join("guest/guest/openclash.status");
    let Ok(raw) = fs::read_to_string(path) else {
        return ClashStatus::default();
    };
    let mut st = ClashStatus::default();
    for line in raw.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let val = v.trim();
        match k.trim() {
            "installed" => st.installed = val == "1",
            "running" => st.running = val == "1",
            "port" => {
                if let Ok(p) = val.parse::<u16>() {
                    if p > 0 {
                        st.port = p;
                    }
                }
            }
            _ => {}
        }
    }
    st
}

pub fn stop_clash_mux(env: &Env) {
    let run = env.run_dir();
    if let Ok(pid_raw) = fs::read_to_string(run.join("clash_mux.pid")) {
        let pid = pid_raw.trim();
        if !pid.is_empty() && is_pid_running(env, pid) {
            let _ = send_signal(pid, Signal::TERM);
            for _ in 0..10 {
                if !is_pid_running(env, pid) {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
            if is_pid_running(env, pid) {
                let _ = send_signal(pid, Signal::KILL);
            }
        }
    }
    for f in [
        "clash_mux.pid",
        "clash_mux.ready",
        "clash-ctl.sock",
        "clash-rpc.sock",
    ] {
        let _ = fs::remove_file(run.join(f));
    }
}

pub fn start_clash_mux(env: &Env) {
    stop_clash_mux(env);
    let run = env.run_dir();
    let ready = run.join("clash_mux.ready");
    if spawn_self_detached(env, &["internal", "clash-mux"]).is_err() {
        log_info(env, "clash", "clash-mux 启动失败");
        return;
    }
    for _ in 0..100 {
        if ready.is_file() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    log_info(env, "clash", "clash-mux 未在预期时间内就绪");
}

struct ClientHandle {
    tx: mpsc::Sender<String>,
    ws_ids: HashSet<String>,
}

struct MuxState {
    hvc_writer: Mutex<Option<UnixStream>>,
    clients: Mutex<HashMap<u64, ClientHandle>>,
    pending_reqs: Mutex<HashMap<String, (u64, Value)>>,
    active_ws: Mutex<HashMap<String, (u64, String)>>,
    ws_by_key: Mutex<HashMap<String, (u64, String)>>,
    seq: AtomicU64,
}

impl MuxState {
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

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

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

    fn send_to_client(&self, client_id: u64, line: String) {
        if let Ok(clients) = self.clients.lock() {
            if let Some(ch) = clients.get(&client_id) {
                let _ = ch.tx.send(line);
            }
        }
    }

    fn register_client(&self, client_id: u64, tx: mpsc::Sender<String>) {
        if let Ok(mut clients) = self.clients.lock() {
            clients.insert(
                client_id,
                ClientHandle {
                    tx,
                    ws_ids: HashSet::new(),
                },
            );
        }
    }

    fn unregister_client(&self, client_id: u64) {
        let orphaned_ws: Vec<String> = if let Ok(mut clients) = self.clients.lock() {
            clients
                .remove(&client_id)
                .map_or_else(Vec::new, |c| c.ws_ids.into_iter().collect())
        } else {
            Vec::new()
        };
        if let Ok(mut pending) = self.pending_reqs.lock() {
            pending.retain(|_, (cid, _)| *cid != client_id);
        }
        if let Ok(mut active) = self.active_ws.lock() {
            for mux_ws in &orphaned_ws {
                active.remove(mux_ws);
            }
        }
        if let Ok(mut by_key) = self.ws_by_key.lock() {
            by_key.retain(|_, (cid, mux_ws)| *cid != client_id && !orphaned_ws.contains(mux_ws));
        }
        for mux_ws in orphaned_ws {
            let close_cmd = json!({"op": "ws_close", "ws_id": mux_ws}).to_string();
            let _ = self.write_hvc_line(&close_cmd);
        }
    }

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

        if let Some(mux_id) = obj.get("id").and_then(Value::as_str).map(str::to_string) {
            if let Ok(mut pending) = self.pending_reqs.lock() {
                if let Some((cid, orig_id)) = pending.remove(&mux_id) {
                    target_client = Some(cid);
                    obj.insert("id".to_string(), orig_id);
                }
            }
        }

        if let Some(mux_ws) = obj.get("ws_id").and_then(Value::as_str).map(str::to_string) {
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
                            by_key.remove(&orig_ws);
                        }
                        if let Ok(mut clients) = self.clients.lock() {
                            if let Some(ch) = clients.get_mut(&cid) {
                                ch.ws_ids.remove(&mux_ws);
                            }
                        }
                    }
                    if target_client.is_none() {
                        target_client = Some(cid);
                    }
                    obj.insert("ws_id".to_string(), Value::String(orig_ws));
                }
            }
        }

        if let Some(cid) = target_client {
            if let Ok(out) = serde_json::to_string(&val) {
                self.send_to_client(cid, out);
            }
        }
    }

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
            .to_string();
        let orig_id = obj.get("id").cloned();

        let mut external_ws_owner: Option<(u64, String, String)> = None;

        match op.as_str() {
            "ws_open" => {
                let orig_ws = obj
                    .get("ws_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if orig_ws.is_empty() {
                    return;
                }
                let mux_ws = format!("w{}", self.next_seq());
                ws_map.insert(orig_ws.clone(), mux_ws.clone());
                if let Ok(mut active) = self.active_ws.lock() {
                    active.insert(mux_ws.clone(), (client_id, orig_ws.clone()));
                }
                if let Ok(mut by_key) = self.ws_by_key.lock() {
                    by_key.insert(orig_ws, (client_id, mux_ws.clone()));
                }
                if let Ok(mut clients) = self.clients.lock() {
                    if let Some(ch) = clients.get_mut(&client_id) {
                        ch.ws_ids.insert(mux_ws.clone());
                    }
                }
                obj.insert("ws_id".to_string(), Value::String(mux_ws));
            }
            "ws_send" | "ws_close" => {
                let orig_ws = obj
                    .get("ws_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let local_mapped = if op == "ws_close" {
                    ws_map.remove(&orig_ws)
                } else {
                    ws_map.get(&orig_ws).cloned()
                };
                let (owner_cid, mux_ws) = if let Some(m) = local_mapped {
                    (client_id, m)
                } else if let Ok(mut by_key) = self.ws_by_key.lock() {
                    let found = if op == "ws_close" {
                        by_key.remove(&orig_ws)
                    } else {
                        by_key.get(&orig_ws).cloned()
                    };
                    if let Some((ocid, m)) = found {
                        external_ws_owner = Some((ocid, orig_ws.clone(), m.clone()));
                        (ocid, m)
                    } else {
                        if let Some(oid) = orig_id {
                            self.send_to_client(
                                client_id,
                                json!({"id": oid, "ok": op == "ws_close"}).to_string(),
                            );
                        }
                        return;
                    }
                } else {
                    return;
                };
                if op == "ws_close" {
                    if let Ok(mut by_key) = self.ws_by_key.lock() {
                        by_key.remove(&orig_ws);
                    }
                    if let Ok(mut active) = self.active_ws.lock() {
                        active.remove(&mux_ws);
                    }
                    if let Ok(mut clients) = self.clients.lock() {
                        if let Some(ch) = clients.get_mut(&owner_cid) {
                            ch.ws_ids.remove(&mux_ws);
                        }
                    }
                    if external_ws_owner.is_some() {
                        self.send_to_client(
                            owner_cid,
                            json!({"op": "ws_close", "ws_id": orig_ws, "code": 1000, "reason": "closed"})
                                .to_string(),
                        );
                    }
                }
                obj.insert("ws_id".to_string(), Value::String(mux_ws));
            }
            _ => {}
        }

        let is_ext_ws_ctl = external_ws_owner.is_some() && (op == "ws_send" || op == "ws_close");
        let mux_req_id = if is_ext_ws_ctl {
            obj.remove("id");
            None
        } else if let Some(ref oid) = orig_id {
            let mid = format!("r{}", self.next_seq());
            if let Ok(mut pending) = self.pending_reqs.lock() {
                pending.insert(mid.clone(), (client_id, oid.clone()));
            }
            obj.insert("id".to_string(), Value::String(mid.clone()));
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
            if let Some(mid) = mux_req_id {
                if let Ok(mut pending) = self.pending_reqs.lock() {
                    pending.remove(&mid);
                }
            }
            if let Some(oid) = orig_id {
                let mut err = Map::new();
                err.insert("id".to_string(), oid);
                err.insert("ok".to_string(), Value::Bool(false));
                err.insert("status".to_string(), Value::from(503));
                err.insert(
                    "error".to_string(),
                    Value::String("hvc1 channel not connected".to_string()),
                );
                self.send_to_client(client_id, Value::Object(err).to_string());
            }
        }
    }
}

fn serve_rpc_client(state: &MuxState, stream: UnixStream, client_id: u64) {
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
    let _ = writer_handle.join();
}

pub fn run_clash_mux(env: &Env) -> i32 {
    let run = env.run_dir();
    let ctl_sock = run.join("clash-ctl.sock");
    let rpc_sock = run.join("clash-rpc.sock");
    let pid_file = run.join("clash_mux.pid");
    let ready_file = run.join("clash_mux.ready");

    let _ = fs::remove_file(&ctl_sock);
    let _ = fs::remove_file(&rpc_sock);
    let _ = fs::remove_file(&ready_file);

    let Ok(ctl_listener) = UnixListener::bind(&ctl_sock) else {
        return 1;
    };
    let Ok(rpc_listener) = UnixListener::bind(&rpc_sock) else {
        let _ = fs::remove_file(&ctl_sock);
        return 1;
    };
    let _ = fs::set_permissions(&ctl_sock, Permissions::from_mode(0o600));
    let _ = fs::set_permissions(&rpc_sock, Permissions::from_mode(0o600));
    let _ = ctl_listener.set_nonblocking(true);
    let _ = rpc_listener.set_nonblocking(true);

    let stop = Arc::new(AtomicBool::new(false));
    let _ = flag::register(SIGTERM, Arc::clone(&stop));
    let _ = flag::register(SIGINT, Arc::clone(&stop));

    let _ = fs::write(&pid_file, format!("{}\n", std::process::id()));
    let _ = fs::write(&ready_file, "ready\n");

    let state = Arc::new(MuxState::new());

    let s_ctl = Arc::clone(&state);
    let stop_ctl = Arc::clone(&stop);
    let ctl_thread = thread::spawn(move || {
        while !stop_ctl.load(Ordering::Relaxed) {
            match ctl_listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nonblocking(false);
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
                    if let Ok(mut guard) = s_ctl.hvc_writer.lock() {
                        *guard = None;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(_) => break,
            }
        }
    });

    let s_rpc = Arc::clone(&state);
    let stop_rpc = Arc::clone(&stop);
    let rpc_thread = thread::spawn(move || {
        let mut next_cid = 1_u64;
        while !stop_rpc.load(Ordering::Relaxed) {
            match rpc_listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let cid = next_cid;
                    next_cid = next_cid.saturating_add(1);
                    let s_cl = Arc::clone(&s_rpc);
                    thread::spawn(move || serve_rpc_client(&s_cl, stream, cid));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(_) => break,
            }
        }
    });

    let started = Instant::now();
    let mut saw_vm = false;
    while !stop.load(Ordering::Relaxed) {
        if vm_running(env) {
            saw_vm = true;
        } else if (saw_vm || started.elapsed() > Duration::from_secs(10))
            && !env.fuse_file().is_file()
        {
            break;
        }
        thread::sleep(Duration::from_millis(300));
    }

    stop.store(true, Ordering::Relaxed);
    let _ = ctl_thread.join();
    let _ = rpc_thread.join();
    for f in [
        "clash_mux.pid",
        "clash_mux.ready",
        "clash-ctl.sock",
        "clash-rpc.sock",
    ] {
        let _ = fs::remove_file(run.join(f));
    }
    0
}

pub fn run_clash_bridge(env: &Env) -> i32 {
    let rpc_sock = env.run_dir().join("clash-rpc.sock");
    let Ok(stream) = UnixStream::connect(&rpc_sock) else {
        println!("{{\"ok\":false,\"error\":\"clash channel unavailable\"}}");
        return 1;
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
    let _ = sock_out.shutdown(Shutdown::Both);
    let _ = stream.shutdown(Shutdown::Both);
    let _ = reader_handle.join();
    0
}

pub fn run_clash_ping(env: &Env) -> i32 {
    let rpc_sock = env.run_dir().join("clash-rpc.sock");
    let Ok(mut stream) = UnixStream::connect(&rpc_sock) else {
        println!("{{\"ok\":false,\"error\":\"clash channel unavailable\"}}");
        return 1;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    if stream.write_all(b"{\"id\":1,\"op\":\"ping\"}\n").is_err() || stream.flush().is_err() {
        println!("{{\"ok\":false,\"error\":\"write failed\"}}");
        return 1;
    }
    let mut br = BufReader::new(stream);
    let mut line = String::new();
    match br.read_line(&mut line) {
        Ok(0) | Err(_) => {
            println!("{{\"ok\":false,\"error\":\"timeout or closed\"}}");
            1
        }
        Ok(_) => {
            let out = line.trim();
            println!("{out}");
            i32::from(!out.contains("\"ok\":true"))
        }
    }
}

fn emit_rpc_line(line: &str) {
    const MAX_CHUNK: usize = 49_152;
    let trimmed = line.trim_end_matches(['\r', '\n']);
    if trimmed.len() <= MAX_CHUNK {
        println!("{trimmed}");
        let _ = std::io::stdout().flush();
        return;
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
        chunks.push(&trimmed[start..end]);
        start = end;
    }
    println!("{{\"_chunks\":{}}}", chunks.len());
    for c in chunks {
        println!("@{c}");
    }
    let _ = std::io::stdout().flush();
}

pub fn run_clash_call(env: &Env, req_json: &str) -> i32 {
    let Ok(mut val) = serde_json::from_str::<Value>(req_json) else {
        println!("{{\"ok\":false,\"status\":400,\"error\":\"invalid json\"}}");
        return 2;
    };
    let Some(obj) = val.as_object_mut() else {
        println!("{{\"ok\":false,\"status\":400,\"error\":\"json object required\"}}");
        return 2;
    };
    if !obj.contains_key("id") {
        obj.insert("id".to_string(), Value::String("c1".to_string()));
    }
    let rpc_sock = env.run_dir().join("clash-rpc.sock");
    let Ok(mut stream) = UnixStream::connect(&rpc_sock) else {
        println!("{{\"ok\":false,\"status\":503,\"error\":\"clash channel unavailable\"}}");
        return 1;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let wire = val.to_string();
    if stream.write_all(wire.as_bytes()).is_err()
        || stream.write_all(b"\n").is_err()
        || stream.flush().is_err()
    {
        println!("{{\"ok\":false,\"status\":502,\"error\":\"write failed\"}}");
        return 1;
    }
    let mut br = BufReader::new(stream);
    let mut line = String::new();
    match br.read_line(&mut line) {
        Ok(0) | Err(_) => {
            println!("{{\"ok\":false,\"status\":504,\"error\":\"timeout or closed\"}}");
            1
        }
        Ok(_) => {
            emit_rpc_line(&line);
            0
        }
    }
}

pub fn run_clash_stream(env: &Env, ws_id: &str, path: &str) -> i32 {
    if ws_id.is_empty() || !path.starts_with('/') {
        println!("{{\"op\":\"ws_close\",\"ok\":false,\"error\":\"invalid args\"}}");
        return 2;
    }
    let rpc_sock = env.run_dir().join("clash-rpc.sock");
    let Ok(mut stream) = UnixStream::connect(&rpc_sock) else {
        println!("{{\"op\":\"ws_close\",\"ws_id\":\"{ws_id}\",\"code\":1006,\"reason\":\"channel unavailable\"}}");
        return 1;
    };
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let open_cmd = json!({"id": "open", "op": "ws_open", "ws_id": ws_id, "path": path}).to_string();
    if stream.write_all(open_cmd.as_bytes()).is_err()
        || stream.write_all(b"\n").is_err()
        || stream.flush().is_err()
    {
        println!("{{\"op\":\"ws_close\",\"ws_id\":\"{ws_id}\",\"code\":1006,\"reason\":\"write failed\"}}");
        return 1;
    }
    let mut br = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match br.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                emit_rpc_line(&line);
                if let Ok(v) = serde_json::from_str::<Value>(line.trim()) {
                    let is_close = v.get("op").and_then(Value::as_str) == Some("ws_close");
                    let open_fail = v.get("id").and_then(Value::as_str) == Some("open")
                        && v.get("ok").and_then(Value::as_bool) == Some(false);
                    if is_close || open_fail {
                        break;
                    }
                }
            }
        }
    }
    0
}

pub fn run_clash_ws_send(env: &Env, ws_id: &str, mode: &str, payload: &str) -> i32 {
    if ws_id.is_empty() {
        return 2;
    }
    let rpc_sock = env.run_dir().join("clash-rpc.sock");
    let Ok(mut stream) = UnixStream::connect(&rpc_sock) else {
        println!("{{\"ok\":false,\"error\":\"clash channel unavailable\"}}");
        return 1;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
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
        println!("{{\"ok\":false,\"error\":\"write failed\"}}");
        return 1;
    }
    let mut br = BufReader::new(stream);
    let mut line = String::new();
    match br.read_line(&mut line) {
        Ok(0) | Err(_) => 1,
        Ok(_) => {
            let out = line.trim();
            println!("{out}");
            i32::from(!out.contains("\"ok\":true"))
        }
    }
}

pub fn run_clash_ws_close(env: &Env, ws_id: &str) -> i32 {
    if ws_id.is_empty() {
        return 2;
    }
    let rpc_sock = env.run_dir().join("clash-rpc.sock");
    let Ok(mut stream) = UnixStream::connect(&rpc_sock) else {
        println!("{{\"ok\":true}}");
        return 0;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
    let cmd = json!({"id": "c1", "op": "ws_close", "ws_id": ws_id}).to_string();
    if stream.write_all(cmd.as_bytes()).is_err()
        || stream.write_all(b"\n").is_err()
        || stream.flush().is_err()
    {
        return 0;
    }
    let mut br = BufReader::new(stream);
    let mut line = String::new();
    let _ = br.read_line(&mut line);
    if line.trim().is_empty() {
        println!("{{\"ok\":true}}");
    } else {
        println!("{}", line.trim());
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_clash_status_parsing() {
        let td = TempDir::new().ok();
        assert!(td.is_some());
        if let Some(d) = td {
            let mut env = Env::from_process();
            env.xr_data = d.path().to_path_buf();
            let st_dir = env.run_dir().join("guest/guest");
            assert!(fs::create_dir_all(&st_dir).is_ok());
            assert!(fs::write(
                st_dir.join("openclash.status"),
                "installed=1\nrunning=1\nport=9095\n"
            )
            .is_ok());

            let off = clash_status(&env, false);
            assert!(!off.installed);
            assert!(!off.running);
            assert_eq!(off.port, 9090);

            let on = clash_status(&env, true);
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
            assert_eq!(r1.get("id"), Some(&Value::from(10)));

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
        assert_eq!(resp.get("id"), Some(&Value::from(99)));
        assert_eq!(resp.get("ok"), Some(&Value::from(false)));
        assert_eq!(resp.get("status"), Some(&Value::from(503)));
    }
}
