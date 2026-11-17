// 本机闭环自测（Linux 上跑）：embedded 隧道全链路
// 前置：本机 sshd 运行中 + ~/.ssh/id_ed25519 可登录自己
// 流程：起假桥(7101) → 隧道连 sshd 请求 forward → agent 连 7101 发 ping → 假桥收请求回 pong
use pong::core::tunnel::{TunnelCfg, tunnel_once};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1) 假桥：监听 7101，一行 JSON 进 → pong JSON 出
    let listener = TcpListener::bind("127.0.0.1:7199").await?;
    let bridge = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let (r, mut w) = tokio::io::split(sock);
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            eprintln!("[bridge] got: {}", line);
            let resp = "{\"id\":\"t1\",\"ok\":true,\"exit_code\":0,\"output\":\"pong\",\"ms\":0}\n";
            let _ = w.write_all(resp.as_bytes()).await;
        }
    });

    // 2) 隧道：本机 sshd（agent 主机 sshd 的替身）；可用环境变量覆盖连真实 sshd
    //    AD_HOST / AD_PORT / AD_USER / AD_KEY / AD_REMOTE_PORT / AD_LOCAL_PORT
    let home = std::env::var("HOME").unwrap();
    let cfg = TunnelCfg {
        host: std::env::var("AD_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
        port: std::env::var("AD_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(2222),
        user: std::env::var("AD_USER").unwrap_or_else(|_| std::env::var("USER").unwrap_or_else(|_| "root".into())),
        key_path: std::env::var("AD_KEY").map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from(home).join(".ssh").join("id_ed25519")),
        remote_port: std::env::var("AD_REMOTE_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(7201),
        local_port: std::env::var("AD_LOCAL_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(7199),
        forwards: Vec::new(),
        password: None,
    };
    let key = russh::keys::load_secret_key(&cfg.key_path, None)?;
    let t = tokio::spawn(async move {
        if let Err(e) = tunnel_once(&cfg, key).await {
            eprintln!("[tunnel] err: {:?}", e);
        }
    });

    // 3) 等隧道起来，agent 连 7101 发 ping
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let mut agent = tokio::net::TcpStream::connect("127.0.0.1:7201").await?;
    let req = "{\"id\":\"t1\",\"kind\":\"ping\"}\n";
    agent.write_all(req.as_bytes()).await?;
    let mut line = String::new();
    let mut r = BufReader::new(agent);
    r.read_line(&mut line).await?;
    println!("[agent] got response: {}", line.trim());

    bridge.abort();
    t.abort();
    println!("SELFTEST-OK");
    Ok(())
}
