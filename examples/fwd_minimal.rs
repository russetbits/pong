// 最小 -R 复现：connect→auth→tcpip_forward→sleep 3s→exit。隔离 tunnel_once 复杂度。
use std::sync::Arc;

struct NoCheck;
impl russh::client::Handler for NoCheck {
    type Error = russh::Error;
    async fn check_server_key(&mut self, _k: &russh::keys::PublicKeyOrCertificate) -> Result<bool, Self::Error> { Ok(true) }
}

#[tokio::main]
async fn main() {
    let host = std::env::args().nth(1).unwrap_or("127.0.0.1".into());
    let port: u16 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(22);
    let fwd_port: u32 = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(45677);
    let mut cfg0 = russh::client::Config::default();
    cfg0.inactivity_timeout = None.into();
    cfg0.keepalive_interval = std::time::Duration::from_secs(30).into();
    let config = Arc::new(cfg0);
    let mut h = russh::client::connect(config, (host.as_str(), port), NoCheck).await.expect("connect");
    let keydata = std::fs::read(std::env::var("HOME").unwrap()+"/.ssh/id_ed25519").expect("key");
    let key = russh::keys::PrivateKey::from_openssh(&keydata).expect("decode");
    let hash = h.best_supported_rsa_hash().await.ok().flatten().flatten();
    let auth = h.authenticate_publickey("user", russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), hash)).await.expect("auth");
    println!("auth success={}", auth.success());
    match h.tcpip_forward("127.0.0.1", fwd_port).await {
        Ok(p) => println!("forward OK bound={p}"),
        Err(e) => println!("forward DENIED: {e:?}"),
    }
}
