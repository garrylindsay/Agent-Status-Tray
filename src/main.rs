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
mod cursor;
mod desktop;
mod icon;
mod liveness;
mod notify;
mod render;
mod session;
mod settings;
mod theme;
mod title;

use std::ptr;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use tray_icon::menu::{IconMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, KillTimer, MSG, PostQuitMessage, SetTimer, TranslateMessage,
    WM_TIMER,
};

use alert::Alerter;
use config::{Config, Sort};
use cursor::Cursor;
use desktop::Desktop;
use notify::{AlertRow, Popup};
use session::{IconState, Registry, Session, Status};
use settings::SettingsWindow;
use title::Titles;

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

    fn apply(&mut self, state: IconState, header: String, rows: Vec<MenuRow>, tooltip: String) {
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
fn build_menu(header: &str, rows: &[MenuRow]) -> Option<Menu> {
    let menu = Menu::new();

    menu.append(&MenuItem::new(header, false, None)).ok()?;
    menu.append(&PredefinedMenuItem::separator()).ok()?;
    for row in rows {
        // A menu item's icon is the only way to get colour into a native Win32 menu without
        // owner-drawing every item.
        let (color, filled) = row.dot;
        let dot = tray_icon::menu::Icon::from_rgba(
            icon::dot_rgba(color, filled),
            icon::DOT_SIZE,
            icon::DOT_SIZE,
        )
        .ok();
        menu.append(&IconMenuItem::with_id(
            format!("session.{}", row.pid),
            &row.text,
            true,
            dot,
            None,
        ))
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

/// One session as the tray menu shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MenuRow {
    text: String,
    pid: u32,
    dot: ([u8; 4], bool),
}

impl MenuRow {
    fn new(session: &Session, now_ms: u64) -> MenuRow {
        MenuRow {
            text: render::row(session, now_ms),
            pid: session.pid,
            dot: icon::status_dot(session.status),
        }
    }
}

/// Everywhere sessions are read from.
///
/// Each provider knows only its own tool; the rest of the program sees one merged list and cannot
/// tell which source a row came from except by its label.
struct Sources {
    registry: Registry,
    titles: Titles,
    desktop: Desktop,
    cursor: Cursor,
}

impl Sources {
    fn new() -> Sources {
        Sources {
            registry: Registry::new(),
            titles: Titles::new(),
            desktop: Desktop::new(),
            cursor: Cursor::new(),
        }
    }

    /// Every provider's sessions, in one list, in the order the settings ask for.
    fn collect(&mut self, now_ms: u64, sort: Sort) -> Vec<Session> {
        // Claude Code: the registry says which sessions exist, the transcript supplies the title,
        // and the desktop record supplies the state the registry no longer reports.
        let mut sessions = self.registry.scan();
        for session in &mut sessions {
            if let Some(id) = session.session_id.clone() {
                session.title = self.titles.get(&session.cwd, &id);
            }
        }
        self.desktop.apply(&mut sessions);

        let live: Vec<String> = sessions.iter().filter_map(|s| s.session_id.clone()).collect();
        self.titles.retain(&live);

        sessions.extend(self.cursor.sessions(now_ms));

        // Sorted across providers, not within each: a failed Cursor agent outranks an idle Claude
        // session. Ties break on pid so the order cannot shuffle between ticks.
        match sort {
            Sort::Attention => sessions.sort_by(|a, b| {
                a.status
                    .rank()
                    .cmp(&b.status.rank())
                    .then(a.since.cmp(&b.since))
                    .then(a.pid.cmp(&b.pid))
            }),
            Sort::Recent => sessions.sort_by(|a, b| b.since.cmp(&a.since).then(a.pid.cmp(&b.pid))),
            Sort::Oldest => sessions.sort_by(|a, b| a.since.cmp(&b.since).then(a.pid.cmp(&b.pid))),
        }
        sessions
    }
}

/// Sessions as the alert draws them. Every alert goes through here — real, test and demo — so a
/// sample can never drift away from the real thing's appearance.
fn alert_rows(sessions: &[Session], now_ms: u64) -> Vec<AlertRow> {
    sessions
        .iter()
        .map(|s| AlertRow {
            text: render::alert_row(s, now_ms),
            pid: s.pid,
            deep_link: s.deep_link(),
            dot: icon::status_dot(s.status),
            repo: s.repo,
        })
        .collect()
}

/// Stand-in sessions for the test and demo alerts: one waiting on you, one working.
fn sample_sessions(now_ms: u64) -> Vec<Session> {
    vec![
        Session {
            provider: session::Provider::ClaudeCode,
            pid: 0,
            name: "api-gateway-f6".to_string(),
            repo: session::Repo::PrOpen,
            cwd: String::new(),
            title: Some("Rate limiting rollout".to_string()),
            session_id: None,
            entrypoint: None,
            desktop_session_id: None,
            status: Status::Waiting,
            waiting_for: Some("permission prompt".to_string()),
            since: now_ms.saturating_sub(240_000),
        },
        Session {
            provider: session::Provider::Cursor,
            pid: 0,
            name: "scale-fun-der".to_string(),
            repo: session::Repo::Branch,
            cwd: String::new(),
            title: Some("Snyk issue sweep".to_string()),
            session_id: None,
            entrypoint: None,
            desktop_session_id: None,
            status: Status::Busy,
            waiting_for: None,
            since: now_ms.saturating_sub(12_000),
        },
    ]
}

/// Colour of the alert's left bar: the colour of the most urgent session in it.
///
/// The bar says how much the alert wants from you, so it earns its colour rather than always
/// having one. Amber when something is waiting, the working grey when something is running, and
/// the same quiet grey as a hollow dot when everything is merely open — a bright bar over a list
/// of sessions that need nothing is just crying wolf.
fn accent_for(sessions: &[Session]) -> (u8, u8, u8) {
    let [r, g, b, _] = if sessions.iter().any(|s| s.status == Status::Waiting) {
        icon::DOT_WAITING
    } else if sessions
        .iter()
        .any(|s| matches!(s.status, Status::Busy | Status::Shell))
    {
        icon::DOT_WORKING
    } else if sessions.iter().any(|s| s.status == Status::Unread) {
        icon::DOT_UNREAD
    } else {
        icon::DOT_DONE
    };
    (r, g, b)
}

/// Returns the sessions it rendered, so a later menu click can resolve a pid back to one.
fn tick(
    sources: &mut Sources,
    ui: &mut Ui,
    config: &Config,
    alerter: &mut Alerter,
    popup: Option<&Popup>,
) -> Vec<Session> {
    // Cheap, and guarded against re-flushing, so the menu follows a theme switched mid-run.
    theme::sync_menu_theme();
    let now = now_ms();
    let sessions = sources.collect(now, config.sort);
    let rows: Vec<MenuRow> = sessions.iter().map(|s| MenuRow::new(s, now)).collect();

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
        let lines = alert_rows(&owned, now);
        popup.show(
            &render::alert_title(&owned),
            &lines,
            accent_for(&owned),
            config.popup_secs,
            config.sound,
            config.max_alert_rows as usize,
        );
    }

    sessions
}

/// A sample alert, so the look and sound can be checked without waiting for a session to block.
/// The rows carry pid 0, so clicking one dismisses without chasing a window.
fn show_test_alert(config: &Config, popup: Option<&Popup>) {
    if let Some(popup) = popup {
        let now = now_ms();
        let sessions = sample_sessions(now);
        popup.show(
            "Test alert",
            &alert_rows(&sessions, now),
            accent_for(&sessions),
            config.popup_secs,
            config.sound,
            config.max_alert_rows as usize,
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
    let mut sessions = Sources::new().collect(now, config.sort);
    if sessions.is_empty() {
        sessions = sample_sessions(now);
    }
    let rows = alert_rows(&sessions, now);

    popup.show(
        &render::alert_title(&sessions),
        &rows,
        accent_for(&sessions),
        0,
        config.sound,
        config.max_alert_rows as usize,
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

    // Before the first menu is built: the theme has to be set for a menu to be created dark.
    theme::sync_menu_theme();

    let mut alerter = Alerter::new();
    let mut sources = Sources::new();
    let sessions = sources.collect(now_ms(), config.sort);
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
    // Most recent scan, so a menu click can resolve its pid back to a session.
    let mut known = sessions.clone();
    let now = now_ms();
    ui.apply(
        state,
        render::header(&sessions),
        sessions.iter().map(|s| MenuRow::new(s, now)).collect(),
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
            known = tick(&mut sources, &mut ui, &config, &mut alerter, popup.as_ref());
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
                let deep_link = known
                    .iter()
                    .find(|s| s.pid == pid)
                    .and_then(|s| s.deep_link());
                activate::focus_session(pid, deep_link.as_deref());
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
