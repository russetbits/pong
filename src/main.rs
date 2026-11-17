// pong —— Windows 侧命令桥（agent door）
// 远程 agent 经 ssh 反向隧道在 127.0.0.1:<port> 下发 JSON 行命令（exec/adb/serial/read/write/...），
// 本程序在 Windows 侧执行并回写 JSON 行结果。托盘常驻 + GUI（Settings 菜单）。
//
// 模块布局：
//   core/    跨平台：协议/文件操作/命令执行/配置/日志/隧道
//   win/     Windows 专属：Win32 FFI/托盘/Job Object/串口 COM
//   lin/     Linux 本地 door（留待将来：给远程 agent 调用）
// Windows 下不弹控制台黑窗（托盘程序；日志全落 bridge.log）。console 调试版需注释掉此行重编。
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

mod core;
#[cfg(target_os = "windows")]
mod win;
#[cfg(not(target_os = "windows"))]
mod lin;
#[cfg(feature = "gui")]
mod gui;

fn main() {
    // GUI/托盘 release 无控制台——panic 落盘（temp/pong/log/gui.err）否则闪退无取证（2026-11-17 用户实测弹窗闪退）
    {
        let logdir = core::config::tmp_dir().join("log");
        let _ = std::fs::create_dir_all(&logdir);
        let path = logdir.join("gui.err");
        std::panic::set_hook(Box::new(move |info| {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                let _ = writeln!(f, "[{now}] {info}");
            }
        }));
    }
    // R2 CLI：pong start（后台 daemon）/ kill / status；无参 = 托盘 GUI 全量
    let mode = std::env::args().nth(1);
    #[cfg(target_os = "windows")]
    win::main_impl::main_impl(mode);
    #[cfg(not(target_os = "windows"))]
    lin::main_impl::main_impl(mode);
}
