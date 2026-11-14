//! `clash-call` / `clash-stream` 的 stdout 文本：短应答原样一行，长应答按 48 KiB 分块。

use xr_test_support::{Failure, ensure};

use super::render_rpc_line;

/// 短应答：去掉行尾 `\r\n` 后加一个换行，与原来逐行打印的字节相同。
///
/// # Errors
///
/// 断言不成立时返回错误。
#[test]
fn short_reply_is_one_line() -> Result<(), Failure> {
    ensure(
        render_rpc_line("{\"ok\":true}\r\n") == "{\"ok\":true}\n",
        "短应答一行",
    )
}

/// 长应答：先输出块数，再逐块输出 `@` 前缀的块，拼回后与原文相同，且不在多字节字符中间切开。
///
/// # Errors
///
/// 断言不成立时返回错误。
#[test]
fn long_reply_is_chunked() -> Result<(), Failure> {
    let body = "中".repeat(20_000);
    let text = render_rpc_line(&format!("{body}\n"));
    let mut lines = text.lines();
    ensure(lines.next() == Some("{\"_chunks\":2}"), "块数行")?;
    let mut joined = String::new();
    for part in lines {
        let chunk = part
            .strip_prefix('@')
            .ok_or_else(|| Failure::new("每块以 @ 开头"))?;
        ensure(chunk.len() <= 49_152, "每块不超过 48 KiB")?;
        joined.push_str(chunk);
    }
    ensure(joined == body, "拼回后与原文相同")?;
    ensure(text.ends_with('\n'), "末行有换行")
}
