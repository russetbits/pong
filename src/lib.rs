// pong lib 门面（examples/集成测试用；bin 仍是 src/main.rs）
#[cfg(target_os = "windows")]
mod win;
#[cfg(not(target_os = "windows"))]
mod lin;
pub mod core;
#[cfg(feature = "gui")]
pub mod gui;
