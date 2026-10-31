//! Key geometry of the Player's rainfall and keyboard (v0.9 V09-S17,
//! `docs/PLAYER.md` P6 / P8): Kiva's layout — white keys share the width,
//! black keys are narrower and sit on the boundary between their white
//! neighbours, drawn after (over) the white ones.

use egui::{Color32, Painter, Pos2, Rect, Stroke};

/// Black keys are this fraction of a white key wide (Kiva).
pub const BLACK_WIDTH: f32 = 0.65;
/// Black keys reach this fraction of the keyboard's height.
pub const BLACK_HEIGHT: f32 = 0.62;

/// Whether `key` (0 = C−1) is a black key.
pub fn is_black(key: usize) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

/// One key's horizontal span on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeySpan {
    pub key: usize,
    pub x0: f32,
    pub x1: f32,
    pub black: bool,
}

/// The spans of keys `0..keys` over `[left, right]` (index = key). White
/// keys share the width equally; a black key is [`BLACK_WIDTH`] of a white
/// key, centred on the right edge of the white key below it (clamped to the
/// area).
pub fn spans(keys: usize, left: f32, right: f32) -> Vec<KeySpan> {
    let whites = (0..keys).filter(|&k| !is_black(k)).count().max(1);
    let w = (right - left) / whites as f32;
    let mut out = Vec::with_capacity(keys);
    let mut white = 0usize;
    for key in 0..keys {
        if is_black(key) {
            let edge = left + white as f32 * w;
            let half = w * BLACK_WIDTH / 2.0;
            out.push(KeySpan {
                key,
                x0: (edge - half).max(left),
                x1: (edge + half).min(right),
                black: true,
            });
        } else {
            out.push(KeySpan {
                key,
                x0: left + white as f32 * w,
                x1: left + (white + 1) as f32 * w,
                black: false,
            });
            white += 1;
        }
    }
    out
}

/// Kiva's note border: `round(0.00091 × width)` physical pixels, at least
/// one; in points.
pub fn inset_points(width_points: f32, pixels_per_point: f32) -> f32 {
    let ppp = pixels_per_point.max(0.1);
    (0.00091 * width_points * ppp).round().max(1.0) / ppp
}

/// The rainfall's columns: `(texel row, x0, x1)` per key — white keys
/// first, then black keys, in key order — each inset by `inset` on both
/// sides (less when the key is narrow, so at least `min_fill` stays).
/// Texel row 0 is the highest key (`rows − 1 − key`).
pub fn columns(
    spans: &[KeySpan],
    rows: usize,
    inset: f32,
    min_fill: f32,
) -> Vec<(usize, f32, f32)> {
    let col = |s: &KeySpan| {
        let w = s.x1 - s.x0;
        let i = inset.min(((w - min_fill) / 2.0).max(0.0));
        (rows.saturating_sub(1 + s.key), s.x0 + i, s.x1 - i)
    };
    let whites = spans.iter().filter(|s| !s.black).map(col);
    let blacks = spans.iter().filter(|s| s.black).map(col);
    whites.chain(blacks).filter(|&(r, _, _)| r < rows).collect()
}

/// What a key shows while notes sound on it (P8): the keyboard tracker's
/// colour bands (`app::keys::KeyDisplay`); `None` or no notes: the key's
/// own colour.
pub type KeyTint<'a> = &'a dyn Fn(usize) -> Option<crate::app::keys::KeyDisplay>;

/// Paints a horizontal keyboard in `rect` with `spans` (from [`spans`] over
/// the same width): white keys, separators, then black keys over them; a
/// sounding key in its colour bands side by side (the roll's keyboard
/// stacks them; up to `app::keys::MAX_BANDS`).
pub fn paint_keyboard(p: &Painter, rect: Rect, spans: &[KeySpan], tint: KeyTint<'_>) {
    const WHITE: Color32 = Color32::from_rgb(236, 236, 232);
    const BLACK: Color32 = Color32::from_rgb(24, 24, 26);
    const EDGE: Color32 = Color32::from_rgb(70, 70, 74);
    p.rect_filled(rect, 0.0, EDGE);
    let black_bottom = rect.top() + rect.height() * BLACK_HEIGHT;
    let key = |r: Rect, k: usize, own: Color32| match tint(k).filter(|d| d.total > 0 && d.n > 0) {
        Some(d) => {
            let n = usize::from(d.n).min(d.bands.len());
            let w = r.width() / n as f32;
            for (i, c) in d.bands[..n].iter().enumerate() {
                let x0 = r.left() + i as f32 * w;
                let band =
                    Rect::from_min_max(Pos2::new(x0, r.top()), Pos2::new(x0 + w, r.bottom()));
                p.rect_filled(band, 0.0, *c);
            }
        }
        None => {
            p.rect_filled(r, 0.0, own);
        }
    };
    for s in spans.iter().filter(|s| !s.black) {
        let r = Rect::from_min_max(Pos2::new(s.x0, rect.top()), Pos2::new(s.x1, rect.bottom()));
        key(r, s.key, WHITE);
        p.vline(s.x1, rect.y_range(), Stroke::new(1.0, EDGE));
    }
    for s in spans.iter().filter(|s| s.black) {
        let r = Rect::from_min_max(Pos2::new(s.x0, rect.top()), Pos2::new(s.x1, black_bottom));
        key(r, s.key, BLACK);
        p.rect_stroke(r, 0.0, Stroke::new(1.0, EDGE), egui::StrokeKind::Inside);
    }
    // The playhead line: the top edge of the keyboard.
    p.hline(
        rect.x_range(),
        rect.top(),
        Stroke::new(1.0, Color32::from_gray(150)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_keys_share_the_width_and_black_keys_sit_on_their_boundaries() {
        for (keys, whites) in [(128usize, 75usize), (256, 149)] {
            let s = spans(keys, 10.0, 10.0 + whites as f32 * 8.0);
            assert_eq!(s.len(), keys);
            assert_eq!(s.iter().filter(|k| !k.black).count(), whites);
            // Whites tile the width without gaps.
            let w: Vec<&KeySpan> = s.iter().filter(|k| !k.black).collect();
            assert_eq!(w[0].x0, 10.0);
            assert!((w.last().unwrap().x1 - (10.0 + whites as f32 * 8.0)).abs() < 1e-3);
            for p in w.windows(2) {
                assert!((p[0].x1 - p[1].x0).abs() < 1e-3);
                assert!((p[0].x1 - p[0].x0 - 8.0).abs() < 1e-3);
            }
            // A black key is centred on the edge of the white key below it.
            for k in s.iter().filter(|k| k.black) {
                let below = &s[k.key - 1];
                assert!(!below.black);
                let centre = (k.x0 + k.x1) / 2.0;
                if k.x1 < 10.0 + whites as f32 * 8.0 {
                    assert!((centre - below.x1).abs() < 1e-3, "key {}", k.key);
                    assert!((k.x1 - k.x0 - 8.0 * BLACK_WIDTH).abs() < 1e-3);
                }
            }
        }
        assert!(is_black(1) && is_black(61) && !is_black(60) && !is_black(64));
    }

    #[test]
    fn columns_are_white_then_black_inset_and_map_keys_to_texel_rows() {
        let s = spans(128, 0.0, 750.0);
        let cols = columns(&s, 128, 1.0, 1.0);
        assert_eq!(cols.len(), 128);
        // White keys first (75), then the 53 black keys.
        let first_black = cols
            .iter()
            .position(|&(r, _, _)| is_black(127 - r))
            .unwrap();
        assert_eq!(first_black, 75);
        assert!(
            cols[first_black..]
                .iter()
                .all(|&(r, _, _)| is_black(127 - r))
        );
        // Inset on both sides; row 0 is the highest key.
        let c60 = cols.iter().find(|&&(r, _, _)| r == 127 - 60).unwrap();
        assert!((c60.1 - (s[60].x0 + 1.0)).abs() < 1e-4);
        assert!((c60.2 - (s[60].x1 - 1.0)).abs() < 1e-4);
        // Narrow keys keep at least `min_fill` (or all of a key narrower
        // than that: the top D# of 256 keys, cut at the edge).
        let ns = spans(256, 0.0, 300.0);
        let narrow = columns(&ns, 256, 2.0, 1.0);
        for &(r, a, b) in &narrow {
            let k = &ns[255 - r];
            assert!(b - a >= (k.x1 - k.x0).min(1.0) - 1e-4, "key {}", k.key);
        }
        // Kiva's border: 0.00091 × width physical px, at least one.
        assert_eq!(inset_points(1000.0, 1.0), 1.0);
        assert_eq!(inset_points(1920.0, 1.25), 2.0 / 1.25);
        assert_eq!(inset_points(200.0, 2.0), 0.5);
    }
}
