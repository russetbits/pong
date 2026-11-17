// 编译期生成版本信息：git short hash + UTC 构建时间（OUT_DIR/version_info.rs）
use std::process::Command;
use std::env;

fn main() {
    // 不声明 rerun-if：cargo 默认每次编译都重跑本脚本（git hash/时间每次刷新；
    // 曾实锤：rerun-if-changed=.git/HEAD 不生效——HEAD 内容 "ref: refs/heads/main" 不随提交变）

    let git_hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());

    // git 脏标记：源码与 HEAD 不一致时标注 dirty
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    let time = time_utc();

    let out = env::var("OUT_DIR").unwrap_or_default();
    std::fs::write(
        format!("{}/version_info.rs", out),
        format!(
            "pub const BUILD_GIT: &str = \"{}{}\";\npub const BUILD_TIME: &str = \"{}\";\n",
            git_hash,
            if dirty { "-dirty" } else { "" },
            time
        ),
    )
    .expect("write version_info.rs");
}

fn time_utc() -> String {
    // 不引 chrono：用 libc-free 的简易法——RUSTC 编译时刻从 SystemTime 格式化
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 简易 UTC 转换（天数->ymd，够用不引库）
    let days = now / 86400;
    let secs = now % 86400;
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

// Howard Hinnant civil_from_days 算法（公历）
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe/1460 + doe/36524 - doe/146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365*yoe + yoe/4 - yoe/100);
    let mp = (5*doy + 2)/153;
    let d = (doy - (153*mp+2)/5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
