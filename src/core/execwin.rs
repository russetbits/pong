// Windows 专属：exec 子进程的 Job Object 进程树清理（外审 P0：cmd /c 孙进程残留）
// 设计：每次 exec 一个 Job；child 用 raw handle Assign 进 job；
// 超时/收尾 TerminateJobObject（幂等：正常结束后 job 内已无进程，调用无害）。
#![cfg(target_os = "windows")]

use crate::core::execwin_ffi::*;

pub struct ExecJob {
    handle: HANDLE,
}

/// 把子进程（raw HANDLE）加入新 Job；失败返回 None（降级：无 job，仅单进程 kill）
/// 外审 2 轮 P1：KILL_ON_JOB_CLOSE——daemon 崩溃时 job handle 关闭自动杀全树
pub fn job_assign(child_raw: *mut core::ffi::c_void) -> Option<ExecJob> {
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return None;
        }
        // JOBOBJECT_EXTENDED_LIMIT_INFORMATION{0}: LimitFlags=JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE(0x2000)
        // 结构体其余字段全 0（64 位下 144 字节；LimitFlags 偏移 16）
        let mut info = [0u8; 144];
        info[16..20].copy_from_slice(&0x2000u32.to_le_bytes());
        if SetInformationJobObject(job, 9, info.as_ptr() as *const core::ffi::c_void, info.len() as u32) == 0 {
            // 设置失败不致命（降级为无 KILL_ON_CLOSE，仍可用）
        }
        if AssignProcessToJobObject(job, child_raw) == 0 {
            CloseHandle(job);
            return None;
        }
        Some(ExecJob { handle: job })
    }
}

/// 恢复 CREATE_SUSPENDED 起的子进程（外审 2 轮 P1：关 spawn→Assign 逃逸窗口）
/// 2026-11-17 实锤修复：旧版拿**进程句柄**调 ResumeThread（需线程句柄）→ 返回 -1 被忽略
/// → 子进程永远 SUSPENDED → exec 全部卡到 60s 超时（Windows 回归 16/25 的根因）。
/// 正路：Toolhelp 枚举该 pid 的线程（快照序=创建序，首个命中=主线程）→ OpenThread → ResumeThread。
/// 返回 ResumeThread 结果（0=已在运行 1=恢复成功 (u32)-1=错误）
pub fn resume_child(pid: u32) -> u32 {
    const TH32CS_SNAPTHREAD: u32 = 0x4;
    const THREAD_SUSPEND_RESUME: u32 = 0x0002;
    // THREADENTRY32（x86/x64 同 28 字节：7×DWORD）
    #[repr(C)]
    struct ThreadEntry {
        dw_size: u32,
        cnt_usage: u32,
        th32_thread_id: u32,
        th32_owner_process_id: u32,
        tp_base_pri: i32,
        tp_delta_pri: i32,
        dw_flags: u32,
    }
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snap.is_null() || snap as usize == usize::MAX {
            return u32::MAX;
        }
        let mut e = ThreadEntry { dw_size: 28, cnt_usage: 0, th32_thread_id: 0, th32_owner_process_id: 0, tp_base_pri: 0, tp_delta_pri: 0, dw_flags: 0 };
        let mut ret = u32::MAX;
        if Thread32First(snap, &mut e as *mut ThreadEntry as *mut core::ffi::c_void) != 0 {
            loop {
                if e.th32_owner_process_id == pid {
                    let th = OpenThread(THREAD_SUSPEND_RESUME, 0, e.th32_thread_id);
                    if !th.is_null() {
                        ret = ResumeThread(th);
                        CloseHandle(th);
                    }
                    break; // 快照序=创建序：首个命中即主线程
                }
                if Thread32Next(snap, &mut e as *mut ThreadEntry as *mut core::ffi::c_void) == 0 { break; }
            }
        }
        CloseHandle(snap);
        ret
    }
}

impl ExecJob {
    /// 终止整棵进程树（幂等）
    pub fn kill_tree(&self) {
        unsafe {
            let _ = TerminateJobObject(self.handle, 1);
        }
    }
}

impl Drop for ExecJob {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}
