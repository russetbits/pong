// 协议层全 kind 自测（跨平台）：直接调 tcp_respond 分发，不依赖真机串口
// 覆盖：list/ping/exec/read/write/stat/ls/chdir（serial* 在 Linux 为 stub，仅验证不 panic）
use pong::core::config::Bridge;
use pong::core::protocol::tcp_respond;
use std::path::PathBuf;
use std::fs;

fn resp(b: &Bridge, line: &str) -> String {
    tcp_respond(b, line)
}

fn main() {
    // 假桥工作目录（in/out/done/log 布局供文件类 kind 用）
    let dir = std::env::temp_dir().join("ad-proto-test");
    let _ = fs::remove_dir_all(&dir);
    for d in ["in", "out", "done", "log"] { fs::create_dir_all(dir.join(d)).unwrap(); }
    let b = Bridge { dir: PathBuf::from(&dir) };

    let mut pass = 0u32; let mut fail = 0u32;
    macro_rules! check {
        ($name:expr, $req:expr, $expect:expr) => {
            let r = resp(&b, $req);
            let ok = r.contains($expect);
            println!("{:>2} {} -> {}", if ok { "OK" } else { "FAIL" }, $name, r);
            if ok { pass += 1 } else { fail += 1 }
        };
    }

    check!("ping",   r#"{"id":"1","kind":"ping"}"#,                      r#""output":"pong""#);
    check!("list",   r#"{"id":"2","kind":"list"}"#,                      "exec");
    check!("exec",   r#"{"id":"3","kind":"exec","cmd":"echo","args":["hi"]}"#, "hi");
    check!("write",  r#"{"id":"4","kind":"write","args":["test.txt"],"data":"aGk="}"#, r#""ok":true"#); // b64 "hi"
    check!("read",   r#"{"id":"5","kind":"read","args":["test.txt"]}"#,    r#""output":"aGk="#);
    check!("stat",   r#"{"id":"6","kind":"stat","args":["test.txt"]}"#,    r#""ok":true"#);
    check!("ls",     r#"{"id":"7","kind":"ls","path":"."}"#,             "test.txt");
    check!("chdir",  r#"{"id":"8","kind":"chdir","args":["."]}"#,          r#""ok":true"#);
    check!("serial", r#"{"id":"9","kind":"serial","port":"/dev/null","send":"x","read_ms":50}"#, "\"ok\":");
    check!("bad",    r#"{"id":"10","kind":"nonexistent"}"#,              "unknown kind");

    println!("\nPROTOCOL-SELFTEST: {} pass, {} fail", pass, fail);
    if fail > 0 { std::process::exit(1); }
    println!("SELFTEST-OK");
}
