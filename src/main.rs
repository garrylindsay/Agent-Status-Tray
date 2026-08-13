//! Tray-resident status for every running Claude Code session.
//!
//! Claude Code keeps a live registry at `%USERPROFILE%\.claude\sessions\<pid>.json`. This polls it
//! once a second, paints the aggregate state onto the tray icon, and lists each session in the
//! tray menu. Display only — nothing here talks back to Claude Code.

#![windows_subsystem = "windows"]

mod icon;
mod liveness;
mod render;
mod session;

use std::ptr;
use std::time::{SystemTime, UNIX_EPOCH};

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, PostQuitMessage, SetTimer, TranslateMessage, WM_TIMER,
};

use session::{IconState, Registry};

const TIMER_ID: usize = 1;
const TICK_MS: u32 = 1_000;
const EXIT_ID: &str = "claude-tray-exit";

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn make_icon(state: IconState) -> Option<Icon> {
    Icon::from_rgba(icon::render(state), icon::WIDTH, icon::HEIGHT).ok()
}

/// What is currently on screen, so ticks that change nothing touch nothing.
struct Ui {
    tray: TrayIcon,
    icon_state: Option<IconState>,
    rows: Vec<String>,
    tooltip: String,
}

impl Ui {
    fn new(tray: TrayIcon) -> Ui {
        Ui {
            tray,
            icon_state: None,
            rows: Vec::new(),
            tooltip: String::new(),
        }
    }

    fn apply(&mut self, state: IconState, header: String, rows: Vec<String>, tooltip: String) {
        if self.icon_state != Some(state) {
            if let Some(icon) = make_icon(state) {
                let _ = self.tray.set_icon(Some(icon));
            }
            self.icon_state = Some(state);
        }

        if self.tooltip != tooltip {
            let _ = self.tray.set_tooltip(Some(&tooltip));
            self.tooltip = tooltip;
        }

        // Elapsed times change every tick, so rows are compared as rendered text.
        if self.rows != rows {
            if let Some(menu) = build_menu(&header, &rows) {
                self.tray.set_menu(Some(Box::new(menu)));
            }
            self.rows = rows;
        }
    }
}

/// Session rows are plain items: clicking one just dismisses the menu. Only Exit acts.
fn build_menu(header: &str, rows: &[String]) -> Option<Menu> {
    let menu = Menu::new();

    menu.append(&MenuItem::new(header, false, None)).ok()?;
    menu.append(&PredefinedMenuItem::separator()).ok()?;
    for row in rows {
        menu.append(&MenuItem::new(row, true, None)).ok()?;
    }
    if !rows.is_empty() {
        menu.append(&PredefinedMenuItem::separator()).ok()?;
    }
    menu.append(&MenuItem::with_id(EXIT_ID, "Exit", true, None))
        .ok()?;
    Some(menu)
}

fn tick(registry: &mut Registry, ui: &mut Ui) {
    let sessions = registry.scan();
    let now = now_ms();
    let rows: Vec<String> = sessions.iter().map(|s| render::row(s, now)).collect();

    ui.apply(
        session::icon_state(&sessions),
        render::header(&sessions),
        rows,
        render::tooltip(&sessions),
    );
}

fn main() {
    let mut registry = Registry::new();
    let sessions = registry.scan();
    let state = session::icon_state(&sessions);

    let mut builder = TrayIconBuilder::new()
        .with_menu_on_left_click(true)
        .with_tooltip(render::tooltip(&sessions));
    if let Some(icon) = make_icon(state) {
        builder = builder.with_icon(icon);
    }

    let tray = match builder.build() {
        Ok(tray) => tray,
        // Nothing to show without a tray icon, and no console to complain to.
        Err(_) => return,
    };

    let mut ui = Ui::new(tray);
    let now = now_ms();
    ui.apply(
        state,
        render::header(&sessions),
        sessions.iter().map(|s| render::row(s, now)).collect(),
        render::tooltip(&sessions),
    );

    MenuEvent::set_event_handler(Some(|event: MenuEvent| {
        if event.id == EXIT_ID {
            // Menu events arrive on the message-loop thread, so this ends the loop below.
            unsafe { PostQuitMessage(0) };
        }
    }));

    unsafe { SetTimer(ptr::null_mut(), TIMER_ID, TICK_MS, None) };

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        let result = unsafe { GetMessageW(&mut msg, ptr::null_mut(), 0, 0) };
        if result <= 0 {
            break;
        }
        // Timer messages are posted with a null hwnd, so they are handled here rather than
        // dispatched to a window procedure.
        if msg.message == WM_TIMER && msg.wParam == TIMER_ID {
            tick(&mut registry, &mut ui);
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
