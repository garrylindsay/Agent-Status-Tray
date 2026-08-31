//! The desktop alert: a small owner-drawn window in the corner of the work area, plus its sound.
//!
//! This is deliberately a plain Win32 popup rather than a WinRT toast. A toast needs a registered
//! AppUserModelID and a Start-menu shortcut, and it is silently swallowed by Focus Assist and by
//! the per-app notification switches — none of which is wanted for something whose whole job is to
//! be seen. This window is owned by us, so it always shows.
//!
//! It never takes focus (`WS_EX_NOACTIVATE` + `SW_SHOWNOACTIVATE`), so it cannot steal a keystroke
//! from the terminal you are typing in.

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr;

use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
    InvalidateRect, PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, HWND_TOPMOST, IDC_ARROW, KillTimer, LoadCursorW,
    RegisterClassW, SPI_GETWORKAREA, SW_HIDE, SW_SHOWNOACTIVATE, SetTimer, SetWindowPos,
    ShowWindow, SystemParametersInfoW, WM_LBUTTONDOWN, WM_PAINT, WM_TIMER, WNDCLASSW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::config::Sound;

/// Timer that dismisses the popup. Scoped to the popup window, so it cannot collide with the
/// null-hwnd poll timer in `main`.
const DISMISS_TIMER: usize = 1;

/// Wide enough that a typical `name — WAITING 4m · permission prompt` row does not ellipsize.
const WIDTH: i32 = 440;
const PAD: i32 = 14;
const ACCENT_W: i32 = 4;
const TITLE_H: i32 = 24;
const ROW_H: i32 = 20;
const GAP: i32 = 6;
/// Rows beyond this collapse into a "+N more" line, so the alert stays alert-sized.
const MAX_ROWS: usize = 4;

/// Transparent background, so `TRANSPARENT` for `SetBkMode`.
const BK_TRANSPARENT: i32 = 1;

// DrawText flags.
const DT_SINGLELINE: u32 = 0x0020;
const DT_VCENTER: u32 = 0x0004;
const DT_END_ELLIPSIS: u32 = 0x8000;
/// Session names can contain `&`, which DrawText would otherwise eat as a mnemonic.
const DT_NOPREFIX: u32 = 0x0800;

// PlaySound flags.
const SND_ASYNC: u32 = 0x0001;
const SND_NODEFAULT: u32 = 0x0002;
const SND_ALIAS: u32 = 0x0001_0000;

/// The plain system beep, `MessageBeep(MB_OK)`.
const MB_OK: u32 = 0;

#[link(name = "winmm")]
unsafe extern "system" {
    /// Declared here rather than pulling in the `Win32_Media_Audio` feature for one function.
    fn PlaySoundW(pszsound: *const u16, hmod: *mut c_void, fdwsound: u32) -> i32;
}

#[link(name = "user32")]
unsafe extern "system" {
    /// Not re-exported by `windows-sys` 0.61 under the features this crate enables.
    fn MessageBeep(utype: u32) -> i32;
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as COLORREF | ((g as COLORREF) << 8) | ((b as COLORREF) << 16)
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// What the window paints on its next `WM_PAINT`.
#[derive(Default, Clone)]
struct Content {
    title: String,
    rows: Vec<String>,
    overflow: usize,
    accent: (u8, u8, u8),
}

thread_local! {
    static CONTENT: RefCell<Content> = RefCell::new(Content::default());
}

pub struct Popup {
    hwnd: HWND,
}

impl Popup {
    /// Creates the (hidden) alert window. `None` if the class or window cannot be created, which
    /// leaves the rest of the tray working without alerts rather than failing to start.
    pub fn new() -> Option<Popup> {
        unsafe {
            let instance = GetModuleHandleW(ptr::null());
            let class_name = wide("ClaudeTrayAlert");

            let class = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(wnd_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance as _,
                hIcon: ptr::null_mut(),
                hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
                hbrBackground: ptr::null_mut(),
                lpszMenuName: ptr::null(),
                lpszClassName: class_name.as_ptr(),
            };
            // A zero return means the class failed to register; there is no recovery worth making.
            if RegisterClassW(&class) == 0 {
                return None;
            }

            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class_name.as_ptr(),
                wide("Claude sessions").as_ptr(),
                WS_POPUP,
                0,
                0,
                WIDTH,
                120,
                ptr::null_mut(),
                ptr::null_mut(),
                instance as _,
                ptr::null(),
            );
            if hwnd.is_null() {
                return None;
            }
            Some(Popup { hwnd })
        }
    }

    /// Shows (or refreshes) the alert. `duration_secs` of 0 leaves it up until it is clicked.
    pub fn show(
        &self,
        title: &str,
        rows: &[String],
        accent: (u8, u8, u8),
        duration_secs: u64,
        sound: Sound,
    ) {
        let shown: Vec<String> = rows.iter().take(MAX_ROWS).cloned().collect();
        let overflow = rows.len().saturating_sub(shown.len());

        CONTENT.with(|c| {
            *c.borrow_mut() = Content {
                title: title.to_string(),
                rows: shown.clone(),
                overflow,
                accent,
            };
        });

        let body_rows = shown.len() as i32 + if overflow > 0 { 1 } else { 0 };
        let height = PAD + TITLE_H + GAP + body_rows * ROW_H + PAD;

        unsafe {
            let mut work = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            // Falls back to a sane corner if the work area cannot be read.
            let ok = SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                &mut work as *mut RECT as *mut c_void,
                0,
            );
            let (x, y) = if ok != 0 {
                (work.right - WIDTH - 12, work.bottom - height - 12)
            } else {
                (100, 100)
            };

            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                x,
                y,
                WIDTH,
                height,
                // No activation: the alert must never take focus from the terminal.
                0x0010 | 0x0040, // SWP_NOACTIVATE | SWP_SHOWWINDOW
            );
            ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            InvalidateRect(self.hwnd, ptr::null(), 1);

            KillTimer(self.hwnd, DISMISS_TIMER);
            if duration_secs > 0 {
                SetTimer(self.hwnd, DISMISS_TIMER, (duration_secs * 1000) as u32, None);
            }
        }

        play(sound);
    }

    pub fn hide(&self) {
        unsafe {
            KillTimer(self.hwnd, DISMISS_TIMER);
            ShowWindow(self.hwnd, SW_HIDE);
        }
    }
}

/// Plays the configured alert sound. Async, so a slow audio device never stalls the poll loop.
pub fn play(sound: Sound) {
    let alias = match sound {
        Sound::None => return,
        Sound::Default => {
            unsafe { MessageBeep(MB_OK) };
            return;
        }
        Sound::Notification => "Notification.Default",
        Sound::Asterisk => "SystemAsterisk",
        Sound::Exclamation => "SystemExclamation",
        Sound::Hand => "SystemHand",
        Sound::Question => "SystemQuestion",
    };

    unsafe {
        let name = wide(alias);
        let ok = PlaySoundW(
            name.as_ptr(),
            ptr::null_mut(),
            SND_ALIAS | SND_ASYNC | SND_NODEFAULT,
        );
        // `Notification.Default` is not a valid alias on every Windows build; fall back to the
        // classic chime rather than alerting silently.
        if ok == 0 && matches!(sound, Sound::Notification) {
            let fallback = wide("SystemAsterisk");
            PlaySoundW(
                fallback.as_ptr(),
                ptr::null_mut(),
                SND_ALIAS | SND_ASYNC | SND_NODEFAULT,
            );
        }
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                paint(hwnd);
                0
            }
            // Click anywhere to dismiss, like an Outlook alert.
            WM_LBUTTONDOWN => {
                KillTimer(hwnd, DISMISS_TIMER);
                ShowWindow(hwnd, SW_HIDE);
                0
            }
            WM_TIMER if wparam == DISMISS_TIMER => {
                KillTimer(hwnd, DISMISS_TIMER);
                ShowWindow(hwnd, SW_HIDE);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

unsafe fn paint(hwnd: HWND) {
    unsafe {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let hdc = BeginPaint(hwnd, &mut ps);

        let content = CONTENT.with(|c| c.borrow().clone());
        let rect = ps.rcPaint;
        // Repaint the whole card, not just the damaged strip: the layout is cheap and this keeps
        // the accent bar and text from being clipped mid-glyph.
        let mut full = RECT {
            left: 0,
            top: 0,
            right: WIDTH,
            bottom: rect.bottom.max(400),
        };

        let bg = CreateSolidBrush(rgb(0x1F, 0x1F, 0x23));
        FillRect(hdc, &full, bg);
        DeleteObject(bg as _);

        let (ar, ag, ab) = content.accent;
        let accent = CreateSolidBrush(rgb(ar, ag, ab));
        full.right = ACCENT_W;
        FillRect(hdc, &full, accent);
        DeleteObject(accent as _);

        SetBkMode(hdc, BK_TRANSPARENT);

        let face = wide("Segoe UI");
        // Negative height asks for a character height rather than a cell height.
        let title_font = CreateFontW(
            -19, 0, 0, 0, 600, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr(),
        );
        let body_font = CreateFontW(
            -16, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr(),
        );

        let old = SelectObject(hdc, title_font as _);
        SetTextColor(hdc, rgb(0xFF, 0xFF, 0xFF));
        let mut line = RECT {
            left: ACCENT_W + PAD,
            top: PAD,
            right: WIDTH - PAD,
            bottom: PAD + TITLE_H,
        };
        let mut text = wide(&content.title);
        DrawTextW(
            hdc,
            text.as_mut_ptr(),
            -1,
            &mut line,
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
        );

        SelectObject(hdc, body_font as _);
        SetTextColor(hdc, rgb(0xE4, 0xE4, 0xEA));
        let mut y = PAD + TITLE_H + GAP;
        for row in &content.rows {
            let mut r = RECT {
                left: ACCENT_W + PAD,
                top: y,
                right: WIDTH - PAD,
                bottom: y + ROW_H,
            };
            let mut t = wide(row);
            DrawTextW(
                hdc,
                t.as_mut_ptr(),
                -1,
                &mut r,
                DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
            y += ROW_H;
        }

        if content.overflow > 0 {
            SetTextColor(hdc, rgb(0x7A, 0x7A, 0x85));
            let mut r = RECT {
                left: ACCENT_W + PAD,
                top: y,
                right: WIDTH - PAD,
                bottom: y + ROW_H,
            };
            let mut t = wide(&format!("+{} more", content.overflow));
            DrawTextW(
                hdc,
                t.as_mut_ptr(),
                -1,
                &mut r,
                DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
            );
        }

        SelectObject(hdc, old);
        DeleteObject(title_font as _);
        DeleteObject(body_font as _);
        EndPaint(hwnd, &ps);
    }
}
