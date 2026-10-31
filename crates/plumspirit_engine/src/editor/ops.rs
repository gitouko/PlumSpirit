//! Direct note editing: draw, delete, move, resize.

use super::target::{HostView, NoteTarget, note_view, notes_mut};
use super::{EditControl, EditError, PreparedEdit, commit, try_records};
use crate::model::document::PlumDocument;
use crate::model::note::NoteRecord;
use crate::model::note_store::{NoteLoc, SelectionBounds};
use crate::model::project::KeyRange;

fn track_ref(doc: &PlumDocument, target: impl Into<NoteTarget>) -> Result<HostView<'_>, EditError> {
    note_view(doc, target)
}

/// Inserts a note drawn by the user. The new note becomes the only selected note.
pub fn insert_note(
    doc: &mut PlumDocument,
    track_index: impl Into<NoteTarget>,
    rec: NoteRecord,
) -> Result<NoteLoc, EditError> {
    rec.end_tick().ok_or(EditError::TickOverflow {
        operation: "Drawing this note",
    })?;
    let target = track_index.into();
    track_ref(doc, target)?;
    // v0.9: a note is drawn only on a key of the project (127, or 255 in
    // 256-Key Mode) — the invariant turning 256-Key Mode off relies on.
    let keys = doc.project().keys;
    let pitch = rec.attrs.pitch();
    if !keys.contains(pitch) {
        return Err(EditError::PitchOutOfRange {
            operation: "Drawing this note",
            lowest: i32::from(pitch),
            highest: i32::from(pitch),
            top: keys.max_pitch(),
        });
    }
    let notes = notes_mut(doc, target)?;
    notes.clear_selection();
    Ok(notes.insert(rec, true)?)
}

/// Deletes the selected notes of a track (batch compaction). Returns how many were
/// deleted; deleting nothing is not an error.
pub fn delete_selected(
    doc: &mut PlumDocument,
    track_index: impl Into<NoteTarget>,
) -> Result<usize, EditError> {
    Ok(notes_mut(doc, track_index)?.remove_selected())
}

/// Clamps a requested drag (tick delta, pitch delta) so the selection stays within
/// tick 0.. and the project's key range (0..=127, or 0..=255 in 256-Key Mode).
/// Used by the UI to show the reachable position while dragging instead of
/// wrapping or failing.
pub fn clamp_move_delta(
    bounds: &SelectionBounds,
    dt: i128,
    dp: i32,
    keys: KeyRange,
) -> (i128, i32) {
    let min_dt = -(bounds.min_start as i128);
    let max_dt = i128::from(u64::MAX) - bounds.max_end as i128;
    let dt = dt.clamp(min_dt, max_dt);
    let min_dp = -i32::from(bounds.min_pitch);
    let max_dp = i32::from(keys.max_pitch()) - i32::from(bounds.max_pitch);
    (dt, dp.clamp(min_dp, max_dp))
}

/// Prepares moving the selected notes by `dt` ticks and `dp` semitones, keeping
/// their relative spacing, channels, velocities and durations.
pub fn prepare_move(
    doc: &PlumDocument,
    track_index: impl Into<NoteTarget>,
    dt: i128,
    dp: i32,
    control: EditControl<'_>,
) -> Result<PreparedEdit, EditError> {
    let t = track_ref(doc, track_index)?;
    let b = t
        .notes
        .selection_bounds()
        .ok_or(EditError::NothingSelected)?;
    let op = "Moving the selection";
    let max_pitch = i32::from(doc.project().keys.max_pitch());
    let lo = i32::from(b.min_pitch) + dp;
    let hi = i32::from(b.max_pitch) + dp;
    if lo < 0 || hi > max_pitch {
        return Err(EditError::PitchOutOfRange {
            operation: op,
            lowest: lo,
            highest: hi,
            top: max_pitch as u8,
        });
    }
    let earliest = b.min_start as i128 + dt;
    if earliest < 0 {
        return Err(EditError::NegativeTick {
            operation: op,
            earliest,
        });
    }
    if b.max_end as i128 + dt > i128::from(u64::MAX) {
        return Err(EditError::TickOverflow { operation: op });
    }
    if dt == 0 && dp == 0 {
        return Ok(PreparedEdit::Identity);
    }
    if dt == 0 {
        let mut table = super::identity_pitch_table();
        for (p, slot) in table.iter_mut().enumerate() {
            let q = p as i32 + dp;
            if (0..=max_pitch).contains(&q) {
                *slot = q as u8;
            }
        }
        return Ok(PreparedEdit::RemapSelectedPitch {
            host: t.host(),
            base_revision: t.notes.revision(),
            table,
            notes: t.notes.selected_count(),
        });
    }
    let mut records = try_records(b.count)?;
    let mut done = 0u64;
    let mut result: Result<(), EditError> = Ok(());
    t.notes.for_each_selected(|v| {
        if result.is_err() {
            return;
        }
        // Validated above: the shifted range stays within 0..=u64::MAX and
        // the key range.
        let start = (v.start_tick as i128 + dt) as u64;
        let pitch = (i32::from(v.attrs.pitch()) + dp) as u8;
        match v.attrs.with_pitch_wide(pitch) {
            Ok(attrs) => records.push(NoteRecord {
                start_tick: start,
                duration_ticks: v.duration_ticks,
                attrs,
                sub: v.sub,
                group: v.group,
            }),
            Err(_) => {
                result = Err(EditError::PitchOutOfRange {
                    operation: op,
                    lowest: lo,
                    highest: hi,
                    top: max_pitch as u8,
                })
            }
        }
        done += 1;
        if done.is_multiple_of(65_536)
            && let Err(e) = control.tick(done)
        {
            result = Err(e);
        }
    });
    result?;
    // A uniform shift preserves order, so `records` is already sorted.
    Ok(PreparedEdit::ReplaceSelected {
        host: t.host(),
        base_revision: t.notes.revision(),
        records,
        shift: Some(super::Shift {
            ticks: dt,
            pitch: dp,
        }),
    })
}

/// Moves the selection (prepare + commit).
pub fn move_selected(
    doc: &mut PlumDocument,
    track_index: impl Into<NoteTarget>,
    dt: i128,
    dp: i32,
) -> Result<(), EditError> {
    let edit = prepare_move(doc, track_index, dt, dp, EditControl::default())?;
    commit(doc, edit)
}

/// Smallest duration delta that keeps every selected note at least one tick long.
pub fn min_resize_delta(bounds: &SelectionBounds) -> i128 {
    1 - i128::from(bounds.min_duration)
}

/// Changes the duration of every selected note by `delta` ticks (their starts stay
/// put). Refuses results shorter than one tick.
pub fn resize_selected(
    doc: &mut PlumDocument,
    track_index: impl Into<NoteTarget>,
    delta: i64,
) -> Result<(), EditError> {
    let target = track_index.into();
    let t = track_ref(doc, target)?;
    let b = t
        .notes
        .selection_bounds()
        .ok_or(EditError::NothingSelected)?;
    if delta == 0 {
        return Ok(());
    }
    if i128::from(delta) < min_resize_delta(&b) {
        return Err(EditError::DurationTooShort);
    }
    u64::try_from(b.max_end as i128 + i128::from(delta)).map_err(|_| EditError::TickOverflow {
        operation: "Resizing",
    })?;
    notes_mut(doc, target)?.add_selected_duration(delta)?;
    Ok(())
}
