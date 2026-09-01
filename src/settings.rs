//! The settings panel.
//!
//! Settings used to live in the tray menu, but a Win32 menu closes the moment anything in it is
//! clicked, so changing three things meant reopening the menu three times. This is an owner-drawn
//! window instead: it stays up while you work through it and closes once the mouse leaves.
//!
//! Layout is produced once by [`layout`] and used for both painting and hit-testing, so a row can
//! never be drawn in one place and clickable in another.

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr;

use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, Ellipse,
    EndPaint, FillRect, InvalidateRect, PAINTSTRUCT, PS_SOLID, SelectObject, SetBkMode,
    SetTextColor,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetCursorPos, HWND_TOPMOST, IDC_ARROW, KillTimer,
    LoadCursorW, RegisterClassW, SPI_GETWORKAREA, SW_HIDE, SW_SHOW, SetForegroundWindow, SetTimer,
    SetWindowPos, ShowWindow, SystemParametersInfoW, WM_KEYDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::config::{self, Config, Sort, Sound};
use crate::session::Status;
use crate::theme::Palette;

/// Ticks the "has the mouse left?" check.
const WATCH_TIMER: usize = 2;
const WATCH_MS: u32 = 250;
/// How long the pointer may sit outside the panel before it closes, once it has been inside.
const GRACE_MS: u32 = 700;
/// Longer leeway before the pointer has ever entered, so a panel opened under a stray cursor is
/// not snatched away before it can be reached.
const ENTRY_GRACE_MS: u32 = 4_000;

const WIDTH: i32 = 400;
const PAD: i32 = 16;
const TITLE_H: i32 = 30;
const SECTION_H: i32 = 26;
const ROW_H: i32 = 26;
const BUTTON_H: i32 = 32;

// Right-hand cycle control: `< value >`.
const NEXT_X0: i32 = WIDTH - PAD - 14;
const VALUE_X1: i32 = NEXT_X0;
const VALUE_X0: i32 = VALUE_X1 - 150;
const PREV_X0: i32 = VALUE_X0 - 14;

const BK_TRANSPARENT: i32 = 1;
const DT_SINGLELINE: u32 = 0x0020;
const DT_VCENTER: u32 = 0x0004;
const DT_CENTER: u32 = 0x0001;
const DT_END_ELLIPSIS: u32 = 0x8000;
const DT_NOPREFIX: u32 = 0x0800;

const VK_ESCAPE: usize = 0x1B;

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as COLORREF | ((g as COLORREF) << 8) | ((b as COLORREF) << 16)
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Which multi-valued setting a cycle row drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Sort,
    Cache,
    ClaudePast,
    ListRows,
    Rows,
    CursorLocal,
    Repeat,
    Sound,
    Popup,
    Poll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    ToggleEnabled,
    ToggleStatus(Status),
    /// `+1` for the next value, `-1` for the previous.
    Cycle(Field, i32),
    Test,
}

enum RowKind {
    Title(String),
    Section(String),
    Check {
        label: String,
        checked: bool,
        /// Status dot, so a row here can be matched to the dot on an alert.
        dot: Option<([u8; 4], bool)>,
    },
    Cycle { label: String, value: String },
    Button { label: String },
}

struct Row {
    top: i32,
    height: i32,
    kind: RowKind,
    action: Option<Action>,
}

/// Steps `value` to the next entry of `choices`, wrapping in either direction.
fn cycle<T: PartialEq + Copy>(choices: &[T], value: T, dir: i32) -> T {
    if choices.is_empty() {
        return value;
    }
    let at = choices.iter().position(|c| *c == value).unwrap_or(0) as i32;
    let len = choices.len() as i32;
    // `rem_euclid` keeps -1 from the first entry landing on the last.
    choices[(at + dir).rem_euclid(len) as usize]
}

fn layout(config: &Config) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut y = PAD;

    let mut push = |kind: RowKind, height: i32, action: Option<Action>, y: &mut i32| {
        rows.push(Row {
            top: *y,
            height,
            kind,
            action,
        });
        *y += height;
    };

    push(
        RowKind::Title("Agent Status Tray".to_string()),
        TITLE_H,
        None,
        &mut y,
    );
    push(
        RowKind::Check {
            label: "Show desktop alerts".to_string(),
            checked: config.notifications_enabled,
            dot: None,
        },
        ROW_H,
        Some(Action::ToggleEnabled),
        &mut y,
    );

    push(
        RowKind::Section("Alert me about".to_string()),
        SECTION_H,
        None,
        &mut y,
    );
    for status in config::NOTIFIABLE {
        push(
            RowKind::Check {
                label: status.menu_label().to_string(),
                checked: config.notifies_on(status),
                dot: Some(crate::icon::status_dot(status)),
            },
            ROW_H,
            Some(Action::ToggleStatus(status)),
            &mut y,
        );
    }

    push(
        RowKind::Section("Session list".to_string()),
        SECTION_H,
        None,
        &mut y,
    );
    push(
        RowKind::Cycle {
            label: "Sort rows by".to_string(),
            value: config.sort.label().to_string(),
        },
        ROW_H,
        Some(Action::Cycle(Field::Sort, 1)),
        &mut y,
    );
    push(
        RowKind::Cycle {
            label: "Context cache window".to_string(),
            value: config::cache_label(config.cache_window_mins),
        },
        ROW_H,
        Some(Action::Cycle(Field::Cache, 1)),
        &mut y,
    );
    push(
        RowKind::Cycle {
            label: "Menu shows at most".to_string(),
            value: format!("{} rows", config.max_list_rows),
        },
        ROW_H,
        Some(Action::Cycle(Field::ListRows, 1)),
        &mut y,
    );
    push(
        RowKind::Cycle {
            label: "Alert shows at most".to_string(),
            value: config::rows_label(config.max_alert_rows),
        },
        ROW_H,
        Some(Action::Cycle(Field::Rows, 1)),
        &mut y,
    );
    push(
        RowKind::Cycle {
            label: "Claude past chats".to_string(),
            value: config::past_label(config.claude_past_days),
        },
        ROW_H,
        Some(Action::Cycle(Field::ClaudePast, 1)),
        &mut y,
    );
    push(
        RowKind::Cycle {
            label: "Cursor local chats".to_string(),
            value: config::cursor_local_label(config.cursor_local_days),
        },
        ROW_H,
        Some(Action::Cycle(Field::CursorLocal, 1)),
        &mut y,
    );

    push(
        RowKind::Section("Timing and sound".to_string()),
        SECTION_H,
        None,
        &mut y,
    );
    push(
        RowKind::Cycle {
            label: "Repeat alert".to_string(),
            value: config::repeat_label(config.repeat_secs),
        },
        ROW_H,
        Some(Action::Cycle(Field::Repeat, 1)),
        &mut y,
    );
    push(
        RowKind::Cycle {
            label: "Alert sound".to_string(),
            value: config.sound.label().to_string(),
        },
        ROW_H,
        Some(Action::Cycle(Field::Sound, 1)),
        &mut y,
    );
    push(
        RowKind::Cycle {
            label: "Alert stays for".to_string(),
            value: config::popup_label(config.popup_secs),
        },
        ROW_H,
        Some(Action::Cycle(Field::Popup, 1)),
        &mut y,
    );
    push(
        RowKind::Cycle {
            label: "Check sessions every".to_string(),
            value: config::poll_label(config.poll_ms),
        },
        ROW_H,
        Some(Action::Cycle(Field::Poll, 1)),
        &mut y,
    );

    y += 8;
    push(
        RowKind::Button {
            label: "Test alert now".to_string(),
        },
        BUTTON_H,
        Some(Action::Test),
        &mut y,
    );

    rows
}

fn panel_height(rows: &[Row]) -> i32 {
    rows.last().map(|r| r.top + r.height).unwrap_or(0) + PAD
}

/// Applies an action. Returns whether the poll interval moved, which the caller needs in order to
/// rebuild its timer.
fn apply(action: Action, config: &mut Config) -> bool {
    match action {
        Action::ToggleEnabled => config.notifications_enabled = !config.notifications_enabled,
        Action::ToggleStatus(status) => config.toggle_status(status),
        Action::Cycle(Field::Sort, dir) => config.sort = cycle(&Sort::ALL, config.sort, dir),
        Action::Cycle(Field::Cache, dir) => {
            config.cache_window_mins = cycle(&config::CACHE_CHOICES, config.cache_window_mins, dir)
        }
        Action::Cycle(Field::ClaudePast, dir) => {
            config.claude_past_days = cycle(&config::PAST_CHOICES, config.claude_past_days, dir)
        }
        Action::Cycle(Field::ListRows, dir) => {
            config.max_list_rows = cycle(&config::LIST_CHOICES, config.max_list_rows, dir)
        }
        Action::Cycle(Field::Rows, dir) => {
            config.max_alert_rows = cycle(&config::ROW_CHOICES, config.max_alert_rows, dir)
        }
        Action::Cycle(Field::CursorLocal, dir) => {
            config.cursor_local_days =
                cycle(&config::CURSOR_LOCAL_CHOICES, config.cursor_local_days, dir)
        }
        Action::Cycle(Field::Repeat, dir) => {
            config.repeat_secs = cycle(&config::REPEAT_CHOICES, config.repeat_secs, dir)
        }
        Action::Cycle(Field::Sound, dir) => {
            config.sound = cycle(&Sound::ALL, config.sound, dir);
            // Immediate feedback, so picking a sound lets you hear it.
            crate::notify::play(config.sound);
        }
        Action::Cycle(Field::Popup, dir) => {
            config.popup_secs = cycle(&config::POPUP_CHOICES, config.popup_secs, dir)
        }
        Action::Cycle(Field::Poll, dir) => {
            config.poll_ms = cycle(&config::POLL_CHOICES, config.poll_ms, dir);
            return true;
        }
        Action::Test => {}
    }
    false
}

#[derive(Default)]
struct State {
    config: Config,
    /// System colours, sampled when the panel is opened.
    palette: Option<Palette>,
    hover: Option<usize>,
    /// Set when the config changed, so the owner can persist and apply it.
    changed: bool,
    poll_changed: bool,
    test_requested: bool,
    /// The pointer has been inside at least once, which arms the shorter close grace.
    entered: bool,
    outside_ms: u32,
    open: bool,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

pub struct SettingsWindow {
    hwnd: HWND,
}

/// What changed while the panel was open.
pub struct Changes {
    pub config: Config,
    pub poll_changed: bool,
    pub test_requested: bool,
}

impl SettingsWindow {
    pub fn new() -> Option<SettingsWindow> {
        unsafe {
            let instance = GetModuleHandleW(ptr::null());
            let class_name = wide("ClaudeTraySettings");

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
            if RegisterClassW(&class) == 0 {
                return None;
            }

            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                class_name.as_ptr(),
                wide("claude-tray settings").as_ptr(),
                WS_POPUP,
                0,
                0,
                WIDTH,
                400,
                ptr::null_mut(),
                ptr::null_mut(),
                instance as _,
                ptr::null(),
            );
            if hwnd.is_null() {
                return None;
            }
            Some(SettingsWindow { hwnd })
        }
    }

    /// Opens the panel seeded with the current settings, anchored to the tray corner.
    pub fn open(&self, config: &Config) {
        let height = panel_height(&layout(config));

        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.config = config.clone();
            s.palette = Some(Palette::current());
            s.hover = None;
            s.changed = false;
            s.poll_changed = false;
            s.test_requested = false;
            s.entered = false;
            s.outside_ms = 0;
            s.open = true;
        });

        unsafe {
            let mut work = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
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
                0x0040, // SWP_SHOWWINDOW
            );
            ShowWindow(self.hwnd, SW_SHOW);
            SetForegroundWindow(self.hwnd);
            InvalidateRect(self.hwnd, ptr::null(), 1);
            SetTimer(self.hwnd, WATCH_TIMER, WATCH_MS, None);
        }
    }

    /// Collects any change made since the last call, for the owner to persist and apply.
    pub fn take_changes(&self) -> Option<Changes> {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            if !s.changed && !s.test_requested {
                return None;
            }
            let changes = Changes {
                config: s.config.clone(),
                poll_changed: s.poll_changed,
                test_requested: s.test_requested,
            };
            s.changed = false;
            s.poll_changed = false;
            s.test_requested = false;
            Some(changes)
        })
    }
}

fn close(hwnd: HWND) {
    unsafe {
        KillTimer(hwnd, WATCH_TIMER);
        ShowWindow(hwnd, SW_HIDE);
    }
    STATE.with(|s| s.borrow_mut().open = false);
}

/// Row under a client-area point, if any.
fn row_at(rows: &[Row], y: i32) -> Option<usize> {
    rows.iter()
        .position(|r| (r.top..r.top + r.height).contains(&y) && r.action.is_some())
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

            WM_MOUSEMOVE => {
                let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
                let rows = STATE.with(|s| layout(&s.borrow().config));
                let hover = row_at(&rows, y);
                let changed = STATE.with(|s| {
                    let mut s = s.borrow_mut();
                    let changed = s.hover != hover;
                    s.hover = hover;
                    changed
                });
                if changed {
                    InvalidateRect(hwnd, ptr::null(), 0);
                }
                0
            }

            WM_LBUTTONUP => {
                let x = (lparam & 0xFFFF) as i16 as i32;
                let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
                let rows = STATE.with(|s| layout(&s.borrow().config));
                if let Some(index) = row_at(&rows, y)
                    && let Some(action) = rows[index].action
                {
                    // The left arrow of a cycle row steps backwards; anywhere else steps forward.
                    let action = match action {
                        Action::Cycle(field, _) if (PREV_X0..VALUE_X0).contains(&x) => {
                            Action::Cycle(field, -1)
                        }
                        other => other,
                    };
                    STATE.with(|s| {
                        let mut s = s.borrow_mut();
                        if action == Action::Test {
                            s.test_requested = true;
                        } else {
                            let mut config = s.config.clone();
                            let poll_changed = apply(action, &mut config);
                            s.config = config;
                            s.changed = true;
                            s.poll_changed |= poll_changed;
                        }
                    });
                    InvalidateRect(hwnd, ptr::null(), 1);
                }
                0
            }

            WM_KEYDOWN if wparam == VK_ESCAPE => {
                close(hwnd);
                0
            }

            // Closing is driven by where the pointer is rather than by focus, so clicking a row
            // never dismisses the panel.
            WM_TIMER if wparam == WATCH_TIMER => {
                let mut point = POINT { x: 0, y: 0 };
                let mut rect = RECT {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                };
                if GetCursorPos(&mut point) != 0
                    && windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect)
                        != 0
                {
                    let inside = point.x >= rect.left
                        && point.x < rect.right
                        && point.y >= rect.top
                        && point.y < rect.bottom;
                    let should_close = STATE.with(|s| {
                        let mut s = s.borrow_mut();
                        if inside {
                            s.entered = true;
                            s.outside_ms = 0;
                            return false;
                        }
                        s.outside_ms = s.outside_ms.saturating_add(WATCH_MS);
                        let grace = if s.entered { GRACE_MS } else { ENTRY_GRACE_MS };
                        s.outside_ms >= grace
                    });
                    if should_close {
                        close(hwnd);
                    }
                }
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

        let (config, hover, palette) = STATE.with(|s| {
            let s = s.borrow();
            (s.config.clone(), s.hover, s.palette)
        });
        let palette = palette.unwrap_or_else(Palette::current);
        let rows = layout(&config);
        let height = panel_height(&rows);

        let bg = CreateSolidBrush(palette.background);
        let mut full = RECT {
            left: 0,
            top: 0,
            right: WIDTH,
            bottom: height,
        };
        FillRect(hdc, &full, bg);
        DeleteObject(bg as _);

        // A thin stripe down the left in the user's Windows accent colour.
        let accent = CreateSolidBrush(palette.accent);
        full.right = 3;
        FillRect(hdc, &full, accent);
        DeleteObject(accent as _);

        SetBkMode(hdc, BK_TRANSPARENT);
        let face = wide("Segoe UI");
        let title_font = CreateFontW(-19, 0, 0, 0, 600, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr());
        let body_font = CreateFontW(-15, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr());
        let section_font = CreateFontW(-13, 0, 0, 0, 600, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr());
        let old = SelectObject(hdc, body_font as _);

        for (index, row) in rows.iter().enumerate() {
            let hovered = hover == Some(index) && row.action.is_some();
            if hovered {
                let brush = CreateSolidBrush(palette.hover);
                let band = RECT {
                    left: 6,
                    top: row.top,
                    right: WIDTH - 6,
                    bottom: row.top + row.height,
                };
                FillRect(hdc, &band, brush);
                DeleteObject(brush as _);
            }

            let mut text_rect = RECT {
                left: PAD,
                top: row.top,
                right: WIDTH - PAD,
                bottom: row.top + row.height,
            };

            match &row.kind {
                RowKind::Title(label) => {
                    SelectObject(hdc, title_font as _);
                    SetTextColor(hdc, palette.text);
                    draw(hdc, label, &mut text_rect, DT_SINGLELINE | DT_VCENTER);
                    SelectObject(hdc, body_font as _);
                }
                RowKind::Section(label) => {
                    SelectObject(hdc, section_font as _);
                    SetTextColor(hdc, palette.dim);
                    draw(hdc, label, &mut text_rect, DT_SINGLELINE | DT_VCENTER);
                    SelectObject(hdc, body_font as _);
                }
                RowKind::Check {
                    label,
                    checked,
                    dot,
                } => {
                    let middle = row.top + row.height / 2;
                    draw_check(hdc, middle, *checked, &palette);
                    text_rect.left = PAD + 24;
                    if let Some(dot) = dot {
                        draw_dot(hdc, PAD + 30, middle, *dot, &palette);
                        text_rect.left = PAD + 44;
                    }
                    SetTextColor(hdc, palette.text);
                    draw(
                        hdc,
                        label,
                        &mut text_rect,
                        DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
                    );
                }
                RowKind::Cycle { label, value } => {
                    SetTextColor(hdc, palette.text);
                    let mut label_rect = RECT {
                        left: PAD,
                        top: row.top,
                        right: PREV_X0 - 6,
                        bottom: row.top + row.height,
                    };
                    draw(
                        hdc,
                        label,
                        &mut label_rect,
                        DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
                    );

                    SetTextColor(hdc, palette.dim);
                    let mut prev = RECT {
                        left: PREV_X0,
                        top: row.top,
                        right: VALUE_X0,
                        bottom: row.top + row.height,
                    };
                    draw(hdc, "\u{2039}", &mut prev, DT_SINGLELINE | DT_VCENTER | DT_CENTER);
                    let mut next = RECT {
                        left: NEXT_X0,
                        top: row.top,
                        right: WIDTH - PAD,
                        bottom: row.top + row.height,
                    };
                    draw(hdc, "\u{203A}", &mut next, DT_SINGLELINE | DT_VCENTER | DT_CENTER);

                    SetTextColor(hdc, palette.accent);
                    let mut value_rect = RECT {
                        left: VALUE_X0,
                        top: row.top,
                        right: VALUE_X1,
                        bottom: row.top + row.height,
                    };
                    draw(
                        hdc,
                        value,
                        &mut value_rect,
                        DT_SINGLELINE | DT_VCENTER | DT_CENTER | DT_END_ELLIPSIS,
                    );
                }
                RowKind::Button { label } => {
                    let face_brush =
                        CreateSolidBrush(if hovered { palette.accent } else { palette.hover });
                    let button = RECT {
                        left: PAD,
                        top: row.top + 2,
                        right: WIDTH - PAD,
                        bottom: row.top + row.height - 2,
                    };
                    FillRect(hdc, &button, face_brush);
                    DeleteObject(face_brush as _);
                    SetTextColor(hdc, if hovered { rgb(0xFF, 0xFF, 0xFF) } else { palette.text });
                    draw(
                        hdc,
                        label,
                        &mut text_rect,
                        DT_SINGLELINE | DT_VCENTER | DT_CENTER,
                    );
                }
            }
        }

        SelectObject(hdc, old);
        DeleteObject(title_font as _);
        DeleteObject(body_font as _);
        DeleteObject(section_font as _);
        EndPaint(hwnd, &ps);
    }
}

unsafe fn draw(hdc: windows_sys::Win32::Graphics::Gdi::HDC, text: &str, rect: &mut RECT, flags: u32) {
    unsafe {
        let mut buf = wide(text);
        DrawTextW(hdc, buf.as_mut_ptr(), -1, rect, flags | DT_NOPREFIX);
    }
}

/// Fills a circle of `radius` at `(cx, cy)`.
unsafe fn circle(
    hdc: windows_sys::Win32::Graphics::Gdi::HDC,
    cx: i32,
    cy: i32,
    radius: i32,
    color: COLORREF,
) {
    unsafe {
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

/// The same status dot the alert draws, so the two can be matched by eye.
unsafe fn draw_dot(
    hdc: windows_sys::Win32::Graphics::Gdi::HDC,
    cx: i32,
    cy: i32,
    dot: ([u8; 4], bool),
    palette: &Palette,
) {
    unsafe {
        let ([r, g, b, _], filled) = dot;
        circle(hdc, cx, cy, 5, rgb(r, g, b));
        if !filled {
            circle(hdc, cx, cy, 3, palette.background);
        }
    }
}

/// A 14px box: border, hole, and a filled core when ticked.
unsafe fn draw_check(
    hdc: windows_sys::Win32::Graphics::Gdi::HDC,
    center_y: i32,
    checked: bool,
    palette: &Palette,
) {
    unsafe {
        let box_rect = RECT {
            left: PAD,
            top: center_y - 7,
            right: PAD + 14,
            bottom: center_y + 7,
        };
        let border = CreateSolidBrush(if checked { palette.accent } else { palette.dim });
        FillRect(hdc, &box_rect, border);
        DeleteObject(border as _);

        let inner = RECT {
            left: box_rect.left + 2,
            top: box_rect.top + 2,
            right: box_rect.right - 2,
            bottom: box_rect.bottom - 2,
        };
        let hole = CreateSolidBrush(palette.background);
        FillRect(hdc, &inner, hole);
        DeleteObject(hole as _);

        if checked {
            let core = RECT {
                left: box_rect.left + 4,
                top: box_rect.top + 4,
                right: box_rect.right - 4,
                bottom: box_rect.bottom - 4,
            };
            let fill = CreateSolidBrush(palette.accent);
            FillRect(hdc, &core, fill);
            DeleteObject(fill as _);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycling_wraps_in_both_directions() {
        let choices = [1u64, 2, 3];
        assert_eq!(cycle(&choices, 1, 1), 2);
        assert_eq!(cycle(&choices, 3, 1), 1);
        assert_eq!(cycle(&choices, 1, -1), 3);
    }

    /// A value that is not in the list (a hand-edited config) must still move.
    #[test]
    fn cycling_from_an_unknown_value_lands_somewhere_valid() {
        let choices = [10u64, 20, 30];
        assert_eq!(cycle(&choices, 99, 1), 20);
    }

    #[test]
    fn every_actionable_row_is_hit_testable() {
        let config = Config::default();
        let rows = layout(&config);
        for (index, row) in rows.iter().enumerate() {
            if row.action.is_some() {
                let middle = row.top + row.height / 2;
                assert_eq!(row_at(&rows, middle), Some(index), "row {index} not hittable");
            }
        }
    }

    /// Rows must not overlap, or a click would land on the wrong setting.
    #[test]
    fn rows_do_not_overlap() {
        let rows = layout(&Config::default());
        for pair in rows.windows(2) {
            assert!(pair[1].top >= pair[0].top + pair[0].height);
        }
    }

    #[test]
    fn toggling_a_status_row_updates_the_config() {
        let mut config = Config::default();
        assert!(!config.notifies_on(Status::Idle));
        apply(Action::ToggleStatus(Status::Idle), &mut config);
        assert!(config.notifies_on(Status::Idle));
    }

    #[test]
    fn the_sort_row_cycles_through_every_order() {
        let mut config = Config::default();
        assert_eq!(config.sort, Sort::Attention);
        apply(Action::Cycle(Field::Sort, 1), &mut config);
        assert_eq!(config.sort, Sort::Recent);
        apply(Action::Cycle(Field::Sort, 1), &mut config);
        assert_eq!(config.sort, Sort::Oldest);
        apply(Action::Cycle(Field::Sort, 1), &mut config);
        assert_eq!(config.sort, Sort::GoingCold);
        // Wraps, and steps backwards.
        apply(Action::Cycle(Field::Sort, 1), &mut config);
        assert_eq!(config.sort, Sort::Attention);
        apply(Action::Cycle(Field::Sort, -1), &mut config);
        assert_eq!(config.sort, Sort::GoingCold);
    }

    #[test]
    fn only_the_poll_row_asks_for_a_timer_rebuild() {
        let mut config = Config::default();
        assert!(apply(Action::Cycle(Field::Poll, 1), &mut config));
        assert!(!apply(Action::Cycle(Field::Repeat, 1), &mut config));
        assert!(!apply(Action::Cycle(Field::Sort, 1), &mut config));
        assert!(!apply(Action::Cycle(Field::Rows, 1), &mut config));
        assert!(!apply(Action::Cycle(Field::ListRows, 1), &mut config));
        assert!(!apply(Action::Cycle(Field::ClaudePast, 1), &mut config));
    }

    /// A 768-high screen has roughly 730px of work area, and the panel is anchored to the bottom
    /// corner, so anything taller than this starts running off the top.
    #[test]
    fn the_panel_fits_on_a_modest_screen() {
        let height = panel_height(&layout(&Config::default()));
        assert!(height < 700, "settings panel is {height}px tall");
    }
}
