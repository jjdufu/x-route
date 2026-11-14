//! edition 2024 语义点的行为测试：锁守卫与 `accept` 结果的析构时机不随 edition 改变。

use super::*;
use std::os::unix::net::UnixStream as TestStream;

/// 失败信息。
type Failure = &'static str;

/// 条件不成立时返回 `Err(msg)`。
///
/// # Errors
///
/// `cond` 为假时返回 `msg`。
fn ensure(cond: bool, msg: Failure) -> Result<(), Failure> {
    if cond { Ok(()) } else { Err(msg) }
}

/// 从通道收一行并解析为 JSON。
///
/// # Errors
///
/// 超时或解析失败时返回错误。
fn recv_json(rx: &mpsc::Receiver<String>) -> Result<Value, Failure> {
    let line = rx
        .recv_timeout(Duration::from_secs(1))
        .map_err(|_recv_err| "等待应答超时")?;
    serde_json::from_str(&line).map_err(|_json_err| "应答不是 JSON")
}

/// 读客机一侧收到的一行 JSON。
///
/// # Errors
///
/// 读失败或解析失败时返回错误。
fn read_guest(br: &mut BufReader<TestStream>) -> Result<Value, Failure> {
    let mut line = String::new();
    let read = br.read_line(&mut line).map_err(|_io_err| "读客机侧失败")?;
    ensure(read > 0, "客机侧已关闭")?;
    serde_json::from_str(&line).map_err(|_json_err| "客机侧不是 JSON")
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
    {
        let mut clients = state.clients.lock().map_err(|_poison| "clients 中毒")?;
        let handle = clients.get_mut(&7).ok_or("未注册")?;
        let inserted = handle.ws_ids.insert("w1".to_string());
        ensure(inserted, "ws id 插入失败")?;
    }
    {
        let mut active = state.active_ws.lock().map_err(|_poison| "active 中毒")?;
        let prev = active.insert("w1".to_string(), (7, "s1".to_string()));
        ensure(prev.is_none(), "active 已有值")?;
    }
    {
        let mut pending = state
            .pending_reqs
            .lock()
            .map_err(|_poison| "pending 中毒")?;
        let prev = pending.insert("r1".to_string(), (7, Value::from(1)));
        ensure(prev.is_none(), "pending 已有值")?;
    }
    state.unregister_client(7);
    let clients = state.clients.try_lock().map_err(|_lock| "clients 仍被锁")?;
    ensure(!clients.contains_key(&7), "clients 仍含 7")?;
    let active = state
        .active_ws
        .try_lock()
        .map_err(|_lock| "active 仍被锁")?;
    ensure(!active.contains_key("w1"), "active 仍含 w1")?;
    let pending = state
        .pending_reqs
        .try_lock()
        .map_err(|_lock| "pending 仍被锁")?;
    ensure(
        pending.values().all(|(cid, _)| *cid != 7),
        "pending 仍有 cid=7",
    )
}

/// 外部客户端按 key 关闭 ws（clash.rs:327）后返回原主人信息，`ws_by_key` 不残留锁；key 不存在时按原样应答。
///
/// # Errors
///
/// 任一断言不成立时返回错误。
#[test]
fn ws_close_by_external_key_returns_owner() -> Result<(), Failure> {
    let (guest_end, mux_end) = TestStream::pair().map_err(|_io_err| "socketpair 失败")?;
    let state = MuxState::new();
    {
        let mut guard = state.hvc_writer.lock().map_err(|_poison| "hvc 中毒")?;
        *guard = Some(mux_end);
    }
    let (tx_a, rx_a) = mpsc::channel::<String>();
    let (tx_b, rx_b) = mpsc::channel::<String>();
    state.register_client(1, tx_a);
    state.register_client(2, tx_b);
    let mut map_a = HashMap::new();
    let mut map_b = HashMap::new();
    state.handle_client_line(
        1,
        &mut map_a,
        "{\"id\":\"o\",\"op\":\"ws_open\",\"ws_id\":\"k1\"}",
    );
    let mut br = BufReader::new(guest_end);
    let opened = read_guest(&mut br)?;
    let mux_ws = opened
        .get("ws_id")
        .and_then(Value::as_str)
        .ok_or("无 mux ws")?
        .to_string();

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
    ensure(reply.get("id") == Some(&Value::from(9)), "应答 id 不对")?;
    ensure(reply.get("ok") == Some(&Value::from(true)), "应答 ok 不对")?;
    let owner_note = recv_json(&rx_a)?;
    ensure(
        owner_note.get("op") == Some(&Value::from("ws_close")),
        "原主人未收到关闭",
    )?;
    {
        let by_key = state
            .ws_by_key
            .try_lock()
            .map_err(|_lock| "ws_by_key 仍被锁")?;
        ensure(!by_key.contains_key("k1"), "ws_by_key 仍含 k1")?;
    }

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
fn temp_env() -> Result<(tempfile::TempDir, Env), Failure> {
    let dir = tempfile::TempDir::new().map_err(|_io_err| "临时目录失败")?;
    let mut env = Env::from_process();
    env.xr_data = dir.path().to_path_buf();
    fs::create_dir_all(env.run_dir()).map_err(|_io_err| "建 run 目录失败")?;
    fs::create_dir_all(env.xr_data.join("config")).map_err(|_io_err| "建 config 目录失败")?;
    fs::write(env.fuse_file(), "1\n").map_err(|_io_err| "写 fuse 失败")?;
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
    fs::remove_file(env.fuse_file()).map_err(|_io_err| "删 fuse 失败")?;
    match handle.join() {
        Ok(_code) => Ok(()),
        Err(_panic) => Err("mux 线程 panic"),
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
    let ctl = TestStream::connect(&ctl_path).map_err(|_io_err| "连 ctl 失败")?;
    let mut rpc = TestStream::connect(&rpc_path).map_err(|_io_err| "连 rpc 失败")?;
    let timeout = Some(Duration::from_millis(200));
    rpc.set_read_timeout(timeout)
        .map_err(|_io_err| "设超时失败")?;
    ctl.set_read_timeout(timeout)
        .map_err(|_io_err| "设超时失败")?;
    let mut rpc_reader = BufReader::new(rpc.try_clone().map_err(|_io_err| "clone 失败")?);
    let mut ctl_reader = BufReader::new(ctl.try_clone().map_err(|_io_err| "clone 失败")?);
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
    let ctl2 = TestStream::connect(&ctl_path).map_err(|_io_err| "重连 ctl 失败")?;
    ctl2.set_read_timeout(timeout)
        .map_err(|_io_err| "设超时失败")?;
    let mut ctl2_reader = BufReader::new(ctl2);
    let forwarded_again = poll_until(40, || {
        rpc.write_all(b"{\"id\":3,\"op\":\"ping\"}\n").is_ok()
            && read_guest(&mut ctl2_reader).is_ok()
    });
    ensure(forwarded_again, "重连后请求未转发")?;
    drop(ctl2_reader);
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
    let mut first = TestStream::connect(&rpc_path).map_err(|_io_err| "连 rpc 1 失败")?;
    let mut second = TestStream::connect(&rpc_path).map_err(|_io_err| "连 rpc 2 失败")?;
    first
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|_io_err| "设超时失败")?;
    second
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|_io_err| "设超时失败")?;
    let mut first_reader = BufReader::new(first.try_clone().map_err(|_io_err| "clone 失败")?);
    let mut second_reader = BufReader::new(second.try_clone().map_err(|_io_err| "clone 失败")?);
    // 无客机：两个客户端各自收到自己的 503，证明 cid 不同且各自送达
    first
        .write_all(b"{\"id\":\"a\",\"op\":\"ping\"}\n")
        .map_err(|_io_err| "写 1 失败")?;
    second
        .write_all(b"{\"id\":\"b\",\"op\":\"ping\"}\n")
        .map_err(|_io_err| "写 2 失败")?;
    let mut line_a = String::new();
    let mut line_b = String::new();
    let _read_a: usize = first_reader
        .read_line(&mut line_a)
        .map_err(|_io_err| "读 1 失败")?;
    let _read_b: usize = second_reader
        .read_line(&mut line_b)
        .map_err(|_io_err| "读 2 失败")?;
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
