//! Reference notes chosen by a track number or by a subchannel (V09-S41):
//! the catalog's "Reference (a track / a subchannel)" + "Reference number"
//! controls — notes of the document a tool reads beside its selection,
//! through its `prepare` hook into [`super::super::Source::aux`]. One
//! reading of those two controls (the S34 – S40 tools that already read
//! references keep their own; new functions use this one).

use super::super::{Source, TNote, TransformError};
use crate::model::PlumDocument;

/// The notes starting in `w0 ..= w1` of track `num` (its number in the
/// track list; 0 = the active track) or, with `by_sub`, of the active
/// track's subchannel at position `num` (0 = its first): time-major, at most
/// `limit`. `what` names the function in the refusals (full sentences).
#[allow(clippy::too_many_arguments)]
pub fn reference_notes(
    doc: &PlumDocument,
    src: &Source,
    by_sub: bool,
    num: i64,
    w0: u64,
    w1: u64,
    limit: usize,
    what: &str,
) -> Result<Vec<TNote>, TransformError> {
    let Some(active) = src.tracks.first() else {
        return Ok(Vec::new());
    };
    let (target, sub) = if by_sub {
        let id = usize::try_from(num)
            .ok()
            .and_then(|i| active.subs.get(i))
            .map(|s| s.0)
            .ok_or_else(|| {
                TransformError::Invalid(format!(
                    "{what}: the active track has no subchannel at position {num} (0 = its first)."
                ))
            })?;
        (active.target(), Some(id))
    } else if num == 0 {
        (active.target(), None)
    } else {
        let i = usize::try_from(num - 1)
            .ok()
            .filter(|&i| doc.track(i).is_some())
            .ok_or_else(|| {
                TransformError::Invalid(format!(
                    "{what}: track {num} does not exist. Choose the track by its number in the track list (0 = the active track)."
                ))
            })?;
        let target = if i == active.index {
            active.target()
        } else {
            crate::editor::NoteTarget::Track(i)
        };
        (target, None)
    };
    let mut v = Vec::new();
    if let Ok(t) = crate::editor::note_view(doc, target) {
        t.notes.query(w0, w1, |_, n| {
            if n.start_tick >= w0
                && n.start_tick <= w1
                && sub.is_none_or(|s| n.sub.0 == s)
                && v.len() < limit
            {
                v.push(TNote::from_record(&n.record(), u16::MAX));
            }
        });
    }
    super::super::source::sort_time_major(&mut v);
    Ok(v)
}
