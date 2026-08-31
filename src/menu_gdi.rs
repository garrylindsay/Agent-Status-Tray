//! Reclaims the menu bitmaps muda allocates but never frees.
//!
//! Every time an `IconMenuItem` is appended, muda calls `to_hbitmap()` and hands the
//! handle to Windows via `MIIM_BITMAP`. It never calls `DeleteObject` on it, and dropping
//! the `Menu` only calls `DestroyMenu`, which does not own item bitmaps. A process gets
//! 10,000 GDI handles, so a menu that is rebuilt as sessions come and go would eventually
//! run the tray out of them. This walks a menu about to be dropped and frees what it holds.

use tray_icon::menu::{ContextMenu, Menu};
use windows_sys::Win32::Graphics::Gdi::DeleteObject;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetMenuItemCount, GetMenuItemInfoW, MENUITEMINFOW, MIIM_BITMAP,
};

/// `hbmpItem` doubles as a slot for `HBMMENU_*` sentinels - small integers, not handles.
/// Nothing real lives that low, so anything under this is left alone.
const HBMMENU_MAX: usize = 32;

pub fn free_item_bitmaps(menu: &Menu) {
    let hmenu = menu.hpopupmenu() as *mut core::ffi::c_void;
    if hmenu.is_null() {
        return;
    }

    unsafe {
        let count = GetMenuItemCount(hmenu);
        for index in 0..count.max(0) {
            let mut info: MENUITEMINFOW = core::mem::zeroed();
            info.cbSize = core::mem::size_of::<MENUITEMINFOW>() as u32;
            info.fMask = MIIM_BITMAP;

            // Positional lookup: session rows carry no menu id.
            if GetMenuItemInfoW(hmenu, index as u32, 1, &mut info) == 0 {
                continue;
            }
            if (info.hbmpItem as usize) <= HBMMENU_MAX {
                continue;
            }
            DeleteObject(info.hbmpItem as *mut core::ffi::c_void);
        }
    }
}
