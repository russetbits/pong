// R6：CLI 语义层（跨平台）——daemon 探活/停机请求（纯 TCP，UI/daemon 共用）
use crate::core::config::{exe_dir, IniConfig};
use std::io::{BufRead, BufReader, Write};

/// 连桥端口发一行 JSON kind，回 Some(应答是否 ok)；None=连不上/超时（未在跑）
pub fn daemon_request(kind: &str) -> Option<bool> {
    let port = IniConfig::load(exe_dir().as_path()).port;
    // 外审：CLI 控制面走本地 ctl 端口（port+1，不经远程可达的桥端口）
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port.saturating_add(1))).ok()?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(2))).ok()?;
    let req = format!("{{\"id\":\"cli\",\"kind\":\"{}\"}}\n", kind);
    s.write_all(req.as_bytes()).ok()?;
    let mut line = String::new();
    BufReader::new(&mut s).read_line(&mut line).ok()?;
    Some(line.contains("\"ok\":true"))
}

/// daemon 探活：ping 应答即活
pub fn daemon_alive() -> bool {
    daemon_request("ping").is_some()
}
