#!/日志（bridge.log + 控制台）
use std::sync::{Mutex, OnceLock};
use std::path::PathBuf;
use std::io::Write as IoWrite;
use std::fs;

pub static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

pub static LOG_LOCK: Mutex<()> = Mutex::new(());
pub fn log(msg: &str) {
    let Some(path) = LOG_PATH.get() else { return };
    let ts = fmt_clock(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
    if let Ok(meta) = fs::metadata(path) {
        if meta.len() > 1_000_000 {
            let _ = fs::rename(path, path.with_extension("1"));
        }
    }
    // 进程内串行化：多 worker 并发写日志曾出现行交错（[[09:41:2109:41:21] ]）
    let _g = LOG_LOCK.lock();
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{ts}] {msg}");
    }
    // 控制台实时回显（GUI 子系统下无控制台时输出被丢弃，git-bash/双击诊断可见；单行不刷屏）
    eprintln!("[{ts}] {msg}");
}

pub fn fmt_clock(secs: u64) -> String {
    let d = secs % 86400;
    format!("{:02}:{:02}:{:02}", d / 3600, (d % 3600) / 60, d % 60)
}

pub fn fmt_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // UTC → 本地时区用 +8（部署环境 Asia/Shanghai），epoch 日内偏移直接算
    fmt_clock(secs + 8 * 3600)
}

pub fn trunc(s: &str) -> String {
    let mut chars: Vec<char> = s.chars().collect();
    if chars.len() > 8000 { chars.truncate(8000); chars.push('…'); }
    chars.into_iter().collect()
}
