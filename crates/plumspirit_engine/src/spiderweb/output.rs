//! A Spiderweb board's shapes → notes for PlumSpirit: every shape's notes
//! (claw, strum, colours), the 15-colour cap, channels (As drawn / Single /
//! Multi), then [`Generated`] notes whose colour is the channel slot, ready
//! for `transform::paste`. Preflight estimates come before anything is made.

use super::custom::Custom;
use super::engine::{self, ChannelMode, Note4, Note6, Split};
use super::paths::{dedupe, pitch_of};
use super::{COLOURS, capped_colours, kind, notes_tracks, shape_strokes, tracks_apart};
use crate::json::Value;
use crate::transform::paste::{ColorInfo, ColorNote, Generated};
use crate::transform::preflight::Estimate;
use std::sync::atomic::{AtomicBool, Ordering};

/// Spiderweb's 15 slot colours (fill), one per channel slot.
pub const SLOT_COLORS: [[u8; 3]; 15] = [
    [0x7e, 0xa6, 0xf5],
    [0xf5, 0x8e, 0x8e],
    [0x8f, 0xd6, 0x8f],
    [0xe6, 0xc6, 0x5c],
    [0xc7, 0x9b, 0xf2],
    [0x6f, 0xd6, 0xd6],
    [0xf2, 0xa3, 0x6b],
    [0xb0, 0xb0, 0xb0],
    [0xf5, 0x9a, 0xd0],
    [0xc2, 0xe3, 0x6b],
    [0x9a, 0xa0, 0xf5],
    [0xc9, 0xa2, 0x7e],
    [0x7f, 0xd6, 0xb0],
    [0xe3, 0x8a, 0xe3],
    [0x8f, 0xb8, 0xd6],
];

/// Bytes per generated note while making and pasting (per-shape rows,
/// rendered rows, colour notes, records).
pub const TEMP_BYTES_PER_NOTE: u64 = 32 + 48 + 24 + 24;

/// How the board's notes become channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Channels {
    pub mode: ChannelMode,
    pub split: Split,
}

impl Default for Channels {
    fn default() -> Self {
        Channels {
            mode: ChannelMode::Auto,
            split: Split::Key,
        }
    }
}

/// Every shape's notes, tracks and whether its tracks stay apart.
pub type Lists = (Vec<Vec<Note4>>, Vec<Option<Vec<i64>>>, Vec<bool>);

/// Every shape's notes, tracks and "apart" (the oracle's flow), or None
/// when cancelled.
pub fn shape_lists(shapes: &[Value], ppq: u32, keys: i64, cancel: &AtomicBool) -> Option<Lists> {
    let (mut lists, mut tracks, mut apart) = (Vec::new(), Vec::new(), Vec::new());
    for sh in shapes {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let (n, t) = notes_tracks(sh, ppq, keys);
        let wanted = match &t {
            Some(t) if !sh.has("notes") => {
                let mut ids = t.clone();
                ids.sort_unstable();
                ids.dedup();
                ids.len()
            }
            _ => 0,
        };
        let t = if wanted > COLOURS {
            t.map(|t| capped_colours(&t))
        } else {
            t
        };
        lists.push(n);
        tracks.push(t);
        apart.push(tracks_apart(sh));
    }
    Some((lists, tracks, apart))
}

/// The board rendered: `(start, end, pitch, velocity, slot, owner)` and the
/// number of slots used (None: cancelled).
pub fn render_board(
    shapes: &[Value],
    ppq: u32,
    keys: i64,
    ch: Channels,
    cancel: &AtomicBool,
) -> Option<(Vec<Note6>, usize)> {
    let (lists, tracks, apart) = shape_lists(shapes, ppq, keys, cancel)?;
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let (mut notes, mut slots) = engine::render(&lists, ch.mode, ch.split, &tracks, &apart);
    // PlumSpirit: shapes with a fixed colour keep it whatever the mode
    let fixed: Vec<Option<i64>> = shapes.iter().map(super::edit::fixed_colour).collect();
    if fixed.iter().any(Option::is_some) {
        for n in &mut notes {
            if let Some(Some(c)) = usize::try_from(n[5]).ok().and_then(|o| fixed.get(o)) {
                n[4] = *c;
                slots = slots.max(*c as usize + 1);
            }
        }
    }
    Some((notes, slots))
}

/// Rendered notes → colour notes (colour = channel slot), relative to the
/// earliest note when `from_zero` (else to tick 0); notes outside the board's
/// `keys` rows (0..keys, as given to [`render_board`]) make nothing. Returns
/// the artwork and the tick it starts at.
pub fn to_generated(
    rendered: &[Note6],
    slots: usize,
    from_zero: bool,
    keys: i64,
) -> (Generated, i64) {
    let t0 = if from_zero {
        rendered.iter().map(|n| n[0]).min().unwrap_or(0)
    } else {
        0
    };
    let mut notes: Vec<ColorNote> = rendered
        .iter()
        .filter(|n| n[1] > n[0] && (0..keys.min(256)).contains(&n[2]))
        .map(|n| ColorNote {
            start: (n[0] - t0).max(0) as u64,
            dur: (n[1] - n[0]) as u64,
            pitch: n[2] as u8,
            vel: n[3].clamp(1, 127) as u8,
            color: n[4].max(0) as u32,
        })
        .collect();
    notes.sort_unstable();
    let colors = (0..slots.max(1))
        .map(|s| {
            let mut c = ColorInfo::new(SLOT_COLORS[s % SLOT_COLORS.len()]);
            c.name = format!("Channel {}", engine::slot_channel(s) + 1);
            c
        })
        .collect();
    (Generated { notes, colors }, t0)
}

/// About how many notes a shape makes, worked out without making them
/// (spam and Hz bass can be millions); an upper bound.
pub fn estimate_notes(sh: &Value, ppq: u32) -> u64 {
    let base = match kind(sh) {
        "custom" => Custom::new(sh, ppq).estimate(),
        "funnel" => super::funnel::funnel_note_count(sh, ppq) as u64,
        _ => {
            // a line makes one note per key it crosses, plus at most one
            // per point (where it turns back in time a new piece starts)
            let mut n = 0u64;
            for st in shape_strokes(sh) {
                let p = dedupe(&st);
                n += p.len() as u64;
                for w in p.windows(2) {
                    n += pitch_of(w[1][1]).abs_diff(pitch_of(w[0][1]));
                }
            }
            n
        }
    };
    let mut n = base;
    // the claw cuts notes at the edges of the slice it throws away
    if let Some(c) = sh.get("claw").filter(|c| c.truthy())
        && super::gets(c, "mode") == Some("time")
        && c.get("trash").is_some_and(Value::truthy)
    {
        let pts: Vec<[f64; 2]> = shape_strokes(sh).concat();
        let lo = pts.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
        let hi = pts.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max);
        let k_lo = pts.iter().map(|p| pitch_of(p[1])).min().unwrap_or(0);
        let k_hi = pts.iter().map(|p| pitch_of(p[1])).max().unwrap_or(0);
        let period = super::getf(c, "period", 1.0).max(1e-6);
        let periods = if hi > lo {
            ((hi - lo) / period).ceil() as u64 + 2
        } else {
            2
        };
        n = n.saturating_add(
            periods
                .saturating_mul(2)
                .saturating_mul((k_hi - k_lo + 1).max(1) as u64),
        );
    }
    // a strum's "chop" cuts held notes where others start (at most 5,000,000 pieces)
    if let Some(st) = sh.get("strum").filter(|c| c.truthy())
        && st.get("chop").is_some_and(Value::truthy)
    {
        n = n.saturating_add(n.saturating_mul(n).min(5_000_000));
    }
    n
}

/// The preflight estimate of making these shapes.
pub fn estimate(shapes: &[Value], ppq: u32) -> Estimate {
    let notes = shapes
        .iter()
        .map(|s| estimate_notes(s, ppq))
        .fold(0u64, u64::saturating_add);
    let mut e = Estimate::for_notes(0, notes);
    e.temp_bytes = notes.saturating_mul(TEMP_BYTES_PER_NOTE);
    e
}
