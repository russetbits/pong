#!/配置：IniConfig/EXAMPLE_INI/resolve/expand_env/exe_dir/find_work_dir/Bridge
pub struct Bridge {
    pub dir: PathBuf,
}

impl Bridge {
    pub fn in_dir(&self) -> PathBuf { self.dir.join("in") }
    pub fn out_dir(&self) -> PathBuf { self.dir.join("out") }
    pub fn done_dir(&self) -> PathBuf { self.dir.join("done") }
    pub fn log_dir(&self) -> PathBuf { self.dir.join("log") }
}
use std::path::{Path, PathBuf};
use std::fs;
use std::sync::OnceLock;

#[derive(Clone)]
pub struct IniConfig {
    pub bridge_dir: Option<String>,   // 桥工作目录（旧第1行 bridge_dir=）
    pub port: u16,                    // TCP 端口（旧第2行；默认 7101）
    pub ssh_host: String,             // ssh 目标（旧第3行；默认 localhost 回环，实际部署在 ini 写真实主机）
    pub cache_dir: String,            // 运行缓存目录（旧第4行；相对 exe 目录，默认 cache）
    pub serial_port: String,          // serial 缺省 COM 口（默认 COM3）
    pub serial_baud: u32,             // serial 缺省波特率（默认 921600）
    pub ssh_port: Option<u16>,        // sshd 端口（embedded 模式用；默认 22）
    pub ssh_user: Option<String>,     // ssh 登录用户（embedded 模式用；默认 %USERNAME%）
    pub ssh_key: Option<String>,      // 私钥路径（embedded 模式用；默认 %USERPROFILE%\.ssh\id_rsa）
    // R5 通用反向代理：forwards=remote:local,remote:local（[tunnel] 段；空=单端口兼容）
    pub forwards: Vec<(u16, u16)>,
}

/// Save 前轻量校验：解析整篇 ini（分段+扁平兼容），报告非法数字/缺失项。
/// 返回 (errors, warnings)：errors 阻断 Save，warnings 只提示（如 key 路径暂不存在）。
pub fn validate_text(text: &str) -> (Vec<String>, Vec<String>) {
    let mut errors: Vec<String> = Vec::new();
    let mut kv_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut section = String::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') { continue; }
        if line.starts_with('[') && line.ends_with(']') {
            let s = line[1..line.len() - 1].trim().to_ascii_lowercase();
            if !matches!(s.as_str(), "tunnel" | "serial" | "bridge") {
                warnings.push(format!("L{}: 未知段 [{}]（将被忽略）", i + 1, s));
            }
            section = s;
            continue;
        }
        if let Some(eq) = line.find('=') {
            let (k, v) = (line[..eq].trim(), line[eq + 1..].trim());
            let key = match (section.as_str(), k) {
                ("tunnel", "listen_port") | ("tunnel", "port") => "port",
                ("serial", "port") => "serial_port",
                ("serial", "baud") => "serial_baud",
                (_, kk) => kk,
            };
            kv_map.insert(key.to_string(), v.to_string());
            match key {
                "port" | "ssh_port" | "serial_baud" => {
                    if v.parse::<u32>().is_err() {
                        errors.push(format!("L{}: {}='{}' 不是合法数字", i + 1, k, v));
                    }
                }
                "forwards" => {
                    if !v.is_empty() && v.split(',').any(|pair| pair.trim().split_once(':')
                        .map(|(a, b)| a.trim().parse::<u16>().is_err() || b.trim().parse::<u16>().is_err())
                        .unwrap_or(true)) {
                        errors.push(format!("L{}: forwards 格式应为 remote:local,remote:local", i + 1));
                    }
                    // 外审：本地控制面端口（port+1）不得被映射（防远程经 forwards 触达 shutdown 面）
                    if let Some(pv) = kv_map.get("port") {
                        if let Ok(port) = pv.parse::<u16>() {
                            let ctl = port.saturating_add(1);
                            if v.split(',').any(|pair| pair.trim().split_once(':')
                                .map(|(_, b)| b.trim().parse::<u16>() == Ok(ctl))
                                .unwrap_or(false)) {
                                errors.push(format!("L{}: forwards 不得映射本地控制面端口 {}", i + 1, ctl));
                            }
                        }
                    }
                }
                "ssh_key" => {
                    if !v.is_empty() {
                        let rp = resolve(v);
                        if !rp.exists() {
                            warnings.push(format!("L{}: ssh_key 文件不存在（先启动后放 key 是允许的，隧道会自动重试）: {}", i + 1, rp.display()));
                        }
                    }
                }
                "bridge_dir" | "cache_dir" => {
                    if !v.is_empty() && v.contains('%') && expand_env(v) == v {
                        warnings.push(format!("L{}: {} 含未识别的 %VAR%（保持原样）", i + 1, k));
                    }
                }
                _ => {}
            }
        }
    }
    (errors, warnings)
}

pub static EXE_DIR: OnceLock<PathBuf> = OnceLock::new();
pub static CFG: OnceLock<IniConfig> = OnceLock::new();

/// R6 运行时配置存储：UI apply / CLI config set 都写它；文件只是持久化层。
/// tunnel_forever 每轮读这里（不再轮询 ini 文件；文件变更需显式 reload/导入）。
pub static RUNTIME: std::sync::RwLock<Option<IniConfig>> = std::sync::RwLock::new(None);

pub fn init_runtime(c: IniConfig) {
    *RUNTIME.write().unwrap() = Some(c);
}

pub fn get_runtime() -> Option<IniConfig> {
    RUNTIME.read().unwrap().clone()
}

pub fn apply_runtime(c: IniConfig) {
    *RUNTIME.write().unwrap() = Some(c);
}

/// 运行时配置落盘到 exe 旁 config.ini（key=value upsert，保留注释行）
pub fn save_runtime() -> std::io::Result<PathBuf> {
    let c = get_runtime().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "runtime not init"))?;
    let path = exe_dir().join("config.ini");
    let old = fs::read_to_string(&path).unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = Default::default();
    for line in old.lines() {
        let t = line.trim();
        if !t.starts_with('#') && !t.is_empty() {
            if let Some(eq) = line.find('=') {
                let k = line[..eq].trim().to_string();
                if let Some(v) = field_value(&c, &k) {
                    out.push(format!("{}={}", k, v));
                    seen.insert(k);
                    continue;
                }
                // 未映射字段：原样保留（未知 key 兼容）
            }
        }
        out.push(line.to_string());
    }
    // 新字段追加
    for (k, v) in all_fields(&c) {
        if !seen.contains(k) {
            out.push(format!("{}={}", k, v));
        }
    }
    // 原子写：tmp+rename（std::fs::rename 在 Windows 用 MoveFileExW(REPLACE_EXISTING)，跨平台原子覆盖）
    let tmp = path.with_extension("ini.tmp");
    fs::write(&tmp, out.join("\n") + "\n")?;
    fs::rename(&tmp, &path)?;
    Ok(path)
}

// 字段映射：内部 IniConfig 字段 <-> ini key（扁平名，与 GUI FIELD_KEYS 一致）
fn field_value(c: &IniConfig, k: &str) -> Option<String> {
    Some(match k {
        "port" => c.port.to_string(),
        "bridge_dir" => c.bridge_dir.clone()?,
        "cache_dir" => c.cache_dir.clone(),
        "ssh_host" => c.ssh_host.clone(),
        "ssh_port" => c.ssh_port?.to_string(),
        "ssh_user" => c.ssh_user.clone()?,
        "ssh_key" => c.ssh_key.clone()?,
        "serial_port" => c.serial_port.clone(),
        "serial_baud" => c.serial_baud.to_string(),
        "forwards" => c.forwards.iter().map(|(a, b)| format!("{}:{}", a, b)).collect::<Vec<_>>().join(","),
        _ => return None,
    })
}

fn all_fields(c: &IniConfig) -> Vec<(&'static str, String)> {
    let mut v: Vec<(&'static str, String)> = vec![
        ("port", c.port.to_string()),
        ("cache_dir", c.cache_dir.clone()),
        ("ssh_host", c.ssh_host.clone()),
        ("serial_port", c.serial_port.clone()),
        ("serial_baud", c.serial_baud.to_string()),
    ];
    if let Some(d) = &c.bridge_dir { v.push(("bridge_dir", d.clone())); }
    if let Some(p) = c.ssh_port { v.push(("ssh_port", p.to_string())); }
    if let Some(u) = &c.ssh_user { v.push(("ssh_user", u.clone())); }
    if let Some(k) = &c.ssh_key { v.push(("ssh_key", k.clone())); }
    if !c.forwards.is_empty() {
        v.push(("forwards", c.forwards.iter().map(|(a, b)| format!("{}:{}", a, b)).collect::<Vec<_>>().join(",")));
    }
    v
}

/// 运行时配置 → ini 文本（config get 读运行时用；字段名同旧扁平格式）
pub fn render_runtime(c: &IniConfig) -> String {
    all_fields(c).into_iter().map(|(k, v)| format!("{}={}", k, v)).collect::<Vec<_>>().join("\n")
}

pub const EXAMPLE_INI: &str = "# pong 配置（临时布局：temp/pong/config.ini；Linux=/tmp/pong，Windows=%TEMP%\\pong）\n# 新设计：CLI 连接参数直接复用 ~/.ssh/config（Host/HostName/User/Port/IdentityFile）\n# 本文件仅存托盘 GUI 模式兼容设置（serial 参数已改 agent 请求传参，不进配置）\n\n[tunnel]\n# 桥监听端口（agent 主机 sshd 转发回本机；agent 连 127.0.0.1:<port>）\nlisten_port=7101\n";


pub fn exe_dir() -> PathBuf {
    EXE_DIR.get().cloned().unwrap_or_else(|| PathBuf::from("."))
}

/// pong 固定临时布局：所有配置/缓存/日志/pid/token/主机指纹都在 temp/pong/
/// （用户定案 2026-11-17：不放 exe 旁；Linux=/tmp/pong，Windows=%TEMP%\pong）
pub fn tmp_dir() -> PathBuf {
    std::env::temp_dir().join("pong")
}

/// pid 文件：temp/pong/bridge.pid（用户级单例正确语义）
pub fn pid_path() -> PathBuf {
    tmp_dir().join("bridge.pid")
}

/// host_key.dat（TOFU 指纹）：temp/pong/（指纹是校验数据非凭据，泄露无鉴权风险；
/// Linux /tmp 重启清理=需重新信任，语义如此接受）
pub fn host_key_path() -> PathBuf {
    tmp_dir().join("host_key.dat")
}

// 相对路径 → 相对 exe 目录；绝对路径原样；%VAR% 环境变量展开（Windows 风格，如 %USERPROFILE%\x、%TEMP%\y）
pub fn expand_env(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    for _ in 0..16 { // 最多 16 段，防病态输入
        match rest.find('%') {
            None => { out.push_str(rest); return out; }
            Some(a) => {
                out.push_str(&rest[..a]);
                let after = &rest[a + 1..];
                match after.find('%') {
                    None => { out.push('%'); out.push_str(after); return out; }
                    Some(b) => {
                        let name = &after[..b];
                        if name.is_empty() || name.contains('\\') || name.contains('/') {
                            out.push('%');
                            rest = after;
                        } else {
                            out.push_str(&std::env::var(name).unwrap_or_default());
                            rest = &after[b + 1..];
                        }
                    }
                }
            }
        }
    }
    out.push_str(rest);
    out
}

// 相对路径 → 相对 exe 目录；绝对路径原样
pub fn resolve(p: &str) -> PathBuf {
    let s = expand_env(p);
    let path = Path::new(&s);
    if path.is_absolute() { path.to_path_buf() } else { exe_dir().join(path) }
}

impl IniConfig {
    // 配置查找：exe 旁 config.ini → %USERPROFILE%\pong\config.ini（唯一配置名）
    // 全缺时在 exe 旁生成 config.ini 默认模板并 stderr 报缺（直接编辑重启生效）
    pub fn load(exe_parent: &Path) -> IniConfig {
        let mut text = String::new();
        let mut found_at: Option<PathBuf> = None;
        let cands = [
            // 环境变量优先（测试隔离），固定布局 temp/pong/config.ini（用户定案：配置不放 exe 旁）
            std::env::var("DSH_PONG_CONFIG").ok().map(PathBuf::from).unwrap_or_default(),
            tmp_dir().join("config.ini"),
        ];
        for c in &cands {
            if c.as_os_str().is_empty() { continue; }
            if let Ok(t) = fs::read_to_string(c) {
                text = t;
                found_at = Some(c.clone());
                break;
            }
        }
        if found_at.is_none() {
            let example = tmp_dir().join("config.ini");
            if !example.exists() {
                let _ = fs::create_dir_all(tmp_dir());
                let _ = fs::write(&example, EXAMPLE_INI);
            }
            eprintln!("pong: config.ini not found");
            eprintln!("  checked: {}", cands.iter().filter(|c| !c.as_os_str().is_empty())
                .map(|c| c.display().to_string()).collect::<Vec<_>>().join(", "));
            eprintln!("  config written: {}（直接编辑后重启生效）", example.display());
        }
        IniConfig::parse(&text)
    }

    pub fn parse(text: &str) -> IniConfig {
        let mut c = IniConfig {
            bridge_dir: None, port: 7101, ssh_host: "localhost".into(),
            cache_dir: String::new(), serial_port: "COM3".into(), serial_baud: 921_600,
            ssh_port: None, ssh_user: None, ssh_key: None,
            forwards: Vec::new(),
        };
        let mut positional = 0usize;
        // 分段支持：[tunnel]=ssh 核心，[serial]=串口，[bridge]=桥/缓存目录；无段=旧扁平格式（全部兼容）
        let mut section = String::new(); // "" = 顶层（旧扁平）
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') { continue; }
            if line.starts_with('[') && line.ends_with(']') {
                section = line[1..line.len() - 1].trim().to_ascii_lowercase();
                continue;
            }
            if let Some(eq) = line.find('=') {
                let (k, v) = (line[..eq].trim(), line[eq + 1..].trim());
                // 分段字段名 -> 旧内部字段映射（同名 key 不同段含义不同：port 在 tunnel=桥端口、在 serial=COM 口）
                let key = match (section.as_str(), k) {
                    ("tunnel", "listen_port") | ("tunnel", "port") => "port",
                    ("tunnel", "host") => "ssh_host",
                    ("tunnel", "key") => "ssh_key",
                    ("serial", "port") => "serial_port",
                    ("serial", "baud") => "serial_baud",
                    ("bridge", "work_dir") | ("bridge", "dir") => "bridge_dir",
                    (_, kk) => kk,
                };
                match key {
                    "bridge_dir" | "dir" => if !v.is_empty() { c.bridge_dir = Some(v.into()); },
                    "port" => c.port = v.parse().unwrap_or(c.port),
                    "ssh_host" | "host" => if !v.is_empty() { c.ssh_host = v.into(); },
                    "cache_dir" => if !v.is_empty() { c.cache_dir = v.into(); },
                    "serial_port" => if !v.is_empty() { c.serial_port = v.into(); },
                    "ssh_port" => c.ssh_port = v.parse().ok(),
                    "ssh_user" => if !v.is_empty() { c.ssh_user = Some(v.into()); },
                    "ssh_key" => if !v.is_empty() { c.ssh_key = Some(v.into()); },
                    "serial_baud" => c.serial_baud = v.parse().unwrap_or(c.serial_baud),
                    "forwards" => {
                        c.forwards = v.split(',').filter_map(|pair| {
                            let (a, b) = pair.trim().split_once(':')?;
                            Some((a.trim().parse::<u16>().ok()?, b.trim().parse::<u16>().ok()?))
                        }).collect();
                    }
                    _ => {}
                }
            } else if section.is_empty() {
                // 旧位置式：第1行 dir 第2行 port 第3行 host 第4行 cache
                match positional {
                    0 if !line.is_empty() => c.bridge_dir = Some(line.into()),
                    1 => c.port = line.parse().unwrap_or(c.port),
                    2 => c.ssh_host = line.into(),
                    3 => c.cache_dir = line.into(),
                    _ => {}
                }
                positional += 1;
            }
        }
        c
    }
}

pub fn find_work_dir() -> Option<PathBuf> {
    // 1) 环境变量（测试隔离）2) 固定布局 temp/pong/bridge（用户定案：全部临时产物进 temp/pong）
    if let Ok(d) = std::env::var("PONG_DIR") {
        if Path::new(&d).is_dir() { return Some(PathBuf::from(d)); }
    }
    let d = tmp_dir().join("bridge");
    if fs::create_dir_all(&d).is_ok() { return Some(d); }
    None
}
