//! Quantize: move note starts ("Ticks") and/or note lengths ("Gates") onto a grid.
//!
//! # Rules (documented and tested)
//!
//! * **Ticks.** A start moves to the nearest grid point `origin + k·G`; an exact
//!   half-way tie goes to the *later* grid point. The note keeps its length, so
//!   its end moves by the same amount.
//! * **Gates.** A length is rounded to the nearest multiple of `G` (ties up). A
//!   note that would become zero ticks long gets one grid step (`G`) instead —
//!   quantizing never creates zero-length notes. Notes that already are zero
//!   ticks long (possible in imported files) stay zero: nothing is invented.
//! * **Both** applies both rules to each note's *original* values, in one step.
//! * Only notes change: tempo, time signatures, markers, pitch bend and pan are
//!   never moved. Overlapping notes stay overlapping — nothing is merged,
//!   shifted or deleted.
//! * All results are computed and validated before anything changes; if any note
//!   cannot be represented (beyond the largest tick) nothing changes at all.
//!
//! Rounding is monotonic, so the notes keep their stored order: the edit needs no
//! sort and costs O(selected notes).

use super::{EditControl, EditError, PreparedEdit, try_records};
use crate::model::PlumDocument;
use crate::model::note::NoteRecord;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum QuantizeMode {
    Ticks,
    Gates,
    Both,
}

impl QuantizeMode {
    pub fn label(self) -> &'static str {
        match self {
            QuantizeMode::Ticks => "Ticks",
            QuantizeMode::Gates => "Gates",
            QuantizeMode::Both => "Both",
        }
    }

    fn ticks(self) -> bool {
        matches!(self, QuantizeMode::Ticks | QuantizeMode::Both)
    }

    fn gates(self) -> bool {
        matches!(self, QuantizeMode::Gates | QuantizeMode::Both)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuantizeSpec {
    pub mode: QuantizeMode,
    /// Grid step in ticks (≥ 1).
    pub grid: u64,
    /// Tick the grid is aligned to (normally 0).
    pub origin: u64,
}

/// Parses a grid size typed by the user: a whole number of ticks ≥ 1.
pub fn parse_grid(input: &str) -> Result<u64, EditError> {
    let s = input.trim().replace(['_', ','], "");
    match s.parse::<u64>() {
        Ok(0) => Err(EditError::InvalidGrid(
            "The grid must be at least 1 tick.".into(),
        )),
        Ok(v) => Ok(v),
        Err(_) if s.starts_with('-') => Err(EditError::InvalidGrid(
            "The grid must be a positive number of ticks.".into(),
        )),
        Err(_) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => Err(
            EditError::InvalidGrid("That grid is larger than the largest possible tick.".into()),
        ),
        Err(_) => Err(EditError::InvalidGrid(format!(
            "\"{}\" is not a whole number of ticks.",
            input.trim()
        ))),
    }
}

/// Nearest grid point to `t` (ties to the later point); `None` when that point
/// is beyond the largest tick.
pub fn quantize_tick(t: u64, grid: u64, origin: u64) -> Option<u64> {
    let g = i128::from(grid.max(1));
    let rel = i128::from(t) - i128::from(origin);
    let down = rel.div_euclid(g) * g;
    let rem = rel - down;
    let q = if rem * 2 >= g { down + g } else { down };
    let mut v = i128::from(origin) + q;
    if v < 0 {
        // The nearest point is before tick 0: use the first grid point at or
        // after 0 instead.
        v += ((-v) + g - 1) / g * g;
    }
    u64::try_from(v).ok()
}

/// Rounded length (see module docs).
pub fn quantize_gate(d: u64, grid: u64) -> Option<u64> {
    if d == 0 {
        return Some(0);
    }
    let g = grid.max(1);
    let down = d - d % g;
    let q = if (d % g).checked_mul(2)? >= g {
        down.checked_add(g)?
    } else {
        down
    };
    Some(q.max(g))
}

/// The quantize rule for one note (shared by the dialog's preview and the commit).
pub fn quantize_record(r: &NoteRecord, spec: &QuantizeSpec) -> Result<NoteRecord, EditError> {
    let overflow = || EditError::TickOverflow {
        operation: "Quantize",
    };
    let start = if spec.mode.ticks() {
        quantize_tick(r.start_tick, spec.grid, spec.origin).ok_or_else(overflow)?
    } else {
        r.start_tick
    };
    let dur = if spec.mode.gates() {
        quantize_gate(r.duration_ticks, spec.grid).ok_or_else(overflow)?
    } else {
        r.duration_ticks
    };
    start.checked_add(dur).ok_or_else(overflow)?;
    Ok(NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: r.attrs,
        sub: r.sub,
        group: r.group,
    })
}

/// What quantizing the selection would do (computed with the same rules).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QuantizePreview {
    pub notes: usize,
    /// Notes whose start or gate changes.
    pub changed: usize,
    pub starts_moved: usize,
    pub gates_changed: usize,
    /// First note that changes: before → after.
    pub example: Option<(NoteRecord, NoteRecord)>,
    /// A note cannot be quantized (the operation would be refused).
    pub error: bool,
}

fn preview_record(p: &mut QuantizePreview, r: NoteRecord, spec: &QuantizeSpec) {
    p.notes += 1;
    match quantize_record(&r, spec) {
        Ok(q) => {
            if q.start_tick != r.start_tick {
                p.starts_moved += 1;
            }
            if q.duration_ticks != r.duration_ticks {
                p.gates_changed += 1;
            }
            if q != r {
                p.changed += 1;
                if p.example.is_none() {
                    p.example = Some((r, q));
                }
            }
        }
        Err(_) => p.error = true,
    }
}

/// What quantizing the selected notes of a track would do.
pub fn preview_quantize(
    doc: &PlumDocument,
    track_index: impl Into<super::NoteTarget>,
    spec: &QuantizeSpec,
) -> QuantizePreview {
    let mut p = QuantizePreview::default();
    if let Ok(t) = super::note_view(doc, track_index) {
        t.notes
            .for_each_selected(|v| preview_record(&mut p, v.record(), spec));
    }
    p
}

/// What quantizing every note of a track would do (the "all notes" scope; the
/// app selects all notes before applying).
pub fn preview_quantize_all(
    doc: &PlumDocument,
    track_index: impl Into<super::NoteTarget>,
    spec: &QuantizeSpec,
) -> QuantizePreview {
    let mut p = QuantizePreview::default();
    if let Ok(t) = super::note_view(doc, track_index) {
        for v in t.notes.iter() {
            preview_record(&mut p, v.record(), spec);
        }
    }
    p
}

/// Prepares quantizing the selected notes of a track (see module docs).
pub fn prepare_quantize(
    doc: &PlumDocument,
    track_index: impl Into<super::NoteTarget>,
    spec: &QuantizeSpec,
    control: EditControl<'_>,
) -> Result<PreparedEdit, EditError> {
    if spec.grid == 0 {
        return Err(EditError::InvalidGrid(
            "The grid must be at least 1 tick.".into(),
        ));
    }
    let t = super::note_view(doc, track_index)?;
    let n = t.notes.selected_count();
    if n == 0 {
        return Err(EditError::NothingSelected);
    }
    let mut records = try_records(n)?;
    let mut changed = false;
    let mut result: Result<(), EditError> = Ok(());
    let mut done = 0u64;
    t.notes.for_each_selected(|v| {
        if result.is_err() {
            return;
        }
        let r = v.record();
        match quantize_record(&r, spec) {
            Ok(q) => {
                changed |= q != r;
                records.push(q);
            }
            Err(e) => result = Err(e),
        }
        done += 1;
        if done.is_multiple_of(65_536)
            && let Err(e) = control.tick(done)
        {
            result = Err(e);
        }
    });
    result?;
    if !changed {
        return Ok(PreparedEdit::Identity);
    }
    debug_assert!(
        records
            .windows(2)
            .all(|w| w[0].start_tick <= w[1].start_tick)
    );
    Ok(PreparedEdit::ReplaceSelected {
        host: t.host(),
        base_revision: t.notes.revision(),
        records,
        shift: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_rounding_is_nearest_with_ties_to_the_later_point() {
        assert_eq!(quantize_tick(0, 240, 0), Some(0));
        assert_eq!(quantize_tick(119, 240, 0), Some(0));
        assert_eq!(quantize_tick(120, 240, 0), Some(240)); // exact tie → later
        assert_eq!(quantize_tick(480, 240, 0), Some(480)); // already aligned
        assert_eq!(quantize_tick(1, 3, 0), Some(0));
        assert_eq!(quantize_tick(2, 3, 0), Some(3));
        // Grid origin.
        assert_eq!(quantize_tick(1000, 240, 10), Some(970));
        assert_eq!(quantize_tick(5, 240, 10), Some(10));
        assert_eq!(quantize_tick(3, 100, 90), Some(90));
        // Huge ticks: the last grid point that fits, or refusal beyond it.
        let g = 1u64 << 40;
        let top = u64::MAX - u64::MAX % g;
        assert_eq!(quantize_tick(top + 1, g, 0), Some(top));
        assert_eq!(quantize_tick(u64::MAX, g, 0), None);
        // Monotonic.
        let mut prev = 0;
        for t in 0..5000u64 {
            let q = quantize_tick(t, 7, 3).unwrap();
            assert!(q >= prev);
            prev = q;
        }
    }

    #[test]
    fn gate_rounding_never_creates_zero_length_notes() {
        assert_eq!(quantize_gate(0, 240), Some(0)); // imported zero-length stays
        assert_eq!(quantize_gate(1, 240), Some(240)); // minimum one step
        assert_eq!(quantize_gate(119, 240), Some(240));
        assert_eq!(quantize_gate(360, 240), Some(480)); // tie → up
        assert_eq!(quantize_gate(359, 240), Some(240));
        assert_eq!(quantize_gate(1_000_000_123, 1000), Some(1_000_000_000));
        assert_eq!(quantize_gate(u64::MAX, 1 << 40), None);
    }

    #[test]
    fn grid_input_is_checked() {
        assert_eq!(parse_grid(" 240 "), Ok(240));
        assert_eq!(parse_grid("1,000"), Ok(1000));
        for bad in ["0", "-5", "abc", "", "1.5", "99999999999999999999999"] {
            assert!(parse_grid(bad).is_err(), "{bad}");
        }
    }
}
