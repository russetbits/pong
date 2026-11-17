#!/ssh config 解析：CLI 连接参数直接复用 ~/.ssh/config（用户定案 2026-11-17——不引入 config.ini）
//! 支持 OpenSSH 常用子集：Host（含通配 *）/HostName/User/Port/IdentityFile；
//! Host 模式匹配按 ssh 语义：**后出现的条目优先**（首个匹配到的参数值生效）。
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone)]
pub struct SshTarget {
    pub host: String,           // HostName 值（缺省=pattern 本身）
    pub user: Option<String>,   // User
    pub port: u16,              // Port（缺省 22）
    pub identity: Option<String>, // IdentityFile（可多条=候选列表，这里存首条）
}

/// 解析 ~/.ssh/config，对 target（user@host 形式或纯 host）返回合并后的连接参数。
/// user@host 拆分：user 显式优先于 config 条目。
pub fn lookup(target: &str) -> Result<SshTarget, String> {
    let (cli_user, hostpat) = match target.split_once('@') {
        Some((u, h)) if !u.is_empty() => (Some(u.to_string()), h.to_string()),
        _ => (None, target.to_string()),
    };
    if hostpat.is_empty() {
        return Err("usage: pong user@host".into());
    }
    let mut t = SshTarget { host: hostpat.clone(), port: 22, ..Default::default() };
    // config 条目合并（首个匹配 pattern 的参数生效——ssh 语义：文件内先到先得）
    if let Some(text) = read_config() {
        let mut cur_patterns: Vec<String> = Vec::new();
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') { continue; }
            let Some((k, v)) = split_kv(line) else { continue };
            let kl = k.to_ascii_lowercase();
            match kl.as_str() {
                "host" => {
                    cur_patterns = v.split_whitespace().map(|s| s.to_string()).collect();
                }
                "hostname" => {
                    if matches(&cur_patterns, &hostpat) && t.host == hostpat {
                        t.host = v;
                    }
                }
                "user" => {
                    if matches(&cur_patterns, &hostpat) && cli_user.is_none() && t.user.is_none() {
                        t.user = Some(v);
                    }
                }
                "port" => {
                    if matches(&cur_patterns, &hostpat) {
                        if let Ok(p) = v.parse::<u16>() { t.port = p; }
                    }
                }
                "identityfile" => {
                    if matches(&cur_patterns, &hostpat) && t.identity.is_none() {
                        t.identity = Some(expand_tilde(&v));
                    }
                }
                _ => {} // Ignore: 未支持的指令跳过（ProxyJump/ForwardAgent 等后续再加）
            }
        }
    }
    t.user = cli_user.or(t.user);
    Ok(t)
}

/// GUI/CLI 枚举：~/.ssh/config 里全部 Host 条目（排除通配 *）；每条附 User/Port 预填值。
/// 同名多条后写覆盖先写（取每模式最后一次出现的参数）。
pub struct HostEntry { pub pattern: String, pub user: Option<String>, pub port: u16, pub identity: Option<String> }
pub fn list_hosts() -> Vec<HostEntry> {
    let mut out: Vec<HostEntry> = Vec::new();
    let Some(text) = read_config() else { return out };
    let mut cur: Option<usize> = None; // out 中当前条目索引
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        let Some((k, v)) = split_kv(line) else { continue };
        match k.to_ascii_lowercase().as_str() {
            "host" => {
                // 多 pattern 一行（"host a b"）逐个开条目；通配 * 不进列表
                cur = None;
                for pat in v.split_whitespace() {
                    if pat == "*" || pat.contains('*') { continue; }
                    out.push(HostEntry { pattern: pat.to_string(), user: None, port: 22, identity: None });
                    cur = Some(out.len() - 1);
                }
            }
            "user" => if let Some(i) = cur { out[i].user = Some(v); },
            "port" => if let Some(i) = cur { if let Ok(p) = v.parse::<u16>() { out[i].port = p; } },
            "identityfile" => if let Some(i) = cur { out[i].identity = Some(expand_tilde(&v)); },
            _ => {}
        }
    }
    out
}

fn matches(patterns: &[String], host: &str) -> bool {
    patterns.iter().any(|p| {
        if p == "*" { return true; }
        if let Some(pre) = p.strip_suffix('*') { return host.starts_with(pre); }
        if let Some(suf) = p.strip_prefix('*') { return host.ends_with(suf); }
        p == host
    })
}

fn split_kv(line: &str) -> Option<(String, String)> {
    // OpenSSH 允许 key=value 与 key value 两种分隔
    if let Some(eq) = line.find('=') {
        let k = line[..eq].trim();
        let v = line[eq + 1..].trim();
        return Some((k.to_string(), v.to_string()));
    }
    let mut it = line.splitn(2, char::is_whitespace);
    let k = it.next()?.trim();
    let v = it.next()?.trim();
    if k.is_empty() || v.is_empty() { None } else { Some((k.to_string(), v.to_string())) }
}

fn expand_tilde(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
        if let Ok(home) = std::env::var("USERPROFILE") {
            return format!("{home}/{rest}");
        }
    }
    p.to_string()
}

fn read_config() -> Option<String> {
    // ~ 扩展：HOME（Linux）/USERPROFILE（Windows）+ .ssh/config
    let home = std::env::var("HOME").ok()
        .or_else(|| std::env::var("USERPROFILE").ok())?;
    let p = Path::new(&home).join(".ssh").join("config");
    std::fs::read_to_string(p).ok()
}
