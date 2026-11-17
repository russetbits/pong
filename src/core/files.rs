#!/文件操作（跨平台）：b64/read/write/stat/ls/list/chdir
use std::fs;
use std::time::Instant;
use crate::core::protocol::{Task, Res};
use crate::core::log::*;

/// CRC-32 (ISO-HDLC, 反射式, poly 0xEDB88320)——fread/fwrite 块级完整性（外审 P1）
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

pub fn b64encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

// b64 解码（kind=write 用）：容忍空白/缺 padding，非法字符报错
pub fn b64decode(s: &str) -> Result<Vec<u8>, String> {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut val: u32 = 0;
    let mut bits = 0u32;
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    for c in s.bytes() {
        if c == b'=' || c == b'\r' || c == b'\n' || c == b' ' || c == b'\t' { continue; }
        let v: u32 = if c == b'+' { 62 }
            else if c == b'/' { 63 }
            else if c.is_ascii_uppercase() { (c - b'A') as u32 }
            else if c.is_ascii_lowercase() { (c - b'a') as u32 + 26 }
            else if c.is_ascii_digit() { (c - b'0') as u32 + 52 }
            else { return Err(format!("invalid char {:?}", c as char)); };
        val = (val << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((val >> bits) & 0xFF) as u8);
        }
    }
    Ok(out)
}

// kind=read：读 Windows 文件 → base64 返回。args=[路径] 或 cmd=路径。单文件上限 8MB（防内存/协议爆）
// ── R1 流式传输（adb push/pull 模型）：fread 分块拉 / fwrite 分块推 ──
// fread: args[0]=path, offset=块起点(默认0), len=块长(默认1MB) → output="<total>\n<b64(≤len)>"
// agent 循环 offset+=got 直到 got==0 或累计==total；offset≥total 时回 "<total>\n"（空）
pub fn run_fread(t: &Task) -> Res {
    let start = Instant::now();
    let path = t.args.first().cloned().unwrap_or_default();
    if path.is_empty() {
        return Res { id: t.id.clone(), ok: false, exit_code: None, output: "fread: no path (args[0])".into(), ms: 0 };
    }
    let total = match fs::metadata(&path) {
        Ok(m) => m.len(),
        Err(e) => return Res { id: t.id.clone(), ok: false, exit_code: None, output: format!("fread: {} ({e})", path), ms: start.elapsed().as_millis() },
    };
    let offset = t.offset.unwrap_or(0) as u64;
    let want = t.len.unwrap_or(1024 * 1024).min(4 * 1024 * 1024) as u64; // 单块上限 4MB
    if offset >= total {
        log(&format!("fread {} offset={} at end (total={})", path, offset, total));
        return Res { id: t.id.clone(), ok: true, exit_code: Some(0), output: format!("{}\n", total), ms: start.elapsed().as_millis() };
    }
    let want = want.min(total - offset) as usize;
    use std::io::{Seek, SeekFrom, Read};
    let mut f = match fs::File::open(&path) {
        Ok(f) => f,
        Err(e) => return Res { id: t.id.clone(), ok: false, exit_code: None, output: format!("fread: {} ({e})", path), ms: start.elapsed().as_millis() },
    };
    if f.seek(SeekFrom::Start(offset)).is_err() {
        return Res { id: t.id.clone(), ok: false, exit_code: None, output: format!("fread: seek fail {}", path), ms: start.elapsed().as_millis() };
    }
    let mut buf = vec![0u8; want];
    let mut got = 0usize;
    while got < want {
        match f.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) => return Res { id: t.id.clone(), ok: false, exit_code: None, output: format!("fread: {} ({e})", path), ms: start.elapsed().as_millis() },
        }
    }
    buf.truncate(got);
    log(&format!("fread {} offset={} got={} (total={})", path, offset, got, total));
    if t.want_crc == Some(true) {
        Res { id: t.id.clone(), ok: true, exit_code: Some(0),
            output: format!("{}\n{}\ncrc:{}", total, b64encode(&buf), crc32(&buf)), ms: start.elapsed().as_millis() }
    } else {
        Res { id: t.id.clone(), ok: true, exit_code: Some(0),
            output: format!("{}\n{}", total, b64encode(&buf)), ms: start.elapsed().as_millis() }
    }
}

// fwrite: args[0]=path, offset=写入起点(0=截断新建), data=b64 块；offset>0 且文件不存在=报错
// 回 output=写入后文件总大小（agent 可用作下一块 offset 的 append 依据）
pub fn run_fwrite(t: &Task) -> Res {
    let start = Instant::now();
    let path = t.args.first().cloned().unwrap_or_default();
    let b64 = t.data.clone().unwrap_or_default();
    if path.is_empty() || t.data.is_none() {
        return Res { id: t.id.clone(), ok: false, exit_code: None, output: "fwrite: need args[0]=path and data=b64".into(), ms: 0 };
    }
    let offset = t.offset.unwrap_or(0);
    let data = match b64decode(&b64) {
        Ok(d) => d,
        Err(e) => return Res { id: t.id.clone(), ok: false, exit_code: None, output: format!("fwrite: b64 ({e})"), ms: 0 },
    };
    // 块级完整性（外审 P1）：请求带 crc 时校验解码后字节
    if let Some(want) = t.crc {
        let got = crc32(&data);
        if got != want {
            return Res { id: t.id.clone(), ok: false, exit_code: None,
                output: format!("fwrite: crc mismatch want={want} got={got}"), ms: start.elapsed().as_millis() };
        }
    }
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = fs::create_dir_all(parent);
    }
    use std::io::{Seek, SeekFrom, Write};
    let open = if offset == 0 {
        fs::OpenOptions::new().create(true).write(true).truncate(true).open(&path)
    } else {
        fs::OpenOptions::new().create(true).write(true).open(&path)
    };
    let mut f = match open {
        Ok(f) => f,
        Err(e) => return Res { id: t.id.clone(), ok: false, exit_code: None, output: format!("fwrite: {} ({e})", path), ms: start.elapsed().as_millis() },
    };
    if offset > 0 && f.seek(SeekFrom::Start(offset)).is_err() {
        return Res { id: t.id.clone(), ok: false, exit_code: None, output: format!("fwrite: seek fail {}", path), ms: start.elapsed().as_millis() };
    }
    if let Err(e) = f.write_all(&data) {
        return Res { id: t.id.clone(), ok: false, exit_code: None, output: format!("fwrite: {} ({e})", path), ms: start.elapsed().as_millis() };
    }
    let total = offset + data.len() as u64;
    log(&format!("fwrite {} offset={} {} bytes (total={})", path, offset, data.len(), total));
    Res { id: t.id.clone(), ok: true, exit_code: Some(0), output: total.to_string(), ms: start.elapsed().as_millis() }
}

pub fn run_read(t: &Task) -> Res {
    let start = Instant::now();
    let path = t.args.first().cloned().or_else(|| t.cmd.clone()).unwrap_or_default();
    if path.is_empty() {
        return Res { id: t.id.clone(), ok: false, exit_code: None, output: "read: no path (args[0])".into(), ms: 0 };
    }
    match fs::metadata(&path) {
        Ok(m) if m.len() > 8 * 1024 * 1024 => {
            return Res { id: t.id.clone(), ok: false, exit_code: None,
                output: format!("read: too big {} bytes (max 8MB), {}", m.len(), path), ms: start.elapsed().as_millis() };
        }
        Ok(_) => {}
        Err(e) => {
            return Res { id: t.id.clone(), ok: false, exit_code: None,
                output: format!("read: {} ({e})", path), ms: start.elapsed().as_millis() };
        }
    }
    match fs::read(&path) {
        Ok(data) => {
            log(&format!("read {} {} bytes", path, data.len()));
            Res { id: t.id.clone(), ok: true, exit_code: Some(0),
                output: format!("{}\n{}", data.len(), b64encode(&data)), ms: start.elapsed().as_millis() }
        }
        Err(e) => Res { id: t.id.clone(), ok: false, exit_code: None,
            output: format!("read fail: {e}"), ms: start.elapsed().as_millis() },
    }
}

// kind=write：b64 数据写 Windows 文件（args[0]=路径，output=b64 数据；8MB 上限；目录自动创建）
pub fn run_write(t: &Task) -> Res {
    let start = Instant::now();
    let path = t.args.first().cloned().or_else(|| t.cmd.clone()).unwrap_or_default();
    if path.is_empty() || t.data.is_none() {
        return Res { id: t.id.clone(), ok: false, exit_code: None,
            output: "write: need args[0]=path and data=b64data".into(), ms: 0 };
    }
    let b64 = t.data.clone().unwrap_or_default();
    if b64.len() > 11 * 1024 * 1024 {
        return Res { id: t.id.clone(), ok: false, exit_code: None,
            output: format!("write: b64 too big {} chars (max ~8MB raw)", b64.len()), ms: 0 };
    }
    match b64decode(&b64) {
        Ok(data) => {
            if let Some(parent) = std::path::Path::new(&path).parent() {
                let _ = fs::create_dir_all(parent);
            }
            match fs::write(&path, &data) {
                Ok(_) => {
                    log(&format!("write {} {} bytes", path, data.len()));
                    Res { id: t.id.clone(), ok: true, exit_code: Some(0),
                        output: format!("{}", data.len()), ms: start.elapsed().as_millis() }
                }
                Err(e) => Res { id: t.id.clone(), ok: false, exit_code: None,
                    output: format!("write fail: {e}"), ms: start.elapsed().as_millis() },
            }
        }
        Err(e) => Res { id: t.id.clone(), ok: false, exit_code: None,
            output: format!("write: bad b64 ({e})"), ms: 0 },
    }
}

// kind=stat：单路径元数据（size/mtime/is_dir/is_file）
pub fn run_stat(t: &Task) -> Res {
    let start = Instant::now();
    let path = t.args.first().cloned().or_else(|| t.cmd.clone()).unwrap_or_default();
    if path.is_empty() {
        return Res { id: t.id.clone(), ok: false, exit_code: None, output: "stat: no path (args[0])".into(), ms: 0 };
    }
    match fs::metadata(&path) {
        Ok(m) => {
            let mtime = m.modified().ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs()).unwrap_or(0);
            let v = serde_json::json!({
                "path": path, "exists": true, "is_dir": m.is_dir(), "is_file": m.is_file(),
                "size": m.len(), "mtime": mtime
            });
            Res { id: t.id.clone(), ok: true, exit_code: Some(0), output: v.to_string(), ms: start.elapsed().as_millis() }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let v = serde_json::json!({ "path": path, "exists": false });
            Res { id: t.id.clone(), ok: true, exit_code: Some(0), output: v.to_string(), ms: start.elapsed().as_millis() }
        }
        Err(e) => Res { id: t.id.clone(), ok: false, exit_code: None,
            output: format!("stat fail: {e}"), ms: start.elapsed().as_millis() },
    }
}

// kind=ls：目录列表 JSON 数组（name/size/mtime/is_dir；args[0]=目录，缺 CWD；read/sta/ls 相对路径走桥 CWD）
pub fn run_ls(t: &Task) -> Res {
    let start = Instant::now();
    let path = t.args.first().cloned().or_else(|| t.cmd.clone()).unwrap_or_else(|| ".".into());
    let rd = match fs::read_dir(&path) {
        Ok(rd) => rd,
        Err(e) => return Res { id: t.id.clone(), ok: false, exit_code: None,
            output: format!("ls fail: {e} ({path})"), ms: start.elapsed().as_millis() },
    };
    let mut items: Vec<serde_json::Value> = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let (is_dir, size, mtime) = match entry.metadata() {
            Ok(m) => (m.is_dir(), m.len(),
                m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs()).unwrap_or(0)),
            Err(_) => (false, 0, 0),
        };
        items.push(serde_json::json!({ "name": name, "is_dir": is_dir, "size": size, "mtime": mtime }));
    }
    items.sort_by(|a, b| {
        let da = a["is_dir"].as_bool().unwrap_or(false);
        let db = b["is_dir"].as_bool().unwrap_or(false);
        db.cmp(&da).then(a["name"].as_str().cmp(&b["name"].as_str()))
    });
    Res { id: t.id.clone(), ok: true, exit_code: Some(0),
        output: serde_json::Value::Array(items).to_string(), ms: start.elapsed().as_millis() }
}

// kind=list：自省——返回桥支持的全部 kind 及一行说明（agent 发现能力用）
pub fn run_list(t: &Task) -> Res {
    let help: &[(&str, &str)] = &[
        ("list", "返回本帮助（本命令自身）"),
        ("ping", "零执行探针，测链路延迟"),
        ("version", "返回 pong 版本/git/构建时间/target/profile（排障首查）"),
        ("exec", "执行 Windows 命令（cmd=程序 args=参数 timeout_ms）"),
        ("adb", "同 exec，语义为 adb 操作（如 devices/shell/getprop）"),
        ("serial", "一次性串口任务（port/baud/send/read_ms/expect）"),
        ("serial_open", "开持久串口会话（幂等，返回 tee log 路径）"),
        ("serial_write", "向持久会话写命令（立即返回，输出走 log）"),
        ("serial_close", "关持久串口会话"),
        ("read", "读 Windows 文件 b64 返回（output=\"<len>\\n<b64>\"，8MB 上限）"),
        ("write", "b64 写 Windows 文件（output=b64 数据，8MB 上限，目录自动创建）"),
        ("stat", "单路径元数据 JSON（exists/is_dir/is_file/size/mtime）"),
        ("ls", "目录列表 JSON 数组（name/is_dir/size/mtime；目录优先排序）"),
        ("chdir", "切桥进程 CWD（args[0]=路径；后续相对路径全落新 CWD）"),
    ];
    let v: Vec<serde_json::Value> = help.iter()
        .map(|(k, d)| serde_json::json!({ "kind": k, "desc": d })).collect();
    Res { id: t.id.clone(), ok: true, exit_code: Some(0),
        output: serde_json::Value::Array(v).to_string(), ms: 0 }
}

// kind=chdir：切桥进程工作目录（agent 传 args[0]=路径；之后 exec/read 的相对路径都落新 CWD。
// 用途：切到 samba 映射盘后无需 read 即可双向拷文件）
pub fn run_chdir(t: &Task) -> Res {
    let start = Instant::now();
    let path = t.args.first().cloned().or_else(|| t.cmd.clone()).unwrap_or_default();
    if path.is_empty() {
        return Res { id: t.id.clone(), ok: false, exit_code: None, output: "chdir: no path (args[0])".into(), ms: 0 };
    }
    match std::env::set_current_dir(&path) {
        Ok(_) => {
            let cur = std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default();
            log(&format!("chdir -> {cur}"));
            Res { id: t.id.clone(), ok: true, exit_code: Some(0), output: cur, ms: start.elapsed().as_millis() }
        }
        Err(e) => Res { id: t.id.clone(), ok: false, exit_code: None,
            output: format!("chdir fail: {e} ({path})"), ms: start.elapsed().as_millis() },
    }
}
