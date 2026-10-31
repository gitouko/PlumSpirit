//! Flip X / Flip Y / Scale X / Scale Y on the selected notes of one track.
//!
//! Only selected notes change. Tempo, controllers and every other preserved event
//! keep their ticks: these are *note* transforms.
//!
//! # Formulas (all integer / exact rational arithmetic)
//!
//! * **Flip X** mirrors each note span inside the selection's time bounding box
//!   `[S, E]` (S = earliest start, E = latest end):
//!   `new_start = S + E − old_end`, `new_end = S + E − old_start`.
//!   Durations are unchanged and flipping twice restores every position exactly.
//! * **Flip Y** mirrors pitches inside the selected pitch range `[lo, hi]`:
//!   `new = lo + hi − old`. Always stays inside `[lo, hi]`.
//! * **Scale X** scales both endpoints of every note around pivot `S`:
//!   `x' = S + f·(x − S)`, rounded to the nearest tick with halves away from the
//!   pivot. With a negative factor the endpoints are swapped so `start <= end`.
//! * **Scale Y** scales pitch around the exact center of the selected pitch range
//!   `(lo + hi) / 2` (which may be a half semitone), rounded to the nearest pitch
//!   with halves away from the pivot; when a note lands exactly on a half-semitone
//!   pivot (factor 0) it rounds up.
//!
//! Every transform validates the complete result first and is refused as a whole if
//! any note would leave the valid tick range or the pitch range 0..=127.

use super::factor::ExactFactor;
use super::preview::NoteKernel;
use super::target::{HostView, NoteTarget, note_view};
use super::{EditControl, EditError, PreparedEdit, commit, try_records};
use crate::model::document::PlumDocument;
use crate::model::note::NoteRecord;
use crate::model::project::KeyRange;
use crate::transform::geom::{CollapseFilter, NoteSpot};

fn selected_track(
    doc: &PlumDocument,
    target: impl Into<NoteTarget>,
) -> Result<HostView<'_>, EditError> {
    let t = note_view(doc, target)?;
    if t.notes.selected_count() == 0 {
        return Err(EditError::NothingSelected);
    }
    Ok(t)
}

/// Sorts records by start tick, keeping their relative order on ties. Skips the
/// sort when already ordered (e.g. positive Scale X preserves order).
fn sort_records(records: &mut [NoteRecord]) {
    if !records.is_sorted_by_key(|r| r.start_tick) {
        records.sort_by_key(|r| r.start_tick);
    }
}

pub fn prepare_flip_x(
    doc: &PlumDocument,
    track_index: impl Into<NoteTarget>,
    control: EditControl<'_>,
) -> Result<PreparedEdit, EditError> {
    let t = selected_track(doc, track_index)?;
    let b = t
        .notes
        .selection_bounds()
        .ok_or(EditError::NothingSelected)?;
    let kernel = NoteKernel::flip_x(&b);
    let records = map_selected(t, &kernel, b.count, control)?;
    Ok(PreparedEdit::ReplaceSelected {
        host: t.host(),
        base_revision: t.notes.revision(),
        records,
        shift: None,
    })
}

/// Evaluates `kernel` for every selected note (the same rule the preview draws),
/// validating all of them; sorted by start. O(selected notes).
fn map_selected(
    t: HostView<'_>,
    kernel: &NoteKernel,
    count: usize,
    control: EditControl<'_>,
) -> Result<Vec<NoteRecord>, EditError> {
    // The loop is instantiated per kernel kind, so the compiler sees the rule
    // itself instead of a dispatch per note (same `NoteKernel::map` code).
    match kernel {
        NoteKernel::FlipX { .. } => {
            map_selected_with(t, count, control, true, None, |r| kernel.map(r))
        }
        NoteKernel::ScaleX { .. } => {
            map_selected_with(t, count, control, true, None, |r| kernel.map(r))
        }
        _ => map_selected_with(t, count, control, true, None, |r| kernel.map(r)),
    }
}

#[inline(always)]
fn map_selected_with<F: Fn(&NoteRecord) -> Result<NoteRecord, EditError>>(
    t: HostView<'_>,
    count: usize,
    control: EditControl<'_>,
    sorted: bool,
    mut collapse: Option<&mut CollapseFilter>,
    map: F,
) -> Result<Vec<NoteRecord>, EditError> {
    let mut records = try_records(count)?;
    let mut done = 0u64;
    let mut failure: Option<EditError> = None;
    t.notes.for_each_selected(|v| {
        if failure.is_some() {
            return;
        }
        let src = v.record();
        match map(&src) {
            Ok(r) => {
                if let Some(f) = collapse.as_deref_mut() {
                    f.push(NoteSpot::of_record(&r), NoteSpot::of_record(&src));
                }
                records.push(r)
            }
            Err(e) => {
                failure = Some(e);
                return;
            }
        }
        done += 1;
        if done.is_multiple_of(65_536)
            && let Err(e) = control.tick(done)
        {
            failure = Some(e);
        }
    });
    if let Some(e) = failure {
        return Err(e);
    }
    if sorted {
        sort_records(&mut records);
    }
    Ok(records)
}

pub fn prepare_flip_y(
    doc: &PlumDocument,
    track_index: impl Into<NoteTarget>,
) -> Result<PreparedEdit, EditError> {
    let t = selected_track(doc, track_index)?;
    let b = t
        .notes
        .selection_bounds()
        .ok_or(EditError::NothingSelected)?;
    let sum = u16::from(b.min_pitch) + u16::from(b.max_pitch);
    let mut table = super::identity_pitch_table();
    for (p, slot) in table.iter_mut().enumerate() {
        if (b.min_pitch..=b.max_pitch).contains(&(p as u8)) {
            *slot = (sum - p as u16) as u8;
        }
    }
    Ok(PreparedEdit::RemapSelectedPitch {
        host: t.host(),
        base_revision: t.notes.revision(),
        table,
        notes: t.notes.selected_count(),
    })
}

pub fn prepare_scale_x(
    doc: &PlumDocument,
    track_index: impl Into<NoteTarget>,
    factor: &ExactFactor,
    control: EditControl<'_>,
) -> Result<PreparedEdit, EditError> {
    let t = selected_track(doc, track_index)?;
    let b = t
        .notes
        .selection_bounds()
        .ok_or(EditError::NothingSelected)?;
    if factor.is_one() {
        return Ok(PreparedEdit::Identity);
    }
    let kernel = NoteKernel::scale_x(&b, factor)?;
    let records = if factor.shrinks() {
        // v0.9 §086: notes the shrink puts on the same spot as another
        // (different) note are removed; identical source notes stay.
        let mut f = CollapseFilter::default();
        let mut records =
            map_selected_with(t, b.count, control, false, Some(&mut f), |r| kernel.map(r))?;
        f.apply(&mut records);
        sort_records(&mut records);
        records
    } else {
        map_selected(t, &kernel, b.count, control)?
    };
    Ok(PreparedEdit::ReplaceSelected {
        host: t.host(),
        base_revision: t.notes.revision(),
        records,
        shift: None,
    })
}

/// The pitch table of Scale Y for the selection of `t` (the kernel the preview
/// and the commit share); results must stay within `keys`.
pub fn scale_y_kernel<'a>(
    t: impl Into<HostView<'a>>,
    factor: &ExactFactor,
    keys: KeyRange,
) -> Result<NoteKernel, EditError> {
    let t = t.into();
    let b = t
        .notes
        .selection_bounds()
        .ok_or(EditError::NothingSelected)?;
    let mut present = [false; 256];
    t.notes
        .for_each_selected(|v| present[usize::from(v.attrs.pitch())] = true);
    let mut table = super::identity_pitch_table();
    let mut lowest = i64::MAX;
    let mut highest = i64::MIN;
    for p in 0..=255u8 {
        let slot = &mut table[usize::from(p)];
        if !present[usize::from(p)] {
            *slot = p;
            continue;
        }
        let v = scale_pitch(p, b.min_pitch, b.max_pitch, factor);
        let v = v.unwrap_or(
            if factor.is_negative()
                == (u16::from(p) * 2 < u16::from(b.min_pitch) + u16::from(b.max_pitch))
            {
                i64::MAX
            } else {
                i64::MIN
            },
        );
        lowest = lowest.min(v);
        highest = highest.max(v);
        *slot = u8::try_from(v).unwrap_or(0);
    }
    if lowest < 0 || highest > i64::from(keys.max_pitch()) {
        let clamp = |x: i64| x.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        return Err(EditError::PitchOutOfRange {
            operation: "Scale Y",
            lowest: clamp(lowest),
            highest: clamp(highest),
            top: keys.max_pitch(),
        });
    }
    Ok(NoteKernel::PitchTable(table))
}

/// New pitch for Scale Y, or `None` if the exact result is astronomically large.
/// Returns an `i64` so out-of-range results can be reported precisely.
fn scale_pitch(p: u8, lo: u8, hi: u8, factor: &ExactFactor) -> Option<i64> {
    let p2 = i64::from(lo) + i64::from(hi); // twice the pivot
    let q = p2 / 2;
    let d2 = 2 * i64::from(p) - p2; // twice the signed distance from the pivot
    let sign_f: i64 = if factor.is_negative() { -1 } else { 1 };
    if p2 % 2 == 0 {
        // Integer pivot q: result = q ± round_half_up(|f| * |p - q|).
        let d = d2 / 2;
        if d == 0 {
            return Some(q);
        }
        let mag = i64::try_from(factor.scale_round(d.unsigned_abs())?).ok()?;
        Some(q + d.signum() * sign_f * mag)
    } else {
        // Half-integer pivot q + 0.5; distance |d2| / 2 with |d2| odd.
        if factor.is_zero() {
            return Some(q + 1);
        }
        let h = i64::try_from(factor.scale_half_floor(d2.unsigned_abs())?).ok()?;
        if d2.signum() * sign_f > 0 {
            Some(q + 1 + h)
        } else {
            Some(q - h)
        }
    }
}

/// Scale Y's options (B1-0115 / B1-0116): Duplicate Notes (`fill`: None =
/// Transform existing notes, else Fill added rows or Copy by pitch step) and
/// the Scale Y-specific overlap prevention.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScaleY {
    pub fill: Option<crate::transform::geom::Fill>,
    pub prevent_overlap: bool,
    pub cleanup: crate::transform::geom::ScaleYCleanup,
}

/// "Clean up Scale Y overlaps" is on by default (B1-0116; since beta2 B2-F28
/// a destructive cleanup of the selection's results with
/// [`crate::transform::geom::ScaleYCleanup`], exact duplicates by default).
pub const SCALE_Y_PREVENT_DEFAULT: bool = true;

pub fn prepare_scale_y(
    doc: &PlumDocument,
    track_index: impl Into<NoteTarget>,
    factor: &ExactFactor,
) -> Result<PreparedEdit, EditError> {
    prepare_scale_y_transform(doc, track_index, factor)
}

/// Scale Y with `opts`: Transform existing notes (`opts.fill` None) or
/// Duplicate Notes, with or without overlap prevention. Bounded by `limits`.
pub fn prepare_scale_y_with(
    doc: &PlumDocument,
    track_index: impl Into<NoteTarget>,
    factor: &ExactFactor,
    opts: ScaleY,
    limits: &crate::transform::Limits,
) -> Result<PreparedEdit, EditError> {
    let target = track_index.into();
    if opts.fill.is_none() && !opts.prevent_overlap {
        return prepare_scale_y_transform(doc, target, factor);
    }
    let t = selected_track(doc, target)?;
    let k = scale_y_run_kernel(t, factor, opts, doc.project().keys)?;
    scale_y_run(doc, target, &k, limits)
}

/// Exact Scale Y mapping and global cleanup, shared by Preview and Apply.
#[derive(Clone)]
pub enum ScaleYKernel {
    Map {
        table: Box<[u8; 256]>,
        shrink: bool,
        prevent: bool,
        cleanup: crate::transform::geom::ScaleYCleanup,
    },
    Fill(crate::transform::geom::BoxScale),
}
impl crate::transform::Kernel for ScaleYKernel {
    fn growth(&self) -> crate::transform::Growth {
        match self {
            Self::Fill(k) => k.growth(),
            _ => crate::transform::Growth::Map,
        }
    }
    fn run(
        &self,
        src: &crate::transform::Source,
        out: &mut crate::transform::Output,
    ) -> Result<(), crate::transform::TransformError> {
        let Self::Map {
            table,
            shrink,
            prevent,
            cleanup,
        } = self
        else {
            if let Self::Fill(k) = self {
                return k.run(src, out);
            }
            unreachable!()
        };
        let mut collapse = (!prevent && *shrink).then(CollapseFilter::default);
        for n in &src.notes {
            let mut result = *n;
            result.pitch = table[usize::from(n.pitch)];
            if let Some(c) = &mut collapse {
                c.push(NoteSpot::of(&result), NoteSpot::of(n));
            }
            out.push(result);
            out.check()?;
        }
        if let Some(c) = collapse {
            out.stats.collapsed += c.apply(&mut out.notes) as u64;
        }
        if *prevent && *shrink {
            let r = crate::transform::geom::clean_scale_y(&mut out.notes, *cleanup);
            out.stats.overlaps_removed += r.removed;
            out.stats.overlaps_trimmed += r.trimmed;
        }
        Ok(())
    }
}

pub fn scale_y_run_kernel<'a>(
    t: impl Into<HostView<'a>>,
    factor: &ExactFactor,
    opts: ScaleY,
    keys: KeyRange,
) -> Result<ScaleYKernel, EditError> {
    let t = t.into();
    if let Some(fill) = opts.fill {
        let mut k = scale_y_fill_kernel(t, factor, fill, keys)?;
        k.prevent = opts.prevent_overlap;
        k.cleanup = opts.cleanup;
        Ok(ScaleYKernel::Fill(k))
    } else {
        let NoteKernel::PitchTable(table) = scale_y_kernel(t, factor, keys)? else {
            unreachable!()
        };
        Ok(ScaleYKernel::Map {
            table,
            shrink: factor.shrinks(),
            prevent: opts.prevent_overlap,
            cleanup: opts.cleanup,
        })
    }
}

/// Scale Y without cleanup (the cleanup runs through [`ScaleYKernel`]).
fn prepare_scale_y_transform(
    doc: &PlumDocument,
    track_index: impl Into<NoteTarget>,
    factor: &ExactFactor,
) -> Result<PreparedEdit, EditError> {
    let t = selected_track(doc, track_index)?;
    if factor.is_one() {
        return Ok(PreparedEdit::Identity);
    }
    let NoteKernel::PitchTable(table) = scale_y_kernel(t, factor, doc.project().keys)? else {
        unreachable!("scale_y_kernel returns a pitch table");
    };
    if factor.shrinks() {
        // v0.9 §086: notes the shrink puts on the same spot as another
        // (different) note are removed; identical source notes stay. Only then
        // is the selection rewritten instead of remapped in place.
        let mut records = Vec::new();
        records
            .try_reserve_exact(t.notes.selected_count())
            .map_err(|_| EditError::Invalid("Not enough memory for Scale Y.".into()))?;
        let mut f = CollapseFilter::default();
        t.notes.for_each_selected(|v| {
            let src = v.record();
            // The table stays within the project's keys (checked above).
            let p = table[usize::from(src.attrs.pitch())];
            let r = NoteRecord {
                attrs: src.attrs.with_pitch_wide(p).unwrap_or(src.attrs),
                ..src
            };
            f.push(NoteSpot::of_record(&r), NoteSpot::of_record(&src));
            records.push(r);
        });
        let removed = f.apply(&mut records);
        if removed > 0 {
            sort_records(&mut records);
            return Ok(PreparedEdit::ReplaceSelected {
                host: t.host(),
                base_revision: t.notes.revision(),
                records,
                shift: None,
            });
        }
    }
    Ok(PreparedEdit::RemapSelectedPitch {
        host: t.host(),
        base_revision: t.notes.revision(),
        table,
        notes: t.notes.selected_count(),
    })
}

/// The Duplicate / Fill kernel of Scale Y for the selection of `t` (shared by
/// the Preview and [`prepare_scale_y_fill`]).
pub fn scale_y_fill_kernel<'a>(
    t: impl Into<HostView<'a>>,
    factor: &ExactFactor,
    fill: crate::transform::geom::Fill,
    keys: KeyRange,
) -> Result<crate::transform::geom::BoxScale, EditError> {
    let t = t.into();
    use crate::transform::geom::{BoxScale, GeomBox};
    let b = t
        .notes
        .selection_bounds()
        .ok_or(EditError::NothingSelected)?;
    if factor.is_negative() || factor.is_zero() {
        return Err(EditError::Invalid(
            "Duplicate / Fill needs a positive factor (mirroring is not filled). Turn it off to mirror."
                .into(),
        ));
    }
    let lo = scale_pitch(b.min_pitch, b.min_pitch, b.max_pitch, factor);
    let hi = scale_pitch(b.max_pitch, b.min_pitch, b.max_pitch, factor);
    let (Some(lo), Some(hi)) = (lo, hi) else {
        return Err(EditError::PitchOutOfRange {
            operation: "Scale Y",
            lowest: i32::MIN,
            highest: i32::MAX,
            top: keys.max_pitch(),
        });
    };
    if lo < 0 || hi > i64::from(keys.max_pitch()) {
        let c = |x: i64| x.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        return Err(EditError::PitchOutOfRange {
            operation: "Scale Y",
            lowest: c(lo),
            highest: c(hi),
            top: keys.max_pitch(),
        });
    }
    let from = GeomBox {
        t0: b.min_start,
        t1: b.max_end.max(b.min_start + 1),
        p0: b.min_pitch,
        p1: b.max_pitch,
    };
    Ok(BoxScale {
        from,
        to: GeomBox {
            p0: lo as u8,
            p1: hi as u8,
            ..from
        },
        fill: Some(fill),
        prevent: false,
        cleanup: Default::default(),
    })
}

/// Scale Y with **Duplicate / Fill** (contract §139): the selected rows are
/// stretched (or squeezed) onto the range Scale Y gives the lowest and highest
/// selected pitch, and every target row inside a note's stretched row cell gets a
/// copy (`fill.res`: every n-th row), so vertical stretching keeps the artwork
/// solid. Times are unchanged. Mirroring (negative factors) is not filled.
/// Bounded by `limits` (preflight before anything is allocated).
pub fn prepare_scale_y_fill(
    doc: &PlumDocument,
    track_index: impl Into<NoteTarget>,
    factor: &ExactFactor,
    fill: crate::transform::geom::Fill,
    limits: &crate::transform::Limits,
) -> Result<PreparedEdit, EditError> {
    scale_y_fill_run(doc, track_index.into(), factor, fill, false, limits)
}

fn scale_y_fill_run(
    doc: &PlumDocument,
    target: NoteTarget,
    factor: &ExactFactor,
    fill: crate::transform::geom::Fill,
    prevent: bool,
    limits: &crate::transform::Limits,
) -> Result<PreparedEdit, EditError> {
    let t = selected_track(doc, target)?;
    let mut k = scale_y_fill_kernel(t, factor, fill, doc.project().keys)?;
    k.prevent = prevent;
    scale_y_run(doc, target, &k, limits)
}

fn scale_y_run(
    doc: &PlumDocument,
    target: NoteTarget,
    k: &dyn crate::transform::Kernel,
    limits: &crate::transform::Limits,
) -> Result<PreparedEdit, EditError> {
    let scope = [crate::transform::TrackScope::of(target, false)];
    let to_edit = |e: crate::transform::TransformError| match e {
        crate::transform::TransformError::TooLarge(m)
        | crate::transform::TransformError::Invalid(m) => EditError::Invalid(m),
        crate::transform::TransformError::Cancelled => EditError::Cancelled,
        _ => EditError::Stale,
    };
    let src = crate::transform::Source::collect(doc, &scope, limits.cap()).map_err(to_edit)?;
    // Copies a pitch step apart can reach past the keyboard: dropped there.
    let run = crate::transform::run_kernel(
        k,
        &src,
        crate::transform::Boundary::default().within(doc.project().keys),
        limits,
        None,
    )
    .map_err(to_edit)?;
    let mut prepared = crate::transform::prepare_records(&run).map_err(to_edit)?;
    let (st, records) = prepared.pop().ok_or(EditError::NothingSelected)?;
    Ok(PreparedEdit::ReplaceSelected {
        host: st.host(),
        base_revision: st.revision,
        records,
        shift: None,
    })
}

/// Flip X (prepare + commit).
pub fn flip_x(doc: &mut PlumDocument, track_index: impl Into<NoteTarget>) -> Result<(), EditError> {
    let e = prepare_flip_x(doc, track_index, EditControl::default())?;
    commit(doc, e)
}

/// Flip Y (prepare + commit).
pub fn flip_y(doc: &mut PlumDocument, track_index: impl Into<NoteTarget>) -> Result<(), EditError> {
    let e = prepare_flip_y(doc, track_index)?;
    commit(doc, e)
}

/// Scale X (prepare + commit).
pub fn scale_x(
    doc: &mut PlumDocument,
    track_index: impl Into<NoteTarget>,
    factor: &ExactFactor,
) -> Result<(), EditError> {
    let e = prepare_scale_x(doc, track_index, factor, EditControl::default())?;
    commit(doc, e)
}

/// Scale Y (prepare + commit).
pub fn scale_y(
    doc: &mut PlumDocument,
    track_index: impl Into<NoteTarget>,
    factor: &ExactFactor,
) -> Result<(), EditError> {
    let e = prepare_scale_y(doc, track_index, factor)?;
    commit(doc, e)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(s: &str) -> ExactFactor {
        ExactFactor::parse(s).unwrap()
    }

    #[test]
    fn scale_pitch_rules() {
        // Even pivot 61 (60..62): factor 0.5 -> 60.5 rounds away from pivot to 60.
        assert_eq!(scale_pitch(60, 60, 62, &f("0.5")), Some(60));
        assert_eq!(scale_pitch(62, 60, 62, &f("0.5")), Some(62));
        assert_eq!(scale_pitch(61, 60, 62, &f("7")), Some(61));
        // Odd pivot 60.5 (60..61)
        assert_eq!(scale_pitch(60, 60, 61, &f("3")), Some(59));
        assert_eq!(scale_pitch(61, 60, 61, &f("3")), Some(62));
        assert_eq!(scale_pitch(60, 60, 61, &f("2")), Some(59)); // 59.5 -> away -> 59
        assert_eq!(scale_pitch(61, 60, 61, &f("2")), Some(62)); // 61.5 -> away -> 62
        assert_eq!(scale_pitch(60, 60, 61, &f("-1")), Some(61));
        assert_eq!(scale_pitch(61, 60, 61, &f("-1")), Some(60));
        assert_eq!(scale_pitch(60, 60, 61, &f("0")), Some(61)); // tie at pivot rounds up
        // -1 always equals Flip Y.
        for lo in 0..=20u8 {
            for hi in lo..=40u8 {
                for p in lo..=hi {
                    assert_eq!(
                        scale_pitch(p, lo, hi, &f("-1")),
                        Some(i64::from(lo) + i64::from(hi) - i64::from(p))
                    );
                }
            }
        }
        assert_eq!(scale_pitch(0, 0, 127, &f("1e40")), None);
    }
}
