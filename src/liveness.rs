//! Process liveness checks.
//!
//! Claude Code leaves `sessions\<pid>.json` behind when a session is killed rather than exited
//! cleanly, so every file has to be checked against a live `claude.exe` before it is displayed.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};

/// True when `pid` is a running process whose image is `claude.exe`.
///
/// The image-name check guards against pid reuse. If the process exists but its path cannot be
/// read (rare — a protected process holding a recycled pid), we err toward showing the session:
/// a stale row is less harmful here than a missing one.
pub fn is_claude_process(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }

        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(handle);

        if ok == 0 {
            return true;
        }

        let path = OsString::from_wide(&buf[..len as usize])
            .to_string_lossy()
            .to_ascii_lowercase();
        let file = path.rsplit(['\\', '/']).next().unwrap_or("");
        file == "claude.exe" || file == "claude"
    }
}
