#!/exec/adb（跨平台 spawn；Windows 分支见 win）
use std::time::{Instant, Duration};
use std::io::Read;
use std::process::Command;
use crate::core::protocol::{Task, Res};
use crate::core::log::*;

/// 收 stdout/stderr 读取线程的结果（join 会等 EOF；进程被杀后管道关闭即返回）
fn collect(out_th: Option<std::thread::JoinHandle<Vec<u8>>>,
           err_th: Option<std::thread::JoinHandle<Vec<u8>>>) -> String {
    let mut out = String::new();
    if let Some(h) = out_th { if let Ok(b) = h.join() { out.push_str(&String::from_utf8_lossy(&b)); } }
    if let Some(h) = err_th { if let Ok(b) = h.join() { out.push_str(&String::from_utf8_lossy(&b)); } }
    out
}

pub fn run_exec(t: &Task) -> Res {
    let start = Instant::now();
    // 命令来源：显式 cmd 字段优先；否则 args[0]=整条命令行（回归/agent 常用），args[1..] 为其参数
    // args[0] 含空格=整条命令行，按首词拆程序名与参数（曾实锤：整串当程序名 spawn 报 os error 3）
    let (cmd, rest): (String, Vec<String>) = if let Some(c) = t.cmd.clone() {
        (c, t.args.clone())
    } else if let Some(first) = t.args.first().cloned() {
        if first.contains(char::is_whitespace) {
            let mut it = first.split_whitespace();
            let head = it.next().unwrap_or("adb").to_string();
            (head, it.map(String::from).chain(t.args.iter().skip(1).cloned()).collect())
        } else {
            (first, t.args.iter().skip(1).cloned().collect())
        }
    } else {
        ("adb".into(), Vec::new())
    };
    let timeout = Duration::from_millis(t.timeout_ms);
    log(&format!("exec {cmd} {:?}", rest));

    let mut command = Command::new(&cmd);
    command.args(&rest);
    #[cfg(target_os = "windows")]
    {
        // CREATE_NO_WINDOW = 0x08000000；CREATE_SUSPENDED = 0x4（外审 2 轮 P1：关 spawn→Assign 逃逸窗口）
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000 | 0x4);
    }
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    // 注：piped 后必须在 spawn 的第一时间起读取线程（见下）——Windows 匿名管道默认缓冲仅 4KB、
    // Linux 64KB；子进程输出超过缓冲就阻塞在 write，而主线程在 try_wait 轮询 → 经典死锁，
    // 只能等到 timeout 被杀（2026-11-25 实测：adb push 64MB 进度输出 + adb shell md5sum 均踩）

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => return Res { id: t.id.clone(), ok: false, exit_code: None,
            output: format!("spawn fail: {e}"), ms: start.elapsed().as_millis() },
    };
    log(&format!("exec: spawned pid={:?}", child.id()));
    // Windows：进程树清理（外审 P0）——child 入 Job，超时/收尾 TerminateJobObject 全杀
    // spawn 即 SUSPENDED：先 Assign 进 job 再 resume（关逃逸窗口）
    #[cfg(target_os = "windows")]
    let job = {
        use std::os::windows::io::AsRawHandle;
        let j = crate::core::execwin::job_assign(child.as_raw_handle());
        log("exec: job assigned");
        let rres = crate::core::execwin::resume_child(child.id());
        log(&format!("exec: resumed rc={rres} (0=running 1=ok -1=err)"));
        j
    };
    // 立刻接管 stdout/stderr：独立线程持续读到 EOF，避免管道缓冲满导致子进程阻塞（死锁根因）
    let out_th = child.stdout.take().map(|mut s| std::thread::spawn(move || {
        let mut b = Vec::new(); let _ = s.read_to_end(&mut b); b }));
    let err_th = child.stderr.take().map(|mut s| std::thread::spawn(move || {
        let mut b = Vec::new(); let _ = s.read_to_end(&mut b); b }));

    let deadline = Instant::now() + timeout;
    let code = loop {
        match child.try_wait() {
            Ok(Some(c)) => break Some(c.code().unwrap_or(-1)),
            Ok(None) => {
                if Instant::now() > deadline {
                    // 全树杀：Job 优先（孙进程也死），单进程 kill 兜底
                    #[cfg(target_os = "windows")]
                    if let Some(j) = &job { j.kill_tree(); }
                    let _ = child.kill();
                    let _ = child.wait();
                    let partial = collect(out_th, err_th);
                    return Res { id: t.id.clone(), ok: false, exit_code: None,
                        output: format!("timeout after {}ms (pass timeout_ms to extend); partial output: {}",
                            t.timeout_ms, trunc(&partial)),
                        ms: start.elapsed().as_millis() };
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Res { id: t.id.clone(), ok: false, exit_code: None,
                output: format!("wait fail: {e}"), ms: start.elapsed().as_millis() },
        }
    };
    // 正常收尾也补一刀（幂等）：清 cmd /c 后台化孙进程残留
    #[cfg(target_os = "windows")]
    if let Some(j) = &job { j.kill_tree(); }
    let mut out = collect(out_th, err_th);
    if out.trim().is_empty() { out = "(no output)".into(); }
    Res { id: t.id.clone(), ok: code.unwrap_or(-1) == 0, exit_code: code,
        output: trunc(&out), ms: start.elapsed().as_millis() }
}
