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

use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, Ellipse,
    EndPaint, FillRect, GetTextExtentPoint32W, HDC, InvalidateRect, PAINTSTRUCT, PS_SOLID,
    SelectObject, SetBkMode, SetTextColor,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, HWND_TOPMOST, IDC_ARROW, IDC_HAND, KillTimer, LoadCursorW,
    RegisterClassW, SPI_GETWORKAREA, SW_HIDE, SW_SHOWNOACTIVATE, SetCursor, SetTimer, SetWindowPos,
    ShowWindow, SystemParametersInfoW, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_PAINT, WM_SETCURSOR,
    WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows_sys::Win32::Graphics::Gdi::{GetDC, ReleaseDC};

use crate::config::Sound;
use crate::theme::Palette;

/// Timer that dismisses the popup. Scoped to the popup window, so it cannot collide with the
/// null-hwnd poll timer in `main`.
const DISMISS_TIMER: usize = 1;

/// The card sizes itself to its longest row so a conversation title is shown in full, within
/// these bounds. Narrower than the minimum looks stunted; wider than the maximum stops being a
/// notification and starts being a window.
const MIN_WIDTH: i32 = 440;
const MAX_WIDTH: i32 = 1100;
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

/// One clickable line of the alert.
#[derive(Clone)]
pub struct AlertRow {
    pub text: String,
    /// Session to raise when the row is clicked. Zero for rows that are not a real session.
    pub pid: u32,
    /// `claude://` link that opens this exact session, where the host supports one.
    pub deep_link: Option<String>,
    /// Status dot: colour, and whether it is filled or a hollow ring.
    pub dot: ([u8; 4], bool),
}

/// What the window paints on its next `WM_PAINT`.
#[derive(Default, Clone)]
struct Content {
    title: String,
    rows: Vec<AlertRow>,
    overflow: usize,
    accent: (u8, u8, u8),
    /// Row under the pointer, for the hover highlight.
    hover: Option<usize>,
    /// Kept so hovering can restart the dismiss timer with the configured duration.
    duration_secs: u64,
    /// System colours, sampled when the alert is shown.
    palette: Option<Palette>,
    /// Width this content was measured for.
    width: i32,
}

/// Width of `text` in the font currently selected into `hdc`.
unsafe fn text_width(hdc: HDC, text: &str) -> i32 {
    unsafe {
        let buf = wide(text);
        let mut size = SIZE { cx: 0, cy: 0 };
        // The trailing NUL is not part of the string being measured.
        GetTextExtentPoint32W(hdc, buf.as_ptr(), buf.len() as i32 - 1, &mut size);
        size.cx
    }
}

/// Card width that fits the title and every row, clamped and kept inside the work area.
unsafe fn measure_width(hwnd: HWND, title: &str, rows: &[AlertRow], overflow: usize) -> i32 {
    unsafe {
        let hdc = GetDC(hwnd);
        if hdc.is_null() {
            return MIN_WIDTH;
        }

        let face = wide("Segoe UI");
        let title_font = CreateFontW(-19, 0, 0, 0, 600, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr());
        let body_font = CreateFontW(-16, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr());

        let old = SelectObject(hdc, title_font as _);
        let mut widest = text_width(hdc, title);

        SelectObject(hdc, body_font as _);
        for row in rows {
            widest = widest.max(text_width(hdc, &row.text) + DOT_COLUMN);
        }
        if overflow > 0 {
            widest = widest.max(text_width(hdc, &format!("+{overflow} more")) + DOT_COLUMN);
        }

        SelectObject(hdc, old);
        DeleteObject(title_font as _);
        DeleteObject(body_font as _);
        ReleaseDC(hwnd, hdc);

        // A couple of pixels of slack, so the last glyph never sits against the ellipsis test.
        let wanted = widest + ACCENT_W + PAD * 2 + 4;

        let mut work = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        let ceiling = if SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            &mut work as *mut RECT as *mut c_void,
            0,
        ) != 0
        {
            MAX_WIDTH.min(work.right - work.left - 24)
        } else {
            MAX_WIDTH
        };

        wanted.clamp(MIN_WIDTH, ceiling.max(MIN_WIDTH))
    }
}

/// Space reserved at the left of a row for its status dot.
const DOT_COLUMN: i32 = 16;
const DOT_R: i32 = 5;

/// Fills a circle of `radius` at `(cx, cy)` in one colour.
unsafe fn circle(
    hdc: windows_sys::Win32::Graphics::Gdi::HDC,
    cx: i32,
    cy: i32,
    radius: i32,
    color: COLORREF,
) {
    unsafe {
        // Ellipse outlines with the current pen, so the pen has to match or the edge reads as a
        // darker ring around the dot.
        let pen = CreatePen(PS_SOLID, 1, color);
        let brush = CreateSolidBrush(color);
        let old_pen = SelectObject(hdc, pen as _);
        let old_brush = SelectObject(hdc, brush as _);

        Ellipse(hdc, cx - radius, cy - radius, cx + radius, cy + radius);

        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
        DeleteObject(pen as _);
        DeleteObject(brush as _);
    }
}

/// A filled dot or a hollow ring, drawn to match the Claude desktop app's session list.
unsafe fn draw_dot(
    hdc: windows_sys::Win32::Graphics::Gdi::HDC,
    cx: i32,
    cy: i32,
    dot: ([u8; 4], bool),
) {
    unsafe {
        let ([r, g, b, _], filled) = dot;
        circle(hdc, cx, cy, DOT_R, rgb(r, g, b));

        if !filled {
            // Punch the middle out against the card, leaving a ring.
            let background = CONTENT
                .with(|c| c.borrow().palette)
                .map(|p| p.background)
                .unwrap_or_else(|| rgb(0x1F, 0x1F, 0x23));
            circle(hdc, cx, cy, DOT_R - 2, background);
        }
    }
}

/// Y of the first session row, which is where hit-testing starts.
const ROWS_TOP: i32 = PAD + TITLE_H + GAP;

/// Row under a client-area point, if the point is on one at all.
fn row_at(y: i32, count: usize) -> Option<usize> {
    if y < ROWS_TOP {
        return None;
    }
    let index = ((y - ROWS_TOP) / ROW_H) as usize;
    (index < count).then_some(index)
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
                MIN_WIDTH,
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
        rows: &[AlertRow],
        accent: (u8, u8, u8),
        duration_secs: u64,
        sound: Sound,
    ) {
        let shown: Vec<AlertRow> = rows.iter().take(MAX_ROWS).cloned().collect();
        let overflow = rows.len().saturating_sub(shown.len());

        let width = unsafe { measure_width(self.hwnd, title, &shown, overflow) };

        CONTENT.with(|c| {
            *c.borrow_mut() = Content {
                title: title.to_string(),
                rows: shown.clone(),
                overflow,
                accent,
                hover: None,
                duration_secs,
                palette: Some(Palette::current()),
                width,
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
                (work.right - width - 12, work.bottom - height - 12)
            } else {
                (100, 100)
            };

            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                x,
                y,
                width,
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
            // A click on a session row raises that session's window; anywhere else just dismisses,
            // as an Outlook alert does.
            WM_LBUTTONDOWN => {
                let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
                let target = CONTENT.with(|c| {
                    let content = c.borrow();
                    row_at(y, content.rows.len())
                        .map(|i| (content.rows[i].pid, content.rows[i].deep_link.clone()))
                });
                KillTimer(hwnd, DISMISS_TIMER);
                ShowWindow(hwnd, SW_HIDE);
                if let Some((pid, deep_link)) = target {
                    crate::activate::focus_session(pid, deep_link.as_deref());
                }
                0
            }

            // Hovering marks the row and holds the alert open, so it cannot expire from under the
            // pointer on the way to a click.
            WM_MOUSEMOVE => {
                let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
                let (changed, hovering) = CONTENT.with(|c| {
                    let mut content = c.borrow_mut();
                    let hover = row_at(y, content.rows.len());
                    let changed = content.hover != hover;
                    content.hover = hover;
                    (changed, hover.is_some())
                });
                if changed {
                    InvalidateRect(hwnd, ptr::null(), 0);
                }
                // Restart rather than cancel, so an alert the pointer merely crosses still goes
                // away on its own.
                let duration = CONTENT.with(|c| c.borrow().duration_secs);
                if hovering && duration > 0 {
                    SetTimer(hwnd, DISMISS_TIMER, (duration * 1000) as u32, None);
                }
                0
            }

            // Rows are clickable, so say so with the cursor.
            WM_SETCURSOR => {
                let hovering = CONTENT.with(|c| c.borrow().hover.is_some());
                if hovering {
                    SetCursor(LoadCursorW(ptr::null_mut(), IDC_HAND));
                    1
                } else {
                    DefWindowProcW(hwnd, msg, wparam, lparam)
                }
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
        let width = if content.width > 0 { content.width } else { MIN_WIDTH };
        let mut full = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: rect.bottom.max(400),
        };

        let palette = content.palette.unwrap_or_else(Palette::current);

        let bg = CreateSolidBrush(palette.background);
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
        SetTextColor(hdc, palette.text);
        let mut line = RECT {
            left: ACCENT_W + PAD,
            top: PAD,
            right: width - PAD,
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
        let mut y = ROWS_TOP;
        for (index, row) in content.rows.iter().enumerate() {
            // Only rows that lead somewhere are worth highlighting as clickable.
            if content.hover == Some(index) && row.pid != 0 {
                let band = RECT {
                    left: ACCENT_W,
                    top: y,
                    right: width,
                    bottom: y + ROW_H,
                };
                let brush = CreateSolidBrush(palette.hover);
                FillRect(hdc, &band, brush);
                DeleteObject(brush as _);
            }

            draw_dot(hdc, ACCENT_W + PAD + 5, y + ROW_H / 2, row.dot);

            SetTextColor(hdc, palette.text);
            let mut r = RECT {
                left: ACCENT_W + PAD + DOT_COLUMN,
                top: y,
                right: width - PAD,
                bottom: y + ROW_H,
            };
            let mut t = wide(&row.text);
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
            SetTextColor(hdc, palette.dim);
            let mut r = RECT {
                left: ACCENT_W + PAD + DOT_COLUMN,
                top: y,
                right: width - PAD,
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
