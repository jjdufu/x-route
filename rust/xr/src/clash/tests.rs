//! `clash` 复用进程的单元测试：状态解析、多客户端复用与清理、跨客户端 ws 控制、无客机时的 503，
//! 以及 edition 2024 语义点（锁守卫与 `accept` 结果的析构时机）和控制通道重连。

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;
use xr_test_support::{Failure, ensure, ensure_eq, ensure_ne, ensure_ok};

use super::{MuxState, run_clash_mux, status};
use crate::env::Env;

/// 从通道收一行并解析为 JSON。
///
/// # Errors
///
/// 超时或解析失败时返回错误。
fn recv_json(rx: &mpsc::Receiver<String>) -> Result<Value, Failure> {
    let line = rx
        .recv_timeout(Duration::from_secs(1))
        .map_err(|_recv_err| Failure::new("等待应答超时"))?;
    serde_json::from_str(&line).map_err(|_json_err| Failure::new("应答不是 JSON"))
}

/// 读客机一侧收到的一行 JSON。
///
/// # Errors
///
/// 读失败或解析失败时返回错误。
fn read_guest(br: &mut BufReader<UnixStream>) -> Result<Value, Failure> {
    let mut line = String::new();
    let read = br
        .read_line(&mut line)
        .map_err(|_io_err| Failure::new("读客机侧失败"))?;
    ensure(read > 0, "客机侧已关闭")?;
    serde_json::from_str(&line).map_err(|_json_err| Failure::new("客机侧不是 JSON"))
}

/// `unregister_client`（clash.rs:188）结束后不残留任何锁，并清掉该客户端的全部状态。
///
/// # Errors
///
/// 任一断言不成立时返回错误。
#[test]
fn unregister_client_releases_locks() -> Result<(), Failure> {
    let state = MuxState::new();
    let (tx, _rx) = mpsc::channel::<String>();
    state.register_client(7, tx);
    let inserted = state
        .clients
        .lock()
        .map_err(|_poison| Failure::new("clients 中毒"))?
        .get_mut(&7)
        .is_some_and(|handle| handle.ws_ids.insert("w1".to_owned()));
    ensure(inserted, "ws id 插入失败")?;
    let prev_active = state
        .active_ws
        .lock()
        .map_err(|_poison| Failure::new("active 中毒"))?
        .insert("w1".to_owned(), (7, "s1".to_owned()));
    ensure(prev_active.is_none(), "active 已有值")?;
    let prev_pending = state
        .pending_reqs
        .lock()
        .map_err(|_poison| Failure::new("pending 中毒"))?
        .insert("r1".to_owned(), (7, Value::from(1_i32)));
    ensure(prev_pending.is_none(), "pending 已有值")?;
    state.unregister_client(7);
    let has_client = state
        .clients
        .try_lock()
        .map_err(|_lock| Failure::new("clients 仍被锁"))?
        .contains_key(&7);
    ensure(!has_client, "clients 仍含 7")?;
    let has_active = state
        .active_ws
        .try_lock()
        .map_err(|_lock| Failure::new("active 仍被锁"))?
        .contains_key("w1");
    ensure(!has_active, "active 仍含 w1")?;
    let pending_clean = state
        .pending_reqs
        .try_lock()
        .map_err(|_lock| Failure::new("pending 仍被锁"))?
        .values()
        .all(|(cid, _)| *cid != 7);
    ensure(pending_clean, "pending 仍有 cid=7")
}

/// 建一个接了客机侧 socketpair 的 `MuxState`，注册客户端 1、2，返回客机读端与两个客户端的接收端。
///
/// # Errors
///
/// socketpair 失败或锁中毒时返回错误。
fn wired_state() -> Result<WiredState, Failure> {
    wired_clients(1, 2)
}

/// 建一个接了客机侧 socketpair 的 `MuxState`，注册客户端 `id_a`、`id_b`，返回客机读端与两个客户端的接收端。
///
/// # Errors
///
/// socketpair 失败或锁中毒时返回错误。
fn wired_clients(id_a: u64, id_b: u64) -> Result<WiredState, Failure> {
    let (guest_end, mux_end) =
        UnixStream::pair().map_err(|_io_err| Failure::new("socketpair 失败"))?;
    let state = MuxState::new();
    *state
        .hvc_writer
        .lock()
        .map_err(|_poison| Failure::new("hvc 中毒"))? = Some(mux_end);
    let (tx_a, rx_a) = mpsc::channel::<String>();
    let (tx_b, rx_b) = mpsc::channel::<String>();
    state.register_client(id_a, tx_a);
    state.register_client(id_b, tx_b);
    Ok((state, BufReader::new(guest_end), rx_a, rx_b))
}

/// `wired_state` 的返回值：状态、客机读端、客户端 1 与 2 的接收端。
type WiredState = (
    MuxState,
    BufReader<UnixStream>,
    mpsc::Receiver<String>,
    mpsc::Receiver<String>,
);

/// 外部客户端按 key 关闭 ws（clash.rs:327）后通知原主人，`ws_by_key` 不残留锁。
///
/// # Errors
///
/// 任一断言不成立时返回错误。
#[test]
fn ws_close_by_external_key_returns_owner() -> Result<(), Failure> {
    let (state, mut br, rx_a, rx_b) = wired_state()?;
    let mut map_a = HashMap::new();
    let mut map_b = HashMap::new();
    state.handle_client_line(
        1,
        &mut map_a,
        "{\"id\":\"o\",\"op\":\"ws_open\",\"ws_id\":\"k1\"}",
    );
    let opened = read_guest(&mut br)?;
    let mux_ws = opened
        .get("ws_id")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::new("无 mux ws"))?
        .to_owned();
    state.handle_client_line(
        2,
        &mut map_b,
        "{\"op\":\"ws_close\",\"ws_id\":\"k1\",\"id\":9}",
    );
    let closed = read_guest(&mut br)?;
    ensure(
        closed.get("ws_id") == Some(&Value::from(mux_ws.as_str())),
        "客机侧 ws_id 不对",
    )?;
    let reply = recv_json(&rx_b)?;
    ensure(reply.get("id") == Some(&Value::from(9_i32)), "应答 id 不对")?;
    ensure(reply.get("ok") == Some(&Value::from(true)), "应答 ok 不对")?;
    let owner_note = recv_json(&rx_a)?;
    ensure(
        owner_note.get("op") == Some(&Value::from("ws_close")),
        "原主人未收到关闭",
    )?;
    let has_key = state
        .ws_by_key
        .try_lock()
        .map_err(|_lock| Failure::new("ws_by_key 仍被锁"))?
        .contains_key("k1");
    ensure(!has_key, "ws_by_key 仍含 k1")
}

/// key 不存在时：`ws_close` 回 `ok:true`，`ws_send` 回 `ok:false`，与改动前一致。
///
/// # Errors
///
/// 任一断言不成立时返回错误。
#[test]
fn ws_ctl_missing_key_replies_as_before() -> Result<(), Failure> {
    let (state, _br, _rx_a, rx_b) = wired_state()?;
    let mut map_b = HashMap::new();
    state.handle_client_line(
        2,
        &mut map_b,
        "{\"op\":\"ws_close\",\"ws_id\":\"nope\",\"id\":10}",
    );
    let missing_close = recv_json(&rx_b)?;
    ensure(
        missing_close.get("ok") == Some(&Value::from(true)),
        "不存在的 key：ws_close 应回 ok:true",
    )?;
    state.handle_client_line(
        2,
        &mut map_b,
        "{\"op\":\"ws_send\",\"ws_id\":\"nope\",\"id\":11}",
    );
    let missing_send = recv_json(&rx_b)?;
    ensure(
        missing_send.get("ok") == Some(&Value::from(false)),
        "不存在的 key：ws_send 应回 ok:false",
    )
}

/// 轮询直到 `pred` 为真，最多 `rounds` 轮、每轮 50 ms。
fn poll_until(rounds: u32, mut pred: impl FnMut() -> bool) -> bool {
    let mut left = rounds;
    while left > 0 {
        if pred() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
        left = left.saturating_sub(1);
    }
    pred()
}

/// 准备一个临时 `Env`，`run` 目录已建好。
///
/// # Errors
///
/// 临时目录创建失败时返回错误。
fn temp_env() -> Result<(TempDir, Env), Failure> {
    let dir = TempDir::new().map_err(|_io_err| Failure::new("临时目录失败"))?;
    let mut env = Env::from_process();
    env.xr_data = dir.path().to_path_buf();
    fs::create_dir_all(env.run_dir()).map_err(|_io_err| Failure::new("建 run 目录失败"))?;
    fs::create_dir_all(env.xr_data.join("config"))
        .map_err(|_io_err| Failure::new("建 config 目录失败"))?;
    fs::write(env.fuse_file(), "1\n").map_err(|_io_err| Failure::new("写 fuse 失败"))?;
    Ok((dir, env))
}

/// 启动 mux 线程并等待 ready。
///
/// # Errors
///
/// 未在 2 s 内就绪时返回错误。
fn start_mux(env: &Env) -> Result<thread::JoinHandle<i32>, Failure> {
    let env_copy = env.clone();
    let handle = thread::spawn(move || run_clash_mux(&env_copy));
    let ready = env.run_dir().join("clash_mux.ready");
    ensure(poll_until(40, || ready.is_file()), "mux 未就绪")?;
    Ok(handle)
}

/// 停止 mux（删除 fuse 文件使主循环退出）并等待线程结束。
///
/// # Errors
///
/// 线程 panic 时返回错误。
fn stop_mux(env: &Env, handle: thread::JoinHandle<i32>) -> Result<(), Failure> {
    fs::remove_file(env.fuse_file()).map_err(|_io_err| Failure::new("删 fuse 失败"))?;
    match handle.join() {
        Ok(_code) => Ok(()),
        Err(_panic) => Err(Failure::new("mux 线程 panic")),
    }
}

/// ctl 连接断开后 `hvc_writer` 被清空（clash.rs:492/510），重新连接后恢复。用 rpc 请求是否得到 503 来观察。
///
/// # Errors
///
/// 任一断言不成立时返回错误。
#[test]
fn ctl_reconnect_clears_writer() -> Result<(), Failure> {
    let (_dir, env) = temp_env()?;
    let mux = start_mux(&env)?;
    let ctl_path = env.run_dir().join("clash-ctl.sock");
    let rpc_path = env.run_dir().join("clash-rpc.sock");
    let ctl = UnixStream::connect(&ctl_path).map_err(|_io_err| Failure::new("连 ctl 失败"))?;
    let mut rpc = UnixStream::connect(&rpc_path).map_err(|_io_err| Failure::new("连 rpc 失败"))?;
    let timeout = Some(Duration::from_millis(200));
    rpc.set_read_timeout(timeout)
        .map_err(|_io_err| Failure::new("设超时失败"))?;
    ctl.set_read_timeout(timeout)
        .map_err(|_io_err| Failure::new("设超时失败"))?;
    let mut rpc_reader = BufReader::new(
        rpc.try_clone()
            .map_err(|_io_err| Failure::new("clone 失败"))?,
    );
    let mut ctl_reader = BufReader::new(
        ctl.try_clone()
            .map_err(|_io_err| Failure::new("clone 失败"))?,
    );
    // 已连：请求应转发到客机侧
    let forwarded = poll_until(40, || {
        rpc.write_all(b"{\"id\":1,\"op\":\"ping\"}\n").is_ok()
            && read_guest(&mut ctl_reader).is_ok()
    });
    ensure(forwarded, "连接后请求未转发到客机")?;
    drop(ctl_reader);
    drop(ctl);
    // 断开：轮询直到请求得到 503
    let got_503 = poll_until(40, || {
        if rpc.write_all(b"{\"id\":2,\"op\":\"ping\"}\n").is_err() {
            return false;
        }
        let mut line = String::new();
        rpc_reader.read_line(&mut line).is_ok() && line.contains("503")
    });
    ensure(got_503, "断开后 hvc_writer 未清空")?;
    // 重连：请求再次被转发
    let ctl_again =
        UnixStream::connect(&ctl_path).map_err(|_io_err| Failure::new("重连 ctl 失败"))?;
    ctl_again
        .set_read_timeout(timeout)
        .map_err(|_io_err| Failure::new("设超时失败"))?;
    let mut again_reader = BufReader::new(ctl_again);
    let forwarded_again = poll_until(40, || {
        rpc.write_all(b"{\"id\":3,\"op\":\"ping\"}\n").is_ok()
            && read_guest(&mut again_reader).is_ok()
    });
    ensure(forwarded_again, "重连后请求未转发")?;
    drop(again_reader);
    drop(rpc_reader);
    drop(rpc);
    stop_mux(&env, mux)
}

/// rpc 接入（clash.rs:527）给客户端依次分配递增的 cid，停止后线程能正常结束。
///
/// # Errors
///
/// 任一断言不成立时返回错误。
#[test]
fn rpc_accept_assigns_increasing_cid() -> Result<(), Failure> {
    let (_dir, env) = temp_env()?;
    let mux = start_mux(&env)?;
    let rpc_path = env.run_dir().join("clash-rpc.sock");
    let mut first =
        UnixStream::connect(&rpc_path).map_err(|_io_err| Failure::new("连 rpc 1 失败"))?;
    let mut second =
        UnixStream::connect(&rpc_path).map_err(|_io_err| Failure::new("连 rpc 2 失败"))?;
    first
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|_io_err| Failure::new("设超时失败"))?;
    second
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|_io_err| Failure::new("设超时失败"))?;
    let mut first_reader = BufReader::new(
        first
            .try_clone()
            .map_err(|_io_err| Failure::new("clone 失败"))?,
    );
    let mut second_reader = BufReader::new(
        second
            .try_clone()
            .map_err(|_io_err| Failure::new("clone 失败"))?,
    );
    // 无客机：两个客户端各自收到自己的 503，证明 cid 不同且各自送达
    first
        .write_all(b"{\"id\":\"a\",\"op\":\"ping\"}\n")
        .map_err(|_io_err| Failure::new("写 1 失败"))?;
    second
        .write_all(b"{\"id\":\"b\",\"op\":\"ping\"}\n")
        .map_err(|_io_err| Failure::new("写 2 失败"))?;
    let mut line_a = String::new();
    let mut line_b = String::new();
    let _read_a: usize = first_reader
        .read_line(&mut line_a)
        .map_err(|_io_err| Failure::new("读 1 失败"))?;
    let _read_b: usize = second_reader
        .read_line(&mut line_b)
        .map_err(|_io_err| Failure::new("读 2 失败"))?;
    ensure(
        line_a.contains("\"a\"") && line_a.contains("503"),
        "客户端 1 应答不对",
    )?;
    ensure(
        line_b.contains("\"b\"") && line_b.contains("503"),
        "客户端 2 应答不对",
    )?;
    drop(first_reader);
    drop(second_reader);
    drop(first);
    drop(second);
    stop_mux(&env, mux)
}

/// 客机上报的 `OpenClash` 状态：虚拟机不在时返回默认值，在时按状态文件解析。
///
/// # Errors
///
/// 准备文件失败或任一断言不成立时返回错误。
#[test]
fn clash_status_parsing() -> Result<(), Failure> {
    let tmp = ensure_ok(TempDir::new(), "创建临时目录")?;
    let mut env = Env::from_process();
    env.xr_data = tmp.path().to_path_buf();
    let st_dir = env.run_dir().join("guest/guest");
    ensure_ok(fs::create_dir_all(&st_dir), "建状态目录")?;
    ensure_ok(
        fs::write(
            st_dir.join("openclash.status"),
            "installed=1\nrunning=1\nport=9095\n",
        ),
        "写状态文件",
    )?;

    let off = status(&env, false);
    ensure(!off.installed, "虚拟机不在时 installed 为假")?;
    ensure(!off.running, "虚拟机不在时 running 为假")?;
    ensure_eq(&off.port, &9090, "虚拟机不在时默认端口")?;

    let on = status(&env, true);
    ensure(on.installed, "installed=1")?;
    ensure(on.running, "running=1")?;
    ensure_eq(&on.port, &9095, "port=9095")
}

/// 收一行应答并解析为 JSON；解析失败时为 `Null`（与改动前的 `unwrap_or(Value::Null)` 相同）。
///
/// # Errors
///
/// 1 秒内没有应答时返回错误。
fn recv_value(rx: &mpsc::Receiver<String>) -> Result<Value, Failure> {
    let line = ensure_ok(rx.recv_timeout(Duration::from_secs(1)), "等待应答")?;
    Ok(serde_json::from_str(&line).unwrap_or(Value::Null))
}

/// 读客机一侧收到的一行并解析为 JSON；解析失败时为 `Null`。
///
/// # Errors
///
/// 读失败时返回错误。
fn guest_value(br: &mut BufReader<UnixStream>) -> Result<Value, Failure> {
    let mut line = String::new();
    let _read = ensure_ok(br.read_line(&mut line), "读客机侧")?;
    Ok(serde_json::from_str(&line).unwrap_or(Value::Null))
}

/// 两个客户端的请求转发给客机时改写为不冲突的编号，客机应答与 ws 消息按编号送回原客户端。
///
/// # Errors
///
/// 任一断言不成立时返回错误。
#[test]
fn mux_multiplex() -> Result<(), Failure> {
    let (state, mut br, rx1, rx2) = wired_state()?;
    let mws2 = open_ws_on_client2(&state, &mut br)?;

    let r1 = recv_value(&rx1)?;
    ensure_eq(
        &r1.get("id"),
        &Some(&Value::from(10_i32)),
        "客户端 1 收到原 id",
    )?;

    let r2a = recv_value(&rx2)?;
    ensure_eq(
        &r2a.get("id"),
        &Some(&Value::from("abc")),
        "客户端 2 收到原 id",
    )?;
    ensure_eq(
        &r2a.get("ws_id"),
        &Some(&Value::from("s1")),
        "客户端 2 收到原 ws_id",
    )?;

    let r2b = recv_value(&rx2)?;
    ensure_eq(
        &r2b.get("ws_id"),
        &Some(&Value::from("s1")),
        "ws 消息带原 ws_id",
    )?;
    ensure_eq(&r2b.get("data"), &Some(&Value::from("hi")), "ws 消息内容")?;
    ensure(!mws2.is_empty(), "复用 ws 编号非空")
}

/// 客户端断开后，复用进程通知客机关闭它打开的 ws。
///
/// # Errors
///
/// 任一断言不成立时返回错误。
#[test]
fn mux_cleanup() -> Result<(), Failure> {
    let (state, mut br, _rx1, _rx2) = wired_state()?;
    let mws2 = open_ws_on_client2(&state, &mut br)?;

    // 客户端 2 断开：向客机发送 ws_close
    state.unregister_client(2);
    let v3 = guest_value(&mut br)?;
    ensure_eq(
        &v3.get("op"),
        &Some(&Value::from("ws_close")),
        "客机收到 ws_close",
    )?;
    ensure_eq(
        &v3.get("ws_id"),
        &Some(&Value::from(mws2.as_str())),
        "关闭的是复用 ws",
    )
}

/// 客户端 1 发 ping、客户端 2 打开 ws，读出客机侧的两行并模拟客机应答；返回客户端 2 的 ws 在复用进程内的编号。
///
/// # Errors
///
/// 读客机侧失败或改写后的编号为空时返回错误。
fn open_ws_on_client2(state: &MuxState, br: &mut BufReader<UnixStream>) -> Result<String, Failure> {
    let mut ws1 = HashMap::new();
    let mut ws2 = HashMap::new();
    state.handle_client_line(1, &mut ws1, "{\"id\":10,\"op\":\"ping\"}");
    state.handle_client_line(
        2,
        &mut ws2,
        "{\"id\":\"abc\",\"op\":\"ws_open\",\"ws_id\":\"s1\",\"path\":\"/traffic\"}",
    );
    let v1 = guest_value(br)?;
    let v2 = guest_value(br)?;
    let mid1 = v1.get("id").and_then(Value::as_str).unwrap_or("");
    let mid2 = v2.get("id").and_then(Value::as_str).unwrap_or("");
    let mws2 = v2.get("ws_id").and_then(Value::as_str).unwrap_or("");
    ensure(
        !mid1.is_empty() && !mid2.is_empty() && !mws2.is_empty(),
        "请求 id 与 ws_id 已改写",
    )?;

    state.handle_guest_line(&format!("{{\"id\":\"{mid1}\",\"ok\":true}}"));
    state.handle_guest_line(&format!(
        "{{\"id\":\"{mid2}\",\"op\":\"ws_open_ok\",\"ok\":true,\"ws_id\":\"{mws2}\"}}"
    ));
    state.handle_guest_line(&format!(
        "{{\"op\":\"ws_msg\",\"ws_id\":\"{mws2}\",\"binary\":false,\"data\":\"hi\"}}"
    ));
    Ok(mws2.to_owned())
}

/// 另一个客户端可以按原 `ws_id` 向某客户端打开的 ws 发送消息和关闭它，原主人收到关闭通知。
///
/// # Errors
///
/// 任一断言不成立时返回错误。
#[test]
fn cross_client_ws_control() -> Result<(), Failure> {
    let (state, mut br, rx_stream, rx_ctl) = wired_clients(10, 11)?;
    let mut ws_stream = HashMap::new();
    let mut ws_ctl = HashMap::new();
    state.handle_client_line(
        10,
        &mut ws_stream,
        "{\"id\":\"open\",\"op\":\"ws_open\",\"ws_id\":\"w_ext_1\",\"path\":\"/logs\"}",
    );
    let v1 = guest_value(&mut br)?;
    let mws = v1
        .get("ws_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    ensure_ne(mws.as_str(), "", "复用 ws 编号非空")?;

    // 客户端 11 向客户端 10 的 ws 发送消息
    state.handle_client_line(
        11,
        &mut ws_ctl,
        "{\"id\":\"s1\",\"op\":\"ws_send\",\"ws_id\":\"w_ext_1\",\"binary\":false,\"data\":\"ping\"}",
    );
    let v2 = guest_value(&mut br)?;
    ensure_eq(
        &v2.get("ws_id"),
        &Some(&Value::from(mws.as_str())),
        "发送到复用 ws",
    )?;
    ensure_eq(&v2.get("data"), &Some(&Value::from("ping")), "消息内容")?;
    let r_send = recv_value(&rx_ctl)?;
    ensure_eq(
        &r_send.get("ok"),
        &Some(&Value::from(true)),
        "发送方收到 ok",
    )?;

    // 客户端 11 关闭客户端 10 的 ws
    state.handle_client_line(
        11,
        &mut ws_ctl,
        "{\"id\":\"c1\",\"op\":\"ws_close\",\"ws_id\":\"w_ext_1\"}",
    );
    let v3 = guest_value(&mut br)?;
    ensure_eq(
        &v3.get("op"),
        &Some(&Value::from("ws_close")),
        "客机收到 ws_close",
    )?;
    ensure_eq(
        &v3.get("ws_id"),
        &Some(&Value::from(mws.as_str())),
        "关闭的是复用 ws",
    )?;

    let close_stream = recv_value(&rx_stream)?;
    ensure_eq(
        &close_stream.get("op"),
        &Some(&Value::from("ws_close")),
        "原主人收到 ws_close",
    )?;
    ensure_eq(
        &close_stream.get("ws_id"),
        &Some(&Value::from("w_ext_1")),
        "原主人收到原 ws_id",
    )
}

/// 客机未连接时，请求直接得到 503 错误应答。
///
/// # Errors
///
/// 任一断言不成立时返回错误。
#[test]
fn mux_without_guest_returns_503() -> Result<(), Failure> {
    let state = MuxState::new();
    let (tx, rx) = mpsc::channel::<String>();
    state.register_client(1, tx);
    let mut ws = HashMap::new();
    state.handle_client_line(1, &mut ws, "{\"id\":99,\"op\":\"ping\"}");
    let resp = recv_value(&rx)?;
    ensure_eq(&resp.get("id"), &Some(&Value::from(99_i32)), "应答原 id")?;
    ensure_eq(&resp.get("ok"), &Some(&Value::from(false)), "ok 为假")?;
    ensure_eq(
        &resp.get("status"),
        &Some(&Value::from(503_i32)),
        "状态 503",
    )
}
