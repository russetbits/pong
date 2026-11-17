// Linux daemon 实现：CLI start/kill/status/--daemon-run（无 GUI/托盘——用户明令 UI 暂缓）
// 复用 core 全套（config/protocol/tunnel/cli/files/exec）；平台差异：TEMP→/tmp、单实例=pid 文件
use std::path::PathBuf;
use std::fs;
use std::sync::Arc;
use crate::core::config::{EXE_DIR, CFG, IniConfig, find_work_dir, resolve, Bridge};
use crate::core::log::{log, LOG_PATH};
use crate::core::protocol::{SHUTDOWN, tcp_loop, ctl_loop, work_loop};

pub fn main_impl(mode: Option<String>) {
    match mode.as_deref() {
        Some("start") => { daemon_start(); return; }
        Some("kill") => { daemon_kill(); return; }
        Some("status") => { daemon_status(); return; }
        Some("--daemon-run") => { daemon_run(); return; }
        _ => {}
    }
    // 无参=前台 daemon（Linux 无托盘；systemd/手工后台由调用方决定）
    daemon_run();
}

/// 单实例：**用户 temp** pid 文件（/tmp/pong/bridge.pid；用户定案——用户级单例，exe/cache 目录多用户互踩）
/// O_EXCL 语义由 write 全量覆盖+存活探测替代（退出的 stale pid 由 kill/status 清理）
fn pid_file() -> Option<PathBuf> {
    let p = crate::core::config::pid_path();
    let _ = fs::create_dir_all(p.parent()?);
    Some(p)
}

fn write_pid() -> bool {
    let Some(p) = pid_file() else { return false };
    if let Ok(old) = fs::read_to_string(&p) {
        if let Ok(oldpid) = old.trim().parse::<i32>() {
            if std::path::Path::new(&format!("/proc/{oldpid}")).exists() {
                eprintln!("pong: already running (pid {oldpid})");
                return false;
            }
        }
    }
    fs::write(&p, format!("{}\n", std::process::id())).is_ok()
}

fn cache_dir() -> Option<PathBuf> {
    // Linux：优先 config.ini cache_dir=；缺省 /tmp/pong
    let exe = std::env::current_exe().ok()?;
    let parent = exe.parent()?;
    let cfg = IniConfig::load(parent);
    let cand = resolve(&cfg.cache_dir);
    if !cand.as_os_str().is_empty() {
        return Some(cand.to_owned());
    }
    Some(PathBuf::from("/tmp/pong"))
}

fn daemon_start() {
    let exe = match std::env::current_exe() { Ok(e) => e, Err(e) => { eprintln!("exe: {e}"); return; } };
    if daemon_alive() { println!("pong: already running"); return; }
    // Linux：fork 双跳脱离终端（父立即退出，子 setsid）
    match unsafe { libc_fork() } {
        0 => {
            let _ = unsafe { libc_setsid() };
            let _ = std::process::Command::new(exe).arg("--daemon-run").status();
            std::process::exit(0);
        }
        -1 => eprintln!("fork fail"),
        _ => { println!("pong daemon starting"); }
    }
}

fn daemon_kill() {
    match daemon_request("shutdown") {
        Some(true) => println!("pong: shutdown requested"),
        _ => println!("pong: not running"),
    }
}

fn daemon_status() {
    println!("pong: {}", if daemon_alive() { "running" } else { "not running" });
}

pub fn daemon_request(kind: &str) -> Option<bool> {
    crate::core::cli::daemon_request(kind)
}

fn daemon_alive() -> bool {
    daemon_request("ping").unwrap_or(false)
}

fn daemon_run() {
    // 单实例
    let _ = fs::create_dir_all(cache_dir().unwrap_or_else(|| PathBuf::from("/tmp/pong")));
    if !write_pid() { std::process::exit(1); }
    run_core_services();
    log("daemon running (headless)");
    loop {
        if SHUTDOWN.load(std::sync::atomic::Ordering::Relaxed) {
            log("daemon shutdown requested");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    if let Some(p) = pid_file() { let _ = fs::remove_file(p); }
    std::process::exit(0);
}

fn run_core_services() -> u16 {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let _ = EXE_DIR.set(parent.to_path_buf());
            let cfg = IniConfig::load(parent);
            let _ = CFG.set(cfg.clone());
            crate::core::config::init_runtime(cfg);
        }
    }
    // 默认工作目录 = /tmp（agent 可 chdir）
    let _ = std::env::set_current_dir("/tmp");
    let Some(dir) = find_work_dir() else {
        eprintln!("pong: bridge dir not found (bridge.ini next to exe)");
        std::process::exit(2);
    };
    for sub in ["in", "out", "done", "log"] {
        let _ = fs::create_dir_all(dir.join(sub));
    }
    let cache_dir = cache_dir().unwrap_or_else(|| dir.join("log"));
    let _ = fs::create_dir_all(&cache_dir);
    let _ = LOG_PATH.set(cache_dir.join("bridge.log"));
    let _ = fs::write(crate::core::config::pid_path(), format!("{}\n", std::process::id()));
    log("start");
    let tcp_port: u16 = CFG.get().map(|c| c.port).unwrap_or(7101);
    let bridge_shared = Arc::new(Bridge { dir: dir.clone() });
    {
        let bridge = bridge_shared.clone();
        std::thread::spawn(move || work_loop(bridge));
    }
    // 跨平台对称：Linux client 与 Windows client 同构（桥+控制面+work+隧道四线程）。
    // 注意：若本机 7101 已被 sshd 的反向转发占用（本程序跑在 sshd 主机上的测试特例），
    // 换个 port= 即可——真机部署 client 不会与 sshd 同机，无此冲突。
    {
        let bridge = bridge_shared.clone();
        std::thread::spawn(move || tcp_loop(bridge, tcp_port));
    }
    {
        let bridge = bridge_shared.clone();
        std::thread::spawn(move || ctl_loop(bridge, tcp_port));
    }
    {
        let port = tcp_port;
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all().build().expect("tokio runtime");
            // GUI 面板命令通道（probe/ladd/ldel/list）：tx 进 static 供 GUI 同进程直发（2026-11-18）
            let (gtx, grx) = tokio::sync::mpsc::channel::<crate::core::tunnel::ControlCmd>(16);
            let _ = crate::core::tunnel::GUI_CMD_TX.set(gtx);
            let locals: crate::core::tunnel::Locals = Default::default();
            rt.block_on(async move {
                crate::core::tunnel::tunnel_forever(port, Some(grx), locals).await;
            });
        });
    }
    tcp_port
}

// 最小 fork/setsid FFI（避免引 libc crate）
unsafe fn libc_fork() -> i32 {
    unsafe { core::arch::asm!("", options(nomem)) }; // 占位防优化；真实现见下
    extern "C" { fn fork() -> i32; }
    fork()
}
unsafe fn libc_setsid() -> i32 {
    extern "C" { fn setsid() -> i32; }
    setsid()
}
