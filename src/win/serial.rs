#!/串口：Windows COM（一次性+持久会话）与 Linux stub
use std::time::{Instant, Duration};
use std::sync::OnceLock;
use std::sync::atomic::Ordering;
use std::fs;
use std::io::Write as IoWrite;
use std::path::PathBuf;
use crate::core::protocol::{Task, Res, SHUTDOWN};
use crate::core::log::*;
use crate::win::main_impl::winffi::*;
use crate::core::config::CFG;

pub static SERIAL_CACHE: OnceLock<PathBuf> = OnceLock::new();

// 会话表：port 名 → 会话。LazyLock 规避 static 非 const；句柄存 isize（裸指针不 Send，跨线程用数值传）
#[cfg(target_os = "windows")]
pub static SERIAL_SESSIONS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, SerialSess>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[cfg(target_os = "windows")]
pub struct SerialSess {
    handle: isize,                   // COM 句柄数值化（isize 可跨线程；win_* 封装内转指针）
    log_path: PathBuf,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>, // 读线程退出标志
}

#[cfg(target_os = "windows")]
pub fn serial_log_dir() -> PathBuf {
    // fallback=temp/pong/serial（2026-11-25 用户实锤：CLI exe 的 daemon 路径不经 main_impl 的
    // SERIAL_CACHE.set → 旧 fallback PathBuf::from("log") 是**相对 CWD/exe 目录**→log 落 exe 旁）
    let base = SERIAL_CACHE.get().cloned().unwrap_or_else(|| crate::core::config::tmp_dir().join("serial"));
    let _ = fs::create_dir_all(&base);
    base
}

// ---- 串口 FFI 薄封装（供持久会话三命令与读线程使用；句柄以 isize 数值传递）----
#[cfg(target_os = "windows")]
pub fn win_crate_open(wide: &[u16]) -> isize {
    unsafe {
        CreateFileW(wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE, 0, std::ptr::null_mut(),
            OPEN_EXISTING, 0, std::ptr::null_mut()) as isize
    }
}

#[cfg(target_os = "windows")]
pub fn win_serial_config(handle: isize, baud: u32) -> bool {
    unsafe {
        let mut dcb: DCB = std::mem::zeroed();
        dcb.DCBlength = std::mem::size_of::<DCB>() as u32;
        if GetCommState(handle as *mut std::ffi::c_void, &mut dcb) == 0 { return false; }
        // 显式清流控（CP210x 驱动默认 DCB 常带 fOutxCtsFlow——WriteFile 等 CTS 卡 5-60s 实测根因）
        // 位定义：fOutxCtsFlow=0x4 fOutxDsrFlow=0x8 fDsrSensitivity=0x40 fOutX=0x100 fInX=0x200 fAbortOnError=0x2000
        dcb.flags &= !(0x4 | 0x8 | 0x40 | 0x100 | 0x200 | 0x2000);
        // fBinary 必开（0x1，否则非二进制模式）；DTR/RTS 拉起（0x10=DTR_CONTROL_ENABLE, 0x1000=RTS_CONTROL_ENABLE）
        dcb.flags |= 0x1 | 0x10 | 0x1000;
        dcb.BaudRate = baud;
        dcb.ByteSize = 8;
        dcb.Parity = 0;
        dcb.StopBits = 0;
        if SetCommState(handle as *mut std::ffi::c_void, &mut dcb) == 0 { return false; }
        // 读 200ms 返回（无数据不阻塞）；写 5s 防 CTS 卡死（一次性版同款定案）
        let to = COMMTIMEOUTS {
            ReadIntervalTimeout: 50,
            ReadTotalTimeoutConstant: 200,
            ReadTotalTimeoutMultiplier: 0,
            WriteTotalTimeoutConstant: 5000,
            WriteTotalTimeoutMultiplier: 0,
        };
        SetCommTimeouts(handle as *mut std::ffi::c_void, &to) != 0
    }
}

// 读一次（内部 COMMTIMEOUTS 已限 200ms），返回字节数
#[cfg(target_os = "windows")]
pub fn win_read_some(handle: isize, buf: &mut [u8]) -> usize {
    unsafe {
        let mut got: u32 = 0;
        if ReadFile(handle as *mut std::ffi::c_void, buf.as_mut_ptr(), buf.len() as u32, &mut got, std::ptr::null_mut()) != 0 {
            got as usize
        } else { 0 }
    }
}

// 写全部（COMMTIMEOUTS 写超时 5s 兜底），返回实际写入字节数
#[cfg(target_os = "windows")]
pub fn win_write_some(handle: isize, data: &[u8]) -> usize {
    unsafe {
        let mut wrote: u32 = 0;
        if WriteFile(handle as *mut std::ffi::c_void, data.as_ptr(), data.len() as u32, &mut wrote, std::ptr::null_mut()) != 0 {
            wrote as usize
        } else { 0 }
    }
}

#[cfg(target_os = "windows")]
pub fn win_close(handle: isize) {
    unsafe { CloseHandle(handle as *mut std::ffi::c_void); }
}

// ---------- read/write 文件（绕过 samba：agent 直接经桥读写 Windows 文件）----------


#[cfg(target_os = "windows")]
// 外审 2 轮后修（用户报 Apply 不生效）：serial 读运行时优先（UI Apply/CLI config set 热生效），CFG 启动快照兜底
pub fn cfg_serial_port() -> String {
    crate::core::config::get_runtime().map(|c| c.serial_port.clone())
        .or_else(|| CFG.get().map(|c| c.serial_port.clone()))
        .unwrap_or_else(|| "COM3".into())
}
#[cfg(target_os = "windows")]
pub fn cfg_serial_baud() -> u32 {
    crate::core::config::get_runtime().map(|c| c.serial_baud)
        .or_else(|| CFG.get().map(|c| c.serial_baud))
        .unwrap_or(921_600)
}

#[cfg(target_os = "windows")]
pub fn run_serial_open(t: &Task) -> Res {
    let start = Instant::now();
    let port = t.port.clone().unwrap_or_else(cfg_serial_port);
    let baud = if t.baud == 0 { cfg_serial_baud() } else { t.baud };
    let mut sessions = SERIAL_SESSIONS.lock().unwrap();
    // 幂等：同口已开 → 返回已有 log 路径
    if let Some(s) = sessions.get(&port) {
        return Res { id: t.id.clone(), ok: true, exit_code: Some(0),
            output: format!("already-open {}", s.log_path.display()), ms: start.elapsed().as_millis() };
    }
    let wide: Vec<u16> = format!("\\\\.\\{port}").encode_utf16().chain(std::iter::once(0)).collect();
    let handle = win_crate_open(&wide);
    if handle == 0 {
        return Res { id: t.id.clone(), ok: false, exit_code: None,
            output: format!("open {port} fail（口被占或不存在）"), ms: start.elapsed().as_millis() };
    }
    // DCB 配置（沿用一次性版逻辑）
    unsafe {
        if !win_serial_config(handle, baud) {
            win_close(handle);
            return Res { id: t.id.clone(), ok: false, exit_code: None,
                output: "config fail".into(), ms: start.elapsed().as_millis() };
        }
    }
    // log 文件：cache/serial/COM3-<epoch>.log
    let epoch = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs()).unwrap_or(0);
    let log_path = serial_log_dir().join(format!("{port}-{epoch}.log"));
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    // 常驻读线程：ReadFile 200ms 超时轮询 → 追加写 log
    {
        let stop = stop.clone();
        let log_path = log_path.clone();
        std::thread::spawn(move || {
            let mut acc: Vec<u8> = Vec::new();
            let mut flush_t = std::time::Instant::now();
            let mut buf = [0u8; 4096];
            loop {
                if stop.load(std::sync::atomic::Ordering::Relaxed)
                    || SHUTDOWN.load(std::sync::atomic::Ordering::Relaxed) { break; }
                let got = win_read_some(handle, &mut buf); // 200ms 内读到的字节数（0=无数据）
                if got > 0 { acc.extend_from_slice(&buf[..got]); }
                // 500ms 或缓冲>4KB 时 flush 一次（减少 samba 小写入）
                if !acc.is_empty() && (flush_t.elapsed() >= Duration::from_millis(500) || acc.len() > 4096) {
                    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&log_path) {
                        let _ = f.write_all(&acc);
                    }
                    acc.clear();
                    flush_t = std::time::Instant::now();
                }
            }
            if !acc.is_empty() {
                if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&log_path) {
                    let _ = f.write_all(&acc);
                }
            }
        });
    }
    sessions.insert(port.clone(), SerialSess { handle, log_path: log_path.clone(), stop });
    log(&format!("serial-open {port}@{baud} log={}", log_path.display()));
    Res { id: t.id.clone(), ok: true, exit_code: Some(0),
        output: log_path.display().to_string(), ms: start.elapsed().as_millis() }
}

// serial_write：向已开会话写命令（自动补 CRLF），立即返回
#[cfg(target_os = "windows")]
#[cfg(target_os = "windows")]
pub fn run_serial_write(t: &Task) -> Res {
    let start = Instant::now();
    let port = t.port.clone().unwrap_or_else(cfg_serial_port);
    let sessions = SERIAL_SESSIONS.lock().unwrap();
    let Some(s) = sessions.get(&port) else {
        return Res { id: t.id.clone(), ok: false, exit_code: None,
            output: format!("{port} not open, call serial_open first"), ms: start.elapsed().as_millis() };
    };
    let send = t.send.clone().unwrap_or_default();
    let line = if send.ends_with('\n') { send } else { format!("{send}\r\n") };
    let n = unsafe { win_write_some(s.handle, line.as_bytes()) };
    let ok = n == line.len();
    Res { id: t.id.clone(), ok, exit_code: Some(if ok { 0 } else { 1 }),
        output: format!("written {n}/{} → {}", line.len(), s.log_path.display()), ms: start.elapsed().as_millis() }
}

// serial_close：停读线程+关句柄+移除会话
#[cfg(target_os = "windows")]
#[cfg(target_os = "windows")]
pub fn run_serial_close(t: &Task) -> Res {
    let start = Instant::now();
    let port = t.port.clone().unwrap_or_else(cfg_serial_port);
    let s = SERIAL_SESSIONS.lock().unwrap().remove(&port);
    match s {
        Some(s) => {
            s.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(300)); // 让读线程跑完最后一次 flush
            unsafe { win_close(s.handle) };
            log(&format!("serial-close {port} log={}", s.log_path.display()));
            Res { id: t.id.clone(), ok: true, exit_code: Some(0),
                output: format!("closed {}", s.log_path.display()), ms: start.elapsed().as_millis() }
        }
        None => Res { id: t.id.clone(), ok: false, exit_code: None,
            output: format!("{port} not open"), ms: start.elapsed().as_millis() },
    }
}

#[cfg(target_os = "windows")]
pub fn run_serial(t: &Task) -> Res {
    let start = Instant::now();
    let stage = |name: &str| log(&format!("serial[{name}] +{}ms", start.elapsed().as_millis()));
    let port = t.port.clone().unwrap_or_else(cfg_serial_port);
    let send = t.send.clone().unwrap_or_default();
    // 读窗口=read_ms+500；总体受任务 timeout_ms 约束（此前未用，serial 卡写时可到 60s）
    let window = Duration::from_millis(t.read_ms + 500).min(Duration::from_millis(t.timeout_ms));
    log(&format!("serial {port}@{} send={send:?}", t.baud));

    stage("begin");
    let wide: Vec<u16> = format!("\\\\.\\{port}").encode_utf16().chain(std::iter::once(0)).collect();
    let handle = unsafe {
        CreateFileW(wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE, 0, std::ptr::null_mut(),
            OPEN_EXISTING, 0, std::ptr::null_mut())
    };
    if handle == INVALID_HANDLE_VALUE {
        return Res { id: t.id.clone(), ok: false, exit_code: None,
            output: format!("open {port} fail（TeraTerm 占用时先断开）"), ms: start.elapsed().as_millis() };
    }
    stage("opened");
    // 配置 DCB
    unsafe {
        let mut dcb: DCB = std::mem::zeroed();
        dcb.DCBlength = std::mem::size_of::<DCB>() as u32;
        if GetCommState(handle, &mut dcb) == 0 {
            CloseHandle(handle);
            return Res { id: t.id.clone(), ok: false, exit_code: None,
                output: "GetCommState fail".into(), ms: start.elapsed().as_millis() };
        }
        // 显式清流控位（CP210x 默认 DCB 带 fOutxCtsFlow → WriteFile 卡 CTS 27-60s 实测根因）+DTR/RTS 拉起+fBinary
        dcb.flags &= !(0x4 | 0x8 | 0x40 | 0x100 | 0x200 | 0x2000);
        dcb.flags |= 0x1 | 0x10 | 0x1000;
        dcb.BaudRate = t.baud;
        dcb.ByteSize = 8;
        dcb.Parity = 0; // NOPARITY
        dcb.StopBits = 0; // ONESTOPBIT
        if SetCommState(handle, &mut dcb) == 0 {
            CloseHandle(handle);
            return Res { id: t.id.clone(), ok: false, exit_code: None,
                output: "SetCommState fail".into(), ms: start.elapsed().as_millis() };
        }
        // 超时：Interval=50ms 字节间隔；TotalConstant=200ms 每次 ReadFile 无数据时最多等 200ms 返回
        // （Total=0 是无限阻塞——静默板卡曾把任务卡 15-45s 实测）；写超时 5s 防 CTS 未拉起卡写（实测 27-60s）
        let to = COMMTIMEOUTS {
            ReadIntervalTimeout: 50,
            ReadTotalTimeoutConstant: 200,
            ReadTotalTimeoutMultiplier: 0,
            WriteTotalTimeoutConstant: 5000,
            WriteTotalTimeoutMultiplier: 0,
        };
        SetCommTimeouts(handle, &to);
    }
    stage("configured");

    let line = if send.ends_with('\n') { send.clone() } else { format!("{send}\r\n") };
    let ok_write = unsafe {
        let mut written: u32 = 0;
        WriteFile(handle, line.as_ptr() as *const u8, line.len() as u32, &mut written, std::ptr::null_mut()) != 0
    };
    if !ok_write {
        unsafe { CloseHandle(handle); }
        return Res { id: t.id.clone(), ok: false, exit_code: None,
            output: format!("write fail +{}ms", start.elapsed().as_millis()), ms: start.elapsed().as_millis() };
    }
    stage("written");

    let deadline = Instant::now() + window;
    let mut acc: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        if SHUTDOWN.load(Ordering::Relaxed) { break; }
        unsafe {
            let mut got: u32 = 0;
            if ReadFile(handle, buf.as_mut_ptr(), buf.len() as u32, &mut got, std::ptr::null_mut()) != 0 && got > 0 {
                acc.extend_from_slice(&buf[..got as usize]);
            }
        }
        let hit = t.expect.as_ref().map(|w| {
            let lw = w.to_lowercase();
            let low = String::from_utf8_lossy(&acc).to_lowercase();
            low.contains(&lw)
        }).unwrap_or(false);
        if Instant::now() > deadline { break; }
        if !acc.is_empty() && hit { break; }
    }
    stage("read-done");
    unsafe { CloseHandle(handle); }
    let text = String::from_utf8_lossy(&acc).to_string();
    if text.trim().is_empty() {
        return Res { id: t.id.clone(), ok: false, exit_code: None,
            output: "(no serial output)".into(), ms: start.elapsed().as_millis() };
    }
    let ok = match &t.expect {
        Some(w) => text.to_lowercase().contains(&w.to_lowercase()),
        None => true,
    };
    Res { id: t.id.clone(), ok, exit_code: None, output: trunc(&text), ms: start.elapsed().as_millis() }
}

#[cfg(not(target_os = "windows"))]
pub fn run_serial_open(_t: &Task) -> Res {
    Res { id: _t.id.clone(), ok: false, exit_code: None, output: "serial session windows-only".into(), ms: 0 }
}
#[cfg(not(target_os = "windows"))]
pub fn run_serial_write(_t: &Task) -> Res {
    Res { id: _t.id.clone(), ok: false, exit_code: None, output: "serial session windows-only".into(), ms: 0 }
}
#[cfg(not(target_os = "windows"))]
pub fn run_serial_close(_t: &Task) -> Res {
    Res { id: _t.id.clone(), ok: false, exit_code: None, output: "serial session windows-only".into(), ms: 0 }
}

#[cfg(not(target_os = "windows"))]
pub fn run_serial(t: &Task) -> Res {
    Res { id: t.id.clone(), ok: false, exit_code: None,
        output: "serial only on windows build".into(), ms: 0 }
}

