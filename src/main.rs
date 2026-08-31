//! Tray-resident status for every running Claude Code session.
//!
//! Claude Code keeps a live registry at `%USERPROFILE%\.claude\sessions\<pid>.json`. This polls it
//! once a second, paints the aggregate state onto the tray icon, and lists each session in the
//! tray menu, each row tagged with a color of its own. Display only — nothing here talks back to
//! Claude Code.

#![windows_subsystem = "windows"]

mod color;
mod icon;
mod liveness;
mod menu_gdi;
mod render;
mod session;

use std::collections::HashMap;
use std::ptr;
use std::time::{SystemTime, UNIX_EPOCH};

use tray_icon::menu::{
    Icon as MenuIcon, IconMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem,
};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, KillTimer, MSG, PostQuitMessage, SetTimer, TranslateMessage,
    WM_TIMER,
};

use color::ColorMap;
use session::{IconState, Registry, Session};

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

/// One session as the menu needs it: what to draw, and what it is drawn for.
struct Row {
    key: String,
    color: usize,
    text: String,
}

fn rows_for(sessions: &[Session], colors: &mut ColorMap, now: u64) -> Vec<Row> {
    sessions
        .iter()
        .map(|session| Row {
            key: session.key.clone(),
            color: colors.index_for(&session.key),
            text: render::row(session, now),
        })
        .collect()
}

/// What is currently on screen, so ticks that change nothing touch nothing.
struct Ui {
    tray: TrayIcon,
    icon_state: Option<IconState>,
    tooltip: String,
    /// Held alongside the copy given to the tray, so its bitmaps can be freed on replacement.
    menu: Option<Menu>,
    /// The session rows of `menu`, in order, so their text can be retargeted in place.
    items: Vec<IconMenuItem>,
    texts: Vec<String>,
    /// `(session key, palette slot)` per row. The menu is rebuilt only when this changes.
    roster: Vec<(String, usize)>,
    /// Rasterized once per palette slot, since the pixels of a chip never change.
    swatches: HashMap<usize, MenuIcon>,
}

impl Ui {
    fn new(tray: TrayIcon) -> Ui {
        Ui {
            tray,
            icon_state: None,
            tooltip: String::new(),
            menu: None,
            items: Vec::new(),
            texts: Vec::new(),
            roster: Vec::new(),
            swatches: HashMap::new(),
        }
    }

    fn swatch(&mut self, slot: usize) -> Option<MenuIcon> {
        if let Some(existing) = self.swatches.get(&slot) {
            return Some(existing.clone());
        }
        let pixels = icon::swatch(color::PALETTE[slot]);
        let built = MenuIcon::from_rgba(pixels, icon::SWATCH_SIZE, icon::SWATCH_SIZE).ok()?;
        self.swatches.insert(slot, built.clone());
        Some(built)
    }

    fn apply(&mut self, state: IconState, header: String, rows: Vec<Row>, tooltip: String) {
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

        let roster: Vec<(String, usize)> = rows.iter().map(|r| (r.key.clone(), r.color)).collect();

        // Elapsed times move every tick. Retargeting the text costs nothing, where rebuilding
        // the menu allocates a GDI bitmap per row that muda never frees.
        if self.menu.is_some() && self.roster == roster {
            for (index, row) in rows.iter().enumerate() {
                if self.texts[index] != row.text {
                    self.items[index].set_text(&row.text);
                    self.texts[index] = row.text.clone();
                }
            }
            return;
        }

        self.rebuild(&header, rows);
        self.roster = roster;
    }

    /// Build a fresh menu, hand it to the tray, then reclaim what the outgoing one held.
    fn rebuild(&mut self, header: &str, rows: Vec<Row>) {
        let menu = Menu::new();
        let mut items = Vec::with_capacity(rows.len());
        let mut texts = Vec::with_capacity(rows.len());

        if menu.append(&MenuItem::new(header, false, None)).is_err()
            || menu.append(&PredefinedMenuItem::separator()).is_err()
        {
            return;
        }

        for row in rows {
            let swatch = self.swatch(row.color);
            let item = IconMenuItem::new(&row.text, true, swatch, None);
            if menu.append(&item).is_err() {
                return;
            }
            items.push(item);
            texts.push(row.text);
        }

        if !items.is_empty() && menu.append(&PredefinedMenuItem::separator()).is_err() {
            return;
        }
        if menu
            .append(&MenuItem::with_id(EXIT_ID, "Exit", true, None))
            .is_err()
        {
            return;
        }

        self.tray.set_menu(Some(Box::new(menu.clone())));

        // Only after the replacement is installed, so no bitmap still on screen is freed.
        if let Some(old) = self.menu.take() {
            menu_gdi::free_item_bitmaps(&old);
        }

        self.menu = Some(menu);
        self.items = items;
        self.texts = texts;
    }
}

fn tick(registry: &mut Registry, colors: &mut ColorMap, ui: &mut Ui) {
    let sessions = registry.scan();
    // Free the slots of departed sessions before handing colors to this tick's rows.
    colors.retain_live(&sessions);

    let now = now_ms();
    ui.apply(
        session::icon_state(&sessions),
        render::header(&sessions),
        rows_for(&sessions, colors, now),
        render::tooltip(&sessions),
    );
}

fn main() {
    let mut registry = Registry::new();
    let mut colors = ColorMap::new();
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
        rows_for(&sessions, &mut colors, now),
        render::tooltip(&sessions),
    );

    MenuEvent::set_event_handler(Some(|event: MenuEvent| {
        if event.id == EXIT_ID {
            // Menu events arrive on the message-loop thread, so this ends the loop below.
            unsafe { PostQuitMessage(0) };
        }
    }));

    // A null hwnd makes Windows pick the timer id and ignore the one asked for, so the returned
    // id is the only thing that will match WM_TIMER.wParam. Zero means SetTimer failed.
    let timer_id = unsafe { SetTimer(ptr::null_mut(), 0, TICK_MS, None) };

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        let result = unsafe { GetMessageW(&mut msg, ptr::null_mut(), 0, 0) };
        if result <= 0 {
            break;
        }
        // Timer messages are posted with a null hwnd, so they are handled here rather than
        // dispatched to a window procedure.
        if timer_id != 0 && msg.message == WM_TIMER && msg.wParam == timer_id {
            tick(&mut registry, &mut colors, &mut ui);
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    if timer_id != 0 {
        unsafe { KillTimer(ptr::null_mut(), timer_id) };
    }
}
