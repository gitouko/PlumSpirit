//! The mandatory Preview architecture (v0.5): one per-note rule ("kernel") shared
//! by the live preview, the full preflight and the commit of a batch note
//! operation.
//!
//! # Contract for every batch note/event manipulation (present and future)
//!
//! A feature that changes many notes at once implements the four steps below
//! (internal API; not a plugin interface):
//!
//! 1. **prepare_preview** — build a [`NoteKernel`] from the operation's
//!    parameters and the committed document (e.g. [`NoteKernel::scale_x`]). Cheap:
//!    O(1) or O(selection bounds), never a copy of the notes.
//! 2. **update_preview(parameters)** — build a new kernel; the app gives it a new
//!    generation id, so a slow summary for old parameters can never replace the
//!    newer one ([`PreviewGeneration`]). Drawing uses [`visible_preview`], which
//!    visits only notes whose result can reach the viewport, with a budget.
//! 3. **apply** — the operation's `prepare_*` function evaluates the *same*
//!    kernel for every selected note, validates the complete result (nothing
//!    changes if any note cannot be represented) and commits it through the undo
//!    history as one step.
//! 4. **cancel** — drop the kernel. Previewing never touches the document: no
//!    dirty flag, no undo entry, no change to saving, export or playback.
//!
//! Direct manipulation (dragging notes, resizing, drawing) already previews
//! transiently and commits once on release; it does not need a dialog.

use super::EditError;
use super::factor::ExactFactor;
use super::quantize::{QuantizeSpec, quantize_record};
use crate::model::correction::{Corrected, Correction};
use crate::model::note::NoteRecord;
use crate::model::note_store::SelectionBounds;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The per-note rule of a batch note operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoteKernel {
    /// `start' = s + (e − end)` (Flip X over the selection's bounds `[s, e]`).
    FlipX { s: u64, e: u64 },
    /// Both endpoints scaled around `pivot` (Scale X).
    ScaleX { pivot: u64, factor: ExactFactor },
    /// New pitch for each old pitch (Scale Y, Flip Y).
    PitchTable(Box<super::PitchTable>),
    /// Quantize ticks and/or gates.
    Quantize(QuantizeSpec),
    /// Every note gets this length (the Gate field with notes selected).
    SetDuration(u64),
    /// Every note gets this velocity, 1–127 (the Velocity field).
    SetVelocity(u8),
    /// How the notes play and export with a track's corrections (Track
    /// Properties). Preview only: Apply stores the properties, never the notes;
    /// playback and export evaluate the same [`Correction::note`]. `max_dur` is
    /// the track's longest note (bounds the visible-window search when the
    /// gate lengthens notes).
    Correction { c: Correction, max_dur: u64 },
}

/// What happens to one note in a preview.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelOutcome {
    /// The note as it will be (possibly unchanged).
    Note(NoteRecord),
    /// The note will not sound or export (outside a correction's key range, or
    /// transposed beyond 0–127): drawn as removed.
    Removed,
}

impl NoteKernel {
    /// Flip X over the selection's time bounds.
    pub fn flip_x(b: &SelectionBounds) -> NoteKernel {
        NoteKernel::FlipX {
            s: b.min_start,
            e: b.max_end,
        }
    }

    /// Scale X around the earliest selected start. Refuses (in O(1)) factors that
    /// would move the farthest endpoint out of range: scaling is monotonic, so
    /// every other endpoint then fits too.
    pub fn scale_x(b: &SelectionBounds, factor: &ExactFactor) -> Result<NoteKernel, EditError> {
        let op = "Scale X";
        let pivot = b.min_start;
        let span = b.max_end - pivot;
        let extreme = factor
            .scale_round(span)
            .ok_or(EditError::TickOverflow { operation: op })?;
        if factor.is_negative() {
            if extreme > pivot {
                return Err(EditError::NegativeTick {
                    operation: op,
                    earliest: pivot as i128 - extreme as i128,
                });
            }
        } else {
            pivot
                .checked_add(extreme)
                .ok_or(EditError::TickOverflow { operation: op })?;
        }
        Ok(NoteKernel::ScaleX {
            pivot,
            factor: factor.clone(),
        })
    }

    /// Applies the rule to one note. An error means the whole operation is
    /// refused (nothing changes).
    #[inline]
    pub fn map(&self, r: &NoteRecord) -> Result<NoteRecord, EditError> {
        match self {
            NoteKernel::FlipX { s, e } => {
                let overflow = || EditError::TickOverflow {
                    operation: "Flip X",
                };
                let end = r.end_tick().ok_or_else(overflow)?;
                let start = e
                    .checked_sub(end)
                    .and_then(|d| s.checked_add(d))
                    .ok_or_else(overflow)?;
                Ok(NoteRecord {
                    start_tick: start,
                    ..*r
                })
            }
            NoteKernel::ScaleX { pivot, factor } => {
                let op = "Scale X";
                let end = r
                    .end_tick()
                    .ok_or(EditError::TickOverflow { operation: op })?;
                let ds = r
                    .start_tick
                    .checked_sub(*pivot)
                    .ok_or(EditError::TickOverflow { operation: op })?;
                let de = end - pivot;
                let ms = factor
                    .scale_round(ds)
                    .ok_or(EditError::TickOverflow { operation: op })?;
                let me = factor
                    .scale_round(de)
                    .ok_or(EditError::TickOverflow { operation: op })?;
                let (start, end) = if factor.is_negative() {
                    (
                        pivot
                            .checked_sub(me)
                            .ok_or_else(|| EditError::NegativeTick {
                                operation: op,
                                earliest: *pivot as i128 - me as i128,
                            })?,
                        pivot - ms.min(*pivot),
                    )
                } else {
                    (
                        pivot
                            .checked_add(ms)
                            .ok_or(EditError::TickOverflow { operation: op })?,
                        pivot
                            .checked_add(me)
                            .ok_or(EditError::TickOverflow { operation: op })?,
                    )
                };
                Ok(NoteRecord {
                    start_tick: start,
                    duration_ticks: end - start,
                    ..*r
                })
            }
            NoteKernel::PitchTable(table) => {
                // Entries were validated against the key range when the
                // table was built.
                let p = table[usize::from(r.attrs.pitch())];
                Ok(NoteRecord {
                    attrs: r
                        .attrs
                        .with_pitch_wide(p)
                        .map_err(|_| EditError::PitchOutOfRange {
                            operation: "This edit",
                            lowest: i32::from(p),
                            highest: i32::from(p),
                            top: crate::model::note::MAX_PITCH_256,
                        })?,
                    ..*r
                })
            }
            NoteKernel::Quantize(spec) => quantize_record(r, spec),
            NoteKernel::SetDuration(d) => {
                if *d == 0 {
                    return Err(EditError::DurationTooShort);
                }
                if r.start_tick.checked_add(*d).is_none() {
                    return Err(EditError::TickOverflow {
                        operation: "Change Gate",
                    });
                }
                Ok(NoteRecord {
                    duration_ticks: *d,
                    ..*r
                })
            }
            NoteKernel::SetVelocity(v) => Ok(NoteRecord {
                attrs: r
                    .attrs
                    .with_velocity(*v)
                    .map_err(|_| EditError::InvalidVelocity(*v))?,
                ..*r
            }),
            NoteKernel::Correction { .. } => match self.outcome(r)? {
                KernelOutcome::Note(n) => Ok(n),
                KernelOutcome::Removed => Err(EditError::Invalid(
                    "A corrected note falls outside the playable keys.".into(),
                )),
            },
        }
    }

    /// The preview result of one note: like [`NoteKernel::map`], but a note that
    /// would stop sounding (a track correction's key range) is
    /// [`KernelOutcome::Removed`] instead of an error.
    #[inline]
    pub fn outcome(&self, r: &NoteRecord) -> Result<KernelOutcome, EditError> {
        match self {
            NoteKernel::Correction { c, .. } => match c.note(*r) {
                Corrected::Note { note, .. } => Ok(KernelOutcome::Note(note)),
                Corrected::OutsideRange | Corrected::PitchOverflow => Ok(KernelOutcome::Removed),
                Corrected::EndOverflow => Err(EditError::Invalid(format!(
                    "A corrected note starting at tick {} would end after the largest possible tick, so the project could not be exported.",
                    { r.start_tick }
                ))),
            },
            _ => self.map(r).map(KernelOutcome::Note),
        }
    }

    /// A tick range `[a, b]` such that every selected note whose result
    /// intersects `[t0, t1]` intersects `[a, b]` (conservative). Used to visit only
    /// the notes that can appear in the viewport.
    pub fn source_window(&self, t0: u64, t1: u64) -> (u64, u64) {
        match self {
            NoteKernel::FlipX { s, e } => {
                let sum = u128::from(*s) + u128::from(*e);
                let clamp = |v: u128| u64::try_from(v).unwrap_or(u64::MAX);
                (
                    clamp(sum.saturating_sub(u128::from(t1))),
                    clamp(sum.saturating_sub(u128::from(t0))),
                )
            }
            NoteKernel::PitchTable(_) | NoteKernel::SetVelocity(_) => (t0, t1),
            // A note whose new length reaches the view may have ended before it.
            NoteKernel::SetDuration(d) => (t0.saturating_sub(*d), t1),
            NoteKernel::Correction { c, max_dur } => {
                // Results move by `ticks` (or not at all before `from_tick`) and
                // may be longer (gate): widen by both.
                let longer = c
                    .gate
                    .apply(*max_dur)
                    .unwrap_or(u64::MAX)
                    .saturating_sub(*max_dur);
                let back = u64::try_from(c.ticks.max(0)).unwrap_or(0);
                let fwd = c.ticks.min(0).unsigned_abs();
                (
                    t0.saturating_sub(back).saturating_sub(longer),
                    t1.saturating_add(fwd),
                )
            }
            NoteKernel::Quantize(spec) => (
                t0.saturating_sub(spec.grid.saturating_mul(2)),
                t1.saturating_add(spec.grid.saturating_mul(2)),
            ),
            NoteKernel::ScaleX { pivot, factor } => {
                let f = factor.approx_f64();
                if f.abs() < 1e-12 {
                    return (0, u64::MAX);
                }
                let inv = |x: u64| -> f64 { *pivot as f64 + (x as f64 - *pivot as f64) / f };
                let (a, b) = (inv(t0), inv(t1));
                let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                // Widen for rounding and f64 error.
                let margin = 2.0 + (hi - lo).abs() * 1e-9 + 1.0 / f.abs();
                let lo = (lo - margin).max(0.0);
                let hi = hi + margin;
                (
                    if lo >= u64::MAX as f64 {
                        u64::MAX
                    } else {
                        lo as u64
                    },
                    if hi >= u64::MAX as f64 {
                        u64::MAX
                    } else {
                        hi as u64
                    },
                )
            }
        }
    }
}

/// Monotonic generation ids: a result computed for an older generation is
/// discarded when it arrives after a newer request (stale preview).
#[derive(Debug, Default)]
pub struct PreviewGeneration(AtomicU64);

impl PreviewGeneration {
    /// Starts a new generation (parameters changed) and returns its id.
    pub fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn current(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    /// Whether a result of generation `g` may still be shown.
    pub fn is_current(&self, g: u64) -> bool {
        self.current() == g
    }
}

/// Full-selection preflight of a kernel: counts and the first change, or the
/// reason the operation would be refused. O(selected notes), cancellable (the
/// flag is checked every 64 Ki notes). Never modifies anything.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreviewSummary {
    pub notes: usize,
    pub changed: usize,
    /// Notes that would stop sounding (track corrections).
    pub removed: usize,
    pub example: Option<(NoteRecord, NoteRecord)>,
    /// Why Apply would be refused (full sentence).
    pub error: Option<String>,
    pub cancelled: bool,
}

/// Which notes an operation applies to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreviewScope {
    /// The selected notes (every batch tool).
    #[default]
    Selected,
    /// Every note of the track (e.g. Quantize "all notes").
    All,
}

pub fn summarize<'a>(
    track: impl Into<super::HostView<'a>>,
    kernel: &NoteKernel,
    scope: PreviewScope,
    cancel: &AtomicBool,
) -> PreviewSummary {
    let track: super::HostView<'a> = track.into();
    let mut s = PreviewSummary::default();
    let mut k = 0u32;
    let mut visit = |v: crate::model::note_store::NoteView| {
        if s.error.is_some() || s.cancelled {
            return;
        }
        k += 1;
        if k.is_multiple_of(65_536) && cancel.load(Ordering::Relaxed) {
            s.cancelled = true;
            return;
        }
        s.notes += 1;
        let r = v.record();
        match kernel.outcome(&r) {
            Ok(KernelOutcome::Note(q)) => {
                if q != r {
                    s.changed += 1;
                    if s.example.is_none() {
                        s.example = Some((r, q));
                    }
                }
            }
            Ok(KernelOutcome::Removed) => s.removed += 1,
            Err(e) => s.error = Some(e.user_message()),
        }
    };
    match scope {
        PreviewScope::Selected => track.notes.for_each_selected(&mut visit),
        PreviewScope::All => track.notes.iter().for_each(&mut visit),
    }
    s
}

/// Visits (original, result) of the selected notes whose result can be visible in
/// `[t0, t1]`, at most `budget` of them; returns how many were visited and whether
/// the budget cut the visit short (the view then shows a simplified preview).
/// A note that would stop sounding is visited with result `None` (where it is
/// now). Work is bounded by the notes near the viewport, never by the whole track.
#[allow(clippy::too_many_arguments)]
pub fn visible_preview<'a, F: FnMut(&NoteRecord, Option<&NoteRecord>)>(
    track: impl Into<super::HostView<'a>>,
    kernel: &NoteKernel,
    scope: PreviewScope,
    t0: u64,
    t1: u64,
    budget: usize,
    mut visit: F,
) -> (usize, bool) {
    let track = track.into();
    let (a, b) = kernel.source_window(t0, t1);
    let mut n = 0usize;
    let mut truncated = false;
    track.notes.query(a, b, |_, v| {
        if (scope == PreviewScope::Selected && !v.selected) || truncated {
            return;
        }
        if n >= budget {
            truncated = true;
            return;
        }
        let r = v.record();
        match kernel.outcome(&r) {
            Ok(KernelOutcome::Note(q))
                if q.start_tick <= t1 && q.end_tick().is_some_and(|e| e >= t0) =>
            {
                visit(&r, Some(&q));
                n += 1;
            }
            Ok(KernelOutcome::Removed)
                if r.start_tick <= t1 && r.end_tick().is_some_and(|e| e >= t0) =>
            {
                visit(&r, None);
                n += 1;
            }
            _ => {}
        }
    });
    (n, truncated)
}
