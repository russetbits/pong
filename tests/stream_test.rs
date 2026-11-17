// R1 流式语义单测（跨平台：Linux 本机可跑）
use pong::core::config::Bridge;
use pong::core::files::{b64decode, b64encode};
use pong::core::protocol::{parse_task, tcp_respond};
use std::sync::Arc;

fn bridge(tmp: &std::path::Path) -> Arc<Bridge> {
    Arc::new(Bridge { dir: tmp.to_path_buf() })
}

fn ask(b: &Arc<Bridge>, line: &str) -> serde_json::Value {
    let out = tcp_respond(b, line);
    serde_json::from_str(&out).unwrap_or(serde_json::json!({"ok": false, "output": out}))
}

#[test]
fn fread_fwrite_chunked_roundtrip() {
    let tmp = std::env::temp_dir().join("ad-stream-test");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let b = bridge(&tmp);
    let blob: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();

    // fwrite 三块推（offset 0/100k/200k）
    let chunk = 100_000usize;
    for i in 0..3u64 {
        let off = i * chunk as u64;
        let end = ((off as usize) + chunk).min(blob.len());
        let part = &blob[off as usize..end];
        let line = format!(
            "{{\"id\":\"w{}\",\"kind\":\"fwrite\",\"args\":[\"big.bin\"],\"offset\":{},\"data\":\"{}\"}}",
            i, off, b64encode(part)
        );
        let r = ask(&b, &line);
        assert!(r["ok"].as_bool().unwrap_or(false), "fwrite {} fail: {}", i, r["output"]);
    }

    // fread 分块拉（len=200k 模拟多块）
    let mut assembled: Vec<u8> = Vec::new();
    let mut offset = 0u64;
    loop {
        let line = format!(
            "{{\"id\":\"r{}\",\"kind\":\"fread\",\"args\":[\"big.bin\"],\"offset\":{},\"len\":200000}}",
            offset, offset
        );
        let r = ask(&b, &line);
        assert!(r["ok"].as_bool().unwrap_or(false), "fread fail: {}", r["output"]);
        let out = r["output"].as_str().unwrap_or("");
        let mut it = out.splitn(2, '\n');
        let total: u64 = it.next().unwrap().parse().unwrap();
        let data = b64decode(it.next().unwrap_or("")).unwrap_or_default();
        assembled.extend(&data);
        offset += data.len() as u64;
        if offset >= total || data.is_empty() { break; }
    }
    assert_eq!(assembled, blob, "roundtrip mismatch");

    // offset>=total：ok + 空块
    let line = format!("{{\"id\":\"e\",\"kind\":\"fread\",\"args\":[\"big.bin\"],\"offset\":99999999,\"len\":1000}}");
    let r = ask(&b, &line);
    assert!(r["ok"].as_bool().unwrap_or(false), "eof fread should be ok");
    let second = r["output"].as_str().unwrap_or("").splitn(2, '\n').nth(1).unwrap_or("");
    assert!(b64decode(second).unwrap_or_default().is_empty(), "expect empty chunk at end");

    let _ = std::fs::remove_dir_all(&tmp);
}
