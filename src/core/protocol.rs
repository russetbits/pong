#!/协议：Task/Res/parse/序列化/TCP 循环/文件桥循环
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use std::io::{BufRead, Write};
use crate::core::config::Bridge;
use crate::core::log::*;
use crate::core::files::*;
#[cfg(target_os = "windows")]
use crate::win::serial::{run_serial, run_serial_open, run_serial_write, run_serial_close};
#[cfg(not(target_os = "windows"))]
use crate::lin::serial_stub::{run_serial, run_serial_open, run_serial_write, run_serial_close};
use crate::core::exec::run_exec;

// build.rs 生成：BUILD_GIT（含 -dirty 标记）/ BUILD_TIME
include!(concat!(env!("OUT_DIR"), "/version_info.rs"));

// 协议数据层 re-export（拆至 proto.rs，外部引用路径不变）
pub use crate::core::proto::{parse_task, Task, Res, json_of_res};

pub static SHUTDOWN: AtomicBool = AtomicBool::new(false);

pub fn write_out(b: &Bridge, r: &Res) {
    let name = format!("{}.json", r.id);
    let tmp = b.out_dir().join(format!("{name}.tmp"));
    let dst = b.out_dir().join(&name);
    let body = json_of_res(r);
    if let Ok(mut f) = fs::File::create(&tmp) {
        let _ = f.write_all(body.as_bytes());
    }
    let _ = fs::remove_file(&dst);
    let _ = fs::rename(&tmp, &dst);
}

pub fn tcp_respond(b: &Bridge, line: &str) -> String {
    let task = match parse_task(line) {
        Some(t) => t,
        None => {
            log("tcp bad task");
            let r = Res { id: "?".into(), ok: false, exit_code: None, output: "invalid json task".into(), ms: 0 };
            return json_of_res(&r);
        }
    };
    let r = match task.kind.as_str() {
        "exec" | "adb" => run_exec(&task),
        "ping" => Res { id: task.id.clone(), ok: true, exit_code: Some(0), output: "pong".into(), ms: 0 },
        // 外审：shutdown 只走本地控制面（127.0.0.1:port+1）——桥端口（远程可达）收到即拒
        "shutdown" => {
            Res { id: task.id.clone(), ok: false, exit_code: Some(-1), output: "shutdown only via local control port".into(), ms: 0 }
        }
        "version" => Res { id: task.id.clone(), ok: true, exit_code: Some(0),
            output: format!("pong v{} git:{} built:{} target:{} profile:{}",
                env!("CARGO_PKG_VERSION"), BUILD_GIT, BUILD_TIME,
                if cfg!(target_os = "windows") { "windows" } else { "linux" }, if cfg!(debug_assertions) { "debug" } else { "release" }),
            ms: 0 },
        "serial" => run_serial(&task),
        "serial_open" => run_serial_open(&task),
        "serial_write" => run_serial_write(&task),
        "serial_close" => run_serial_close(&task),
        "read" => run_read(&task),
        "write" => run_write(&task),
        "config" => run_config(&task),
        "fread" => crate::core::files::run_fread(&task),
        "fwrite" => crate::core::files::run_fwrite(&task),
        "stat" => run_stat(&task),
        "ls" => run_ls(&task),
        "list" => run_list(&task),
        "chdir" => run_chdir(&task),
        other => Res { id: task.id.clone(), ok: false, exit_code: None,
            output: format!("unknown kind {other}"), ms: 0 },
    };
    log(&format!("task {} kind={} ok={} code={:?} ms={}", task.id, task.kind, r.ok, r.exit_code, r.ms));
    // TCP 模式结果也落 out/<id>.json（诊断留痕；agent 拿到回包后由 agent 端清理语义不适用文件桥）
    let _ = fs::write(b.out_dir().join(format!("{}.json", task.id)), json_of_res(&r));
    json_of_res(&r)
}

// R6 CLI 运行时配置：config get（回当前 config.ini 全文） / config set key=value...（upsert 落盘+apply runtime）
fn run_config(t: &Task) -> Res {
    let start = std::time::Instant::now();
    let op = t.args.first().map(String::as_str).unwrap_or("get");
    let exe = crate::core::config::exe_dir();
    let path = exe.join("config.ini");
    match op {
        "get" => {
            // 读运行时优先（UI Apply / CLI config set 热生效即见；用户报 Apply 后 get 看不到的根因），文件兜底（未初始化时）
            let text = match crate::core::config::get_runtime() {
                Some(c) => crate::core::config::render_runtime(&c),
                None => fs::read_to_string(&path).unwrap_or_default(),
            };
            Res { id: t.id.clone(), ok: true, exit_code: Some(0), output: text, ms: start.elapsed().as_millis() }
        }
        "set" => {
            if t.args.len() < 2 {
                return Res { id: t.id.clone(), ok: false, exit_code: None, output: "config set: need key=value args".into(), ms: 0 };
            }
            // 校验（数字/forwards 格式）
            let mut out: Vec<String> = Vec::new();
            let old = fs::read_to_string(&path).unwrap_or_default();
            let sets: Vec<(String, String)> = t.args[1..].iter().filter_map(|kv| {
                let (k, v) = kv.split_once('=')?;
                Some((k.trim().to_string(), v.trim().to_string()))
            }).collect();
            // 外审 P0-2 缓解：forwards 是高危配置（=内网穿透面），远程 config set 一律禁改；要改走 GUI Save
            if sets.iter().any(|(k, _)| k == "forwards") {
                return Res { id: t.id.clone(), ok: false, exit_code: None,
                    output: "config set: forwards is protected (edit via local GUI Save)".into(), ms: start.elapsed().as_millis() };
            }
            for line in old.lines() {
                let tr = line.trim();
                if !tr.starts_with('#') && !tr.is_empty() {
                    if let Some(eq) = line.find('=') {
                        let k = line[..eq].trim();
                        if let Some((_, v)) = sets.iter().find(|(sk, _)| sk == k) {
                            out.push(format!("{}={}", k, v));
                            continue;
                        }
                    }
                }
                out.push(line.to_string());
            }
            for (k, v) in &sets {
                if !old.lines().any(|l| l.trim_start().starts_with(&format!("{}=", k))) {
                    out.push(format!("{}={}", k, v));
                }
            }
            let final_text = out.join("
") + "
";
            let (errs, _) = crate::core::config::validate_text(&final_text);
            if let Some(e) = errs.first() {
                return Res { id: t.id.clone(), ok: false, exit_code: None, output: format!("config set rejected: {}", e), ms: start.elapsed().as_millis() };
            }
            // 原子写：tmp+rename（同 save_runtime）
            let tmpp = path.with_extension("ini.tmp");
            match fs::write(&tmpp, &final_text).and_then(|_| fs::rename(&tmpp, &path)) {
                Ok(()) => {
                    crate::core::config::apply_runtime(crate::core::config::IniConfig::parse(&final_text));
                    let keys: Vec<&str> = sets.iter().map(|(k, _)| k.as_str()).collect();
                    log(&format!("config set: [{}] applied+saved", keys.join(",")));
                    Res { id: t.id.clone(), ok: true, exit_code: Some(0), output: format!("applied+saved {} key(s)", sets.len()), ms: start.elapsed().as_millis() }
                }
                Err(e) => Res { id: t.id.clone(), ok: false, exit_code: None, output: format!("config write fail: {e}"), ms: start.elapsed().as_millis() },
            }
        }
        other => Res { id: t.id.clone(), ok: false, exit_code: None, output: format!("config: unknown op {}", other), ms: 0 },
    }
}

/// 应用层认证连续失败计数（每连接线程本地语义足够——static 全局也行：多连接共享爆破计数更严）
static AUTH_FAILS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
// v2 nonce 去重表（nonce→expiry unix 秒；内存态不持久化，设计文档声明重启窗口风险）
static NONCE_SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, i64>>> = std::sync::OnceLock::new();
/// pong pair-code 连续错码计数（3 次断连；成功不计——绑定后不再检查）
static PAIR_FAILS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// pair 比对（复用 auth 常量时间比较；空码视为不匹配）
fn const_eq_pair(a: &str, b: &str) -> bool { !a.is_empty() && crate::core::auth::const_eq(a, b) }
fn nonce_seen() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, i64>> {
    NONCE_SEEN.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new())).lock().unwrap_or_else(|p| p.into_inner())
}

pub fn tcp_loop(b: Arc<Bridge>, port: u16) {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => { log(&format!("tcp bind {port} fail {e}")); return; }
    };
    // 僵死修复①：accept 用非阻塞轮询——Exit 置 SHUTDOWN 后本循环可退出（阻塞 accept 会挂死 join）
    let _ = listener.set_nonblocking(true);
    // 外审 P0：并发上限（每连接一线程无上限=OOM DoS；1000 连接×20MB 即数 GB）
    static LIVE_CONNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    const MAX_CONNS: usize = 16;
    log(&format!("tcp bridge listening on 127.0.0.1:{port}"));
    'outer: loop {
        let mut stream = loop {
            if SHUTDOWN.load(Ordering::Relaxed) { break 'outer; }
            match listener.accept() {
                Ok((s, _)) => break s,
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(300));
                    continue;
                }
                Err(_) => { std::thread::sleep(Duration::from_millis(300)); continue; }
            }
        };
        let _ = stream.set_nonblocking(false);
        // 僵死修复②：读超时 2s 轮询——agent 静默连接时也能感知 SHUTDOWN
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
        if LIVE_CONNS.fetch_add(1, Ordering::SeqCst) >= MAX_CONNS {
            LIVE_CONNS.fetch_sub(1, Ordering::SeqCst);
            log(&format!("tcp connect {peer} rejected: at max {MAX_CONNS} conns"));
            let _ = std::io::Write::write_all(&mut stream, b"{\"id\":\"?\",\"ok\":false,\"exit_code\":null,\"output\":\"server busy (max concurrent connections reached)\",\"ms\":0}\n");
            continue;
        }
        log(&format!("tcp connect {peer}"));
        // 每连接一线程：单连接挂着（半开/客户端假死）不再饿死 accept
        // （曾实锤：一个超时未关的连接垄断单线程桥，后续全部连接 15s 超时）
        let bb = b.clone();
        let peer2 = peer.clone();
        std::thread::spawn(move || {
            let mut stream = stream;
            let reader = match stream.try_clone() { Ok(s) => BufReader::new(s), Err(_) => { LIVE_CONNS.fetch_sub(1, Ordering::SeqCst); return } };
            let mut reader = reader;
            let mut pair_ok = false; // 会话握手状态：首条 pair 匹配后置 true

            // 请求行长上限 1MB（外审 2 轮 P1-5：无限长行=内存 DoS 面；正常最大块=fwrite b64 3MB 分块每块 <1MB）
            const MAX_LINE: usize = 1_100_000;
            let mut line = String::new();
            loop {
                if SHUTDOWN.load(Ordering::Relaxed) { break; }
                line.clear();
                // 受限读取（外审 2 轮 P1-5）：逐段 fill_buf 攒行，累计超 1MB 断连（无限长行=内存 DoS 面）
                const MAX_LINE: usize = 1_100_000;
                let mut got_line = false;
                let mut eof = false;
                loop {
                    let available = match reader.fill_buf() {
                        Ok(b) => b,
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut => continue,
                        Err(_) => { eof = true; break; }
                    };
                    if available.is_empty() { eof = true; break; }
                    if let Some(nl) = available.iter().position(|&b| b == b'\n') {
                        line.push_str(std::str::from_utf8(&available[..nl]).unwrap_or(""));
                        reader.consume(nl + 1);
                        got_line = true;
                        break;
                    }
                    line.push_str(std::str::from_utf8(available).unwrap_or(""));
                    let n = available.len();
                    reader.consume(n);
                    if line.len() > MAX_LINE {
                        log(&format!("tcp line exceeds {} bytes - disconnect", MAX_LINE));
                        let _ = stream.write_all(b"{\"id\":\"?\",\"ok\":false,\"exit_code\":null,\"output\":\"line too long\",\"ms\":0}\n");
                        eof = true;
                        break;
                    }
                }
                if eof { break; }
                let req = line.trim();
                if req.is_empty() { continue; }
                // pong 动态配对码（用户定案 2026-11-17）：PAIR 启用时**首条消息**须带正确 pair 绑定连接；
                // 绑定后本连接免 pair（会话握手语义——ssh 通道已加密+host 回环限定，无需逐请求）。
                if !pair_ok && crate::core::auth::pair().is_some() {
                    let pv = serde_json::from_str::<serde_json::Value>(req).ok();
                    let pair_id = pv.as_ref().and_then(|x| x.get("id").cloned()).unwrap_or_default();
                    let pv2 = pv.as_ref().and_then(|x| x.get("pair").and_then(|p| p.as_str().map(String::from)));
                    match pv2 {
                        Some(p) if const_eq_pair(&p, crate::core::auth::pair().as_deref().unwrap_or("")) => {
                            pair_ok = true;
                            log(&format!("pair ok from {peer2}"));
                            // 握手回执：agent 需确认 pair 过门（否则双方互等=死锁，2026-11-17 实测实锤）
                            let ack = serde_json::json!({ "id": pair_id, "ok": true, "pair_ack": true });
                            let _ = stream.write_all(format!("{}\n", ack).as_bytes());
                            // 首条消息本身就是握手，不执行命令——继续读下一条
                            continue;
                        }
                        _ => {
                            PAIR_FAILS.fetch_add(1, Ordering::SeqCst);
                            log(&format!("pair reject from {peer2} ({})", PAIR_FAILS.load(Ordering::SeqCst)));
                            let resp = format!("{{\"id\":\"?\",\"ok\":false,\"exit_code\":null,\"output\":\"pair code rejected\",\"ms\":0}}\n");
                            let _ = stream.write_all(resp.as_bytes());
                            // 错码限速：3 次即断连（pair 比 token 短，容错更小）
                            if PAIR_FAILS.load(Ordering::SeqCst) >= 3 { break; }
                            continue;
                        }
                    }
                }
                // 应用层认证门（外审 P0-1）：启用 token 后逐请求比对；连续 5 次失败断连
                // v2 分流（docs/design-hmac-v2.md）：请求带 sig 字段=HMAC 验证（token 不再明文上线）
                let v = serde_json::from_str::<serde_json::Value>(req).ok();
                let provided = v.as_ref().and_then(|v| v.get("token").and_then(|t| t.as_str().map(String::from)));
                let v2 = v.as_ref().and_then(|v| {
                    if let (Some(sig), Some(nonce), Some(bh), Some(ts)) = (
                        v.get("sig").and_then(|x| x.as_str()),
                        v.get("nonce").and_then(|x| x.as_str()),
                        v.get("body_hash").and_then(|x| x.as_str()),
                        v.get("ts").and_then(|x| x.as_i64()),
                    ) {
                        Some(crate::core::auth::V2Sig { ts, nonce: nonce.into(), body_hash: bh.into(), sig: sig.into() })
                    } else { None }
                });
                let auth_ok = match (crate::core::auth::token(), v2) {
                    // 未启用认证：放行（兼容旧客户端）
                    (None, _) => true,
                    // v2 请求：HMAC 验证（NONCE_SEEN 全局去重）
                    (Some(tok), Some(sig)) => {
                        let mut seen = nonce_seen();
                        crate::core::auth::nonce_gc(&mut seen);
                        let kind = v.as_ref().and_then(|v| v.get("kind").and_then(|k| k.as_str())).unwrap_or("");
                        match crate::core::auth::verify_v2(&tok, kind, req, &sig, &mut seen) {
                            Ok(()) => true,
                            Err(e) => { log(&format!("v2 auth reject: {e}")); false }
                        }
                    }
                    // v1 请求（带 token 无 sig）：原常量时间比对
                    (Some(_), None) => crate::core::auth::check(provided.as_deref()),
                };
                if !auth_ok {
                    AUTH_FAILS.fetch_add(1, Ordering::SeqCst);
                    let resp = format!("{{\"id\":\"?\",\"ok\":false,\"exit_code\":null,\"output\":\"auth failed ({} consecutive)\",\"ms\":0}}\n", AUTH_FAILS.load(Ordering::SeqCst));
                    let _ = stream.write_all(resp.as_bytes());
                    if AUTH_FAILS.load(Ordering::SeqCst) >= 5 {
                        log("auth: 5 consecutive failures - disconnect");
                        break;
                    }
                    continue;
                }
                AUTH_FAILS.store(0, Ordering::SeqCst);
                let resp = tcp_respond(&bb, req);
                if stream.write_all(resp.as_bytes()).is_err()
                    || stream.write_all(b"
").is_err() { break; }
            }
            LIVE_CONNS.fetch_sub(1, Ordering::SeqCst);
            log(&format!("tcp disconnect {peer2}"));
        });
    }
    log("tcp bridge stop");
}

/// 本地控制面（外审：CLI kill/status 不经桥端口——127.0.0.1:port+1，仅 ping/status/shutdown）
/// 绑定回环本机；forwards validate 禁止映射本端口（run_config 校验）——远程无法触达
pub fn ctl_loop(b: Arc<Bridge>, port: u16) {
    let listen = std::net::TcpListener::bind(("127.0.0.1", port.saturating_add(1)));
    let listener = match listen {
        Ok(l) => l,
        Err(e) => { log(&format!("ctl bind {} fail {e}", port + 1)); return; }
    };
    let _ = listener.set_nonblocking(true);
    log(&format!("ctl listening on 127.0.0.1:{}", port + 1));
    loop {
        if SHUTDOWN.load(Ordering::Relaxed) { log("ctl stop"); return; }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut line = String::new();
                use std::io::BufRead;
                let mut rd = std::io::BufReader::new(&stream);
                if rd.read_line(&mut line).is_err() { continue; }
                let resp = match parse_task(line.trim()) {
                    Some(t) => match t.kind.as_str() {
                        "ping" => Res { id: t.id.clone(), ok: true, exit_code: Some(0), output: "pong".into(), ms: 0 },
                        "status" => Res { id: t.id.clone(), ok: true, exit_code: Some(0), output: format!("alive, cwd={}", std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default()), ms: 0 },
                        "shutdown" => {
                            SHUTDOWN.store(true, std::sync::atomic::Ordering::Relaxed);
                            Res { id: t.id.clone(), ok: true, exit_code: Some(0), output: "shutting down".into(), ms: 0 }
                        }
                        _ => Res { id: t.id.clone(), ok: false, exit_code: Some(-1), output: "ctl supports ping/status/shutdown only".into(), ms: 0 },
                    },
                    None => Res { id: "?".into(), ok: false, exit_code: Some(-1), output: "bad request".into(), ms: 0 },
                };
                let out = json_of_res(&resp);
                let _ = std::io::Write::write_all(&mut stream, format!("{out}\n").as_bytes());
                // shutdown 应答后稍候再关，给 CLI 读到回包的时间窗
                if resp.output == "shutting down" {
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(300));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(300)),
        }
    }
}

pub fn work_loop(b: Arc<Bridge>) {
    log("bridge loop start");
    while !SHUTDOWN.load(Ordering::Relaxed) {
        let mut jobs: Vec<PathBuf> = Vec::new();
        if let Ok(rd) = fs::read_dir(b.in_dir()) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().map(|x| x == "json").unwrap_or(false) { jobs.push(p); }
            }
        }
        jobs.sort();
        for job in jobs {
            if SHUTDOWN.load(Ordering::Relaxed) { break; }
            let name = job.file_name().unwrap_or_default().to_string_lossy().to_string();
            let text = match fs::read_to_string(&job) {
                Ok(t) => t,
                Err(_) => continue,
            };
            let task = match parse_task(&text) {
                Some(t) => t,
                None => {
                    log(&format!("bad task {name}"));
                    let id = name.trim_end_matches(".json").to_string();
                    let r = Res { id, ok: false, exit_code: None, output: "invalid json task".into(), ms: 0 };
                    write_out(&b, &r);
                    let _ = fs::rename(&job, b.done_dir().join(&name));
                    continue;
                }
            };
            let r = match task.kind.as_str() {
                "exec" | "adb" => run_exec(&task),
                "ping" => Res { id: task.id.clone(), ok: true, exit_code: Some(0), output: "pong".into(), ms: 0 },
                "version" => Res { id: task.id.clone(), ok: true, exit_code: Some(0),
                    output: format!("pong v{} git:{} built:{} target:{} profile:{}",
                        env!("CARGO_PKG_VERSION"), BUILD_GIT, BUILD_TIME,
                        if cfg!(target_os = "windows") { "windows" } else { "linux" }, if cfg!(debug_assertions) { "debug" } else { "release" }),
                    ms: 0 },
                "serial" => run_serial(&task),
                "serial_open" => run_serial_open(&task),
                "serial_write" => run_serial_write(&task),
                "serial_close" => run_serial_close(&task),
                "read" => run_read(&task),
                "chdir" => run_chdir(&task),
        "chdir" => run_chdir(&task),
                other => Res { id: task.id.clone(), ok: false, exit_code: None,
                    output: format!("unknown kind {other}"), ms: 0 },
            };
            log(&format!("task {} kind={} ok={} code={:?} ms={}", task.id, task.kind, r.ok, r.exit_code, r.ms));
            write_out(&b, &r);
            let _ = fs::rename(&job, b.done_dir().join(&name));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    log("bridge loop stop");
}
