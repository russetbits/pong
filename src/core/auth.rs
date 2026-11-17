// 应用层认证（外审 P0-1；用户安全定案 2026-11-17）：token **纯内存**不落盘——
// generate() 生成后仅存 OnceLock 内存（daemon 生命周期有效），stdout 打印交给用户转交 agent；
// 重启后 token 失效需重新生成（安全性优先于便利性：temp 目录落盘有被其他本机进程读取风险）。
// 隧道内传输已由 ssh 加密保护，v1 逐请求比对 token；连续失败断连冷却在协议层。
use std::sync::OnceLock;

static TOKEN: OnceLock<Option<String>> = OnceLock::new();

/// 读 token（缓存）：None=未启用认证；Some(t)=启用。**纯内存**——只反映 generate() 注入的值。
pub fn token() -> Option<String> {
    TOKEN.get().cloned().flatten()
}

// ---- pong 动态配对码（用户定案 2026-11-17）：会话握手——agent 首条消息带 pair 绑定连接 ----
static PAIR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// 注入/轮换本会话配对码（connect 启动、repick 重生成——OnceLock 不能二次 set，repick 静默失败实锤过）
pub fn set_pair(code: &str) {
    if let Ok(mut g) = PAIR.lock() { *g = Some(code.to_string()); }
}

/// 读当前配对码（None=未启用 pair 握手）
pub fn pair() -> Option<String> {
    PAIR.lock().ok().and_then(|g| g.clone())
}

/// 生成 6 字符配对码（大写字母+数字去易混字符）
pub fn gen_pair() -> std::io::Result<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789"; // 去 I/L/O/0/1
    let mut buf = [0u8; 6];
    fill_random(&mut buf)?;
    Ok(buf.iter().map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char).collect())
}

/// 生成 32 字节随机 token（hex 64 字符）注入内存并返回；不落盘。
/// 外审 2 轮 P1：无降级路径——随机源失败=硬失败（可预测 token 比失败更危险）
pub fn generate() -> std::io::Result<String> {
    let mut buf = [0u8; 32];
    fill_random(&mut buf)?;
    let hex: String = buf.iter().map(|b| format!("{:02x}", b)).collect();
    let _ = TOKEN.set(Some(hex.clone()));
    Ok(hex)
}

/// 平台随机源：Windows=BCryptGenRandom；Unix=/dev/urandom（失败=Err，无降级）
fn fill_random(buf: &mut [u8]) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    unsafe {
        // BCryptGenRandom(NULL, pbBuffer, cbBuffer, BCRYPT_USE_SYSTEM_PREFERRED_RNG=0x2)
        extern "system" {
            fn BCryptGenRandom(
                halg: *mut core::ffi::c_void,
                pbbuffer: *mut u8,
                cbbuffer: u32,
                flags: u32,
            ) -> i32;
        }
        let r = BCryptGenRandom(std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32, 0x2);
        if r != 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, format!("BCryptGenRandom fail {r:#x}")));
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        use std::io::Read;
        let mut f = std::fs::File::open("/dev/urandom")
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("open /dev/urandom: {e}")))?;
        f.read_exact(buf)?;
    }
    Ok(())
}

/// 请求认证检查：未启用=放行；启用=逐请求常量时间比对（防时序侧信道，外审 2 轮 P1）
pub fn check(provided: Option<&str>) -> bool {
    match (token(), provided) {
        (None, _) => true, // 未启用
        (Some(t), Some(p)) => const_eq(&t, p),
        _ => false,
    }
}

/// 常量时间字节比较（长度不同也走满循环，无早退）
pub fn const_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i % a.len().max(1)).copied().unwrap_or(0);
        let y = b.get(i % b.len().max(1)).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    diff == 0
}

// ---------- v2 HMAC 请求认证（docs/design-hmac-v2.md；外审 2 轮双模型一致方案） ----------

/// v2 请求字段（Task 顶层 ts/nonce/body_hash/sig）
pub struct V2Sig {
    pub ts: i64,
    pub nonce: String,
    pub body_hash: String,
    pub sig: String,
}

/// 验证 v2 签名：返回 Ok(()) 或拒因（本地系统时间）
/// sig = hex(HMAC-SHA256(key=token, msg="v2|" + kind + "|" + body_hash + "|" + ts + "|" + nonce))
pub fn verify_v2(
    token: &str,
    kind: &str,
    body: &str,
    s: &V2Sig,
    nonce_seen: &mut std::collections::HashMap<String, i64>,
) -> Result<(), String> {
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};
    type HmacSha256 = Hmac<Sha256>;
    // ① 时钟窗口 ±120s（同机/局域网）
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if (now - s.ts).abs() > 120 {
        return Err(format!("v2 ts out of window (delta {}s)", now - s.ts));
    }
    // ② nonce 窗口内唯一（240s 过期；内存态不持久化——设计文档声明）
    if nonce_seen.contains_key(&s.nonce) {
        return Err("v2 nonce replay".into());
    }
    // ③ body_hash 一致性（先比哈希再验签，防 body 篡改+快速失败）
    // body=请求体**去掉签名字段后的规范化 JSON**（键排序无空白）——sig 在行内，不能对整行自哈希
    let obj: serde_json::Value = serde_json::from_str(body).map_err(|e| format!("v2 body parse: {e}"))?;
    let mut canon = std::collections::BTreeMap::new();
    if let Some(map) = obj.as_object() {
        for (k, val) in map {
            if k != "ts" && k != "nonce" && k != "body_hash" && k != "sig" && k != "token" {
                canon.insert(k.clone(), val.clone());
            }
        }
    }
    let canon_str = serde_json::to_string(&canon).map_err(|e| format!("v2 canon: {e}"))?;
    let bh = hex_lower(&Sha256::digest(canon_str.as_bytes()));
    if !const_eq(&bh, &s.body_hash) {
        return Err(format!("v2 body hash mismatch (want={} got={}; canon={:?})", s.body_hash, bh, canon_str));
    }
    // ④ sig 常量时间验证
    let msg = format!("v2|{}|{}|{}|{}", kind, s.body_hash, s.ts, s.nonce);
    let mut mac = <HmacSha256 as Mac>::new_from_slice(token.as_bytes())
        .map_err(|e| format!("v2 hmac init: {e}"))?;
    mac.update(msg.as_bytes());
    let want = hex_lower(&mac.finalize().into_bytes());
    if !const_eq(&want, &s.sig) {
        return Err("v2 sig mismatch".into());
    }
    let exp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64 + 240)
        .unwrap_or(0);
    nonce_seen.insert(s.nonce.clone(), exp);
    Ok(())
}

fn hex_lower(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

/// nonce 清理（每 N 请求调一次或定时——简化：验证入口每次顺手清理过期，
/// nonce 存 expiry 时间戳版见 protocol.rs 的 NONCE_SEEN）
pub fn nonce_gc(nonce_seen: &mut std::collections::HashMap<String, i64>) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    nonce_seen.retain(|_, exp| *exp > now);
}

/// GUI/CLI 显示当前 token（脱敏：前 8 后 4）
pub fn masked() -> String {
    match token() {
        None => "(not enabled)".into(),
        Some(t) => format!("{}...{}", &t[..8.min(t.len())], &t[t.len().saturating_sub(4)..]),
    }
}
