//! Draws the tray icon: a status-colored disc with the session count on it.
//!
//! 32x32 RGBA, rendered from scratch so there are no image assets or font dependencies.

use crate::session::{IconKind, IconState};

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

    #[test]
    fn counts_above_nine_collapse_to_nine_plus() {
        assert_eq!(badge_glyphs(0).len(), 0);
        assert_eq!(badge_glyphs(7), vec![7]);
        assert_eq!(badge_glyphs(12), vec![9, 10]);
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
