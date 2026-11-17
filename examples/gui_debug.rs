// 最小实验2：加 native_pixels_per_point=Some(1.0) 看是否破坏 glyph
use eframe::egui;
use eframe::epaint::Shape;

fn main() {
    let ctx = egui::Context::default();
    let screen = egui::vec2(320.0, 200.0);
    let mut input = egui::RawInput::default();
    input.viewport_id = egui::ViewportId::ROOT;
    input.screen_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen));
    input.viewports.insert(egui::ViewportId::ROOT, egui::ViewportInfo {
        native_pixels_per_point: Some(1.0),
        title: Some("t".into()),
        ..Default::default()
    });
    let _ = ctx.run(input.clone(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| { ui.label("hello world"); });
    });
    let second = ctx.run(input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| { ui.label("hello world"); });
    });
    for cs in &second.shapes {
        if let Shape::Text(t) = &cs.shape {
            for glyph in t.galley.rows.iter().flat_map(|r| r.glyphs.iter()).take(3) {
                println!("glyph {:?} uv_min={:?}", glyph.chr, glyph.uv_rect.min);
            }
        }
    }
}
