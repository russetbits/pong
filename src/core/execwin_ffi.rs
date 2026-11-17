// Windows FFI（exec Job Object 专用，core 内独立小声明避免依赖 win::winffi）
#![cfg(target_os = "windows")]

pub type HANDLE = *mut core::ffi::c_void;

#[link(name = "kernel32")]
extern "system" {
    pub fn CreateJobObjectW(attrs: *const core::ffi::c_void, name: *const u16) -> HANDLE;
    pub fn AssignProcessToJobObject(job: HANDLE, proc_handle: HANDLE) -> i32;
    pub fn TerminateJobObject(job: HANDLE, exit_code: u32) -> i32;
    pub fn CloseHandle(h: HANDLE) -> i32;
    pub fn ResumeThread(h: HANDLE) -> u32;
    pub fn SetInformationJobObject(
        job: HANDLE,
        class: i32, // JobObjectExtendedLimitInformation = 9
        info: *const core::ffi::c_void,
        size: u32,
    ) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    // Toolhelp 线程枚举（resume_child 专用：std Child 只给进程句柄，主线程句柄需枚举拿）
    pub fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> HANDLE;
    pub fn Thread32First(h: HANDLE, e: *mut core::ffi::c_void) -> i32;
    pub fn Thread32Next(h: HANDLE, e: *mut core::ffi::c_void) -> i32;
    pub fn OpenThread(access: u32, inherit: i32, tid: u32) -> HANDLE;
}
