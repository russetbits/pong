// pong — 内置 ssh 隧道（russh）
// 程序以 SSH client 身份连 agent 主机（远程 agent 所在机器）sshd，请求 tcpip-forward（等同 ssh -R 7101:127.0.0.1:7101），
// server 每接受一个连接就开 forwarded-tcpip channel，我们把它桥接回本地 127.0.0.1:<port>
// （pong 自己监听的 TCP 桥）。断线自动重连。
//
// 配置（config.ini）：
//   ssh_key=<path>   私钥路径，缺省 %USERPROFILE%\.ssh\id_rsa
//   ssh_user=<user>  缺省当前用户名
//   ssh_port=22
use std::sync::Arc;
use std::path::PathBuf;

use russh::client::{self, Handler, Handle};
use russh::keys::PrivateKey;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use crate::core::config::IniConfig;
use crate::core::log::log;

pub static TUNNEL_ONLINE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
// GUI/托盘控制隧道启停：false 时重连循环暂停（不连接只等待）
pub static TUNNEL_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
/// 最近一次隧道错误摘要（GUI 红灯提示用；空=无错误）
pub static TUNNEL_LAST_ERR: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// GUI connect 请求（2026-11-18）：GUI 弹窗确认后置位，tunnel_forever 每轮优先消费；密码不落盘。
pub static GUI_CONNECT: std::sync::Mutex<Option<(crate::core::sshconf::SshTarget, Option<String>)>> = std::sync::Mutex::new(None);
pub fn set_gui_connect(t: crate::core::sshconf::SshTarget, pw: Option<String>) {
    if let Ok(mut g) = GUI_CONNECT.lock() { *g = Some((t, pw)); }
}
fn take_gui_connect() -> Option<(crate::core::sshconf::SshTarget, Option<String>)> {
    GUI_CONNECT.lock().ok().and_then(|mut g| g.take())
}

/// GUI 面板操作命令通道（probe/ladd/ldel/list）：tunnel_forever 起 channel 后存 tx，GUI 同进程直发。
pub static GUI_CMD_TX: std::sync::OnceLock<CmdTx> = std::sync::OnceLock::new();
/// GUI 线程的带超时 oneshot 等待（blocking_recv_timeout 在本 tokio 版本不存在——
/// 自建 current_thread mini runtime block_on+timeout；GUI 线程非 runtime worker，合法）
fn gui_wait<T>(rx: tokio::sync::oneshot::Receiver<T>, dur: std::time::Duration) -> Option<T> {
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    let rt = RT.get_or_init(|| tokio::runtime::Builder::new_current_thread()
        .enable_time().build().expect("gui runtime"));
    rt.block_on(async { tokio::time::timeout(dur, rx).await.ok() }).and_then(|r| r.ok())
}

/// GUI 专用操作（oneshot 收回执；GUI 线程阻塞调用，命令均秒级）
pub fn gui_probe() -> Result<Vec<PortInfo>, String> {
    let tx = GUI_CMD_TX.get().ok_or_else(|| "tunnel not running".to_string())?;
    let (otx, orx) = tokio::sync::oneshot::channel();
    tx.blocking_send(ControlCmd::Probe { reply: otx }).map_err(|e| e.to_string())?;
    // 超时兜底（2026-11-18 用户卡死实锤）：隧道未 up（key fail 重试循环中）Probe 排队无人应答——
    // blocking_recv 永等→GUI 线程卡死。3s 超时返回错误，UI 保持响应。
    match gui_wait(orx, std::time::Duration::from_secs(3)) {
        Some(r) => r,
        None => Err("probe timeout (tunnel busy or not up)".to_string()),
    }
}
pub fn gui_locals() -> Vec<(u16, u16)> {
    let Some(tx) = GUI_CMD_TX.get() else { return Vec::new() };
    let (otx, orx) = tokio::sync::oneshot::channel();
    if tx.blocking_send(ControlCmd::ListLocals { reply: otx }).is_ok() {
        gui_wait(orx, std::time::Duration::from_secs(1)).unwrap_or_default()
            .into_iter().map(|p| (p, p)).collect()
    } else { Vec::new() }
}
pub fn gui_ladd(port: u16) -> Result<(), String> {
    let tx = GUI_CMD_TX.get().ok_or_else(|| "tunnel not running".to_string())?;
    let (otx, orx) = tokio::sync::oneshot::channel();
    tx.blocking_send(ControlCmd::AddLocal { port, reply: otx }).map_err(|e| e.to_string())?;
    match gui_wait(orx, std::time::Duration::from_secs(1)) {
        Some(r) => r,
        None => Err("timeout (tunnel busy or not up)".to_string()),
    }
}
pub fn gui_ldel(port: u16) -> Result<(), String> {
    let tx = GUI_CMD_TX.get().ok_or_else(|| "tunnel not running".to_string())?;
    let (otx, orx) = tokio::sync::oneshot::channel();
    tx.blocking_send(ControlCmd::RetireLocal { port, reply: otx }).map_err(|e| e.to_string())?;
    match gui_wait(orx, std::time::Duration::from_secs(1)) {
        Some(r) => r,
        None => Err("timeout (tunnel busy or not up)".to_string()),
    }
}

#[derive(Debug, Clone)]
pub struct TunnelCfg {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub key_path: PathBuf,
    /// sshd 侧监听端口（agent 连接的端口，等同 ssh -R 的 <remote>:127.0.0.1:<remote>）
    pub remote_port: u16,
    /// 本地桥端口（forwarded channel 回来时我们 connect 的目标；部署上与 remote_port 相同）
    pub local_port: u16,
    /// R5 通用反向代理：N 对 (remote_port, local_port)；空=退回 remote_port/local_port 单对
    pub forwards: Vec<(u16, u16)>,
    /// GUI 密码认证（2026-11-18 定案：GUI connect 弹窗可选密码）；None=key/agent 路径
    pub password: Option<String>,
}


impl TunnelCfg {
    /// pong CLI 模式：参数来自 ~/.ssh/config（SshTarget）+动态端口（用户定案 2026-11-17）
    pub fn from_ssh_target(t: &crate::core::sshconf::SshTarget, remote_port: u16, local_port: u16, forwards: Vec<(u16, u16)>, password: Option<String>) -> TunnelCfg {
        let key = t.identity.clone().unwrap_or_default();
        let key = if key.is_empty() {
            // ssh 默认行为：~/.ssh/id_* 依次试（这里取第一个存在的）
            let home = std::env::var("HOME").ok().or_else(|| std::env::var("USERPROFILE").ok()).unwrap_or_default();
            ["id_ed25519", "id_ecdsa", "id_rsa"].iter()
                .map(|n| PathBuf::from(&home).join(".ssh").join(n))
                .find(|p| p.exists())
                .unwrap_or_else(|| PathBuf::from(&home).join(".ssh").join("id_ed25519"))
        } else { PathBuf::from(crate::core::config::resolve(&key)) };
        TunnelCfg {
            host: t.host.clone(),
            port: t.port,
            user: t.user.clone().unwrap_or_else(|| {
                std::env::var("USER").ok().or_else(|| std::env::var("USERNAME").ok()).unwrap_or_else(|| "agent".into())
            }),
            key_path: key,
            remote_port,
            local_port,
            forwards,
            password,
        }
    }

    pub fn from_config(c: Option<&IniConfig>, local_port: u16) -> TunnelCfg {
        let host = c.map(|c| c.ssh_host.clone()).unwrap_or_else(|| "localhost".into());
        let port = c.and_then(|c| c.ssh_port).unwrap_or(22);
        let user = std::env::var("USERNAME").unwrap_or_else(|_| "agent".into());
        let user = c.and_then(|c| c.ssh_user.clone()).unwrap_or(user);
        let key = c.and_then(|c| c.ssh_key.clone()).unwrap_or_default();
        let key = if key.is_empty() {
            std::env::var("USERPROFILE").map(|u| PathBuf::from(u).join(".ssh").join("id_rsa")).unwrap_or_default()
        } else { PathBuf::from(crate::core::config::resolve(&key)) };
        TunnelCfg { host, port, user, key_path: key, remote_port: local_port, local_port, forwards: c.map(|c| c.forwards.clone()).unwrap_or_default(), password: None }
    }
}

// Handler：把 server 打开的 forwarded-tcpip channel 推进队列
struct FwdHandler {
    tx: mpsc::Sender<(russh::Channel<russh::client::Msg>, u32)>,
}

impl Handler for FwdHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // TOFU（外审 P0）：首连记 fingerprint 到 exe 旁 host_key.dat，后续不符即拒
        // （防 sshd 主机被替换/中间人；变更时红灯+日志，用户删 host_key.dat 重置信任）
        // 标准 sshd=裸公钥；证书变体（罕见）直接拒绝（我们场景不存在）
        let fp = match server_public_key {
            russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } => {
                key.fingerprint(russh::keys::HashAlg::Sha256).to_string()
            }
            _ => {
                log("host key: certificate form refused");
                return Ok(false);
            }
        };
        let path = crate::core::config::host_key_path();
        match std::fs::read_to_string(&path) {
            Ok(stored) => {
                let stored = stored.trim().to_string();
                if stored == fp {
                    Ok(true)
                } else {
                    log(&format!("HOST KEY CHANGED! stored={stored} got={fp} - refusing (delete host_key.dat to re-trust)"));
                    Ok(false)
                }
            }
            Err(_) => {
                let _ = std::fs::write(&path, format!("{fp}\n"));
                log(&format!("host key first-use recorded: {fp}"));
                Ok(true)
            }
        }
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<russh::client::Msg>,
        _connected_address: &str,
        _connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut russh::client::Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        let _ = self.tx.send((channel, _connected_port)).await;
        Ok(())
    }
}

// 单次连接会话：connect→auth→tcpip_forward→桥循环。返回 Ok(()) 表示会话结束（掉线）需重连。
pub async fn tunnel_once(cfg: &TunnelCfg, key: PrivateKey) -> Result<(), String> {
    tunnel_once_onup(cfg, key, None, &mut None, Default::default()).await
}

/// daemon 控制指令（pong map/repick/port --list 经本地 ctl 端口送进隧道循环）
pub enum ControlCmd {
    /// 加 forward：remote=None=sshd 自选（回填）；Some(r)=指定远端端口
    AddForward { remote: Option<u16>, local: u16, reply: tokio::sync::oneshot::Sender<Result<u16, String>> },
    /// 撤 forward（cancel_tcpip_forward）
    RetireForward { remote: u16, reply: tokio::sync::oneshot::Sender<Result<(), String>> },
    /// 列出现存映射
    List { reply: tokio::sync::oneshot::Sender<Vec<(u16, u16)>> },
    /// repick：撤全部动态 forward→sshd 重选→回新 remote 端口；new_pair 由调用方 gen_pair 传入（select 分支统一 set_pair——2026-11-18 修 GUI 模式 pair 不轮换 bug）
    Repick { new_pair: String, reply: tokio::sync::oneshot::Sender<Result<u16, String>> },
    /// 枚举 host（sshd 侧）127.0.0.1 监听端口（map --list / GUI 列表；2026-11-18）——自建 ssh 连接 exec netstat
    Probe { reply: tokio::sync::oneshot::Sender<Result<Vec<PortInfo>, String>> },
    /// -L 表枚举（GUI 端口面板 mapped 徽标；List 是 -R 表——2026-11-18 实锤混用致 mapped 状态不更新）
    ListLocals { reply: tokio::sync::oneshot::Sender<Vec<u16>> },
    /// GUI Disconnect：立即打断当前 tunnel_once_onup（select 循环收到即 return）——进程不退（区别于 Exit）
    Disconnect,
    /// -L：client 监听 port → host 127.0.0.1:port（client 用户访问 host 服务）
    AddLocal { port: u16, reply: tokio::sync::oneshot::Sender<Result<(), String>> },
    /// 摘除 -L
    RetireLocal { port: u16, reply: tokio::sync::oneshot::Sender<Result<(), String>> },
}

/// -L 现存表（port → stop sender；随 tunnel 循环生命周期）
pub type Locals = std::sync::Arc<std::sync::Mutex<std::collections::HashMap<u16, tokio::sync::mpsc::Sender<()>>>>;

pub type CmdTx = tokio::sync::mpsc::Sender<ControlCmd>;
pub type CmdRx = tokio::sync::mpsc::Receiver<ControlCmd>;

/// -L local forward 引擎（2026-11-18 用户需求：client 用户经 client 本地端口访问 host 上的服务）。
/// 每端口一个 accept 任务：client 127.0.0.1:<port> accept 后把连接交给隧道循环（handle 唯一不可 clone——
/// direct-tcpip channel 的开启在隧道 select 循环的 lreq 分支完成，数据拷贝再 spawn 独立任务）。
async fn local_forward_loop(
    port: u16,
    listener: TcpListener,
    stop: tokio::sync::mpsc::Receiver<()>,
    lreq: tokio::sync::mpsc::UnboundedSender<(u16, TcpStream)>,
) {
    let mut stop = stop;
    log(&format!("local forward listening 127.0.0.1:{port} -> host:{port}"));
    loop {
        tokio::select! {
            _ = stop.recv() => { log(&format!("local forward {port} stopped")); return; }
            accept = listener.accept() => {
                let Ok((client_sock, _)) = accept else { continue };
                let _ = lreq.send((port, client_sock));
            }
        }
    }
}

/// 带 up 回调版本：tcpip_forward 全部注册成功后、进桥循环前调用 on_up（pong connect 在此打印 port=/pair=——
/// 桥循环阻塞返回不了，打印放返回后会永远发不出去，2026-11-17 用户实测实锤）
pub async fn tunnel_once_onup(cfg: &TunnelCfg, key: PrivateKey, on_up: Option<&(dyn Fn(u16) + Send + Sync)>, ctl_rx: &mut Option<CmdRx>, locals: Locals) -> Result<(), String> {
    let client_config = Arc::new(client::Config {
        inactivity_timeout: None.into(),
        keepalive_interval: std::time::Duration::from_secs(30).into(),
        ..Default::default()
    });

    let (tx, mut rx) = mpsc::channel::<(russh::Channel<russh::client::Msg>, u32)>(16);
    let addrs = (cfg.host.as_str(), cfg.port);

    // 连接全程（TCP+KEX+auth 前段）限时 15s：防 TCP 半开/防火墙 drop 挂死循环
    // （曾实锤：Stop 后会话卡住不退出，后续 Start 永远等不到循环回来）
    let mut handle: Handle<FwdHandler> = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        client::connect(client_config, addrs, FwdHandler { tx }),
    )
    .await
    .map_err(|_| format!("connect {cfg:?} timeout (15s)"))?
    .map_err(|e| format!("connect {cfg:?} fail: {e}"))?;

    // 认证源三层：ini ssh_key= → ssh-agent（Windows named pipe/Pageant；Unix SSH_AUTH_SOCK）→ ~/.ssh 默认 key 文件
    // RSA key 需与 sshd 协商签名 hash（新 sshd 已禁 ssh-rsa/SHA-1，须 sha2）
    let hash = handle.best_supported_rsa_hash().await.ok().flatten().flatten();
    let key_raw = key.clone(); // 原始 key（probe 等分支用；下行遮蔽为带 hash 包装版）
    let key = russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), hash);

    let mut auth_note = String::new();
    let authed = authenticate(&mut handle, cfg, key, &mut auth_note).await;
    let authed = match authed {
        Some(a) => a,
        None => return Err(format!("no auth method available: {auth_note}")),
    };
    if !authed.success() {
        return Err(format!("auth rejected ({auth_note})"));
    }
    log(&format!("auth ok via: {auth_note}"));

    // 等同 ssh -R <remote>:127.0.0.1:<local>（R5：多对映射）
    // **r=0 = sshd 自选端口**（pong 动态模式）：sshd bind 空闲端口并经返回值告知——
    // 避免固定/预探测端口在 sshd 侧被占（同机自测时还被自家桥占用，2026-11-17 实锤根因）
    let live: std::sync::Arc<std::sync::Mutex<Vec<(u16, u16)>>> = Default::default();
    for (r, l) in pairs(cfg) {
        if r == 0 {
            let bound = handle.tcpip_forward("127.0.0.1", 0)
                .await.map_err(|e| format!("tcpip_forward(auto) fail: {e}"))?;
            let bound = bound as u16;
            log(&format!("forward remote auto->{} registered (local {})", bound, l));
            live.lock().unwrap().push((bound, l));
        } else {
            handle.tcpip_forward("127.0.0.1", r as u32)
                .await.map_err(|e| format!("tcpip_forward {r} fail: {e}"))?;
            log(&format!("forward remote {} registered", r));
            live.lock().unwrap().push((r, l));
        }
    }

    crate::core::tunnel::TUNNEL_ONLINE.store(true, std::sync::atomic::Ordering::Relaxed);
    let first_remote = live.lock().unwrap().first().map(|(r, _)| *r).unwrap_or(cfg.remote_port);
    if let Some(f) = on_up { f(first_remote); }
    log(&format!("ssh tunnel up: {host}:{port} -R {lp}", host = cfg.host, port = cfg.port, lp = cfg.local_port));

    // -L accept 请求通道（accept_loop → 本循环开 direct-tcpip；handle 唯一不可 clone）
    let (lreq_tx, mut lreq_rx) = mpsc::unbounded_channel::<(u16, TcpStream)>();
    // 桥循环：server 打开的每条 forwarded channel ↔ 本地 pong TCP 端口
    // select 版：Stop（TUNNEL_ENABLED=false）时 1s 内退出会话（不再等 server 断开）
    loop {
        let ch = tokio::select! {
            c = rx.recv() => match c {
                Some(c) => c,
                None => break,
            },
            // -L：本机 accept 到达 → 开 direct-tcpip 到 host 服务端口 → spawn 双向拷贝
            lreq = lreq_rx.recv() => match lreq {
                Some((lport, client_sock)) => {
                    match handle.channel_open_direct_tcpip("127.0.0.1", lport as u32, "127.0.0.1", 0).await {
                        Ok(ch) => {
                            let stream = ch.into_stream();
                            tokio::spawn(async move {
                                let (mut cr, mut cw) = tokio::io::split(client_sock);
                                let (mut sr, mut sw) = tokio::io::split(stream);
                                let a = tokio::io::copy(&mut cr, &mut sw);
                                let b = tokio::io::copy(&mut sr, &mut cw);
                                // 任一方向结束即收（请求-响应型，无半开需求）
                                tokio::select! { _ = a => {}, _ = b => {} }
                            });
                        }
                        Err(e) => log(&format!("direct-tcpip {lport} fail: {e}")),
                    }
                    continue;
                }
                None => break,
            },
            // daemon 控制指令（map/repick/port --list）
            c = async {
                match ctl_rx.as_mut() { Some(r) => r.recv().await, None => std::future::pending().await }
            } , if ctl_rx.is_some() => match c {
                Some(cmd) => {
                    match cmd {
                        ControlCmd::AddForward { remote, local, reply } => {
                            let r = match remote {
                                None => handle.tcpip_forward("127.0.0.1", 0).await.map(|b| b as u16),
                                Some(r0) => handle.tcpip_forward("127.0.0.1", r0 as u32).await.map(|_| r0),
                            };
                            match r {
                                Ok(bound) => {
                                    log(&format!("ctl: forward added remote={bound} local={local}"));
                                    live.lock().unwrap().push((bound, local));
                                    let _ = reply.send(Ok(bound));
                                }
                                Err(e) => { let _ = reply.send(Err(format!("{e}"))); }
                            }
                        }
                        ControlCmd::RetireForward { remote, reply } => {
                            match handle.cancel_tcpip_forward("127.0.0.1", remote as u32).await {
                                Ok(()) => {
                                    live.lock().unwrap().retain(|(rr, _)| *rr != remote);
                                    log(&format!("ctl: forward retired remote={remote}"));
                                    let _ = reply.send(Ok(()));
                                }
                                Err(e) => { let _ = reply.send(Err(format!("{e}"))); }
                            }
                        }
                        ControlCmd::List { reply } => {
                            let _ = reply.send(live.lock().unwrap().clone());
                        }
                        ControlCmd::ListLocals { reply } => {
                            // -L 表枚举（GUI mapped 徽标用；与 -R 的 List 区分——2026-11-18 混用实锤）
                            let keys: Vec<u16> = locals.lock().unwrap().keys().copied().collect();
                            let _ = reply.send(keys);
                        }
                        ControlCmd::Disconnect => {
                            // GUI Disconnect：立即打断当前会话（tunnel_forever 循环见 TUNNEL_ENABLED=false 停连——进程不退）
                            return Err("disconnected by user".to_string());
                        }
                        ControlCmd::Repick { new_pair, reply } => {
                            // 撤现存全部 forward（repick 语义：端口+码都换）；local 先记主桥端口
                            // pair 轮换统一在此 set_pair（2026-11-18：GUI 模式无 ctl 线程，原 daemon 侧 set_pair 不触发→pair 不换 bug）
                            if !new_pair.is_empty() { crate::core::auth::set_pair(&new_pair); }
                            let olds: Vec<(u16, u16)> = live.lock().unwrap().clone();
                            let main_local = olds.first().map(|(_, l)| *l).unwrap_or(0);
                            for (r, _) in &olds {
                                let _ = handle.cancel_tcpip_forward("127.0.0.1", *r as u32).await;
                            }
                            live.lock().unwrap().clear();
                            // sshd 重选端口（配对码由 daemon ctl 线程 set_pair）
                            match handle.tcpip_forward("127.0.0.1", 0).await {
                                Ok(b) => {
                                    let b = b as u16;
                                    if main_local > 0 { live.lock().unwrap().push((b, main_local)); }
                                    log(&format!("ctl: repick new remote={b}"));
                                    let _ = reply.send(Ok(b));
                                }
                                Err(e) => { let _ = reply.send(Err(format!("{e}"))); }
                            }
                        }
                        ControlCmd::Probe { reply } => {
                            // 独立 ssh 连接 exec netstat 枚举 host 服务端口；cfg/key 克隆进 spawn
                            let pcfg = cfg.clone();
                            let pkey = key_raw.clone();
                            tokio::spawn(async move {
                                let r = probe_host_listen_ports(&pcfg, pkey).await;
                                let _ = reply.send(r);
                            });
                        }
                        ControlCmd::AddLocal { port, reply } => {
                            // 先查表（guard 不跨 await：bind 在锁外做）
                            if locals.lock().unwrap().contains_key(&port) {
                                let _ = reply.send(Err(format!("local forward {port} already exists")));
                            } else {
                                match TcpListener::bind(("127.0.0.1", port)).await {
                                    Ok(l) => {
                                        let mut tab = locals.lock().unwrap();
                                        if tab.contains_key(&port) {
                                            let _ = reply.send(Err(format!("local forward {port} already exists")));
                                        } else {
                                            let (stx, srx) = tokio::sync::mpsc::channel::<()>(1);
                                            tokio::spawn(local_forward_loop(port, l, srx, lreq_tx.clone()));
                                            tab.insert(port, stx);
                                            log(&format!("ctl: local forward added port={port}"));
                                            let _ = reply.send(Ok(()));
                                        }
                                    }
                                    Err(e) => { let _ = reply.send(Err(format!("bind {port}: {e}"))); }
                                }
                            }
                        }
                        ControlCmd::RetireLocal { port, reply } => {
                            let tx = locals.lock().unwrap().remove(&port);
                            match tx {
                                Some(tx) => { let _ = tx.send(()).await; log(&format!("ctl: local forward retired port={port}")); let _ = reply.send(Ok(())); }
                                None => { let _ = reply.send(Err(format!("local forward {port} not found"))); }
                            }
                        }
                    }
                    continue;
                }
                None => break,
            },
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                if !TUNNEL_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
                    log("ssh tunnel stop requested - closing session");
                    // 连接 drop 即撤销 forward（sshd 侧随会话关闭自动清理）
                    break;
                }
                continue;
            }
        };
        let (ch, rport) = ch;
        // R5：按 connected_port 查映射（live 含 r=0 自选回填项；未登记拒绝防错配）
        let lp = live.lock().unwrap().iter().find(|(rr, _)| *rr == rport as u16).map(|(_, ll)| *ll)
            .or_else(|| lookup(cfg, rport)).unwrap_or(0);
        let mut stream = ch.into_stream();
        if lp == 0 {
            crate::core::log::log(&format!("tunnel: no local target for remote port {} - reject", rport));
            continue;
        }
        tokio::spawn(async move {
            let local = match TcpStream::connect(("127.0.0.1", lp)).await {
                Ok(s) => s,
                Err(e) => { crate::core::log::log(&format!("tunnel local connect fail: {e}")); return; }
            };
            // 双向搬运：ssh channel ↔ 本地 TCP（pong 自己的桥监听口）
            // 泄漏修复（2026-11-17 实锤）：join 等双拷贝=agent 关（channel EOF）后 daemon→agent 拷贝
            // 永远读不到 daemon EOF（daemon 等下一条请求不关 socket）→ 互等 → 桥线程永不释放 → 16 槽耗尽。
            // 改 select：任一方向结束即全关（pong=请求-响应协议，无半开需求）。
            let (mut lr, mut lw) = tokio::io::split(local);
            let (mut sr, mut sw) = tokio::io::split(stream);
            let a = tokio::io::copy(&mut sr, &mut lw);
            let b = tokio::io::copy(&mut lr, &mut sw);
            tokio::select! {
                _ = a => {},  // agent 侧先关（channel EOF/close）
                _ = b => {},  // daemon 侧先关
            }
            // 显式全关：socket drop → 桥线程立即 EOF 释放槽位；channel 半亦撤
            drop(lr); drop(lw); drop(sr); drop(sw);
        });
    }
    // 会话结束：-L accept_loop 全停（stop 信号）+表清空（重连后用户重新 map；GUI refresh 同步）
    {
        let mut tab = locals.lock().unwrap();
        for (_, tx) in tab.drain() { let _ = tx.send(()); }
    }
    Ok(())
}

/// R5：映射对集合（forwards 非空用它；空=单对 remote/local 回退）
fn pairs(cfg: &TunnelCfg) -> Vec<(u16, u16)> {
    if cfg.forwards.is_empty() {
        vec![(cfg.remote_port, cfg.local_port)]
    } else {
        cfg.forwards.clone()
    }
}

//// 枚举 host（agent 所在服务器）绑定在 127.0.0.1 上的监听端口（2026-11-18 用户需求：VSCode Ports 面板形态）。
/// 用途：这些 host 服务端口可被映射（-L local forward），client 用户经 client 本地端口访问 host 服务。
/// 实现：独立 ssh 连接 exec netstat/ss 解析 LISTEN 行（host OS 未知→两命令都试，首个出数者为准）。
/// probe 结果条目：端口+监听进程名（VSCode Ports 面板形态：每行带进程名——2026-11-18 用户需求）
/// pid：Linux 侧从 ss users: 抽出（用于第二段 /proc/<pid>/cmdline 取完整命令行；0=无）
#[derive(Debug, Clone)]
pub struct PortInfo { pub port: u16, pub proc_name: String, pub pid: u32 }
pub async fn probe_host_listen_ports(cfg: &TunnelCfg, key: PrivateKey) -> Result<Vec<PortInfo>, String> {
    let client_config = Arc::new(client::Config {
        inactivity_timeout: None.into(),
        keepalive_interval: std::time::Duration::from_secs(30).into(),
        ..Default::default()
    });
    let addrs = (cfg.host.as_str(), cfg.port);
    let mut handle = tokio::time::timeout(std::time::Duration::from_secs(15),
        client::connect(client_config, addrs, FwdHandler { tx: mpsc::channel(1).0 }))
        .await.map_err(|_| "probe connect timeout".to_string())?
        .map_err(|e| format!("probe connect fail: {e}"))?;
    let hash = handle.best_supported_rsa_hash().await.ok().flatten().flatten();
    let file_key = russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), hash);
    let mut note = String::new();
    let auth = authenticate(&mut handle, cfg, file_key, &mut note).await
        .ok_or_else(|| format!("probe auth fail: {note}"))?;
    if !auth.success() { return Err(format!("probe auth rejected: {note}")); }
    let mut ports: Vec<u16> = Vec::new();
    // 进程归属过滤（2026-11-18 用户定案：只列当前登录用户进程监听的端口，系统/他人服务忽略——VSCode 同思路）
    // Linux host：`id -u; ss -tlnpe` 首行=登录用户 uid，LISTEN 行 uid=<n> 与之比对
    // Windows host：PowerShell Get-NetTCPConnection+Win32_Process owner 比对 %USERNAME%（失败退 netstat 全量）
    let mut infos: Vec<PortInfo> = Vec::new();
    for cmd in [
        // Windows host：PowerShell 一条命令（监听 127.0.0.1 + 进程 owner==当前用户 → "端口 进程名" 行）
        "powershell -NoProfile -Command \"$me=$env:USERNAME; Get-NetTCPConnection -State Listen -LocalAddress 127.0.0.1 | ForEach-Object { $o=(Get-CimInstance Win32_Process -Filter ($_.OwningProcess.ToString())) | Invoke-CimMethod -MethodName GetOwner; if ($o.User -eq $me) { $p=Get-Process -Id $_.OwningProcess -ErrorAction SilentlyContinue; Write-Output ($_.LocalPort.ToString() + ' ' + $p.ProcessName) } }\"",
        // Linux host：首行 id -u 输出=登录 uid；ss -tlnpe 各行带 uid:<n> 与 users:(("name",pid=...))——解析比对
        "id -u; ss -tlnpe",
        // 兜底（无过滤——老系统/异常环境）
        "netstat -an -p tcp | findstr LISTENING",
        "ss -tln",
    ] {
        let mut ch = match handle.channel_open_session().await {
            Ok(c) => c,
            Err(_) => continue,
        };
        if ch.exec(false, cmd).await.is_err() { continue; }
        let mut out = String::new();
        let mut reader = ch.into_stream();
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 65536];
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let n = match tokio::time::timeout_at(deadline, reader.read(&mut buf)).await {
                Ok(Ok(0)) | Err(_) => break,
                Ok(Ok(n)) => n,
                Ok(Err(_)) => break,
            };
            out.push_str(&String::from_utf8_lossy(&buf[..n]));
            if out.len() > 1_000_000 { break; }
        }
        parse_probe_output(&out, &mut infos);
        if !infos.is_empty() { break; }
    }
    // Linux：第二段 exec 取 /proc/<pid>/cmdline 完整命令行（ss 只给线程名 comm，如 MainThread——用户要看 VSCode 式完整命令）
    let pids: Vec<u32> = infos.iter().map(|i| i.pid).filter(|p| *p > 0).collect();
    if !pids.is_empty() {
        let plist = pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(" ");
        let cmd2 = format!("for p in {plist}; do printf '%s ' \"$p\"; tr '\\0' ' ' < /proc/$p/cmdline 2>/dev/null; echo; done");
        if let Ok(mut ch2) = handle.channel_open_session().await {
            if ch2.exec(false, cmd2.as_str()).await.is_ok() {
                let mut out2 = String::new();
                let mut r2 = ch2.into_stream();
                use tokio::io::AsyncReadExt;
                let mut b2 = vec![0u8; 65536];
                let dl2 = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    match tokio::time::timeout_at(dl2, r2.read(&mut b2)).await {
                        Ok(Ok(0)) | Err(_) | Ok(Err(_)) => break,
                        Ok(Ok(n)) => out2.push_str(&String::from_utf8_lossy(&b2[..n])),
                    }
                    if out2.len() > 500_000 { break; }
                }
                // 解析：每行 "<pid> <cmdline>"
                for line in out2.lines() {
                    let t = line.trim();
                    let Some(sp) = t.find(' ') else { continue };
                    let Ok(pp) = t[..sp].parse::<u32>() else { continue };
                    let cmdline = t[sp + 1..].trim().to_string();
                    if cmdline.is_empty() { continue; }
                    for i in infos.iter_mut() {
                        if i.pid == pp {
                            i.proc_name = cmdline.chars().take(64).collect(); // VSCode 式截断
                        }
                    }
                }
            }
        }
    }
    infos.sort_by_key(|i| i.port);
    infos.dedup_by_key(|i| i.port);
    let _ = handle.disconnect(russh::Disconnect::ByApplication, "probe done", "en").await;
    Ok(infos)
}

/// probe 输出解析（形态自适应，2026-11-18 进程归属过滤）——三种形态：
/// ①PowerShell（Windows host 过滤版）：每行纯数字=端口，直接收
/// ②Linux uid 版：首行=id -u 的 uid；后续 ss -tlnpe 行含 "uid:<n>"——仅 n==uid 的 127.0.0.1 LISTEN 行收（当前用户进程）
/// ③兜底 netstat/ss（无过滤）：127.0.0.1 LISTEN 行全收
fn parse_probe_output(text: &str, out: &mut Vec<PortInfo>) {
    let lines: Vec<&str> = text.lines().collect();
    // 形态①：PowerShell（Windows host）输出="<port> <procname>" 行（首行 port 数字+空格+名字）
    if lines.iter().any(|l| {
        let t = l.trim();
        match t.split_once(' ') {
            Some((a, b)) => !a.is_empty() && a.chars().all(|c| c.is_ascii_digit()) && !b.is_empty() && t.parse::<u16>().is_err(),
            None => false,
        }
    }) && !lines.iter().any(|l| l.contains("127.0.0.1")) {
        for l in lines {
            let t = l.trim();
            if let Some((a, b)) = t.split_once(' ') {
                if let Ok(p) = a.parse::<u16>() {
                    if p > 0 && !b.is_empty() { out.push(PortInfo { port: p, proc_name: b.trim().to_string(), pid: 0 }); }
                }
            }
        }
        return;
    }
    // 形态②：首行纯数字=uid（id -u 输出），后行 ss -tlnpe（users:(("name",pid=..)) 带进程名）
    let mut my_uid: Option<String> = None;
    let mut body_start = 0;
    if let Some(first) = lines.first() {
        let t = first.trim();
        if t.chars().all(|c| c.is_ascii_digit()) && !t.is_empty() {
            my_uid = Some(t.to_string());
            body_start = 1;
        }
    }
    for line in &lines[body_start..] {
        let l = line.trim();
        let lu = l.to_ascii_uppercase();
        if lu.starts_with("PROTO") || lu.starts_with("ACTIVE") || lu.starts_with("NETSTAT") || lu.starts_with("STATE") || lu.starts_with("NETID") { continue; }
        if !l.contains("127.0.0.1") { continue; }
        if !(lu.contains("LISTENING") || lu.contains("LISTEN")) { continue; }
        // uid 过滤（ss -e 生效时 my_uid 已知）：行内 uid: 列表有一项==登录 uid 才收；无 uid 信息=看不到属主→忽略
        if let Some(uid) = &my_uid {
            let mut owner_uids: Vec<String> = Vec::new();
            for tok in l.split_whitespace() {
                if let Some(u) = tok.strip_prefix("uid:") {
                    owner_uids.push(u.trim_end_matches(',').to_string());
                }
            }
            if owner_uids.is_empty() || !owner_uids.iter().any(|u| u == uid) { continue; }
        }
        // 端口
        let mut port: Option<u16> = None;
        for tok in l.split_whitespace() {
            if let Some(rest) = tok.strip_prefix("127.0.0.1:") {
                if let Ok(p) = rest.parse::<u16>() { port = Some(p); }
            }
            if let Some(rest) = tok.strip_prefix("[127.0.0.1]:") {
                if let Ok(p) = rest.parse::<u16>() { port = Some(p); }
            }
        }
        let Some(p) = port else { continue };
        if p == 0 { continue; }
        // 进程名+pid：users:(("procname",pid=N,fd=M)) 段抽第一个引号名与 pid（无=未知）
        let mut proc_name = String::new();
        let mut pid: u32 = 0;
        if let Some(u) = l.find("users:(('").or_else(|| l.find("users:((\"")) {
            let rest = &l[u + 9..];
            if let Some(endq) = rest.find('\'').or_else(|| rest.find('"')) {
                proc_name = rest[..endq].to_string();
            }
            // pid=N 抽取（cmdline 二段查询用）
            if let Some(pk) = rest.find("pid=") {
                let ps = &rest[pk + 4..];
                let num: String = ps.chars().take_while(|c| c.is_ascii_digit()).collect();
                pid = num.parse::<u32>().unwrap_or(0);
            }
        }
        out.push(PortInfo { port: p, proc_name, pid });
    }
}

// R5：connected_port -> 本地目标端口；未登记 = None
fn lookup(cfg: &TunnelCfg, rport: u32) -> Option<u16> {
    let r = rport as u16;
    pairs(cfg).into_iter().find(|(rr, _)| *rr == r).map(|(_, ll)| ll)
}

// 认证源解析（三层）。返回 Some(AuthResult)=认证完成；None=无可用凭据（note 说明原因）。
// 文件 key 与 agent 身份都试过仍未成功 → None。
async fn authenticate(
    handle: &mut Handle<FwdHandler>,
    cfg: &TunnelCfg,
    file_key: russh::keys::PrivateKeyWithHashAlg,
    note: &mut String,
) -> Option<russh::client::AuthResult> {
    // ① 文件 key（ini ssh_key= 显式路径；或缺省 ~/.ssh 扫描——from_config 已定）
    {
        let r = handle.authenticate_publickey(&cfg.user, file_key).await;
        match r {
            Ok(a) => {
                note.push_str("key file; ");
                if a.success() { return Some(a); }
                // AUTHFAIL=结构化标记（外审 2 轮 P1-8：退避分类不再靠宽松 contains("auth") 文本匹配——
                // 库错误消息可能本地化/改版；此标记由我们自己生成，精确且稳定）
                note.push_str("key rejected; AUTHFAIL; ");
            }
            Err(e) => note.push_str(&format!("key error: {e}; ")),
        }
    }

    // ② ssh-agent 身份（passphrase key / 多 key 场景）
    if auth_via_agent(handle, &cfg.user, note).await.is_some() {
        // 成功结果已在函数内返回；走到这里说明全失败
    }
    // ③ 密码（GUI connect 弹窗可选密码；2026-11-18）
    if let Some(pw) = &cfg.password {
        match handle.authenticate_password(&cfg.user, pw).await {
            Ok(a) => {
                note.push_str("password; ");
                if a.success() { return Some(a); }
                note.push_str("password rejected; AUTHFAIL; ");
            }
            Err(e) => note.push_str(&format!("password error: {e}; ")),
        }
    }
    None
}

// agent 身份逐一试签（client 类型无关的公共逻辑）
async fn try_agent_auth<S>(
    handle: &mut Handle<FwdHandler>,
    user: &str,
    client: &mut russh::keys::agent::client::AgentClient<S>,
    note: &mut String,
) -> Option<russh::client::AuthResult>
where
    S: russh::keys::agent::client::AgentStream + Unpin + Send,
{
    let ids = match client.request_identities().await {
        Ok(i) => i,
        Err(e) => { note.push_str(&format!("agent ids error: {e}; ")); return None; }
    };
    for id in &ids {
        let pub_key = id.public_key().into_owned();
        let r = handle
            .authenticate_publickey_with(user, pub_key, None, client)
            .await;
        match r {
            Ok(a) => {
                note.push_str("agent; ");
                if a.success() { return Some(a); }
            }
            Err(e) => note.push_str(&format!("agent sign error: {e}; ")),
        }
    }
    note.push_str("agent keys rejected; ");
    None
}

// agent 认证：连接本机 ssh-agent（Windows named pipe/Pageant；Unix SSH_AUTH_SOCK），
// 逐个身份试签名。成功返回 Some(AuthResult)；连接失败/全部拒绝返回 None。
async fn auth_via_agent(
    handle: &mut Handle<FwdHandler>,
    user: &str,
    note: &mut String,
) -> Option<russh::client::AuthResult> {
    #[cfg(target_os = "windows")]
    {
        const OPENSSH_AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";
        // OpenSSH agent 服务（named pipe）优先，失败再试 Pageant（PuTTY 用户）——两种流类型无法统一，分别处理
        if let Ok(mut client) = russh::keys::agent::client::AgentClient::connect_named_pipe(OPENSSH_AGENT_PIPE).await {
            if let Some(r) = try_agent_auth(handle, user, &mut client, note).await { return Some(r); }
            return None;
        }
        if let Ok(mut client) = russh::keys::agent::client::AgentClient::connect_pageant().await {
            if let Some(r) = try_agent_auth(handle, user, &mut client, note).await { return Some(r); }
            return None;
        }
        note.push_str("no agent(pipe/pageant); ");
        None
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut client = match russh::keys::agent::client::AgentClient::connect_env().await {
            Ok(c) => c,
            Err(_) => { note.push_str("no agent(SOCK); "); return None; }
        };
        let ids = match client.request_identities().await {
            Ok(i) => i,
            Err(e) => { note.push_str(&format!("agent ids error: {e}; ")); return None; }
        };
        for id in &ids {
            let pub_key = id.public_key().into_owned();
            let r = handle
                .authenticate_publickey_with(user, pub_key, None, &mut client)
                .await;
            match r {
                Ok(a) => {
                    note.push_str("agent; ");
                    if a.success() { return Some(a); }
                }
                Err(e) => note.push_str(&format!("agent sign error: {e}; ")),
            }
        }
        note.push_str("agent keys rejected; ");
        None
    }
}


// spawn 常驻重连任务（5s 间隔），内部用 tokio 运行时
/// 隧道常驻循环（调用方 block_on 永久驱动——runtime 不能 drop，否则任务被 abort）
/// 每轮迭代重读 ini + 重 load key：配置热生效（Reload/Save 后自动加载）、
/// 先启动后放 key 自愈（曾实锤：key 预加载失败直接 return，隧道永不启动，Reload 无效）
pub async fn tunnel_forever(local_port: u16, mut ctl_rx: Option<CmdRx>, locals: Locals) {
    let mut backoff: u64 = 0; // 指数退避秒数（外审 P1）
    loop {
        // GUI/托盘停用隧道时不连接只等待（重新启用立即恢复）
        if !TUNNEL_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
            TUNNEL_ONLINE.store(false, std::sync::atomic::Ordering::Relaxed);
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        }
        // 每轮读运行时配置（UI apply / CLI config set 实时生效；未初始化则从 ini 导入一次）
        // key 文件仍每轮重 load（先启动后放 key 的自愈保留）
        let parsed = crate::core::config::get_runtime().unwrap_or_else(|| {
            let exe = crate::core::config::exe_dir();
            let cfg_text = std::fs::read_to_string(exe.join("config.ini")).unwrap_or_default();
            crate::core::config::IniConfig::parse(&cfg_text)
        });
        // GUI connect 请求优先（2026-11-18：GUI connect 弹窗置位——target+可选密码；消费后清空）
        let cfg = if let Some((target, pw)) = take_gui_connect() {
            TunnelCfg::from_ssh_target(&target, 0, local_port, vec![(0u16, local_port)], pw)
        } else {
            TunnelCfg::from_config(Some(&parsed), local_port)
        };
        // key 加载多候选回退（2026-11-18 用户 GUI 实测 key load fail 而同机 CLI 成功——
        // 疑 GUI 扫描选中 id_ed25519（顺序优先）但该文件 load 失败且不试 id_rsa）——
        // 顺序：cfg.key_path 优先，失败再试 ~/.ssh/{id_ed25519,id_ecdsa,id_rsa}，首个 load 成功者胜出
        let home = std::env::var("HOME").ok().or_else(|| std::env::var("USERPROFILE").ok()).unwrap_or_default();
        let candidates: Vec<std::path::PathBuf> = {
            let mut v = vec![cfg.key_path.clone()];
            for n in ["id_ed25519", "id_ecdsa", "id_rsa"] {
                let p = std::path::PathBuf::from(&home).join(".ssh").join(n);
                if p != cfg.key_path { v.push(p); }
            }
            v
        };
        let mut load_notes: Vec<String> = Vec::new();
        let key = {
            let mut found: Option<PrivateKey> = None;
            for cp in &candidates {
                match russh::keys::load_secret_key(cp, None) {
                    Ok(k) => { found = Some(k); break; }
                    Err(e) => load_notes.push(format!("{}: {e}", cp.display())),
                }
            }
            match found {
                Some(k) => k,
                None => {
                    let msg = format!("key load fail (all candidates): {}", load_notes.join(" | "));
                    if let Ok(mut g) = TUNNEL_LAST_ERR.lock() { *g = msg.chars().take(300).collect(); }
                    log(&msg);
                    TUNNEL_ONLINE.store(false, std::sync::atomic::Ordering::Relaxed);
                    backoff = (backoff * 2).max(4).min(60);
                    tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                    continue;
                }
            }
        };

        match tunnel_once_onup(&cfg, key.clone(), None, &mut ctl_rx, locals.clone()).await {
            Ok(()) => {
                if let Ok(mut g) = TUNNEL_LAST_ERR.lock() { g.clear(); }
                log("ssh tunnel session ended");
                backoff = 0; // 成功清零
            }
            Err(e) => {
                let msg = format!("ssh tunnel error: {e}");
                if let Ok(mut g) = TUNNEL_LAST_ERR.lock() { *g = e.chars().take(120).collect(); }
                log(&msg);
                // 指数退避（外审 P1）：1→2→4→…→60s；认证失败上限 300s（防 fail2ban 锁定风暴）
                let auth_fail = e.contains("AUTHFAIL");
                backoff = if auth_fail { (backoff * 2).max(8).min(300) } else { (backoff * 2).max(2).min(60) };
            }
        }
        TUNNEL_ONLINE.store(false, std::sync::atomic::Ordering::Relaxed);
        tokio::time::sleep(std::time::Duration::from_secs(backoff.max(1))).await;
    }
}

/// 兼容旧接口：spawn 到当前 runtime（调用方 runtime 必须继续存活，如 selftest）
pub async fn spawn_tunnel_task(cfg: TunnelCfg, key: PrivateKey) {
    tokio::spawn(async move {
        let mut backoff: u64 = 0;
        loop {
            // GUI/托盘停用隧道时不连接只等待（重新启用立即恢复）
            if !TUNNEL_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
                TUNNEL_ONLINE.store(false, std::sync::atomic::Ordering::Relaxed);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
            match tunnel_once(&cfg, key.clone()).await {
                Ok(()) => {
                    if let Ok(mut g) = TUNNEL_LAST_ERR.lock() { g.clear(); }
                    log("ssh tunnel session ended");
                }
                Err(e) => {
                    let msg = format!("ssh tunnel error: {e}");
                    if let Ok(mut g) = TUNNEL_LAST_ERR.lock() { *g = e.chars().take(120).collect(); }
                    log(&msg);
                    let auth_fail = e.contains("AUTHFAIL");
                    backoff = if auth_fail { (backoff * 2).max(8).min(300) } else { (backoff * 2).max(2).min(60) };
                }
            }
            TUNNEL_ONLINE.store(false, std::sync::atomic::Ordering::Relaxed);
            tokio::time::sleep(std::time::Duration::from_secs(backoff.max(1))).await;
        }
    });
}
