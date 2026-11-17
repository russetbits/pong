// win/gui_glue.rs —— GUI 包装粘合层（自 main_impl.rs 拆出，2026-11-16 src 树整理）：
// 托盘/GUI exe 对 eframe 窗口的唤醒、退出请求、GUI 线程 spawn。
// 条件编译（用户架构）：CLI exe（feature cli，无 gui）用空实现；真身在 feature = "gui" 下。

// 自带最小 FFI（不依赖 main_impl 内部块的组织）
#[cfg(all(target_os = "windows", feature = "gui"))]
mod gffi {
    pub type HWND = *mut core::ffi::c_void;
    pub type BOOL = i32;
    #[link(name = "user32")]
    extern "system" {
        pub fn FindWindowW(cls: *const u16, title: *const u16) -> HWND;
        pub fn ShowWindow(hwnd: HWND, cmd: i32) -> BOOL;
        pub fn SetForegroundWindow(h: HWND) -> BOOL;
    }
    pub const SW_RESTORE: i32 = 9;
}

/// ShowWindow 唤醒隐藏的 eframe 窗口（绕开 egui 帧循环——隐藏窗口时 eframe 可能停止 update）
#[cfg(all(target_os = "windows", feature = "gui"))]
pub fn show_gui_window() {
    use gffi::*;
    unsafe {
        // winit 窗口标题 = viewport builder 的 with_title
        // 标题=gui.rs run_gui with_title（2026-11-18 同步为 pong——旧值 pong settings 致 FindWindowW miss，托盘唤醒失效实锤）
        let title: Vec<u16> = "pong\0".encode_utf16().collect();
        let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
        if !hwnd.is_null() {
            ShowWindow(hwnd, SW_RESTORE); // SW_SHOW 对 hidden 也有效，SW_RESTORE 顺带还原最小化
            SetForegroundWindow(hwnd);
            crate::core::log::log("gui: window shown via ShowWindow");
        } else {
            crate::core::log::log("gui: FindWindowW miss (pong)");
        }
    }
}

#[cfg(all(target_os = "windows", not(feature = "gui")))]
pub fn show_gui_window() {}

/// GUI Exit 按钮 -> 走托盘既有 Exit 路径：SHUTDOWN、停桥、退出
#[cfg(all(target_os = "windows", feature = "gui"))]
pub fn request_app_exit() {
    unsafe {
        // SHUTDOWN + 清 pidfile（原版 process::exit 跳过 main_impl 尾部清理→pid 残留实锤 2026-11-18）+退出
        crate::core::protocol::SHUTDOWN.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = std::fs::remove_file(crate::core::config::pid_path());
        std::process::exit(0);
    }
}

#[cfg(all(target_os = "windows", not(feature = "gui")))]
pub fn request_app_exit() {}

/// GUI 线程常驻：EventLoop 不能二次创建（winit "EventLoop can't be recreated"）。
/// 已在跑 → 只发 show 请求（窗口从隐藏态重显）；未在跑 → spawn 常驻线程。
#[cfg(all(target_os = "windows", feature = "gui"))]
pub fn spawn_gui_thread() {
    use super::main_impl::winffi::*;
    if crate::gui::GUI_ALIVE.load(std::sync::atomic::Ordering::Relaxed) {
        // 双保险：①egui 帧级 show（隐藏态下 eframe 可能不再调 update，此路不一定活）
        crate::gui::request_gui_show();
        // ②Win32 直接 ShowWindow 找到 winit 窗口拉回前台（不依赖 egui 帧循环）
        show_gui_window();
        return;
    }
    std::thread::spawn(|| {
        unsafe {
            let hr = CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED);
            crate::core::log::log(&format!("gui thread COM init hr={hr:#x} (changed-mode is ok)"));
            let _ = RPC_E_CHANGED_MODE; // 已初始化为 MTA 时忽略——eframe 自行处理
        }
        crate::core::log::log("gui thread starting eframe");
        // run_gui 的 catch_unwind/日志内聚在 gui 模块（CLI 桩与真实 GUI 各自实现，签名统一 ()——
        // 原 match 写法对桩（返回 ()）与真 GUI（Result）不兼容，CLI bin 曾因此编不过）
        crate::gui::run_gui_logged();
    });
}

#[cfg(all(target_os = "windows", not(feature = "gui")))]
pub fn spawn_gui_thread() {}
