// pong GUI（egui/eframe）：功能界面（2026-11-18 用户定案——配置界面删除）
// 主界面=Connect 按钮 + 状态灯；connect 弹窗=ssh config 检测 hosts 列表 + 手输 user@host/可选密码；
// 连接后=host 服务端口面板（probe 枚举 127.0.0.1 监听 → 映射/取消映射 -L），CLI 同款 pong map --list。
use std::sync::atomic::Ordering;
use eframe::egui;
use crate::core::tunnel::{TUNNEL_ONLINE, TUNNEL_ENABLED};

#[derive(PartialEq, Clone, Copy)]
pub enum View { Main, Connect }

pub struct PongApp {
    pub view: View,
    pub status: String,
    status_err: bool,
    // connect 弹窗输入缓冲
    pub target_input: String,   // user@host
    pub password_input: String,
    pub ssh_hosts: Vec<(String, String, u16)>, // (pattern, user, port)——弹窗打开时刷新
    // 端口面板数据（Refresh 时拉取）
    pub probe_ports: Vec<crate::core::tunnel::PortInfo>,
    pub mapped_locals: Vec<u16>, // 已建 -L 的 client 本地端口
    pub probe_err: String,
    /// 窗口可见状态（自维护：egui input 读不到隐藏态，保活泵用）
    shown: bool,
}

/// GUI 线程是否已在跑（eframe 常驻：窗口关闭=hide 不退出，Settings/双击=重新 show）
pub static GUI_ALIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// 全局 show 请求（托盘线程置位 → GUI 线程每帧检查）
pub static GUI_SHOW_REQ: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// GUI 的 egui::Context 句柄（创建后全局可用；托盘线程用它唤醒 repaint 消费 show 请求）
static GUI_CTX: std::sync::OnceLock<egui::Context> = std::sync::OnceLock::new();

/// 托盘侧调用：置 show 请求并唤醒 GUI 帧循环（窗口隐藏时 eframe 不自动 repaint，必须显式唤醒）
pub fn request_gui_show() {
    GUI_SHOW_REQ.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(ctx) = GUI_CTX.get() {
        ctx.request_repaint();
    }
}

impl PongApp {
    pub fn new() -> Self {
        Self { view: View::Main, status: String::new(), status_err: false,
            target_input: String::new(), password_input: String::new(), ssh_hosts: Vec::new(),
            probe_ports: Vec::new(), mapped_locals: Vec::new(), probe_err: String::new(), shown: true }
    }
    // 快照预览专用（无真实配置依赖）
    pub fn new_for_snapshot() -> Self {
        Self { view: View::Main, status: String::new(), status_err: false,
            target_input: String::new(), password_input: String::new(), ssh_hosts: Vec::new(),
            probe_ports: Vec::new(), mapped_locals: Vec::new(), probe_err: String::new(), shown: true }
    }

    /// update 帧头调用：X 关闭请求 → 隐藏窗口；全局 show 请求 → 重新显示
    fn handle_show_request(&mut self, ctx: &egui::Context) {
        // egui 0.33：点 X 会置 viewport 的 close_requested；CancelClose 拦住退出 + 直接隐藏
        let user_close = ctx.input(|i| i.viewport().close_requested());
        // ESC 键与 X 等价：隐藏窗口（EventLoop 常驻，托盘可再弹）
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if user_close || esc {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.shown = false;
            crate::core::log::log("gui: window hidden (close button)");
        }
        if GUI_SHOW_REQ.swap(false, std::sync::atomic::Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.shown = true;
            crate::core::log::log("gui: window re-shown");
        }
        // 保活泵：窗口隐藏后 eframe 不再自动 repaint（show 请求将永远无人消费）——
        // 隐藏态下低频定时唤醒保持 update 循环活着，直到重新显示
        if !self.shown {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
    }

    /// 发起连接（connect 弹窗确认 / ssh config host 点选共用）
    fn do_connect(&mut self, t: crate::core::sshconf::SshTarget, pw: Option<String>) {
        // 防重入（2026-11-18 用户实锤）：connecting 中重复点 Connect 覆盖目标会造成状态错乱——
        // 已 enabled 且未 online 时忽略新 connect（提示先 Disconnect）；已 online 时按钮已是 Disconnect 不会到这
        let enabled = TUNNEL_ENABLED.load(Ordering::Relaxed);
        let online = TUNNEL_ONLINE.load(Ordering::Relaxed);
        if enabled && !online {
            self.status = "already connecting - Disconnect first".to_string();
            self.status_err = true;
            self.view = View::Main;
            return;
        }
        crate::core::tunnel::set_gui_connect(t, pw);
        TUNNEL_ENABLED.store(true, Ordering::Relaxed);
        self.view = View::Main;
        self.status = "connecting...".into();
        self.status_err = false;
    }

    /// 拉取 host 端口 + 已映射表（GUI 线程阻塞调用——probe 为独立 ssh 连接，秒级）
    fn refresh_ports(&mut self) {
        match crate::core::tunnel::gui_probe() {
            Ok(ports) => {
                self.probe_ports = ports;
                self.probe_err.clear();
            }
            Err(e) => { self.probe_err = format!("probe: {e}"); self.probe_ports.clear(); }
        }
        self.mapped_locals = crate::core::tunnel::gui_locals().into_iter().map(|(p, _)| p).collect();
    }
}

impl eframe::App for PongApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_show_request(ctx);
        // 顶栏：状态灯
        egui::TopBottomPanel::top("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let online = TUNNEL_ONLINE.load(Ordering::Relaxed);
                let enabled = TUNNEL_ENABLED.load(Ordering::Relaxed);
                let last_err = crate::core::tunnel::TUNNEL_LAST_ERR.lock().map(|g| g.clone()).unwrap_or_default();
                let (color, word) = if !enabled {
                    (egui::Color32::from_rgb(0xF4, 0x43, 0x36), "tunnel stopped".to_string())
                } else if online {
                    (egui::Color32::from_rgb(0x4C, 0xAF, 0x50), "tunnel online".to_string())
                } else if !last_err.is_empty() {
                    // 顶栏只留短词（2026-11-18 二次修正：60 字符在窄窗仍挤掉 host 指纹）——
                    // 详细错误进底栏 status（随窗口宽度截）+顶栏 hover 看全文
                    (egui::Color32::from_rgb(0xE5, 0x39, 0x35), "connect failed".to_string())
                } else {
                    (egui::Color32::from_rgb(0xFF, 0xC1, 0x07), "tunnel connecting...".to_string())
                };
                let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 6.0, color);
                if !last_err.is_empty() && !online {
                    ui.label(word).on_hover_text(format!("connect failed: {last_err}")); // 全文 hover
                } else {
                    ui.label(word);
                }
                // TOFU 指纹（外审 P1-4：静默 TOFU 至少要可见）
                let hk = crate::core::config::host_key_path();
                if let Ok(fp) = std::fs::read_to_string(&hk) {
                    let fp = fp.trim();
                    if !fp.is_empty() {
                        ui.separator();
                        ui.label(egui::RichText::new(format!("host {}", &fp[..fp.len().min(20)])).weak().small())
                            .on_hover_text(format!("sshd 主机指纹（TOFU 首连记录，全文={}）\n若与服务器实际指纹不符：删除 exe 旁 host_key.dat 重置信任", fp));
                    }
                }
            });
        });
        // 底栏：状态文本 + Exit
        egui::TopBottomPanel::bottom("action_bar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button(egui::RichText::new("Exit").color(egui::Color32::from_rgb(0xE5, 0x39, 0x35))).clicked() {
                    #[cfg(target_os = "windows")]
                    crate::win::gui_glue::request_app_exit();
                    #[cfg(not(target_os = "windows"))]
                    crate::core::protocol::SHUTDOWN.store(true, std::sync::atomic::Ordering::Relaxed);
                    self.status = "exiting...".to_string();
                }
                // 底栏状态：操作回执优先；无回执时显示隧道错误详情（长文本放底栏不挤顶栏指纹）
                let last_err_bottom = if self.status.is_empty() {
                    let le = crate::core::tunnel::TUNNEL_LAST_ERR.lock().map(|g| g.clone()).unwrap_or_default();
                    // 仅连接尝试中显示（enabled&&未 online）——Disconnect 后不再挂旧错误
                    if !le.is_empty() && !TUNNEL_ONLINE.load(Ordering::Relaxed) && TUNNEL_ENABLED.load(Ordering::Relaxed) { format!("connect failed: {le}") } else { String::new() }
                } else { String::new() };
                let shown: (&str, bool) = if !self.status.is_empty() { (self.status.as_str(), self.status_err) } else if !last_err_bottom.is_empty() { (last_err_bottom.as_str(), true) } else { ("", false) };
                if !shown.0.is_empty() {
                    let c = if shown.1 { egui::Color32::from_rgb(0xE5, 0x39, 0x35) } else { egui::Color32::GRAY };
                    // 底栏宽度自适应：长文本让 ScrollArea 横向滚（避免换行顶高）
                    egui::ScrollArea::horizontal().show(ui, |ui| {
                        ui.label(egui::RichText::new(shown.0).color(c));
                    });
                }
            });
            ui.add_space(4.0);
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.view {
                View::Main => {
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        // 按钮状态机（2026-11-18 用户需求：暂时不能点的灰掉）：
                        // online→Disconnect（可点）；connecting(enabled&&!online)→灰 Connect 显示 Connecting...（Disconnect 仍可点打断）；停用→Connect
                        let online_now = TUNNEL_ONLINE.load(Ordering::Relaxed);
                        let enabled_now = TUNNEL_ENABLED.load(Ordering::Relaxed);
                        if enabled_now {
                            // Disconnect（connecting 中也可点=打断当前连接尝试）
                            let label = if online_now { "Disconnect" } else { "Stop connecting" };
                            let btn = egui::Button::new(egui::RichText::new(label).size(22.0)).min_size(egui::vec2(140.0, 44.0));
                            if ui.add(btn).clicked() {
                                // 断开隧道但进程不退：置 TUNNEL_ENABLED=false（循环不再重连）+ 发 Disconnect 打断当前会话
                                TUNNEL_ENABLED.store(false, Ordering::Relaxed);
                                if let Some(tx) = crate::core::tunnel::GUI_CMD_TX.get() {
                                    let _ = tx.blocking_send(crate::core::tunnel::ControlCmd::Disconnect);
                                }
                                self.status = "disconnected".to_string();
                                self.probe_ports.clear();
                                self.mapped_locals.clear();
                            }
                        } else {
                            let btn = egui::Button::new(egui::RichText::new("Connect").size(22.0)).min_size(egui::vec2(140.0, 44.0));
                            if ui.add(btn).clicked() {
                                // 弹窗打开：刷新 ssh config hosts 列表
                                self.ssh_hosts = crate::core::sshconf::list_hosts().into_iter()
                                    .map(|h| (h.pattern, h.user.unwrap_or_default(), h.port)).collect();
                                self.target_input.clear();
                                self.password_input.clear();
                                self.view = View::Connect;
                            }
                        }
                        ui.add_space(12.0);
                        ui.vertical(|ui| {
                            // Repick/Refresh 需隧道在线（离线时灰掉——2026-11-18 用户需求）
                            if ui.add_enabled(online_now, egui::Button::new("Repick"))
                                .on_disabled_hover_text("tunnel must be online").on_hover_text("regenerate port + pair code").clicked() {
                                self.status = match repick_via_cmd() {
                                    Ok((p, pair)) => { self.status_err = false; format!("port={p} pair={pair}") }
                                    Err(e) => { self.status_err = true; format!("repick failed: {e}") }
                                };
                            }
                            if ui.add_enabled(online_now, egui::Button::new("Refresh ports"))
                                .on_disabled_hover_text("tunnel must be online").clicked() {
                                self.refresh_ports();
                            }
                        });
                    });
                    ui.add_space(10.0);
                    // 端口面板（probe 后展示；每行 map/unmap）
                    ui.separator();
                    ui.heading("host service ports (127.0.0.1)");
                    if !self.probe_err.is_empty() {
                        ui.label(egui::RichText::new(&self.probe_err).color(egui::Color32::from_rgb(0xE5, 0x39, 0x35)).small());
                    }
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        let mapped = self.mapped_locals.clone();
                        let ports = self.probe_ports.clone();
                        for pi in &ports {
                            let p = pi.port;
                            let online_row = TUNNEL_ONLINE.load(Ordering::Relaxed);
                            ui.horizontal(|ui| {
                                ui.label(format!("127.0.0.1:{p}"));
                                // 进程名列（VSCode Ports 面板形态）——截 28 字符防挤
                                let pname = if pi.proc_name.is_empty() { "(unknown)".to_string() } else { pi.proc_name.clone() };
                                let short = if pname.chars().count() > 28 { let c: String = pname.chars().take(28).collect(); format!("{c}…") } else { pname };
                                ui.label(egui::RichText::new(short).weak().small());
                                if mapped.contains(&p) {
                                    if ui.add_enabled(online_row, egui::Button::new("unmap")).on_disabled_hover_text("tunnel must be online").clicked() {
                                        match crate::core::tunnel::gui_ldel(p) {
                                            Ok(()) => { self.status = format!("unmapped {p}"); self.status_err = false; }
                                            Err(e) => { self.status = format!("unmap failed: {e}"); self.status_err = true; }
                                        }
                                        self.refresh_ports();
                                    }
                                    ui.label(egui::RichText::new("mapped").color(egui::Color32::from_rgb(0x4C, 0xAF, 0x50)).small());
                                } else {
                                    if ui.add_enabled(online_row, egui::Button::new("map")).on_disabled_hover_text("tunnel must be online").clicked() {
                                        match crate::core::tunnel::gui_ladd(p) {
                                            Ok(()) => { self.status = format!("mapped client:{p} -> host:{p}"); self.status_err = false; }
                                            Err(e) => { self.status = format!("map failed: {e}"); self.status_err = true; }
                                        }
                                        self.refresh_ports();
                                    }
                                    ui.label(egui::RichText::new("can map").weak().small());
                                }
                            });
                        }
                        if self.probe_ports.is_empty() && self.probe_err.is_empty() {
                            ui.label(egui::RichText::new("(click Refresh ports; tunnel must be online)").weak());
                        }
                    });
                }
                View::Connect => {
                    ui.add_space(8.0);
                    ui.heading("Connect to host");
                    ui.separator();
                    // ① ssh config 检测 hosts（点选即连）
                    ui.label(egui::RichText::new("from ~/.ssh/config:").weak());
                    if self.ssh_hosts.is_empty() {
                        ui.label(egui::RichText::new("(no hosts found)").weak());
                    }
                    egui::ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                        let hosts = self.ssh_hosts.clone();
                        for (pat, user, port) in &hosts {
                            let label = if user.is_empty() { format!("{pat} (port {port})") } else { format!("{user}@{pat} (port {port})") };
                            if ui.selectable_label(false, &label).clicked() {
                                match crate::core::sshconf::lookup(pat) {
                                    Ok(t) => self.do_connect(t, None),
                                    Err(e) => { self.status = format!("lookup failed: {e}"); self.status_err = true; }
                                }
                            }
                        }
                    });
                    ui.add_space(8.0);
                    ui.separator();
                    // ② 手输 user@host + 可选密码
                    ui.label(egui::RichText::new("manual:").weak());
                    egui::Grid::new("conn_grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                        ui.label("user@host");
                        ui.add(egui::TextEdit::singleline(&mut self.target_input).desired_width(260.0).hint_text("user@host"));
                        ui.end_row();
                        ui.label("password");
                        ui.add(egui::TextEdit::singleline(&mut self.password_input).password(true).desired_width(260.0).hint_text("(optional - key/agent tried first)"));
                        ui.end_row();
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Connect").clicked() {
                            let tgt = self.target_input.trim().to_string();
                            match crate::core::sshconf::lookup(&tgt) {
                                Ok(t) => {
                                    let pw = if self.password_input.is_empty() { None } else { Some(self.password_input.clone()) };
                                    self.do_connect(t, pw);
                                }
                                Err(e) => { self.status = format!("{e}"); self.status_err = true; }
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            self.view = View::Main;
                        }
                    });
                }
            }
        });
    }
}

/// GUI 侧 repick：经 GUI_CMD_TX 发 Repick；pair 轮换与端口重选在 tunnel 循环侧
fn repick_via_cmd() -> Result<(u16, String), String> {
    let tx = crate::core::tunnel::GUI_CMD_TX.get().ok_or_else(|| "tunnel not running".to_string())?;
    let (otx, orx) = tokio::sync::oneshot::channel();
    let new_pair = crate::core::auth::gen_pair().unwrap_or_default();
    tx.blocking_send(crate::core::tunnel::ControlCmd::Repick { new_pair, reply: otx }).map_err(|e| e.to_string())?;
    let port = orx.blocking_recv().map_err(|e| e.to_string())??;
    // pair 已由 select 分支 set_pair（GUI/CLI 同路径）——读回展示
    let pair = crate::core::auth::pair().unwrap_or_default();
    Ok((port, pair))
}

// 中文字体：Windows 用微软雅黑，Linux 试 NotoSansCJK（缺失则回退 egui 默认，中文变方块但不崩）
fn install_cjk_font(ctx: &egui::Context) {
    let candidates: &[&str] = if cfg!(target_os = "windows") {
        &["C:/Windows/Fonts/msyh.ttc", "C:/Windows/Fonts/simhei.ttf", "C:/Windows/Fonts/simsun.ttc"]
    } else {
        &[
            // 中文字体：环境变量 PONG_FONT 优先，其次常见系统字体路径（占位见 .cargo/config.toml.example 同理）
            &std::env::var("PONG_FONT").unwrap_or_else(|_| "C:/Windows/Fonts/msyh.ttc".into()),
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        ]
    };
    for path in candidates {
        if let Ok(data) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert("cjk".into(), egui::FontData::from_owned(data).into());
            // 追加到两族后备链尾（拉丁仍用默认字体，中文落 CJK 字体）
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                if let Some(list) = fonts.families.get_mut(&family) {
                    list.push("cjk".into());
                }
            }
            ctx.set_fonts(fonts);
            crate::core::log::log(&format!("gui: cjk font loaded from {path}"));
            return;
        }
    }
    crate::core::log::log("gui: no cjk font found (chinese text shows boxes)");
}

// 启动入口（gui_glue 调用）：catch_unwind + 日志内聚（panic 落盘 hook 已在 main.rs 装）
pub fn run_gui_logged() {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run_gui));
    match r {
        Ok(Ok(())) => crate::core::log::log("gui closed normally"),
        Ok(Err(e)) => crate::core::log::log(&format!("gui error: {e}")),
        Err(p) => {
            let msg = if let Some(s) = p.downcast_ref::<&str>() { (*s).to_string() }
                else if let Some(s) = p.downcast_ref::<String>() { s.clone() }
                else { "unknown panic".into() };
            crate::core::log::log(&format!("gui panicked: {msg}"));
        }
    }
}

// 启动 GUI 窗口（调用方 spawn 新线程）
pub fn run_gui() -> Result<(), eframe::Error> {
    let mut opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([640.0, 480.0])
            .with_title("pong"),
        ..Default::default()
    };
    // GUI 跑在托盘 spawn 的次线程：winit 默认禁止非主线程建 EventLoop，
    // Windows 扩展 with_any_thread(true) 放行（eframe 的 event_loop_builder 钩子在 build 前回调）
    #[cfg(target_os = "windows")]
    {
        opts.event_loop_builder = Some(Box::new(|builder| {
            use winit::platform::windows::EventLoopBuilderExtWindows;
            builder.with_any_thread(true);
        }));
    }
    GUI_ALIVE.store(true, std::sync::atomic::Ordering::Relaxed);
    let r = eframe::run_native("pong", opts, Box::new(|cc| {
        install_cjk_font(&cc.egui_ctx);
        let _ = GUI_CTX.set(cc.egui_ctx.clone());
        Ok(Box::new(PongApp::new()) as Box<dyn eframe::App>)
    }));
    GUI_ALIVE.store(false, std::sync::atomic::Ordering::Relaxed);
    r
}
