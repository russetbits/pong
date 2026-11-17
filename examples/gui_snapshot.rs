// GUI 设计快照（无显示环境预览）：跑一帧 PongApp 的 egui 绘制，导出 mesh+纹理图集 JSON，
// 由 tools/gui_render.py 软件光栅化成 PNG（cairosvg 路线画不了文本——文本是 atlas 纹理贴图，必须按 uv 采样）。
// 开发闭环：改 UI → cargo run --example gui_snapshot → python gui_render.py → 看 PNG。
use pong::gui::PongApp;
use eframe::egui;
use eframe::epaint::Primitive;

fn main() {
    let ctx = egui::Context::default();
    // 中文字体（Linux 用工作区缓存的 NotoSansSC；Windows 真机用微软雅黑）
    {
        let cjk_path = if cfg!(target_os = "windows") {
            "C:\\Windows\\Fonts\\msyh.ttc".to_string()
        } else {
            std::env::var("PONG_FONT").unwrap_or_else(|_| "C:/Windows/Fonts/msyh.ttc".to_string())
        };
        if let Ok(data) = std::fs::read(&cjk_path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert("cjk".into(), egui::FontData::from_owned(data).into());
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                if let Some(list) = fonts.families.get_mut(&family) {
                    list.push("cjk".into());
                }
            }
            ctx.set_fonts(fonts);
        }
    }
    let screen = egui::vec2(640.0, 480.0);

    pong::core::tunnel::TUNNEL_ONLINE.store(true, std::sync::atomic::Ordering::Relaxed);
    pong::core::tunnel::TUNNEL_ENABLED.store(true, std::sync::atomic::Ordering::Relaxed);

    let mut app = PongApp::new_for_snapshot();
    // AD_SNAPSHOT_CONNECT=1：预览 connect 弹窗视图
    let connect_mode = std::env::var("AD_SNAPSHOT_CONNECT").is_ok();
    if connect_mode {
        app.view = pong::gui::View::Connect;
    }
    let mut input = egui::RawInput::default();
    input.viewport_id = egui::ViewportId::ROOT;
    input.screen_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen));
    input.viewports.insert(
        egui::ViewportId::ROOT,
        egui::ViewportInfo {
            title: Some("pong settings".into()),
            ..Default::default()
        },
    );

    // egui 需要两帧才出文本（第一帧布局测量、第二帧才产生 glyph mesh）
    // 字体图集在第一帧 textures_delta 里（第二帧增量不带）——纹理取第一帧，mesh 取第二帧
    let first = ctx.run(input.clone(), |ctx| {
        eframe::App::update(&mut app, ctx, &mut eframe::Frame::_new_kittest())
    });
    let second = ctx.run(input, |ctx| {
        eframe::App::update(&mut app, ctx, &mut eframe::Frame::_new_kittest())
    });
    let shapes = second.shapes;
    let textures_src = &first.textures_delta;
    let tess = ctx.tessellate(shapes, 1.0);

    // 纹理图集（字体等）: textures_delta.set 里拿出图像（Color32 pixels → hex rgba 字节流）
    let mut textures = serde_json::Map::new();
    for (id, img_data) in &textures_src.set {
        let egui::ImageData::Color(cimg) = &img_data.image else { continue };
        let (w, h) = (cimg.width(), cimg.height());
        let mut buf = String::with_capacity(w * h * 8);
        for p in cimg.pixels.iter() {
            buf.push_str(&format!("{:02x}{:02x}{:02x}{:02x}", p.r(), p.g(), p.b(), p.a()));
        }
        textures.insert(format!("{:?}", id), serde_json::json!({ "w": w, "h": h, "rgba_hex": buf }));
    }

    let mut prims = Vec::new();
    for clip in &tess {
        let Primitive::Mesh(mesh) = &clip.primitive else { continue };
        let cr = clip.clip_rect;
        prims.push(serde_json::json!({
            "clip": [cr.min.x, cr.min.y, cr.max.x, cr.max.y],
            "verts": mesh.vertices.iter().map(|v| serde_json::json!({
                "x": v.pos.x, "y": v.pos.y,
                "u": v.uv.x, "v": v.uv.y,
                "c": [v.color.r(), v.color.g(), v.color.b(), v.color.a()]
            })).collect::<Vec<_>>(),
            "idx": mesh.indices.iter().map(|i| *i as u32).collect::<Vec<_>>()
        }));
    }

    let doc = serde_json::json!({ "w": 640, "h": 480, "textures": textures, "prims": prims });
    let out = if connect_mode { "gui_snapshot_connect.json" } else { "gui_snapshot.json" };
    std::fs::write(out, serde_json::to_string(&doc).unwrap()).unwrap();
    println!("JSON written: {out}");
}
