//! Tray-resident status for every running Claude Code session.
//!
//! Claude Code keeps a live registry at `%USERPROFILE%\.claude\sessions\<pid>.json`. This polls it
//! on a configurable interval, paints the aggregate state onto the tray icon, lists each session in
//! the tray menu, and raises a desktop alert while any session sits in a status you asked to be
//! told about. Display only — nothing here talks back to Claude Code.

#![windows_subsystem = "windows"]

mod activate;
mod alert;
mod config;
mod icon;
mod liveness;
mod notify;
mod render;
mod session;
mod settings;
mod theme;

use std::ptr;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, KillTimer, MSG, PostQuitMessage, SetTimer, TranslateMessage,
    WM_TIMER,
};

use alert::Alerter;
use config::Config;
use notify::{AlertRow, Popup};
use session::{IconState, Registry, Session, Status};
use settings::SettingsWindow;

const EXIT_ID: &str = "claude-tray-exit";
const SETTINGS_ID: &str = "claude-tray-settings";

/// Menu clicks are delivered on this thread while `DispatchMessageW` runs, so the loop drains this
/// queue immediately afterwards. A queue rather than direct mutation keeps the handler closure from
/// having to borrow the config.
static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());

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
    /// The rendered rows, so the menu is rebuilt only when they actually change.
    menu_signature: String,
    tooltip: String,
    /// The builder ships no menu, so the first apply has to install one even if it is empty.
    menu_installed: bool,
}

impl Ui {
    fn new(tray: TrayIcon) -> Ui {
        Ui {
            tray,
            icon_state: None,
            menu_signature: String::new(),
            tooltip: String::new(),
            menu_installed: false,
        }
    }

    fn apply(
        &mut self,
        state: IconState,
        header: String,
        rows: Vec<(String, u32)>,
        tooltip: String,
    ) {
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
        let signature = format!("{header}|{rows:?}");
        if !self.menu_installed || self.menu_signature != signature {
            if let Some(menu) = build_menu(&header, &rows) {
                self.tray.set_menu(Some(Box::new(menu)));
                self.menu_installed = true;
            }
            self.menu_signature = signature;
        }
    }
}

/// Clicking a session row raises the window hosting that session, as clicking an alert row does.
///
/// Settings deliberately open a window rather than living in submenus here: a Win32 menu closes on
/// every click, so changing several settings meant reopening the menu once per change.
fn build_menu(header: &str, rows: &[(String, u32)]) -> Option<Menu> {
    let menu = Menu::new();

    menu.append(&MenuItem::new(header, false, None)).ok()?;
    menu.append(&PredefinedMenuItem::separator()).ok()?;
    for (text, pid) in rows {
        menu.append(&MenuItem::with_id(format!("session.{pid}"), text, true, None))
            .ok()?;
    }
    if !rows.is_empty() {
        menu.append(&PredefinedMenuItem::separator()).ok()?;
    }

    menu.append(&MenuItem::with_id(SETTINGS_ID, "Settings\u{2026}", true, None))
        .ok()?;
    menu.append(&PredefinedMenuItem::separator()).ok()?;
    menu.append(&MenuItem::with_id(EXIT_ID, "Exit", true, None))
        .ok()?;
    Some(menu)
}

/// Accent for the alert, taken from the most urgent session in it.
fn accent_for(sessions: &[Session]) -> (u8, u8, u8) {
    if sessions.iter().any(|s| s.status == Status::Waiting) {
        (0xE5, 0x48, 0x2F)
    } else if sessions
        .iter()
        .any(|s| matches!(s.status, Status::Busy | Status::Shell))
    {
        (0x2E, 0x8B, 0xE0)
    } else {
        (0x9A, 0xA0, 0xAA)
    }
}

fn tick(
    registry: &mut Registry,
    ui: &mut Ui,
    config: &Config,
    alerter: &mut Alerter,
    popup: Option<&Popup>,
) {
    let sessions = registry.scan();
    let now = now_ms();
    let rows: Vec<(String, u32)> = sessions
        .iter()
        .map(|s| (render::row(s, now), s.pid))
        .collect();

    ui.apply(
        session::icon_state(&sessions),
        render::header(&sessions),
        rows,
        render::tooltip(&sessions),
    );

    let matching: Vec<&Session> = sessions
        .iter()
        .filter(|s| config.notifies_on(s.status))
        .collect();
    let pids: Vec<u32> = matching.iter().map(|s| s.pid).collect();

    if alerter.should_fire(&pids, now, config)
        && let Some(popup) = popup
    {
        let owned: Vec<Session> = matching.into_iter().cloned().collect();
        let lines: Vec<AlertRow> = owned
            .iter()
            .map(|s| AlertRow {
                text: render::alert_row(s, now),
                pid: s.pid,
            })
            .collect();
        popup.show(
            &render::alert_title(&owned),
            &lines,
            accent_for(&owned),
            config.popup_secs,
            config.sound,
        );
    }
}

/// A sample alert, so the look and sound can be checked without waiting for a session to block.
/// The rows carry pid 0, so clicking one dismisses without chasing a window.
fn show_test_alert(config: &Config, popup: Option<&Popup>) {
    if let Some(popup) = popup {
        popup.show(
            "Test alert",
            &[
                AlertRow {
                    text: "\u{25cf} api-gateway-f6 \u{2014} WAITING 4m \u{b7} permission prompt"
                        .to_string(),
                    pid: 0,
                },
                AlertRow {
                    text: "\u{25d0} claude-tray-97 \u{2014} BUSY 12s".to_string(),
                    pid: 0,
                },
            ],
            (0xE5, 0x48, 0x2F),
            config.popup_secs,
            config.sound,
        );
    }
}

/// `claude-tray.exe --demo-alert` shows one alert and exits. No tray icon, no registry polling —
/// it exists so the popup can be eyeballed (and screenshotted) without waiting for a real session
/// to block.
fn demo_alert() {
    let config = Config::load();
    let Some(popup) = Popup::new() else { return };

    // Prefer the sessions actually running, so clicking a row really does jump to one.
    let now = now_ms();
    let sessions = Registry::new().scan();
    let rows: Vec<AlertRow> = if sessions.is_empty() {
        vec![
            AlertRow {
                text: "\u{25cf} api-gateway-f6 \u{2014} WAITING 4m \u{b7} permission prompt"
                    .to_string(),
                pid: 0,
            },
            AlertRow {
                text: "\u{25cf} claude-tray-97 \u{2014} WAITING 38s \u{b7} input needed"
                    .to_string(),
                pid: 0,
            },
        ]
    } else {
        sessions
            .iter()
            .map(|s| AlertRow {
                text: render::alert_row(s, now),
                pid: s.pid,
            })
            .collect()
    };

    popup.show(
        &render::alert_title(&sessions),
        &rows,
        (0xE5, 0x48, 0x2F),
        0,
        config.sound,
    );

    // Quit on its own so the demo cannot leave an orphan process behind.
    let quit = unsafe { SetTimer(ptr::null_mut(), 0, 10_000, None) };
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        let result = unsafe { GetMessageW(&mut msg, ptr::null_mut(), 0, 0) };
        if result <= 0 {
            break;
        }
        if msg.message == WM_TIMER && msg.hwnd.is_null() && msg.wParam == quit {
            break;
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    if quit != 0 {
        unsafe { KillTimer(ptr::null_mut(), quit) };
    }
}

/// `claude-tray.exe --demo-settings` opens the settings panel on its own, for checking its look and
/// behaviour without going through the tray. Changes are still saved.
fn demo_settings() {
    let mut config = Config::load();
    let Some(settings) = SettingsWindow::new() else {
        return;
    };
    settings.open(&config);

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        let result = unsafe { GetMessageW(&mut msg, ptr::null_mut(), 0, 0) };
        if result <= 0 {
            break;
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if let Some(changes) = settings.take_changes()
            && changes.config != config
        {
            config = changes.config;
            config.save();
        }
    }
}

fn main() {
    if std::env::args().any(|a| a == "--demo-alert") {
        demo_alert();
        return;
    }
    if std::env::args().any(|a| a == "--demo-settings") {
        demo_settings();
        return;
    }

    let config_missing = config::path().map(|p| !p.exists()).unwrap_or(false);
    let mut config = Config::load();
    // Write the defaults out on first run, so the file is there to inspect or hand-edit rather
    // than only appearing once a setting is changed.
    if config_missing {
        config.save();
    }

    let mut registry = Registry::new();
    let mut alerter = Alerter::new();
    let sessions = registry.scan();
    let state = session::icon_state(&sessions);

    // Right-click opens the menu, as every other tray icon does. Left-click is deliberately left
    // alone rather than repurposed.
    let mut builder = TrayIconBuilder::new()
        .with_menu_on_left_click(false)
        .with_tooltip(render::tooltip(&sessions));
    if let Some(icon) = make_icon(state) {
        builder = builder.with_icon(icon);
    }

    let tray = match builder.build() {
        Ok(tray) => tray,
        // Nothing to show without a tray icon, and no console to complain to.
        Err(_) => return,
    };

    // Alerts and the settings panel are niceties: if either window cannot be made, the tray still
    // works without it.
    let popup = Popup::new();
    let settings = SettingsWindow::new();

    let mut ui = Ui::new(tray);
    let now = now_ms();
    ui.apply(
        state,
        render::header(&sessions),
        sessions
            .iter()
            .map(|s| (render::row(s, now), s.pid))
            .collect(),
        render::tooltip(&sessions),
    );

    MenuEvent::set_event_handler(Some(|event: MenuEvent| {
        if let Ok(mut pending) = PENDING.lock() {
            pending.push(event.id.0.clone());
        }
    }));

    // A null hwnd makes Windows pick the timer id and ignore the one asked for, so the returned
    // id is the only thing that will match WM_TIMER.wParam. Zero means SetTimer failed.
    let mut timer_id = unsafe { SetTimer(ptr::null_mut(), 0, config.poll_ms as u32, None) };

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        let result = unsafe { GetMessageW(&mut msg, ptr::null_mut(), 0, 0) };
        if result <= 0 {
            break;
        }
        // Timer messages are posted with a null hwnd, so they are handled here rather than
        // dispatched to a window procedure. The hwnd check keeps the popup's own dismiss timer,
        // which is window-scoped, from being mistaken for a poll tick.
        if timer_id != 0
            && msg.message == WM_TIMER
            && msg.hwnd.is_null()
            && msg.wParam == timer_id
        {
            tick(&mut registry, &mut ui, &config, &mut alerter, popup.as_ref());
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // Menu handlers run inline during the dispatch above.
        let ids: Vec<String> = PENDING
            .lock()
            .map(|mut p| std::mem::take(&mut *p))
            .unwrap_or_default();
        let mut poll_changed = false;
        for id in ids {
            if id == EXIT_ID {
                unsafe { PostQuitMessage(0) };
            } else if id == SETTINGS_ID {
                if let Some(settings) = settings.as_ref() {
                    settings.open(&config);
                }
            } else if let Some(pid) = id.strip_prefix("session.")
                && let Ok(pid) = pid.parse::<u32>()
            {
                activate::focus_session(pid);
            }
        }

        // The panel edits its own copy and hands back whatever moved, so settings apply the moment
        // they are clicked while the panel stays open.
        if let Some(settings) = settings.as_ref()
            && let Some(changes) = settings.take_changes()
        {
            if changes.test_requested {
                show_test_alert(&changes.config, popup.as_ref());
            }
            if changes.config != config {
                // Re-arm, so a change to what counts as alertable reports anything already waiting
                // rather than waiting out the current repeat interval.
                alerter.reset();
                config = changes.config;
                config.save();
                poll_changed |= changes.poll_changed;
            }
        }

        if poll_changed {
            if timer_id != 0 {
                unsafe { KillTimer(ptr::null_mut(), timer_id) };
            }
            timer_id = unsafe { SetTimer(ptr::null_mut(), 0, config.poll_ms as u32, None) };
        }
    }

    if let Some(popup) = popup.as_ref() {
        popup.hide();
    }
    if timer_id != 0 {
        unsafe { KillTimer(ptr::null_mut(), timer_id) };
    }
}
