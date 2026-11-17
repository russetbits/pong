// core/proto.rs —— 行协议数据层（自 protocol.rs 拆出，2026-11-16 src 树整理）：
// Task/Res 结构与 parse_task/json_of_res 纯协议编解码；分发与循环留在 protocol.rs。
// protocol.rs 对外 re-export（pub use），既有引用路径 crate::core::protocol::Task 不变。

/// 一条解析后的桥请求
pub struct Task {
    pub id: String,
    pub kind: String,
    pub cmd: Option<String>,
    pub args: Vec<String>,
    pub timeout_ms: u64,
    pub port: Option<String>,
    pub baud: u32,
    pub send: Option<String>,
    pub read_ms: u64,
    pub expect: Option<String>,
    pub data: Option<String>,  // kind=write：b64 编码的文件内容
    // R1 流式：fread(offset,len) / fwrite(offset,create)；缺省=None（旧客户端不受影响）
    pub offset: Option<u64>,
    pub len: Option<u64>,
    pub create: Option<bool>,
    // 应用层认证（外审 P0-1）：请求方携带的 token；None=未携带
    pub token: Option<String>,
    // fread 请求方要 crc 校验行（外审 P1）：true 时响应追加第三行 crc:<十进制>
    pub want_crc: Option<bool>,
    // fwrite 块级校验：data 解码后字节应有的 CRC32（缺省不校验，兼容旧客户端）
    pub crc: Option<u32>,
}

pub fn parse_task(text: &str) -> Option<Task> {
    // serde_json 路径（tmux 编译通路打通后引入）；未知字段宽容忽略
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let id = v.get("id")?.as_str()?.to_string();
    let kind = v.get("kind")?.as_str()?.to_string();
    let args = |k: &str| v.get(k).and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|y| y.as_str().map(|s| s.to_string())).collect::<Vec<_>>())
        .unwrap_or_default();
    let num = |k: &str, d: u64| v.get(k).and_then(|x| x.as_u64()).unwrap_or(d);
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
    Some(Task {
        id, kind,
        cmd: s("cmd"),
        args: args("args"),
        timeout_ms: num("timeout_ms", 60_000),
        port: s("port"),
        baud: num("baud", 921600) as u32,
        send: s("send"),
        read_ms: num("read_ms", 800),
        expect: s("expect"),
        data: s("data"),
        offset: v.get("offset").and_then(|x| x.as_u64()),
        len: v.get("len").and_then(|x| x.as_u64()),
        create: v.get("create").and_then(|x| x.as_bool()),
        token: s("token"),
        want_crc: v.get("want_crc").and_then(|x| x.as_bool()),
        crc: v.get("crc").and_then(|x| x.as_u64().map(|n| n as u32)),
    })
}

/// 一条桥响应
pub struct Res {
    pub id: String,
    pub ok: bool,
    pub exit_code: Option<i32>,
    pub output: String,
    pub ms: u128,
}

pub fn json_of_res(r: &Res) -> String {
    let v = serde_json::json!({
        "id": r.id, "ok": r.ok, "exit_code": r.exit_code,
        "output": r.output, "ms": r.ms,
    });
    v.to_string()
}
