//! Tray-resident status for every running Claude Code session.
//!
//! Claude Code keeps a live registry at `%USERPROFILE%\.claude\sessions\<pid>.json`. This polls it
//! on a configurable interval, paints the aggregate state onto the tray icon, lists each session in
//! the tray menu, and raises a desktop alert while any session sits in a status you asked to be
//! told about. Display only — nothing here talks back to Claude Code.

#![windows_subsystem = "windows"]

mod alert;
mod config;
mod icon;
mod liveness;
mod notify;
mod render;
mod session;

use std::ptr;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use tray_icon::menu::{
    CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, KillTimer, MSG, PostQuitMessage, SetTimer, TranslateMessage,
    WM_TIMER,
};

use alert::Alerter;
use config::{Config, Sound};
use notify::Popup;
use session::{IconState, Registry, Session, Status};

const EXIT_ID: &str = "claude-tray-exit";
const TEST_ID: &str = "claude-tray-test-alert";
const ENABLED_ID: &str = "claude-tray-notify-enabled";

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
    /// Rendered rows plus the settings state, so the menu is rebuilt when either moves.
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
        rows: Vec<String>,
        tooltip: String,
        config: &Config,
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
        let signature = format!("{header}|{rows:?}|{config:?}");
        if !self.menu_installed || self.menu_signature != signature {
            if let Some(menu) = build_menu(&header, &rows, config) {
                self.tray.set_menu(Some(Box::new(menu)));
                self.menu_installed = true;
            }
            self.menu_signature = signature;
        }
    }
}

/// Settings submenu. Every item writes the config to disk as soon as it is clicked, so nothing is
/// lost to a reboot or a kill.
fn build_settings(config: &Config) -> Option<Submenu> {
    let settings = Submenu::new("Settings", true);

    settings
        .append(&CheckMenuItem::with_id(
            ENABLED_ID,
            "Show desktop alerts",
            true,
            config.notifications_enabled,
            None,
        ))
        .ok()?;

    let statuses = Submenu::new("Alert me about", true);
    for status in config::NOTIFIABLE {
        statuses
            .append(&CheckMenuItem::with_id(
                format!("notif.status.{}", status.key()),
                status.menu_label(),
                true,
                config.notifies_on(status),
                None,
            ))
            .ok()?;
    }
    settings.append(&statuses).ok()?;

    let repeat = Submenu::new("Repeat alert", true);
    for secs in config::REPEAT_CHOICES {
        repeat
            .append(&CheckMenuItem::with_id(
                format!("notif.repeat.{secs}"),
                config::repeat_label(secs),
                true,
                config.repeat_secs == secs,
                None,
            ))
            .ok()?;
    }
    settings.append(&repeat).ok()?;

    let sound = Submenu::new("Alert sound", true);
    for choice in Sound::ALL {
        sound
            .append(&CheckMenuItem::with_id(
                format!("notif.sound.{}", choice.key()),
                choice.label(),
                true,
                config.sound == choice,
                None,
            ))
            .ok()?;
    }
    settings.append(&sound).ok()?;

    let duration = Submenu::new("Alert stays for", true);
    for secs in config::POPUP_CHOICES {
        duration
            .append(&CheckMenuItem::with_id(
                format!("notif.dur.{secs}"),
                config::popup_label(secs),
                true,
                config.popup_secs == secs,
                None,
            ))
            .ok()?;
    }
    settings.append(&duration).ok()?;

    settings.append(&PredefinedMenuItem::separator()).ok()?;

    let poll = Submenu::new("Check sessions every", true);
    for ms in config::POLL_CHOICES {
        poll.append(&CheckMenuItem::with_id(
            format!("poll.{ms}"),
            config::poll_label(ms),
            true,
            config.poll_ms == ms,
            None,
        ))
        .ok()?;
    }
    settings.append(&poll).ok()?;

    settings.append(&PredefinedMenuItem::separator()).ok()?;
    settings
        .append(&MenuItem::with_id(TEST_ID, "Test alert now", true, None))
        .ok()?;

    Some(settings)
}

/// Session rows are plain items: clicking one just dismisses the menu. Only settings and Exit act.
fn build_menu(header: &str, rows: &[String], config: &Config) -> Option<Menu> {
    let menu = Menu::new();

    menu.append(&MenuItem::new(header, false, None)).ok()?;
    menu.append(&PredefinedMenuItem::separator()).ok()?;
    for row in rows {
        menu.append(&MenuItem::new(row, true, None)).ok()?;
    }
    if !rows.is_empty() {
        menu.append(&PredefinedMenuItem::separator()).ok()?;
    }

    let settings = build_settings(config)?;
    menu.append(&settings as &dyn IsMenuItem).ok()?;
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
    let rows: Vec<String> = sessions.iter().map(|s| render::row(s, now)).collect();

    ui.apply(
        session::icon_state(&sessions),
        render::header(&sessions),
        rows,
        render::tooltip(&sessions),
        config,
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
        let lines: Vec<String> = owned.iter().map(|s| render::alert_row(s, now)).collect();
        popup.show(
            &render::alert_title(&owned),
            &lines,
            accent_for(&owned),
            config.popup_secs,
            config.sound,
        );
    }
}

/// Applies a settings click. Returns true when the poll timer has to be rebuilt.
fn handle_menu_id(id: &str, config: &mut Config, alerter: &mut Alerter, popup: Option<&Popup>) -> bool {
    let mut poll_changed = false;

    if id == ENABLED_ID {
        config.notifications_enabled = !config.notifications_enabled;
        // Re-arm, so switching alerts back on tells you about anything already waiting.
        alerter.reset();
    } else if let Some(key) = id.strip_prefix("notif.status.") {
        if let Some(status) = Status::from_key(key) {
            config.toggle_status(status);
            alerter.reset();
        }
    } else if let Some(secs) = id.strip_prefix("notif.repeat.") {
        if let Ok(secs) = secs.parse::<u64>() {
            config.repeat_secs = secs;
        }
    } else if let Some(key) = id.strip_prefix("notif.sound.") {
        if let Some(sound) = Sound::from_key(key) {
            config.sound = sound;
            // Immediate feedback, so picking a sound lets you hear it.
            notify::play(sound);
        }
    } else if let Some(secs) = id.strip_prefix("notif.dur.") {
        if let Ok(secs) = secs.parse::<u64>() {
            config.popup_secs = secs;
        }
    } else if let Some(ms) = id.strip_prefix("poll.") {
        if let Ok(ms) = ms.parse::<u64>() {
            config.poll_ms = ms;
            poll_changed = true;
        }
    } else if id == TEST_ID {
        if let Some(popup) = popup {
            popup.show(
                "Test alert",
                &[
                    "\u{25cf} api-gateway-f6 \u{2014} WAITING 4m \u{b7} permission prompt"
                        .to_string(),
                    "\u{25d0} claude-tray-97 \u{2014} BUSY 12s".to_string(),
                ],
                (0xE5, 0x48, 0x2F),
                config.popup_secs,
                config.sound,
            );
        }
        // A test must not shift the real repeat schedule.
        return false;
    } else {
        return false;
    }

    config.save();
    poll_changed
}

/// `claude-tray.exe --demo-alert` shows one alert and exits. No tray icon, no registry polling —
/// it exists so the popup can be eyeballed (and screenshotted) without waiting for a real session
/// to block.
fn demo_alert() {
    let config = Config::load();
    let Some(popup) = Popup::new() else { return };
    popup.show(
        "2 Claude sessions are waiting on you",
        &[
            "\u{25cf} api-gateway-f6 \u{2014} WAITING 4m \u{b7} permission prompt".to_string(),
            "\u{25cf} claude-tray-97 \u{2014} WAITING 38s \u{b7} input needed".to_string(),
        ],
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

fn main() {
    if std::env::args().any(|a| a == "--demo-alert") {
        demo_alert();
        return;
    }

    let mut config = Config::load();
    let mut registry = Registry::new();
    let mut alerter = Alerter::new();
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

    // Alerts are a nicety: if the window cannot be made, the tray still works.
    let popup = Popup::new();

    let mut ui = Ui::new(tray);
    let now = now_ms();
    ui.apply(
        state,
        render::header(&sessions),
        sessions.iter().map(|s| render::row(s, now)).collect(),
        render::tooltip(&sessions),
        &config,
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
            } else {
                poll_changed |= handle_menu_id(&id, &mut config, &mut alerter, popup.as_ref());
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
