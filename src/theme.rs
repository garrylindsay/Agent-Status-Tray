//! System colours for the owner-drawn windows.
//!
//! `GetSysColor` is the right source for these, but it predates dark mode and keeps returning the
//! light scheme whichever theme is set, so the app theme is read from the registry and a dark
//! palette substituted. Windows has no API that just hands over the dark equivalents.

use std::ffi::c_void;

use windows_sys::Win32::Foundation::COLORREF;
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW,
};
use windows_sys::Win32::Graphics::Gdi::GetSysColor;

const COLOR_WINDOW: i32 = 5;
const COLOR_WINDOWTEXT: i32 = 8;
const COLOR_BTNFACE: i32 = 15;
const COLOR_GRAYTEXT: i32 = 17;

/// The colours both windows paint with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub background: COLORREF,
    pub text: COLORREF,
    /// Secondary text: section headings, the "+N more" line.
    pub dim: COLORREF,
    /// Row highlight under the pointer.
    pub hover: COLORREF,
    /// The user's Windows accent colour.
    pub accent: COLORREF,
    pub dark: bool,
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as COLORREF | ((g as COLORREF) << 8) | ((b as COLORREF) << 16)
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Reads a DWORD from HKCU, `None` when absent.
fn hkcu_dword(subkey: &str, value: &str) -> Option<u32> {
    let subkey = wide(subkey);
    let value = wide(value);
    let mut data: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            &mut data as *mut u32 as *mut c_void,
            &mut size,
        )
    };
    (status == 0).then_some(data)
}

/// True when apps are set to the dark theme. Absent value means light, which is the Windows
/// default when the key has never been written.
pub fn dark_mode() -> bool {
    hkcu_dword(
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "AppsUseLightTheme",
    )
    .map(|v| v == 0)
    .unwrap_or(false)
}

/// The user's accent colour, falling back to the Windows default blue.
fn accent() -> COLORREF {
    // ColorizationColor is 0xAARRGGBB; COLORREF wants 0x00BBGGRR.
    match hkcu_dword(r"Software\Microsoft\Windows\DWM", "ColorizationColor") {
        Some(value) => {
            let r = ((value >> 16) & 0xFF) as u8;
            let g = ((value >> 8) & 0xFF) as u8;
            let b = (value & 0xFF) as u8;
            rgb(r, g, b)
        }
        None => rgb(0x2E, 0x8B, 0xE0),
    }
}

impl Palette {
    /// Reads the current system colours. Cheap enough to call when a window is shown, which is
    /// also how a theme switched while the app runs gets picked up.
    pub fn current() -> Palette {
        let accent = accent();
        if dark_mode() {
            // Windows exposes no dark system colours, so these match the shell's own dark
            // flyouts rather than being invented.
            Palette {
                background: rgb(0x20, 0x20, 0x20),
                text: rgb(0xFF, 0xFF, 0xFF),
                dim: rgb(0xA0, 0xA0, 0xA6),
                hover: rgb(0x32, 0x32, 0x32),
                accent,
                dark: true,
            }
        } else {
            unsafe {
                Palette {
                    background: GetSysColor(COLOR_WINDOW),
                    text: GetSysColor(COLOR_WINDOWTEXT),
                    dim: GetSysColor(COLOR_GRAYTEXT),
                    hover: GetSysColor(COLOR_BTNFACE),
                    accent,
                    dark: false,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whatever the machine's theme, the palette must be usable: text and background have to
    /// differ, or the windows paint invisible.
    #[test]
    fn palette_is_legible() {
        let p = Palette::current();
        assert_ne!(p.background, p.text, "text is the same colour as the background");
        assert_ne!(p.hover, p.text, "hovered text would be invisible");
    }

    #[test]
    fn accent_is_never_absent() {
        // Falls back to the default blue rather than black when DWM has no value.
        assert_ne!(accent(), 0);
    }

    /// A missing key must read as light rather than panicking or defaulting to dark.
    #[test]
    fn a_missing_theme_value_reads_as_light() {
        assert!(hkcu_dword(r"Software\claude-tray\nope", "Missing").is_none());
    }
}
