#![cfg(target_os = "windows")]
use winffi::*;
use crate::win::gui_glue::{show_gui_window, request_app_exit, spawn_gui_thread};
use std::path::PathBuf;
use std::fs;
use crate::core::config::{EXE_DIR, CFG, IniConfig, find_work_dir, resolve, Bridge};
use crate::core::log::{log, LOG_PATH};
use crate::core::protocol::{SHUTDOWN, tcp_loop, work_loop};
use std::sync::Arc;
use crate::win::serial::SERIAL_CACHE;
use std::sync::atomic::{AtomicU32, Ordering};

pub static MAIN_THREAD_ID: AtomicU32 = AtomicU32::new(0);

pub mod winffi {
    #![allow(non_snake_case, non_camel_case_types, dead_code)]
    use std::ffi::c_void;

    pub type HWND = *mut c_void;
    pub type HICON = *mut c_void;
    pub type HINSTANCE = *mut c_void;
    pub type HANDLE = *mut c_void;
    pub type HMENU = *mut c_void;
    pub type WPARAM = usize;
    pub type LPARAM = isize;
    pub type LRESULT = isize;
    pub type UINT = u32;
    pub type DWORD = u32;
    pub type BOOL = i32;
    pub type LPCWSTR = *const u16;
    pub type LPWSTR = *mut u16;
    pub type LPCTSTR = *const u16;

    pub const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;
    pub const GENERIC_READ: DWORD = 0x8000_0000;
    pub const GENERIC_WRITE: DWORD = 0x4000_0000;
    pub const OPEN_EXISTING: DWORD = 3;
    pub const WM_QUIT: UINT = 0x0012;
    pub const WM_CLOSE: UINT = 0x0010;
    pub const WM_APP: UINT = 0x8000;
    pub const WM_USER: UINT = 0x0400;
    pub const WM_TRAYICON: UINT = WM_APP + 1;
    pub const WM_LBUTTONUP: UINT = 0x0202;
    pub const WM_RBUTTONUP: UINT = 0x0205;
    pub const WM_LBUTTONDBLCLK: UINT = 0x0203;
    pub const WM_COMMAND: UINT = 0x0111;
    pub const TPM_BOTTOMALIGN: UINT = 0x0020;
    pub const NIM_ADD: DWORD = 0;
    pub const NIM_MODIFY: DWORD = 1;
    pub const NIM_DELETE: DWORD = 2;
    pub const IDI_APPLICATION: LPCWSTR = 32512 as LPCWSTR;
    pub const NIF_MESSAGE: DWORD = 1;
    pub const NIF_ICON: DWORD = 2;
    pub const NIF_TIP: DWORD = 4;

    #[repr(C)]
    pub struct NOTIFYICONDATAW {
        pub cbSize: DWORD,
        pub hWnd: HWND,
        pub uID: UINT,
        pub uFlags: DWORD,
        pub uCallbackMessage: UINT,
        pub hIcon: HICON,
        pub szTip: [u16; 128],
        pub dwState: DWORD,
        pub dwStateMask: DWORD,
        pub szInfo: [u16; 256],
        pub uVersion: UINT,
        pub szInfoTitle: [u16; 64],
        pub dwInfoFlags: DWORD,
        pub guidItem: [u8; 16],
        pub hBalloonIcon: HICON,
    }

    #[repr(C)]
    pub struct DCB {
        pub DCBlength: DWORD,
        pub BaudRate: DWORD,
        pub flags: DWORD,
        pub wReserved: u16,
        pub XonLim: u16,
        pub XoffLim: u16,
        pub ByteSize: u8,
        pub Parity: u8,
        pub StopBits: u8,
        pub XonChar: u8,
        pub XoffChar: u8,
        pub ErrorChar: u8,
        pub EofChar: u8,
        pub EvtChar: u8,
        pub wReserved1: u16,
    }

    #[repr(C)]
    pub struct COMMTIMEOUTS {
        pub ReadIntervalTimeout: DWORD,
        pub ReadTotalTimeoutMultiplier: DWORD,
        pub ReadTotalTimeoutConstant: DWORD,
        pub WriteTotalTimeoutMultiplier: DWORD,
        pub WriteTotalTimeoutConstant: DWORD,
    }

    #[repr(C)]
    pub struct MSG {
        pub hwnd: HWND,
        pub message: UINT,
        pub wParam: WPARAM,
        pub lParam: LPARAM,
        pub time: DWORD,
        pub pt: POINT,
    }

    #[repr(C)]
    pub struct POINT { pub x: i32, pub y: i32 }

    extern "system" {
        pub fn CreateFileW(a: LPCWSTR, b: DWORD, c: DWORD, d: *mut c_void, e: DWORD, f: DWORD, g: HANDLE) -> HANDLE;
        pub fn ReadFile(h: HANDLE, buf: *mut u8, n: DWORD, got: *mut DWORD, o: *mut c_void) -> BOOL;
        pub fn WriteFile(h: HANDLE, buf: *const u8, n: DWORD, wrote: *mut DWORD, o: *mut c_void) -> BOOL;
        pub fn CloseHandle(h: HANDLE) -> BOOL;
        pub fn GetCommState(h: HANDLE, dcb: *mut DCB) -> BOOL;
        pub fn SetCommState(h: HANDLE, dcb: *mut DCB) -> BOOL;
        pub fn SetCommTimeouts(h: HANDLE, t: *const COMMTIMEOUTS) -> BOOL;
        pub fn GetCurrentThreadId() -> DWORD;
        pub fn PostThreadMessageW(tid: DWORD, msg: UINT, w: WPARAM, l: LPARAM) -> BOOL;
        pub fn RegisterClassW(wc: *const WNDCLASSW) -> u16;
        pub fn CreateWindowExW(ex: DWORD, cls: LPCWSTR, name: LPCWSTR, style: DWORD,
            x: i32, y: i32, w: i32, h: i32, parent: HWND, menu: HMENU, inst: HINSTANCE, param: *mut c_void) -> HWND;
        pub fn GetMessageW(m: *mut MSG, h: HWND, a: UINT, b: UINT) -> BOOL;
        pub fn TranslateMessage(m: *const MSG) -> BOOL;
        pub fn DispatchMessageW(m: *const MSG) -> LRESULT;
        pub fn LoadIconW(inst: HINSTANCE, name: LPCWSTR) -> HICON;
        pub fn CreateIconIndirect(info: *const ICONINFO_) -> HICON;
        pub fn DestroyIcon(h: HICON) -> BOOL;
        pub fn Shell_NotifyIconW(msg: DWORD, data: *mut NOTIFYICONDATAW) -> BOOL;
        pub fn GetCursorPos(p: *mut POINT) -> BOOL;
        pub fn CreatePopupMenu() -> HMENU;
        pub fn AppendMenuW(m: HMENU, flags: UINT, id: usize, text: LPCWSTR) -> BOOL;
        pub fn TrackPopupMenu(m: HMENU, flags: UINT, x: i32, y: i32, reserved: i32, hwnd: HWND, rect: *const c_void) -> BOOL;
        pub fn PostMessageW(h: HWND, msg: UINT, w: WPARAM, l: LPARAM) -> BOOL;
        pub fn DefWindowProcW(h: HWND, msg: UINT, w: WPARAM, l: LPARAM) -> LRESULT;
        // ole32：GUI 线程需要 STA COM（winit/eframe 事件循环依赖）
        pub fn CoInitializeEx(pv: *mut c_void, dw_co_init: u32) -> i32;
        // R2 daemon：DETACHED spawn 自身
        pub fn CreateProcessW(app: LPCWSTR, cmd: *mut u16, pa: *mut c_void, ta: *mut c_void,
            inherit: DWORD, flags: DWORD, env: *mut c_void, cwd: *const u16,
            si: *mut STARTUPINFOW, pi: *mut PROCESS_INFORMATION) -> BOOL;
    }
    pub const COINIT_APARTMENTTHREADED: u32 = 0x2;
    pub const RPC_E_CHANGED_MODE: i32 = -2147417850; // 0x80010106

    #[repr(C)]
    pub struct STARTUPINFOW {
        pub cb: DWORD,
        pub lpReserved: *mut u16,
        pub lpDesktop: *mut u16,
        pub lpTitle: *mut u16,
        pub dwX: DWORD, pub dwY: DWORD, pub dwXSize: DWORD, pub dwYSize: DWORD,
        pub dwXCountChars: DWORD, pub dwYCountChars: DWORD, pub dwFillAttribute: DWORD,
        pub dwFlags: DWORD,
        pub wShowWindow: u16, pub cbReserved2: u16,
        pub lpReserved2: *mut u8,
        pub hStdInput: HANDLE, pub hStdOutput: HANDLE, pub hStdError: HANDLE,
    }
    #[repr(C)]
    pub struct PROCESS_INFORMATION {
        pub hProcess: HANDLE,
        pub hThread: HANDLE,
        pub dwProcessId: DWORD,
        pub dwThreadId: DWORD,
    }

    #[repr(C)]
    pub struct WNDCLASSW {
        pub style: UINT,
        pub lpfnWndProc: WNDPROC,
        pub cbClsExtra: i32,
        pub cbWndExtra: i32,
        pub hInstance: HINSTANCE,
        pub hIcon: HICON,
        pub hCursor: *mut c_void,
        pub hbrBackground: HANDLE,
        pub lpszMenuName: LPCWSTR,
        pub lpszClassName: LPCWSTR,
    }
    pub type WNDPROC = Option<unsafe extern "system" fn(HWND, UINT, WPARAM, LPARAM) -> LRESULT>;

    #[repr(C)]
    pub struct ICONINFO_ {
        pub fIcon: BOOL,
        pub xHotspot: DWORD,
        pub yHotspot: DWORD,
        pub hbmMask: HANDLE,
        pub hbmColor: HANDLE,
    }
}

// ---------- 托盘实现（隐藏窗口 + Shell_NotifyIcon） ----------

#[cfg(target_os = "windows")]
const MENU_EXIT_ID: usize = 1001;
const MENU_SETTINGS_ID: usize = 1002;
const MENU_OPEN_CFG_ID: usize = 1003;
const SW_RESTORE: i32 = 9;
static SETTINGS_TEXT: [u16; 9] = [0x0053, 0x0065, 0x0074, 0x0074, 0x0069, 0x006E, 0x0067, 0x0073, 0]; // "Settings\0"

#[cfg(target_os = "windows")]
unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT {
    use winffi::*;
    match msg {
        WM_TRAYICON => {
            let evt = (lp & 0xffff) as UINT;
            // 托盘事件全部经 uCallbackMessage=WM_TRAYICON 到达（顶层 WM_LBUTTONDBLCLK 收不到托盘事件）
            #[cfg(all(target_os = "windows", feature = "gui"))]
            if evt == WM_LBUTTONDBLCLK {
                spawn_gui_thread();
            }
            if evt == WM_RBUTTONUP {
                let mut pt = POINT { x: 0, y: 0 };
                GetCursorPos(&mut pt);
                let m = CreatePopupMenu();
                AppendMenuW(m, 0, MENU_SETTINGS_ID, SETTINGS_TEXT.as_ptr());
                static OPEN_CFG_TEXT: [u16; 7] = [0x6253, 0x5F00, 0x914D, 0x7F6E, 0x76EE, 0x5F55, 0];
                AppendMenuW(m, 0, MENU_OPEN_CFG_ID, OPEN_CFG_TEXT.as_ptr());
                AppendMenuW(m, 0, MENU_EXIT_ID, EXIT_TEXT.as_ptr());
                SetForegroundWindow(hwnd);
                TrackPopupMenu(m, TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, std::ptr::null());
                DestroyMenu(m);
            }
            0
        }
        WM_COMMAND => {
            if wp as usize == MENU_EXIT_ID {
                SHUTDOWN.store(true, Ordering::Relaxed);
                PostMessageW(hwnd, WM_CLOSE, 0, 0);
            }
            #[cfg(feature = "gui")]
            if wp as usize == MENU_SETTINGS_ID {
                spawn_gui_thread();
            }
            if wp as usize == MENU_OPEN_CFG_ID {
                // explorer 打开 %TEMP%\pong（2026-11-18：工作目录已迁 temp——bridge.log/gui.err/pid/state 都在那）
                unsafe {
                    use std::os::windows::ffi::OsStrExt;
                    let dir = crate::core::config::tmp_dir();
                    let _ = fs::create_dir_all(&dir);
                    let mut w: Vec<u16> = dir.as_os_str().encode_wide().collect();
                    w.push(0);
                    static OPEN_VERB: [u16; 5] = [0x6F, 0x70, 0x65, 0x6E, 0]; // "open"
                    ShellExecuteW(0 as HWND, OPEN_VERB.as_ptr(), w.as_ptr(), std::ptr::null(), std::ptr::null(), 1);
                }
            }
            0
        }
        #[cfg(all(target_os = "windows", feature = "gui"))]
        WM_LBUTTONDBLCLK => {
            // 托盘双击 = 弹设置 GUI
            spawn_gui_thread();
            0
        }
        WM_CLOSE => { PostQuitMessage(0); 0 }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(target_os = "windows")]
static EXIT_TEXT: [u16; 5] = [0x9000, 0x51FA, 0, 0, 0]; // 退出

#[cfg(target_os = "windows")]
extern "system" {
    fn SetForegroundWindow(h: HWND) -> BOOL;
    fn DestroyMenu(m: HMENU) -> BOOL;
    fn PostQuitMessage(code: i32);
    fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(DWORD) -> BOOL>, add: BOOL) -> BOOL;
    fn GetCurrentProcessId() -> DWORD;
    fn CreateJobObjectW(lpJobAttributes: *mut std::ffi::c_void, lpName: *const u16) -> HANDLE;
    fn SetInformationJobObject(hJob: HANDLE, infoClass: i32, lpInfo: *mut std::ffi::c_void, cbInfo: u32) -> i32;
    fn AssignProcessToJobObject(hJob: HANDLE, hProcess: HANDLE) -> i32;
    fn GetCurrentProcess() -> HANDLE;
    fn ShellExecuteW(hwnd: HWND, op: *const u16, file: *const u16, params: *const u16, dir: *const u16, show: i32) -> HANDLE;
    fn FindWindowW(cls: *const u16, title: *const u16) -> HWND;
    fn ShowWindow(hwnd: HWND, cmd: i32) -> BOOL;
    fn CreateMutexW(lpMutexAttributes: *mut std::ffi::c_void, bInitialOwner: BOOL, lpName: *const u16) -> HANDLE;
    fn GetLastError() -> DWORD;
}

// 单实例互斥：named mutex 已存在（ERROR_ALREADY_EXISTS=183）则说明已有实例在跑，
// 干净退出（第二实例不注册托盘、不占端口——防"两个托盘图标/端口抢占"）。
#[cfg(target_os = "windows")]
const ERROR_ALREADY_EXISTS: u32 = 183;
#[cfg(target_os = "windows")]
pub fn enforce_single_instance() {
    unsafe {
        let name: &[u16] = &[
            b'L' as u16, b'o' as u16, b'c' as u16, b'a' as u16, b'l' as u16, 0x5C,
            b'p' as u16, b'o' as u16, b'n' as u16, b'g' as u16, b'-' as u16, b's' as u16,
            b'i' as u16, b'n' as u16, b'g' as u16, b'l' as u16, b'e' as u16, 0,
        ];
        let h = CreateMutexW(std::ptr::null_mut(), 0, name.as_ptr());
        if !h.is_null() && GetLastError() == ERROR_ALREADY_EXISTS {
            log("second instance detected (mutex exists) - exiting");
            std::process::exit(0);
        }
        // 句柄故意不 Close：持有到进程结束，锁才有效
    }
}

// 僵死修复③：Job Object——主进程退出时内核自动收割全部子进程（ssh.exe 不再孤儿残留）
#[cfg(target_os = "windows")]
const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;
#[cfg(target_os = "windows")]
const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_SIZE: usize = 144; // sizeof(JOBELE) x64
#[cfg(target_os = "windows")]
const JOBOBJECTINFOCLASS_EXTENDED: i32 = 9;
#[cfg(target_os = "windows")]
pub fn setup_job_object() {
    unsafe {
        let job = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null_mut());
        if job.is_null() { return; }
        // JOBOBJECT_EXTENDED_LIMIT_INFORMATION：BasicLimitInformation.LimitFlags 在偏移 16（x64）
        let mut info = [0u8; JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_SIZE];
        let flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE.to_ne_bytes();
        info[16..20].copy_from_slice(&flags);
        SetInformationJobObject(job, JOBOBJECTINFOCLASS_EXTENDED, info.as_mut_ptr().cast(), JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_SIZE as u32);
        AssignProcessToJobObject(job, GetCurrentProcess());
        // job 句柄故意不 Close：保持 KILL_ON_JOB_CLOSE 生效到进程结束
    }
}

const CTRL_C_EVENT: u32 = 0;
const CTRL_CLOSE_EVENT: u32 = 2;
const CTRL_BREAK_EVENT: u32 = 1;

unsafe extern "system" fn console_ctrl(ctrl: u32) -> i32 {
    match ctrl {
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT => {
            SHUTDOWN.store(true, Ordering::Relaxed);
            // 唤醒消息泵：向主线程投递 WM_QUIT
            let tid = MAIN_THREAD_ID.load(Ordering::Relaxed);
            if tid != 0 {
                unsafe extern "system" { fn PostThreadMessageW(tid: u32, m: u32, w: usize, l: isize) -> i32; }
                unsafe { PostThreadMessageW(tid, 0x0012, 0, 0); } // WM_QUIT
            }
            1 // TRUE：已处理该信号
        }
        _ => 0, // FALSE：走默认处理
    }
}

#[cfg(target_os = "windows")]
// ── R2 CLI daemon（参考 adb start-server/kill-server 模型）──
// start：DETACHED spawn 自身 --daemon-run 后立即返回（daemon 与终端解耦）
fn daemon_start() {
    if daemon_alive() {
        eprintln!("pong: already running");
        return;
    }
    let exe = std::env::current_exe().expect("exe");
    spawn_detached(&exe, &["--daemon-run"]);
    eprintln!("pong: started (daemon)");
}

fn daemon_kill() {
    match daemon_request("shutdown") {
        Some(true) => eprintln!("pong: killed"),
        Some(false) => eprintln!("pong: shutdown refused?"),
        None => eprintln!("pong: not running (no bridge port reply)"),
    }
}

fn daemon_status() {
    match daemon_alive() {
        true => eprintln!("pong: running"),
        false => eprintln!("pong: not running"),
    }
}

// daemon_request/daemon_alive 已上移 core::cli（R6：跨平台 CLI 语义层）
fn daemon_request(kind: &str) -> Option<bool> {
    crate::core::cli::daemon_request(kind)
}

fn daemon_alive() -> bool {
    crate::core::cli::daemon_alive()
}

// DETACHED_PROCESS spawn 自身（无控制台窗口，父终端退出不影响）
fn spawn_detached(exe: &std::path::Path, args: &[&str]) {
    use winffi::*;
    unsafe {
        let mut cmd: Vec<u16> = exe.as_os_str().to_string_lossy().encode_utf16().collect();
        cmd.push(0x20);
        for a in args {
            cmd.extend(a.encode_utf16());
            cmd.push(0x20);
        }
        cmd.push(0);
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let _ = CreateProcessW(std::ptr::null(), cmd.as_mut_ptr(), std::ptr::null_mut(),
            std::ptr::null_mut(), 0, 0x0000_0008 /*DETACHED_PROCESS*/, std::ptr::null_mut(),
            std::ptr::null(), &mut si, &mut pi);
    }
}

// --daemon-run：完整桥+隧道+work_loop（无托盘无 GUI），主线程轮询 SHUTDOWN 优雅收尾
fn daemon_run() {
    enforce_single_instance();
    let (_tcp_port, _worker, _tcp_worker) = run_core_services();
    log("daemon running (no tray)");
    loop {
        if SHUTDOWN.load(std::sync::atomic::Ordering::Relaxed) {
            log("daemon shutdown requested");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    std::process::exit(0);
}

/// R2 daemon 与托盘共用的核心服务初始化+启动；返回桥 tcp_port
fn run_core_services() -> (u16, std::thread::JoinHandle<()>, std::thread::JoinHandle<()>) {
    // exe 目录 + bridge.ini 配置一次性装载（所有相对路径的基准）
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let _ = EXE_DIR.set(parent.to_path_buf());
            let cfg = IniConfig::load(parent);
            let _ = CFG.set(cfg.clone());
            crate::core::config::init_runtime(cfg);
        }
    }
    // 默认工作目录 = Windows 用户 TEMP（agent 可用 kind=chdir 切到 samba 映射等处）
    if let Ok(tmp) = std::env::var("TEMP") {
        if std::env::set_current_dir(&tmp).is_ok() {
            log(&format!("cwd={tmp} (default TEMP)"));
        }
    }
    unsafe {
        MAIN_THREAD_ID.store(GetCurrentThreadId(), Ordering::Relaxed);
        SetConsoleCtrlHandler(Some(console_ctrl), 1);
    }

    let Some(dir) = find_work_dir() else {
        // 无目录：控制台说明原因后退出（GUI 下无窗口，git-bash/双击诊断可见）
        eprintln!("pong: bridge dir not found");
        eprintln!("  checked: WIN_BRIDGE_DIR, exe-side bridge.ini, exe-side in/out layout, Z:/Y:/X:/W:\\board-bridge");
        eprintln!("  fix: create bridge.ini next to pong.exe with one line = bridge dir path");
        std::process::exit(2);
    };
    for sub in ["in", "out", "done", "log"] {
        let _ = fs::create_dir_all(dir.join(sub));
    }
    // 运行时中间产物（日志/pid/serial log）落缓存目录：bridge.ini cache_dir= 可配；
    // 未配的缺省 = %TEMP%\pong（临时产物落临时区）；创建失败退回桥目录 log/（原因打 stderr）
    let cache_dir: PathBuf = {
        let cand = CFG.get().map(|c| resolve(&c.cache_dir)).unwrap_or_else(|| {
            std::env::var("TEMP").ok()
                .map(|t| PathBuf::from(t).join("pong"))
                .unwrap_or_else(|| dir.join("log"))
        });
        match fs::create_dir_all(&cand) {
            Ok(_) => cand,
            Err(e) => {
                eprintln!("cache_dir {} unavailable: {e}", cand.display());
                dir.join("log")
            }
        }
    };
    let _ = LOG_PATH.set(cache_dir.join("bridge.log"));
    let _ = SERIAL_CACHE.set(cache_dir.join("serial"));
    // pid 文件：外部 taskkill /PID 或 kill $(cat bridge.pid) 退出通道；用户 temp（用户级单例）
    let _ = fs::create_dir_all(std::env::temp_dir().join("pong"));
    let _ = fs::write(crate::core::config::pid_path(), format!("{}\n", unsafe { GetCurrentProcessId() }));
    log("start");

    // TCP 桥端口：bridge.ini port=（默认 7101）
    let tcp_port: u16 = CFG.get().map(|c| c.port).unwrap_or(7101);
    let bridge_shared = Arc::new(Bridge { dir: dir.clone() });
    let worker = {
        let bridge = bridge_shared.clone();
        std::thread::spawn(move || work_loop(bridge))
    };
    let tcp_worker = {
        let bridge = bridge_shared.clone();
        std::thread::spawn(move || tcp_loop(bridge, tcp_port))
    };
    // 本地控制面（外审：CLI kill/status 走 port+1，不经远程可达的桥端口）
    let _ctl_worker = {
        let bridge = bridge_shared.clone();
        std::thread::spawn(move || crate::core::protocol::ctl_loop(bridge, tcp_port))
    };
    // ssh -R 隧道：内置 russh 客户端，把 Linux 的 127.0.0.1:<tcp_port> 反向绑到本机 <tcp_port>
    // 断线 5 秒重连；ssh_key/ssh_user/ssh_port 从 pong.ini 读
    {
        let port = tcp_port;
        std::thread::spawn(move || {
            // 专用 tokio runtime：隧道任务独占一条线程，桥循环线程不受影响
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all().build().expect("tokio runtime");
            // GUI 面板命令通道（probe/ladd/ldel/list）：tx 进 static 供 GUI 同进程直发（2026-11-18）
            let (gtx, grx) = tokio::sync::mpsc::channel::<crate::core::tunnel::ControlCmd>(16);
            let _ = crate::core::tunnel::GUI_CMD_TX.set(gtx);
            let locals: crate::core::tunnel::Locals = Default::default();
            // 配置/key 由 tunnel_forever 每轮自读自愈（热生效：Save/Reload/后放 key 均自动加载）
            rt.block_on(async move {
                crate::core::tunnel::tunnel_forever(port, Some(grx), locals).await;
            });
        });
    }
    (tcp_port, worker, tcp_worker)
}

pub fn main_impl(mode: Option<String>) {
    use winffi::*;
    // R2 CLI dispatch: start / kill / status; default = tray GUI
    match mode.as_deref() {
        Some("start") => { daemon_start(); return; }
        Some("kill") => { daemon_kill(); return; }
        Some("status") => { daemon_status(); return; }
        Some("--daemon-run") => { daemon_run(); return; }
        _ => {}
    }
    // 条件编译（用户架构）：cli 包装=无参即前台 daemon（纯 CLI exe 无托盘）；gui 包装=托盘
    #[cfg(not(feature = "gui"))]
    { daemon_run(); }
    #[cfg(feature = "gui")]
    enforce_single_instance();
    setup_job_object();
    // 核心服务（配置/工作目录/cache/pid/work_loop/tcp_loop/隧道）——daemon 与托盘共用
    let (_tcp_port, worker, tcp_worker) = run_core_services();

    unsafe {
        // 注册窗口类 + 隐藏消息窗口
        let cls_name: Vec<u16> = "win_bridge_cls\0".encode_utf16().collect();
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wnd_proc);
        wc.lpszClassName = cls_name.as_ptr();
        wc.hInstance = std::mem::zeroed(); // NULL = 当前模块
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(0, cls_name.as_ptr(), cls_name.as_ptr(), 0,
            0, 0, 0, 0, std::ptr::null_mut(), std::ptr::null_mut(), std::mem::zeroed(), std::ptr::null_mut());

        // 32x32 图标：内存位图 + CreateIconIndirect（简化：用 LoadIconW 默认图标）
        let icon = LoadIconW(std::ptr::null_mut(), IDI_APPLICATION);

        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_TRAYICON;
        nid.hIcon = icon;
        let tip: Vec<u16> = "pong\0".encode_utf16().collect();
        for (i, c) in tip.iter().enumerate() { if i < 127 { nid.szTip[i] = *c; } }
        Shell_NotifyIconW(NIM_ADD, &mut nid);

        // 消息泵
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // 退出：删托盘
        Shell_NotifyIconW(NIM_DELETE, &mut nid);
    }

    SHUTDOWN.store(true, Ordering::Relaxed);
    log("exit");
    let _ = fs::remove_file(crate::core::config::pid_path());
    let _ = worker.join();
    let _ = tcp_worker.join();
    std::process::exit(0);
}
