//! Bringing the window that hosts a session to the front.
//!
//! A Claude Code session is a console process with no window of its own, so the window to raise
//! belongs to whatever is hosting it — the Claude desktop app, Windows Terminal, a console host.
//! This walks up the process tree from the session pid until it finds a real top-level window.
//!
//! Where several sessions share one host window (the desktop app runs them all as children of a
//! single window), every one of those sessions necessarily raises that same window: which tab it
//! lands on is the host's business, and there is no supported way to ask it. See the limitations
//! in the README.

use std::ptr;

use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GetForegroundWindow, GetWindow, GetWindowLongW, GetWindowTextLengthW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, SW_RESTORE, SetForegroundWindow,
    ShowWindow, WS_EX_TOOLWINDOW,
};

/// How far up the process tree to look before giving up.
const MAX_DEPTH: usize = 5;

/// Ancestors that host everything on the machine. Walking into one of these would raise the shell
/// (or nothing), never the session, so the walk stops here.
const SYSTEM_HOSTS: [&str; 8] = [
    "explorer.exe",
    "svchost.exe",
    "services.exe",
    "wininit.exe",
    "winlogon.exe",
    "sihost.exe",
    "csrss.exe",
    "userinit.exe",
];

const GWL_EXSTYLE: i32 = -20;

#[link(name = "user32")]
unsafe extern "system" {
    /// Not re-exported by `windows-sys` 0.61 under the features this crate enables.
    fn AttachThreadInput(idattach: u32, idattachto: u32, fattach: i32) -> i32;
}

struct Entry {
    pid: u32,
    parent: u32,
    name: String,
}

/// One pass over the process table, so walking a chain does not re-snapshot per level.
fn snapshot() -> Vec<Entry> {
    let mut out = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap.is_null() || snap as isize == -1 {
            return out;
        }

        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(entry.szExeFile.len());
                out.push(Entry {
                    pid: entry.th32ProcessID,
                    parent: entry.th32ParentProcessID,
                    name: String::from_utf16_lossy(&entry.szExeFile[..len]).to_ascii_lowercase(),
                });
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    out
}

/// Collected by [`find_window`] while enumerating.
struct Search {
    want: u32,
    found: HWND,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> i32 {
    unsafe {
        let search = &mut *(lparam as *mut Search);

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid != search.want || IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        // Owned windows are dialogs and popups; tool windows are palettes. Neither is the window a
        // user thinks of as "the app", and a titleless window is not one either.
        if !GetWindow(hwnd, GW_OWNER).is_null()
            || GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW != 0
            || GetWindowTextLengthW(hwnd) == 0
        {
            return 1;
        }

        search.found = hwnd;
        0 // Stop enumerating.
    }
}

fn find_window(pid: u32) -> Option<HWND> {
    let mut search = Search {
        want: pid,
        found: ptr::null_mut(),
    };
    unsafe {
        EnumWindows(Some(enum_proc), &mut search as *mut Search as LPARAM);
    }
    if search.found.is_null() {
        None
    } else {
        Some(search.found)
    }
}

/// Raises a window that has been found. Windows only lets the foreground process hand focus away,
/// and the alert deliberately never takes focus, so this borrows the foreground thread's input
/// queue for the call — the standard way to make `SetForegroundWindow` stick.
fn raise(hwnd: HWND) -> bool {
    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        if SetForegroundWindow(hwnd) != 0 {
            return true;
        }

        let foreground = GetForegroundWindow();
        if foreground.is_null() {
            return false;
        }
        let their_thread = GetWindowThreadProcessId(foreground, ptr::null_mut());
        let our_thread = GetCurrentThreadId();
        if their_thread == 0 || their_thread == our_thread {
            return false;
        }

        AttachThreadInput(our_thread, their_thread, 1);
        let ok = SetForegroundWindow(hwnd) != 0;
        AttachThreadInput(our_thread, their_thread, 0);
        ok
    }
}

/// Hands a `claude://` deep link to the shell, which routes it to the registered handler.
///
/// Best effort by design: while the app's feature flag keeps the handler switched off this does
/// nothing beyond bringing the app forward, and the window raise below is what actually works.
fn open_deep_link(uri: &str) {
    let uri: Vec<u16> = uri.encode_utf16().chain(std::iter::once(0)).collect();
    let verb: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            verb.as_ptr(),
            uri.as_ptr(),
            ptr::null(),
            ptr::null(),
            windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        );
    }
}

/// Brings the window hosting `pid` to the front. False when no window could be attributed to it,
/// which is not worth reporting anywhere: the click simply does nothing, as it did before.
///
/// `deep_link` is tried first where the host can act on one, so that the moment the desktop app's
/// feature flag is enabled a click lands on the exact session rather than merely its window. The
/// raise still runs either way, which is what makes the click work today.
pub fn focus_session(pid: u32, deep_link: Option<&str>) -> bool {
    if let Some(uri) = deep_link {
        open_deep_link(uri);
    }
    if pid == 0 {
        return false;
    }

    if let Some(hwnd) = find_window(pid) {
        return raise(hwnd);
    }

    let processes = snapshot();
    let mut current = pid;
    for _ in 0..MAX_DEPTH {
        let Some(entry) = processes.iter().find(|e| e.pid == current) else {
            return false;
        };
        if entry.parent == 0 || entry.parent == current {
            return false;
        }
        let Some(parent) = processes.iter().find(|e| e.pid == entry.parent) else {
            return false;
        };
        if SYSTEM_HOSTS.contains(&parent.name.as_str()) {
            return false;
        }
        if let Some(hwnd) = find_window(parent.pid) {
            return raise(hwnd);
        }
        current = parent.pid;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_zero_is_never_chased() {
        assert!(!focus_session(0, None));
    }

    /// A pid that cannot exist must fail rather than walk into something arbitrary.
    #[test]
    fn an_impossible_pid_finds_nothing() {
        assert!(!focus_session(u32::MAX - 1, None));
    }

    #[test]
    fn the_process_table_is_readable() {
        let processes = snapshot();
        assert!(!processes.is_empty(), "no processes enumerated");
        assert!(
            processes.iter().any(|e| e.pid == std::process::id()),
            "this test process is missing from the snapshot"
        );
    }

    /// The walk must refuse to climb into the shell, or a click would raise Explorer.
    #[test]
    fn system_hosts_are_excluded() {
        assert!(SYSTEM_HOSTS.contains(&"explorer.exe"));
        assert!(SYSTEM_HOSTS.contains(&"svchost.exe"));
    }
}
