// 二分 3：pong 的 runtime 形态（std::thread + current_thread rt）。
use std::sync::Arc;
use tokio::sync::mpsc;

struct FwdHandler {
    tx: mpsc::Sender<(russh::Channel<russh::client::Msg>, u32)>,
}
impl russh::client::Handler for FwdHandler {
    type Error = russh::Error;
    async fn check_server_key(&mut self, _k: &russh::keys::PublicKeyOrCertificate) -> Result<bool, Self::Error> { Ok(true) }
    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<russh::client::Msg>,
        _connected_addr: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: russh::client::ChannelOpenHandle,
        _session: &mut russh::client::Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        let _ = self.tx.send((channel, connected_port)).await;
        Ok(())
    }
}

fn main() {
    let host = std::env::args().nth(1).unwrap_or("127.0.0.1".into());
    let port: u16 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(22);
    let fwd_port: u32 = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(45683);
    // **pong 形态**：std::thread + current_thread rt
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio");
        rt.block_on(async move {
            let mut cfg0 = russh::client::Config::default();
            cfg0.inactivity_timeout = None.into();
            cfg0.keepalive_interval = std::time::Duration::from_secs(30).into();
            let config = Arc::new(cfg0);
            let (tx, mut rx) = mpsc::channel(16);
            let mut h = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                russh::client::connect(config, (host.as_str(), port), FwdHandler { tx }),
            ).await.expect("connect timeout").expect("connect");
            let keydata = std::fs::read(std::env::var("HOME").unwrap()+"/.ssh/id_ed25519").expect("key");
            let key = russh::keys::PrivateKey::from_openssh(&keydata).expect("decode");
            let hash = h.best_supported_rsa_hash().await.ok().flatten().flatten();
            let auth = h.authenticate_publickey("user", russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), hash)).await.expect("auth");
            println!("auth success={}", auth.success());
            match h.tcpip_forward("127.0.0.1", fwd_port).await {
                Ok(p) => println!("forward OK bound={p}"),
                Err(e) => println!("forward DENIED: {e:?}"),
            }
            let _ = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv()).await;
            println!("done");
        });
    });
    let _ = handle.join();
}
