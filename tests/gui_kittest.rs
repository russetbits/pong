// GUI 快照（egui_kittest headless 官方测试框架）：fonts/glyph/atlas 全链路自动处理。
// 运行：cargo test --features gui --test gui_kittest
// 快照输出：target/snapshots/*（Cargo.toml 里 snapshot 路径规则）——当前直接落 CWD：snapshots/
use pong::gui::PongApp;
use egui_kittest::Harness;

#[test]
fn gui_snapshot_layout() {
    pong::core::tunnel::TUNNEL_ONLINE.store(true, std::sync::atomic::Ordering::Relaxed);
    pong::core::tunnel::TUNNEL_ENABLED.store(true, std::sync::atomic::Ordering::Relaxed);

    let mut harness = Harness::builder()
        .with_size(egui::vec2(640.0, 480.0))
        .build_eframe(|_cc| PongApp::new_for_snapshot());
    harness.run(); // 默认 2 帧 warm-up
    harness.snapshot("pong_settings");
    // snapshot feature 开启时写文件并人工比对；未开启时仅无操作
}
