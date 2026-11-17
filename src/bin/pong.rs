// pong —— CLI 可执行（与 pong-gui 托盘 exe 分开编译）
// 子命令（用户定案 2026-11-17）：connect(默认 user@host)/help/status/map/port/repick/exit/version
// 无参=ssh 式用法报错（对齐 ssh 行为：`usage: ping ...` 风格）。
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "console")]

#[path = "../core/mod.rs"]
mod core;
#[cfg(target_os = "windows")]
#[path = "../win/mod.rs"]
mod win;
#[cfg(not(target_os = "windows"))]
#[path = "../lin/mod.rs"]
mod lin;
// 兼容桩：win/main_impl 引用的 gui 门面——CLI bin 无 gui，桩恒空转
pub mod gui {
    use std::sync::atomic::{AtomicBool, Ordering};
    pub static GUI_ALIVE: AtomicBool = AtomicBool::new(false);
    pub fn request_gui_show() {}
    pub fn run_gui_logged() {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
    pub fn spawn_gui_thread() {}
}

const HELP: &str = "\
usage: pong [-p port] user@host   connect to host over ssh (reverse tunnel back to this machine)\n\
   or: pong help                  show this help\n\
   or: pong status                show connection/port/pair-code/forwards\n\
   or: pong port                  list active forwards (fallback = list)\n\
   or: pong port map <local>              add forward (sshd picks the remote port)\n\
   or: pong port map <remote>:<local>     add forward (fixed remote port)\n\
   or: pong port unmap <remote>           remove a forward\n\
   or: pong repick                regenerate port+pair-code (old pair invalidated)\n\
   or: pong exit                  stop the running pong connection\n\
   or: pong version               print version/build info\n\
\nOptions:\n\
  -p PORT       ssh port of the remote host (default 22)\n\
\nWith no arguments, pong reads ~/.ssh/config like ssh(1) and errors the same way.\n";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        // ssh 行为：无参=usage 到 stderr 退 255（ssh 无 host 时 exit 255）
        eprint!("{}", HELP);
        std::process::exit(255);
    }
    let cmd = args[0].as_str();
    match cmd {
        "help" | "--help" | "-h" => {
            print!("{}", HELP);
        }
        "version" | "--version" | "-V" => {
            print_version();
        }
        "status" => cmd_status(),
        "exit" => cmd_exit(),
        "port" => cmd_port(&args[1..]),   // port map/unmap/list（无参=fallback list）——agent 桥 forward 族
        "map" => cmd_map(&args[1..]),     // map --list / map <port> / （-L 服务转发族：client 用户访问 host 服务）
        "unmap" => cmd_unmap(&args[1..]), // unmap <port>（摘 -L）
        "repick" => cmd_repick(),
        // 内部子命令（daemon 模式——用户定案：connect 打印后返回，连接挂 daemon 类似 adb）
        "--daemon-connect" => cmd_daemon_connect(&args[1..]),
        _ => cmd_connect(&args),
    }
}

/// 运行状态文件（temp/pong/bridge.state）：daemon 写 up/down/err，connect 父进程轮询
fn state_path() -> std::path::PathBuf { core::config::tmp_dir().join("bridge.state") }

fn write_state(s: &str) {
    let _ = std::fs::create_dir_all(core::config::tmp_dir());
    let _ = std::fs::write(state_path(), s);
}

fn print_version() {
    // 与行协议 version kind 同源（编译期 env! 注入）
    println!("pong v{} git:{} built:{} target:{}",
        env!("CARGO_PKG_VERSION"),
        option_env!("PONG_GIT").unwrap_or("dev"),
        option_env!("PONG_BUILD").unwrap_or("dev"),
        std::env::consts::OS);
}

fn cmd_connect(args: &[String]) {
    // -p PORT user@host（ssh 语义：-p 在 host 前）；也接受 host 在前 -p 在后（宽松）
    let mut ssh_port: Option<u16> = None;
    let mut target: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-p" | "-P" => {
                match it.next().and_then(|v| v.parse::<u16>().ok()) {
                    Some(p) => ssh_port = Some(p),
                    None => { eprintln!("pong: invalid port"); std::process::exit(255); }
                }
            }
            s if s.starts_with('-') => {
                eprintln!("pong: unknown option '{s}'");
                eprint!("{}", HELP);
                std::process::exit(255);
            }
            _ => target = Some(a.clone()),
        }
    }
    let Some(target) = target else {
        eprintln!("pong: missing host");
        eprint!("{}", HELP);
        std::process::exit(255);
    };
    // 单实例互斥（用户定案 2026-11-17）：已有 daemon 活着即拒绝任何新 connect
    // （pong exit 先停旧的；陈旧 pidfile 清理后放行）
    if let Ok(s) = std::fs::read_to_string(core::config::pid_path()).map(|x| x.trim().to_string()) {
        if !s.is_empty() {
            #[cfg(target_os = "windows")]
            let alive = std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {s}"), "/NH"])
                .output().map(|o| {
                    let t = String::from_utf8_lossy(&o.stdout);
                    t.to_uppercase().contains("PONG") || (!t.contains("INFO:") && !t.trim().is_empty() && t.contains(s.as_str()))
                }).unwrap_or(false);
            #[cfg(not(target_os = "windows"))]
            let alive = std::process::Command::new("kill").args(["-0", &s]).status().map(|st| st.success()).unwrap_or(false);
            if alive {
                eprintln!("pong: already running (pid {s}) - run 'pong exit' first");
                std::process::exit(255);
            }
            let _ = std::fs::remove_file(core::config::pid_path());
            let _ = std::fs::remove_file(state_path());
        }
    }
    let t = match core::sshconf::lookup(&target) {
        Ok(t) => t,
        Err(e) => { eprintln!("pong: {e}"); std::process::exit(255); }
    };
    let port = ssh_port.unwrap_or(t.port);
    eprintln!("connecting to {} port {} (user: {}) ...", t.host, port,
        t.user.as_deref().unwrap_or("<default>"));

    // 端口分配移交：本机桥 :0 自选 + sshd 侧 tcpip_forward(port 0) 自选（2026-11-17 定案——
    // 预探测端口在 sshd 侧被占/同机被自家桥占实锤 rejected）
    // pair-code（会话握手凭证；注入 auth 内存供桥门校验）
    let pair = match core::auth::gen_pair() {
        Ok(p) => p,
        Err(e) => { eprintln!("pong: random fail: {e}"); std::process::exit(255); }
    };

    // adb 式 daemon（用户定案 2026-11-17）：connect 解析后 spawn 自身 --daemon-connect，
    // 父进程轮询状态文件，up 后打印 port/pair 直接返回；连接由 daemon 常驻保持
    let me = std::env::current_exe().map_err(|e| format!("current_exe: {e}"));
    let me = match me { Ok(p) => p, Err(e) => { eprintln!("pong: {e}"); std::process::exit(255); } };
    let mut child_args = vec![
        "--daemon-connect".to_string(),
        "0".to_string(),
        pair.clone(),
        target.clone(),
    ];
    if let Some(p) = ssh_port { child_args.push("-p".into()); child_args.push(p.to_string()); }
    // Windows：脱离终端控制台（2026-11-24 用户实测：关 shell 后 daemon 被连带杀）——
    // DETACHED_PROCESS(0x08)=不继承父控制台（关窗不殃及）| CREATE_NEW_PROCESS_GROUP(0x200)=独立 Ctrl+C 组
    let mut cmd_build = std::process::Command::new(&me);
    cmd_build.args(&child_args);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd_build.creation_flags(0x08 | 0x200);
    }
    let child = cmd_build
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from({
            let logdir = core::config::tmp_dir().join("log");
            let _ = std::fs::create_dir_all(&logdir);
            match std::fs::OpenOptions::new().create(true).append(true).open(logdir.join("daemon.err")) {
                Ok(f) => std::process::Stdio::from(f),
                Err(_) => std::process::Stdio::null(),
            }
        }))
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => { eprintln!("pong: spawn daemon: {e}"); std::process::exit(255); }
    };
    // 等待 up（轮询状态文件，15s 超时=tunnel 连接超时上限）；daemon 崩溃立即报
    write_state("connecting");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(16);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        match std::fs::read_to_string(state_path()).unwrap_or_default() {
            s if s.contains("\"state\":\"up\"") || s.starts_with("up") => {
                // state JSON：{"state":"up","port":R,...}（兼容旧 "up port=R"）
                let rport = serde_json::from_str::<serde_json::Value>(&s).ok()
                    .and_then(|v| v.get("port").and_then(|x| x.as_u64()))
                    .map(|x| x as u16)
                    .or_else(|| s.split("port=").nth(1).and_then(|v| v.trim().parse::<u16>().ok()))
                    .unwrap_or(0);
                println!("port={}", rport);
                println!("pair={}", pair);
                return; // 打印即返回；daemon 常驻
            }
            s if s.starts_with("err") || s.contains("\"state\":\"err\"") => {
                let _ = child.kill();
                let msg = serde_json::from_str::<serde_json::Value>(&s).ok()
                    .and_then(|v| v.get("error").and_then(|x| x.as_str()).map(String::from))
                    .unwrap_or_else(|| s.trim_start_matches("err:").trim().to_string());
                eprintln!("pong: {msg}");
                std::process::exit(255);
            }
            _ => {}
        }
        if let Ok(Some(_)) = child.try_wait() {
            eprintln!("pong: daemon exited unexpectedly (see {} log)", core::config::tmp_dir().display());
            std::process::exit(255);
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            eprintln!("pong: connect timeout (16s)");
            std::process::exit(255);
        }
    }
}

/// daemon 连接模式（内部子命令）：起桥+tunnel+写状态文件，常驻直到断开
fn cmd_daemon_connect(args: &[String]) {
    // args: <port> <pair> <target> [-p sshport]
    if args.len() < 3 {
        eprintln!("pong: --daemon-connect needs <port> <pair> <target>");
        std::process::exit(255);
    }
    // CLI daemon 的运行时产物落点：serial_log_dir() 的 fallback 已改 temp/pong/serial（2026-11-25 修复）——
    // 此处不再显式 set（crate::win 仅 gui feature 可见）
    let _dyn_port: u16 = args[0].parse().unwrap_or(0); // 兼容旧调用形态（端口分配已移交 sshd 自选）
    let pair = args[1].clone();
    let target = args[2].clone();
    let ssh_port = args.iter().position(|a| a == "-p").and_then(|i| args.get(i + 1)).and_then(|v| v.parse::<u16>().ok());
    let t = match core::sshconf::lookup(&target) {
        Ok(t) => t,
        Err(e) => { write_state(&format!("err: {e}")); std::process::exit(255); }
    };
    let port = ssh_port.unwrap_or(t.port);
    core::auth::set_pair(&pair);

    // 本地桥：bind :0 自选（与 sshd 侧端口解耦——同机/跨机都不冲突）
    let dir = core::config::find_work_dir().unwrap_or_else(|| {
        write_state("err: work dir unavailable");
        std::process::exit(2);
    });
    let _ = std::fs::create_dir_all(dir.join("log"));
    let _ = core::log::LOG_PATH.set(dir.join("log").join("bridge.log"));
    // 桥端口本机 :0 自选（bind→drop→重 bind 竞态窗口极小可接受；sshd 侧端口独立分配）
    let local_port: u16 = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .unwrap_or(0);
    if local_port == 0 {
        write_state("err: no local port available");
        std::process::exit(2);
    }
    LOCAL_PORT.store(local_port, std::sync::atomic::Ordering::Relaxed);
    let bridge = std::sync::Arc::new(core::config::Bridge { dir });
    {
        let b2 = bridge.clone();
        std::thread::spawn(move || core::protocol::tcp_loop(b2, local_port));
    }
    // pidfile + 状态文件
    let _ = std::fs::create_dir_all(core::config::tmp_dir());
    let _ = std::fs::write(core::config::pid_path(), format!("{}\n", std::process::id()));

    // forwards=[(0, local)]：sshd 自选远端端口（tcpip_forward 返回值回填 live 映射）
    let cfg = core::tunnel::TunnelCfg::from_ssh_target(&t, 0, local_port, vec![(0u16, local_port)], None);
    let key = match load_private_key(&cfg.key_path) {
        Ok(k) => k,
        Err(e) => { write_state(&format!("err: key load fail: {e}")); std::process::exit(255); }
    };
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio");
    let cfg2 = cfg.clone();
    rt.block_on(async move {
        // ctl 通道：本地控制端口线程 → mpsc → tunnel 循环 select 处理
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<core::tunnel::ControlCmd>(16);
        // ctl 监听端口（loopback 自选）；**up 后才对客户端开放**——写进 state JSON 的 ctl 字段
        let ctl = std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map(|a| a.port()).unwrap_or(0);
        // ctl 服务线程：收行 JSON 转译 ControlCmd（回执 oneshot 由 tunnel 循环发回，此处转发）
        if ctl > 0 {
            let tx2 = cmd_tx.clone();
            std::thread::spawn(move || {
                let l = match std::net::TcpListener::bind(("127.0.0.1", ctl)) {
                    Ok(l) => l, Err(_) => return };
                for s in l.incoming().flatten() {
                    let tx3 = tx2.clone();
                    let mut s = s;
                    std::thread::spawn(move || {
                        use std::io::{BufRead, Write};
                        let Some(rd2) = s.try_clone().ok() else { return };
                        let rd = std::io::BufReader::new(rd2);
                        for line in rd.lines().flatten() {
                            let v: serde_json::Value = match serde_json::from_str(&line) { Ok(v) => v, Err(_) => continue };
                            let id = v.get("id").cloned().unwrap_or_default();
                            match v.get("kind").and_then(|k| k.as_str()) {
                                Some("ctl_list") => {
                                    let (otx, orx) = tokio::sync::oneshot::channel();
                                    if tx3.blocking_send(core::tunnel::ControlCmd::List { reply: otx }).is_ok() {
                                        if let Ok(list) = orx.blocking_recv() {
                                            let fl: Vec<serde_json::Value> = list.iter()
                                                .map(|(r, l)| serde_json::json!({"remote": r, "local": l})).collect();
                                            let _ = serde_json::to_writer(&mut s, &serde_json::json!({"id": id, "ok": true, "forwards": fl}));
                                            let _ = s.write_all(b"\n");
                                        }
                                    }
                                }
                                Some("ctl_map") => {
                                    // spec: "<local>" 或 "<remote>:<local>"（本机目标=127.0.0.1:<local> 语义同 R5）
                                    let spec = v.get("spec").and_then(|x| x.as_str()).unwrap_or("").to_string();
                                    let (remote, local) = parse_map_spec(&spec);
                                    let (otx, orx) = tokio::sync::oneshot::channel();
                                    let cmd = core::tunnel::ControlCmd::AddForward { remote, local, reply: otx };
                                    if tx3.blocking_send(cmd).is_ok() {
                                        if let Ok(res) = orx.blocking_recv() {
                                            let body = match res { Ok(b) => serde_json::json!({"id": id, "ok": true, "remote": b}), Err(e) => serde_json::json!({"id": id, "ok": false, "error": e}) };
                                            let _ = serde_json::to_writer(&mut s, &body);
                                            let _ = s.write_all(b"\n");
                                        }
                                    }
                                }
                                Some("ctl_repick") => {
                                    // repick：pair 生成后随命令传入（set_pair 统一在 tunnel select 分支做——GUI/CLI 同路径）
                                    let new_pair = core::auth::gen_pair().unwrap_or_default();
                                    let pair_echo = new_pair.clone(); // 回执打印用（命令里 move 走原件）
                                    let (otx, orx) = tokio::sync::oneshot::channel();
                                    let cmd = core::tunnel::ControlCmd::Repick { new_pair, reply: otx };
                                    if tx3.blocking_send(cmd).is_ok() {
                                        if let Ok(res) = orx.blocking_recv() {
                                            let body = match res {
                                                Ok(port) => serde_json::json!({"id": id, "ok": true, "port": port, "pair": pair_echo}),
                                                Err(e) => serde_json::json!({"id": id, "ok": false, "error": e}),
                                            };
                                            let _ = serde_json::to_writer(&mut s, &body);
                                            let _ = s.write_all(b"\n");
                                            // state 同步新端口（pair 不落盘）
                                            if let Some(np) = body.get("port").and_then(|x| x.as_u64()) {
                                                let local = LOCAL_PORT.load(std::sync::atomic::Ordering::Relaxed);
                                                let ctlp = CTL_PORT.load(std::sync::atomic::Ordering::Relaxed);
                                                write_state(&format!("{{\"state\":\"up\",\"port\":{np},\"local\":{local},\"ctl\":{ctlp}}}"));
                                            }
                                        }
                                    }
                                }
                                Some("ctl_unmap") => {
                                    let remote = v.get("remote").and_then(|x| x.as_u64()).unwrap_or(0) as u16;
                                    let (otx, orx) = tokio::sync::oneshot::channel();
                                    let cmd = core::tunnel::ControlCmd::RetireForward { remote, reply: otx };
                                    if tx3.blocking_send(cmd).is_ok() {
                                        if let Ok(res) = orx.blocking_recv() {
                                            let body = match res { Ok(()) => serde_json::json!({"id": id, "ok": true}), Err(e) => serde_json::json!({"id": id, "ok": false, "error": e}) };
                                            let _ = serde_json::to_writer(&mut s, &body);
                                            let _ = s.write_all(b"\n");
                                        }
                                    }
                                }
                                Some("ctl_probe") => {
                                    // 枚举 host 127.0.0.1 监听端口（-L 服务转发候选；ssh exec netstat，秒级）
                                    let (otx, orx) = tokio::sync::oneshot::channel();
                                    if tx3.blocking_send(core::tunnel::ControlCmd::Probe { reply: otx }).is_ok() {
                                        if let Ok(res) = orx.blocking_recv() {
                                            let body = match res {
                                                Ok(ports) => {
                                                    // PortInfo 无 serde derive（不引 serde 直依赖）——手动转数组 [{port,proc_name}]
                                                    let arr: Vec<serde_json::Value> = ports.into_iter()
                                                        .map(|i| serde_json::json!({"port": i.port, "proc_name": i.proc_name})).collect();
                                                    serde_json::json!({"id": id, "ok": true, "ports": arr})
                                                }
                                                Err(e) => serde_json::json!({"id": id, "ok": false, "error": e}),
                                            };
                                            let _ = serde_json::to_writer(&mut s, &body);
                                            let _ = s.write_all(b"\n");
                                        }
                                    }
                                }
                                Some("ctl_ladd") => {
                                    let port = v.get("port").and_then(|x| x.as_u64()).unwrap_or(0) as u16;
                                    let (otx, orx) = tokio::sync::oneshot::channel();
                                    if tx3.blocking_send(core::tunnel::ControlCmd::AddLocal { port, reply: otx }).is_ok() {
                                        if let Ok(res) = orx.blocking_recv() {
                                            let body = match res { Ok(()) => serde_json::json!({"id": id, "ok": true, "port": port}), Err(e) => serde_json::json!({"id": id, "ok": false, "error": e}) };
                                            let _ = serde_json::to_writer(&mut s, &body);
                                            let _ = s.write_all(b"\n");
                                        }
                                    }
                                }
                                Some("ctl_ldel") => {
                                    let port = v.get("port").and_then(|x| x.as_u64()).unwrap_or(0) as u16;
                                    let (otx, orx) = tokio::sync::oneshot::channel();
                                    if tx3.blocking_send(core::tunnel::ControlCmd::RetireLocal { port, reply: otx }).is_ok() {
                                        if let Ok(res) = orx.blocking_recv() {
                                            let body = match res { Ok(()) => serde_json::json!({"id": id, "ok": true, "port": port}), Err(e) => serde_json::json!({"id": id, "ok": false, "error": e}) };
                                            let _ = serde_json::to_writer(&mut s, &body);
                                            let _ = s.write_all(b"\n");
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    });
                }
            });
            // state 写 ctl 端口（up 时 onup 里更新）
            CTL_PORT.store(ctl, std::sync::atomic::Ordering::Relaxed);
        }
        let ctl_static = ctl;
        let onup = move |remote: u16| {
            let local = LOCAL_PORT.load(std::sync::atomic::Ordering::Relaxed);
            write_state(&format!("{{\"state\":\"up\",\"port\":{remote},\"local\":{local},\"ctl\":{ctl_static}}}"));
        };
        let locals: core::tunnel::Locals = Default::default();
        let mut cmd_rx_opt = Some(cmd_rx);
        if let Err(e) = core::tunnel::tunnel_once_onup(&cfg2, key, Some(&onup), &mut cmd_rx_opt, locals.clone()).await {
            write_state(&format!("err: tunnel fail: {e}"));
            std::process::exit(255);
        }
        // 断开（正常/异常）：状态文件清 up
        write_state("down");
    });
}

// daemon 全局小状态：ctl 端口与本机桥端口（onup 回调写 state 需要）
static CTL_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
static LOCAL_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

/// "3080" → (None, 3080)；"3080:3080" → (Some(3080), 3080)；"9230:3080" → (Some(9230), 3080)
fn parse_map_spec(spec: &str) -> (Option<u16>, u16) {
    let parts: Vec<&str> = spec.split(':').collect();
    match parts.as_slice() {
        [l] => (None, l.parse().unwrap_or(0)),
        [r, l] => (Some(r.parse().unwrap_or(0)), l.parse().unwrap_or(0)),
        _ => (None, 0),
    }
}

fn load_private_key(path: &std::path::Path) -> Result<russh::keys::PrivateKey, String> {
    // russh keys 解码（OpenSSH 格式）；路径缺省时三层回退已在 from_ssh_target 处理
    let data = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    russh::keys::PrivateKey::from_openssh(&data).map_err(|e| format!("decode {}: {e}", path.display()))
}

fn cmd_status() {
    // 读 temp/pong/ 运行时状态：pidfile（进程活性）+ 状态文件（连接状态）
    let pid = core::config::pid_path();
    match std::fs::read_to_string(&pid) {
        Ok(s) => {
            let p: u32 = s.trim().parse().unwrap_or(0);
            let alive = process_alive(p);
            println!("daemon pid: {p} ({} Running)", if alive { "is" } else { "not" });
            match std::fs::read_to_string(state_path()).unwrap_or_default() {
                st if st.starts_with("up") => println!("state: {}", st.trim()),
                st if !st.is_empty() => println!("state: {}", st.trim()),
                _ => println!("state: unknown (no state file)"),
            }
            if !alive { println!("hint: daemon not alive - run pong user@host"); }
        }
        Err(_) => {
            println!("not running (no pidfile at {})", pid.display());
        }
    }
}

fn process_alive(pid: u32) -> bool {
    if pid == 0 { return false; }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output().map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string())).unwrap_or(false)
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
}

fn cmd_exit() {
    // 杀 daemon：读 pidfile → 活性探测（Windows tasklist /FI、Unix kill -0）→ 活着才 kill
    // （2026-11-17 实锤：陈旧 pidfile 直接 taskkill 会把 "ERROR: The process not found" 溅到用户终端+假成功）
    let pid = core::config::pid_path();
    match std::fs::read_to_string(&pid).map(|s| s.trim().to_string()) {
        Ok(s) if !s.is_empty() => {
            #[cfg(target_os = "windows")]
            let alive = std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {s}"), "/NH"])
                .output().map(|o| {
                    let t = String::from_utf8_lossy(&o.stdout);
                    t.to_uppercase().contains("PONG") || (!t.contains("INFO:") && !t.trim().is_empty() && t.contains(s.as_str()))
                }).unwrap_or(false);
            #[cfg(not(target_os = "windows"))]
            let alive = std::process::Command::new("kill").args(["-0", &s]).status().map(|st| st.success()).unwrap_or(false);
            if alive {
                #[cfg(target_os = "windows")]
                { let _ = std::process::Command::new("taskkill").args(["/PID", &s, "/F"]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status(); }
                #[cfg(not(target_os = "windows"))]
                { let _ = std::process::Command::new("kill").arg(&s).status(); }
                let _ = std::fs::remove_file(state_path());
                let _ = std::fs::remove_file(&pid);
                println!("pong: exit signal sent to pid {s}");
            } else {
                // 死进程：清残留状态文件，报 not running（不打扰用户）
                let _ = std::fs::remove_file(state_path());
                let _ = std::fs::remove_file(&pid);
                println!("pong: not running (stale pidfile cleaned)");
            }
        }
        _ => {
            println!("pong: not running");
        }
    }
}

/// 读 state JSON 拿 ctl 端口（daemon 常驻时存在）
fn ctl_port_of_state() -> Option<u16> {
    let st = std::fs::read_to_string(state_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&st).ok()?;
    v.get("ctl").and_then(|x| x.as_u64()).map(|x| x as u16)
}

/// 向 daemon ctl 端口发一条指令并收一行 JSON 回执
fn ctl_send(payload: &serde_json::Value) -> Result<serde_json::Value, String> {
    let port = ctl_port_of_state().ok_or("no daemon ctl port (daemon not up)?")?;
    use std::io::{BufRead, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).map_err(|e| format!("ctl connect: {e}"))?;
    s.write_all(format!("{}\n", payload).as_bytes()).map_err(|e| format!("ctl send: {e}"))?;
    let mut rd = std::io::BufReader::new(s);
    let mut line = String::new();
    rd.read_line(&mut line).map_err(|e| format!("ctl recv: {e}"))?;
    serde_json::from_str(&line).map_err(|e| format!("ctl parse: {e}"))
}

fn cmd_port(rest: &[String]) {
    let sub = rest.first().map(|s| s.as_str()).unwrap_or("list"); // 无参 fallback=list
    let sub = if sub == "--list" { "list" } else { sub };
    match sub {
        "list" => {
            match ctl_send(&serde_json::json!({"id":"l1", "kind":"ctl_list"})) {
                Ok(v) if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) => {
                    println!("forwards (remote -> local):");
                    if let Some(fl) = v.get("forwards").and_then(|x| x.as_array()) {
                        if fl.is_empty() { println!("  (none)"); }
                        for f in fl {
                            println!("  {} -> {}", f.get("remote").and_then(|x| x.as_u64()).unwrap_or(0),
                                f.get("local").and_then(|x| x.as_u64()).unwrap_or(0));
                        }
                    }
                }
                Ok(_) => { eprintln!("pong: port list failed"); std::process::exit(255); }
                Err(e) => { eprintln!("pong: {e}"); std::process::exit(255); }
            }
        }
        "map" => {
            // port map <local> | port map <remote>:<local>；--list 顺手支持（=port list）
            if rest.get(1).map(|s| s.as_str()) == Some("--list") {
                return cmd_port(&["list".to_string()]);
            }
            let Some(spec) = rest.get(1) else {
                eprintln!("usage: pong port map <local>            (sshd picks the remote port)");
                eprintln!("       pong port map <remote>:<local>   (fixed remote port - fails if taken)");
                std::process::exit(255);
            };
            let spec = spec.trim().to_string();
            let parts: Vec<&str> = spec.split(':').collect();
            // spec 必须全数字段（<local> 或 <remote>:<local>）——防 '--list' 之类误入 ctl_map 乱映射
            let ok = parts.iter().all(|p| p.parse::<u16>().is_ok()) && !parts.is_empty() && parts.len() <= 2;
            if !ok {
                eprintln!("pong: invalid spec '{spec}' - use 'port map <local>' or 'port map <remote>:<local>'");
                eprintln!("      (to enumerate host service ports use 'pong map --list')");
                std::process::exit(255);
            }
            match ctl_send(&serde_json::json!({"id":"m1", "kind":"ctl_map", "spec": spec})) {
                Ok(v) if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) => {
                    let remote = v.get("remote").and_then(|x| x.as_u64()).unwrap_or(0);
                    let local = parts.last().and_then(|x| x.parse::<u16>().ok()).unwrap_or(0);
                    println!("mapped: remote {remote} -> local {local}");
                }
                Ok(v) => {
                    eprintln!("pong: map failed: {}", v.get("error").and_then(|x| x.as_str()).unwrap_or("?"));
                    std::process::exit(255);
                }
                Err(e) => { eprintln!("pong: {e}"); std::process::exit(255); }
            }
        }
        "unmap" | "rm" | "del" => {
            // port unmap <remote>
            let Some(remote) = rest.get(1).and_then(|x| x.parse::<u16>().ok()) else {
                eprintln!("usage: pong port unmap <remote>    (remote port as shown by 'port list')");
                std::process::exit(255);
            };
            match ctl_send(&serde_json::json!({"id":"u1", "kind":"ctl_unmap", "remote": remote})) {
                Ok(v) if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) => {
                    println!("unmapped: remote {remote}");
                }
                Ok(v) => {
                    eprintln!("pong: unmap failed: {}", v.get("error").and_then(|x| x.as_str()).unwrap_or("?"));
                    std::process::exit(255);
                }
                Err(e) => { eprintln!("pong: {e}"); std::process::exit(255); }
            }
        }
        _ => {
            eprintln!("usage: pong port               list forwards");
            eprintln!("       pong port map <local>            (sshd picks remote port)");
            eprintln!("       pong port map <remote>:<local>   (fixed remote port)");
            eprintln!("       pong port unmap <remote>");
            std::process::exit(255);
        }
    }
}

/// 服务转发族（-L，2026-11-18）：client 用户经 client 本地端口访问 host（agent 服务器）上的 127.0.0.1 服务。
/// map --list = 枚举 host 127.0.0.1 监听端口（ssh exec netstat）；map <port> = 建 -L；unmap <port> = 摘除。
fn cmd_map(rest: &[String]) {
    match rest.first().map(|s| s.as_str()) {
        Some("--list") | Some("-l") | None => {
            match ctl_send(&serde_json::json!({"id":"pl1", "kind":"ctl_probe"})) {
                Ok(v) if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) => {
                    println!("host (127.0.0.1) listening ports (current user):  PORT  PROCESS");
                    if let Some(arr) = v.get("ports").and_then(|x| x.as_array()) {
                        if arr.is_empty() { println!("  (none)"); }
                        for p in arr {
                            let port = p.get("port").and_then(|x| x.as_u64()).unwrap_or(0);
                            let proc_name = p.get("proc_name").and_then(|x| x.as_str()).unwrap_or("");
                            if proc_name.is_empty() {
                                println!("  {:>6}  (unknown)", port);
                            } else {
                                println!("  {:>6}  {}", port, proc_name);
                            }
                        }
                    }
                    println!("map with: pong map <port>");
                }
                Ok(v) => {
                    eprintln!("pong: map --list failed: {}", v.get("error").and_then(|x| x.as_str()).unwrap_or("?"));
                    std::process::exit(255);
                }
                Err(e) => { eprintln!("pong: {e}"); std::process::exit(255); }
            }
        }
        Some(p) => {
            let Some(port) = p.parse::<u16>().ok().filter(|x| *x > 0) else {
                eprintln!("usage: pong map --list        enumerate host listening ports");
                eprintln!("       pong map <port>        forward client:<port> -> host:<port>");
                std::process::exit(255);
            };
            match ctl_send(&serde_json::json!({"id":"la1", "kind":"ctl_ladd", "port": port})) {
                Ok(v) if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) => {
                    println!("mapped: client 127.0.0.1:{port} -> host 127.0.0.1:{port}");
                }
                Ok(v) => {
                    eprintln!("pong: map failed: {}", v.get("error").and_then(|x| x.as_str()).unwrap_or("?"));
                    std::process::exit(255);
                }
                Err(e) => { eprintln!("pong: {e}"); std::process::exit(255); }
            }
        }
    }
}

fn cmd_unmap(rest: &[String]) {
    let Some(port) = rest.first().and_then(|x| x.parse::<u16>().ok()) else {
        eprintln!("usage: pong unmap <port>       (port as shown by 'pong map --list' after mapping)");
        std::process::exit(255);
    };
    match ctl_send(&serde_json::json!({"id":"ld1", "kind":"ctl_ldel", "port": port})) {
        Ok(v) if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) => {
            println!("unmapped: local {port}");
        }
        Ok(v) => {
            eprintln!("pong: unmap failed: {}", v.get("error").and_then(|x| x.as_str()).unwrap_or("?"));
            std::process::exit(255);
        }
        Err(e) => { eprintln!("pong: {e}"); std::process::exit(255); }
    }
}

fn cmd_repick() {
    // repick（用户定案）：重生成 port+pair——pair=daemon 内存 set_pair 新码；port=撤旧 -R、sshd 重选
    // daemon 经 ctl 端口收 ctl_repick：旧 pair 失效+新码经 ctl 回执给客户端打印（不落盘）
    match ctl_send(&serde_json::json!({"id":"r1", "kind":"ctl_repick"})) {
        Ok(v) if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) => {
            println!("port={}", v.get("port").and_then(|x| x.as_u64()).unwrap_or(0));
            println!("pair={}", v.get("pair").and_then(|x| x.as_str()).unwrap_or(""));
        }
        Ok(v) => {
            eprintln!("pong: repick failed: {}", v.get("error").and_then(|x| x.as_str()).unwrap_or("?"));
            std::process::exit(255);
        }
        Err(e) => { eprintln!("pong: {e}"); std::process::exit(255); }
    }
}
