//! Draws the tray icon: a status-colored disc with the session count on it.
//!
//! 32x32 RGBA, rendered from scratch so there are no image assets or font dependencies.

use crate::session::{IconKind, IconState, Repo, Status};

const SIZE: u32 = 32;
const OUTER_R: f32 = 15.0;
const RING_INNER_R: f32 = 11.5;
/// Samples per axis for anti-aliasing the disc edge.
const AA: u32 = 4;

/// Digit scale: each 3x5 glyph is drawn at 3x, i.e. 9x15 px.
const GLYPH_SCALE: u32 = 3;
const GLYPH_W: u32 = 3;
const GLYPH_H: u32 = 5;
const GLYPH_GAP: u32 = 2;

/// 3x5 bitmaps for `0`-`9` and `+`, one byte per row, low 3 bits, MSB-of-3 leftmost.
const FONT: [[u8; GLYPH_H as usize]; 11] = [
    [0b111, 0b101, 0b101, 0b101, 0b111], // 0
    [0b010, 0b110, 0b010, 0b010, 0b111], // 1
    [0b111, 0b001, 0b111, 0b100, 0b111], // 2
    [0b111, 0b001, 0b111, 0b001, 0b111], // 3
    [0b101, 0b101, 0b111, 0b001, 0b001], // 4
    [0b111, 0b100, 0b111, 0b001, 0b111], // 5
    [0b111, 0b100, 0b111, 0b101, 0b111], // 6
    [0b111, 0b001, 0b010, 0b010, 0b010], // 7
    [0b111, 0b101, 0b111, 0b101, 0b111], // 8
    [0b111, 0b101, 0b111, 0b001, 0b111], // 9
    [0b000, 0b010, 0b111, 0b010, 0b000], // +
];

type Rgba = [u8; 4];

/// Disc color, glyph color.
fn palette(kind: IconKind) -> (Rgba, Rgba) {
    match kind {
        // Red-orange: something is blocked on you.
        IconKind::Waiting => ([0xE5, 0x48, 0x2F, 0xFF], [0xFF, 0xFF, 0xFF, 0xFF]),
        // Blue: working.
        IconKind::Busy => ([0x2E, 0x8B, 0xE0, 0xFF], [0xFF, 0xFF, 0xFF, 0xFF]),
        // Neutral gray, legible on both light and dark taskbars.
        IconKind::Idle => ([0x9A, 0xA0, 0xAA, 0xFF], [0x9A, 0xA0, 0xAA, 0xFF]),
        IconKind::Empty => ([0x6A, 0x70, 0x7A, 0xFF], [0x6A, 0x70, 0x7A, 0xFF]),
    }
}

/// Idle and empty states draw a ring so "nothing needs you" reads differently at a glance.
fn is_ring(kind: IconKind) -> bool {
    matches!(kind, IconKind::Idle | IconKind::Empty)
}

/// Glyph indices for the badge: counts above 9 render as `9+`.
fn badge_glyphs(count: usize) -> Vec<usize> {
    match count {
        0 => Vec::new(),
        1..=9 => vec![count],
        _ => vec![9, 10],
    }
}

fn blend(dst: &mut [u8], color: Rgba, coverage: f32) {
    if coverage <= 0.0 {
        return;
    }
    let a = coverage.min(1.0);
    for c in 0..3 {
        dst[c] = (dst[c] as f32 * (1.0 - a) + color[c] as f32 * a).round() as u8;
    }
    dst[3] = (dst[3] as f32 * (1.0 - a) + color[3] as f32 * a).round() as u8;
}

/// Render the icon as RGBA8 rows, top to bottom.
pub fn render(state: IconState) -> Vec<u8> {
    let (disc, glyph) = palette(state.kind);
    let ring = is_ring(state.kind);
    let center = SIZE as f32 / 2.0;

    let mut buf = vec![0u8; (SIZE * SIZE * 4) as usize];

    for y in 0..SIZE {
        for x in 0..SIZE {
            let mut hits = 0u32;
            for sy in 0..AA {
                for sx in 0..AA {
                    let px = x as f32 + (sx as f32 + 0.5) / AA as f32 - center;
                    let py = y as f32 + (sy as f32 + 0.5) / AA as f32 - center;
                    let d = (px * px + py * py).sqrt();
                    let inside = d <= OUTER_R && (!ring || d >= RING_INNER_R);
                    if inside {
                        hits += 1;
                    }
                }
            }
            if hits > 0 {
                let idx = ((y * SIZE + x) * 4) as usize;
                blend(
                    &mut buf[idx..idx + 4],
                    disc,
                    hits as f32 / (AA * AA) as f32,
                );
            }
        }
    }

    draw_badge(&mut buf, &badge_glyphs(state.count), glyph);
    buf
}

/// Centered count, drawn opaque so it reads on a filled disc and inside a ring alike.
fn draw_badge(buf: &mut [u8], glyphs: &[usize], color: Rgba) {
    if glyphs.is_empty() {
        return;
    }

    // Two glyphs only fit inside the ring at the smaller scale.
    let scale = if glyphs.len() > 1 {
        GLYPH_SCALE - 1
    } else {
        GLYPH_SCALE
    };
    let glyph_w = GLYPH_W * scale;
    let glyph_h = GLYPH_H * scale;
    let total_w = glyphs.len() as u32 * glyph_w + (glyphs.len() as u32 - 1) * GLYPH_GAP;
    let origin_x = (SIZE - total_w) / 2;
    let origin_y = (SIZE - glyph_h) / 2;

    for (i, &g) in glyphs.iter().enumerate() {
        let gx = origin_x + i as u32 * (glyph_w + GLYPH_GAP);
        for row in 0..GLYPH_H {
            let bits = FONT[g][row as usize];
            for col in 0..GLYPH_W {
                if bits & (1 << (GLYPH_W - 1 - col)) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let x = gx + col * scale + dx;
                        let y = origin_y + row * scale + dy;
                        if x >= SIZE || y >= SIZE {
                            continue;
                        }
                        let idx = ((y * SIZE + x) * 4) as usize;
                        buf[idx..idx + 4].copy_from_slice(&color);
                    }
                }
            }
        }
    }
}

pub const WIDTH: u32 = SIZE;
pub const HEIGHT: u32 = SIZE;

/// Status dot colours, sampled from the Claude desktop app's own session list so a row here reads
/// the same as the corresponding row there.
pub const DOT_WAITING: Rgba = [0xFA, 0xB2, 0x19, 0xFF];
pub const DOT_WORKING: Rgba = [0x6A, 0x69, 0x65, 0xFF];
pub const DOT_DONE: Rgba = [0x4B, 0x4A, 0x47, 0xFF];
/// Finished, with something you have not looked at yet.
pub const DOT_UNREAD: Rgba = [0x2A, 0x78, 0xD6, 0xFF];
/// Ended badly. Claude's palette has no failure colour, so this is the one addition to it.
pub const DOT_ERROR: Rgba = [0xE5, 0x48, 0x4D, 0xFF];

/// Repository marks, sampled from the same session list as the status dots.
pub const REPO_OPEN: Rgba = [0x26, 0xD7, 0x37, 0xFF];
pub const REPO_MERGED: Rgba = [0xB2, 0x8B, 0xF8, 0xFF];

/// Colour for a repository state, and `None` where there is nothing to draw.
pub fn repo_mark(repo: Repo) -> Option<Rgba> {
    match repo {
        Repo::Nothing => None,
        Repo::PrOpen => Some(REPO_OPEN),
        // A merged pull request and a branch without one share a colour in the session list; the
        // shape is what tells them apart.
        Repo::PrMerged | Repo::Branch => Some(REPO_MERGED),
    }
}

/// Colour and fill for a status, following the Claude desktop app's convention: amber is waiting
/// on you, grey filled is working, a hollow ring is finished.
///
/// The app also distinguishes "finished and seen" (hollow) from "finished and not yet seen"
/// (blue). Nothing here knows whether you have looked at a session, so idle takes the hollow
/// ring — claiming the blue would be inventing the one fact that separates them.
///
/// A session whose status was never reported takes the same hollow ring. A fifth, dimmer grey was
/// tried to keep the two apart, but at this size a shade of grey communicates nothing, and on a
/// build that reports no status at all it just washes out every row. The row text carries the
/// distinction instead: `IDLE 4m` against `up 4h56m`.
pub fn status_dot(status: Status) -> (Rgba, bool) {
    match status {
        Status::Waiting => (DOT_WAITING, true),
        Status::Busy | Status::Shell => (DOT_WORKING, true),
        Status::Unread => (DOT_UNREAD, true),
        Status::Error => (DOT_ERROR, true),
        Status::Idle | Status::Unknown => (DOT_DONE, false),
    }
}

/// Size of the bitmap handed to a tray menu item.
pub const DOT_SIZE: u32 = 16;

fn put(buf: &mut [u8], x: i32, y: i32, color: Rgba) {
    if x < 0 || y < 0 || x >= DOT_SIZE as i32 || y >= DOT_SIZE as i32 {
        return;
    }
    let idx = ((y as u32 * DOT_SIZE + x as u32) * 4) as usize;
    buf[idx..idx + 4].copy_from_slice(&color);
}

/// Straight line, thick enough to survive the menu's own scaling.
fn line(buf: &mut [u8], from: (i32, i32), to: (i32, i32), color: Rgba) {
    let steps = (to.0 - from.0).abs().max((to.1 - from.1).abs()).max(1);
    for step in 0..=steps {
        let x = from.0 + (to.0 - from.0) * step / steps;
        let y = from.1 + (to.1 - from.1) * step / steps;
        put(buf, x, y, color);
        put(buf, x, y + 1, color);
    }
}

fn disc(buf: &mut [u8], cx: i32, cy: i32, radius: i32, color: Rgba) {
    for y in (cy - radius)..=(cy + radius) {
        for x in (cx - radius)..=(cx + radius) {
            let (dx, dy) = (x - cx, y - cy);
            if dx * dx + dy * dy <= radius * radius {
                put(buf, x, y, color);
            }
        }
    }
}

/// The bitmap for one menu row: the status dot, and the repository mark beside it when there is
/// one.
///
/// A menu item gets a single icon, so both have to share it. Without a repository mark the dot
/// keeps the whole bitmap and stays the size it always was.
pub fn menu_icon_rgba(dot: (Rgba, bool), repo: Repo) -> Vec<u8> {
    let (color, filled) = dot;
    let Some(mark) = repo_mark(repo) else {
        return dot_rgba(color, filled);
    };

    let mut buf = vec![0u8; (DOT_SIZE * DOT_SIZE * 4) as usize];
    let mid = DOT_SIZE as i32 / 2;

    // Status dot on the left, a size down to make room.
    disc(&mut buf, 3, mid, 3, color);
    if !filled {
        disc(&mut buf, 3, mid, 1, [0, 0, 0, 0]);
    }

    match repo {
        Repo::PrOpen | Repo::PrMerged => {
            // An arrow into a node.
            line(&mut buf, (8, mid), (12, mid), mark);
            line(&mut buf, (10, mid - 2), (12, mid), mark);
            line(&mut buf, (10, mid + 2), (12, mid), mark);
            disc(&mut buf, 13, mid, 1, mark);
        }
        Repo::Branch => {
            // A fork.
            line(&mut buf, (10, mid + 4), (10, mid), mark);
            line(&mut buf, (10, mid), (8, mid - 3), mark);
            line(&mut buf, (10, mid), (13, mid - 3), mark);
            disc(&mut buf, 8, mid - 3, 1, mark);
            disc(&mut buf, 13, mid - 3, 1, mark);
        }
        Repo::Nothing => {}
    }
    buf
}
const DOT_R: f32 = 4.6;
const DOT_RING_INNER_R: f32 = 2.9;

/// A single status dot as RGBA, for a menu item's icon.
pub fn dot_rgba(color: Rgba, filled: bool) -> Vec<u8> {
    let mut buf = vec![0u8; (DOT_SIZE * DOT_SIZE * 4) as usize];
    let center = DOT_SIZE as f32 / 2.0;

    for y in 0..DOT_SIZE {
        for x in 0..DOT_SIZE {
            let mut hits = 0u32;
            for sy in 0..AA {
                for sx in 0..AA {
                    let px = x as f32 + (sx as f32 + 0.5) / AA as f32 - center;
                    let py = y as f32 + (sy as f32 + 0.5) / AA as f32 - center;
                    let d = (px * px + py * py).sqrt();
                    if d <= DOT_R && (filled || d >= DOT_RING_INNER_R) {
                        hits += 1;
                    }
                }
            }
            if hits > 0 {
                let idx = ((y * DOT_SIZE + x) * 4) as usize;
                blend(
                    &mut buf[idx..idx + 4],
                    color,
                    hits as f32 / (AA * AA) as f32,
                );
            }
        }
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ascii(state: IconState) -> String {
        let buf = render(state);
        let (_, glyph) = palette(state.kind);
        let mut out = String::new();
        for y in 0..SIZE {
            for x in 0..SIZE {
                let i = ((y * SIZE + x) * 4) as usize;
                let px = &buf[i..i + 4];
                out.push(match px[3] {
                    0 => ' ',
                    1..=200 => '.',
                    _ if px[..3] == glyph[..3] => '#',
                    _ => '*',
                });
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn buffer_is_rgba8_of_the_declared_size() {
        let state = IconState {
            kind: IconKind::Waiting,
            count: 2,
        };
        assert_eq!(render(state).len(), (WIDTH * HEIGHT * 4) as usize);
    }

    /// Amber must mean waiting and nothing else: it is the one colour that says "act now".
    #[test]
    fn status_colours_follow_the_desktop_app() {
        assert_eq!(status_dot(Status::Waiting), (DOT_WAITING, true));
        assert_eq!(status_dot(Status::Busy), (DOT_WORKING, true));
        assert_eq!(status_dot(Status::Shell), (DOT_WORKING, true));
        // Neither finished nor unreported is ever filled: only an active session is.
        assert_eq!(status_dot(Status::Idle), (DOT_DONE, false));
        assert_eq!(status_dot(Status::Unknown), (DOT_DONE, false));
        assert!(!status_dot(Status::Idle).1);
        // Unread is filled: it is something to act on, not a finished-and-seen ring.
        assert_eq!(status_dot(Status::Unread), (DOT_UNREAD, true));
    }

    #[test]
    fn a_dot_is_rgba_of_the_declared_size() {
        assert_eq!(
            dot_rgba(DOT_WAITING, true).len(),
            (DOT_SIZE * DOT_SIZE * 4) as usize
        );
    }

    /// A hollow ring must actually have a hole, or it is just a filled dot.
    #[test]
    fn a_hollow_dot_is_transparent_in_the_middle() {
        let ring = dot_rgba(DOT_DONE, false);
        let middle = (((DOT_SIZE / 2) * DOT_SIZE + DOT_SIZE / 2) * 4) as usize;
        assert_eq!(ring[middle + 3], 0, "ring centre is not transparent");

        let filled = dot_rgba(DOT_DONE, true);
        assert!(filled[middle + 3] > 0, "filled dot has a hole");
    }

    #[test]
    fn counts_above_nine_collapse_to_nine_plus() {
        assert_eq!(badge_glyphs(0).len(), 0);
        assert_eq!(badge_glyphs(7), vec![7]);
        assert_eq!(badge_glyphs(12), vec![9, 10]);
    }

    /// Both glyphs have to stay legible sharing one 16px bitmap.
    /// `cargo test -- --nocapture menu_icons`
    #[test]
    fn menu_icons() {
        for (label, dot, repo) in [
            ("waiting + PR open", status_dot(Status::Waiting), Repo::PrOpen),
            ("unread + branch", status_dot(Status::Unread), Repo::Branch),
            ("idle + PR open (hollow, shared)", status_dot(Status::Idle), Repo::PrOpen),
            ("idle, no repo", status_dot(Status::Idle), Repo::Nothing),
        ] {
            let buf = menu_icon_rgba(dot, repo);
            println!("--- {label}");
            for y in 0..DOT_SIZE {
                let mut row = String::new();
                for x in 0..DOT_SIZE {
                    let i = ((y * DOT_SIZE + x) * 4) as usize;
                    row.push(if buf[i + 3] == 0 { '.' } else { '#' });
                }
                println!("{row}");
            }
        }
    }

    /// Run with `cargo test -- --nocapture` to eyeball the four states.
    #[test]
    fn preview() {
        for (label, kind, count) in [
            ("waiting x1", IconKind::Waiting, 1),
            ("busy x2", IconKind::Busy, 2),
            ("idle x4", IconKind::Idle, 4),
            ("empty", IconKind::Empty, 0),
        ] {
            println!("--- {label}\n{}", ascii(IconState { kind, count }));
        }
    }
}
