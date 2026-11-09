//! Native import comparison, prepared on the Lua worker after successful staging.
//! No notes, payloads or document roots are copied into the bounded review DTO.
use super::bridge::{NativeBudget, Reservation};
use super::{Category, stop};
use plumspirit_engine::model::events::{ArenaRange, EventStore};
use plumspirit_engine::model::{PlumDocument, TrackId};
use std::collections::HashMap;

pub const MAX_SAMPLES: usize = 32;
const REPORT_BYTES: usize = 64 * 1024;
const LABEL_CHARS: usize = 80;
type Check<'a> = &'a mut dyn FnMut() -> mlua::Result<()>;
fn budget_error() -> mlua::Error {
    stop(
        Category::Budget,
        "Import review exceeds its native memory budget.",
    )
}
fn memory_error() -> mlua::Error {
    stop(
        Category::Memory,
        "Cannot allocate bounded import review data.",
    )
}
fn add(total: &mut u64, n: usize) -> mlua::Result<()> {
    *total = total
        .checked_add(n as u64)
        .ok_or_else(|| stop(Category::Runtime, "Import review count overflow."))?;
    Ok(())
}
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Counts {
    pub tracks: usize,
    pub subtracks: usize,
    pub track_notes: u64,
    pub track_events: u64,
    pub contents: usize,
    pub clip_notes: u64,
    pub clip_events: u64,
    pub placements: usize,
    pub tempos: usize,
    pub signatures: usize,
    pub markers: usize,
    pub master: usize,
    pub groups: usize,
    pub attributes: usize,
}
impl Counts {
    fn capture(doc: &PlumDocument, check: Check<'_>) -> mlua::Result<Self> {
        let mut c = Self {
            tracks: doc.track_count(),
            contents: doc.clips().contents().len(),
            placements: doc.clips().explicit_placements().len(),
            tempos: doc.conductor().tempo_points().len(),
            signatures: doc.conductor().time_signatures().len(),
            markers: doc.conductor().markers().len(),
            master: doc.conductor().master_volume().len(),
            groups: doc.project().groups.len(),
            attributes: doc.project().metadata.len(),
            ..Self::default()
        };
        for (i, t) in doc.tracks().iter().enumerate() {
            if i % 256 == 0 {
                check()?;
            }
            c.subtracks += usize::from(t.parent().is_some());
            add(&mut c.track_notes, t.notes.len())?;
            add(&mut c.track_events, t.events.len())?;
        }
        for (i, t) in doc.clips().contents().iter().enumerate() {
            if i % 256 == 0 {
                check()?;
            }
            add(&mut c.clip_notes, t.notes.len())?;
            add(&mut c.clip_events, t.events.len())?;
        }
        Ok(c)
    }
}
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Changes {
    pub added: usize,
    pub removed: usize,
    pub notes: usize,
    pub events: usize,
    pub settings: usize,
    pub order: bool,
    pub clips: bool,
    pub tempo: bool,
    pub meter: bool,
    pub markers: bool,
    pub master: bool,
    pub groups: bool,
    pub attributes: bool,
    pub project: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sample {
    pub track: TrackId,
    pub label: String,
    pub kind: &'static str,
    pub notes: bool,
    pub events: bool,
    pub settings: bool,
}
pub struct Report {
    pub before: Counts,
    pub after: Counts,
    pub changes: Changes,
    pub samples: Vec<Sample>,
    pub sampled_tracks: usize,
    _reservation: Reservation,
}
fn equal<T: PartialEq>(a: &[T], b: &[T], check: Check<'_>) -> mlua::Result<bool> {
    check()?;
    if a.len() != b.len() {
        return Ok(false);
    }
    if std::ptr::eq(a.as_ptr(), b.as_ptr()) {
        return Ok(true);
    }
    for (a, b) in a.chunks(256).zip(b.chunks(256)) {
        check()?;
        if a != b {
            return Ok(false);
        }
    }
    Ok(true)
}
fn event_equal(a: &EventStore, b: &EventStore, check: Check<'_>) -> mlua::Result<bool> {
    if !equal(a.events(), b.events(), check)? || a.arena_len() != b.arena_len() {
        return Ok(false);
    }
    let size = u32::try_from(a.arena_len())
        .map_err(|_| stop(Category::Runtime, "Import event arena is out of range."))?;
    let a = a.payload(ArenaRange {
        offset: 0,
        len: size,
    });
    let b = b.payload(ArenaRange {
        offset: 0,
        len: size,
    });
    if std::ptr::eq(a.as_ptr(), b.as_ptr()) {
        return Ok(true);
    }
    for (a, b) in a.chunks(64 * 1024).zip(b.chunks(64 * 1024)) {
        check()?;
        if a != b {
            return Ok(false);
        }
    }
    Ok(true)
}
fn text_equal(a: Option<&str>, b: Option<&str>, check: Check<'_>) -> mlua::Result<bool> {
    match (a, b) {
        (Some(a), Some(b)) => equal(a.as_bytes(), b.as_bytes(), check),
        (None, None) => Ok(true),
        _ => Ok(false),
    }
}
fn label(name: Option<&str>) -> mlua::Result<String> {
    let mut out = String::new();
    out.try_reserve(LABEL_CHARS * 4 + 4)
        .map_err(|_| memory_error())?;
    let mut chars = name.unwrap_or("Track").chars();
    for c in chars.by_ref().take(LABEL_CHARS) {
        out.push(if c.is_control() { ' ' } else { c });
    }
    if chars.next().is_some() {
        out.push('…');
    }
    Ok(out)
}
impl Report {
    pub fn prepare(
        source: &PlumDocument,
        result: &PlumDocument,
        budget: &NativeBudget,
        mut check: impl FnMut() -> mlua::Result<()>,
    ) -> mlua::Result<Self> {
        check()?;
        let workspace = source
            .track_count()
            .checked_mul(64)
            .and_then(|n| n.checked_add(4096))
            .ok_or_else(budget_error)?;
        let _workspace = budget.reserve(workspace).map_err(|e| e.lua())?;
        let reservation = budget.reserve(REPORT_BYTES).map_err(|e| e.lua())?;
        let mut index = HashMap::new();
        index
            .try_reserve(source.track_count())
            .map_err(|_| memory_error())?;
        for (i, t) in source.tracks().iter().enumerate() {
            if i % 256 == 0 {
                check()?;
            }
            index.insert(t.id(), i);
        }
        let mut report = Self {
            before: Counts::capture(source, &mut check)?,
            after: Counts::capture(result, &mut check)?,
            changes: Changes::default(),
            samples: Vec::new(),
            sampled_tracks: 0,
            _reservation: reservation,
        };
        report
            .samples
            .try_reserve_exact(MAX_SAMPLES)
            .map_err(|_| memory_error())?;
        for (i, t) in result.tracks().iter().enumerate() {
            if i % 256 == 0 {
                check()?;
            }
            let old = index.remove(&t.id());
            let (kind, notes, events, settings) = if let Some(old) = old {
                let a = source.track(old).expect("indexed source track");
                report.changes.order |= old != i || a.parent() != t.parent();
                let notes = a.notes.content_revision() != t.notes.content_revision();
                let events = !event_equal(&a.events, &t.events, &mut check)?;
                let settings = a.props() != t.props()
                    || !text_equal(a.name(), t.name(), &mut check)?
                    || a.end_of_track_tick != t.end_of_track_tick
                    || a.mute != t.mute
                    || a.solo != t.solo
                    || !equal(
                        a.subchannels().entries(),
                        t.subchannels().entries(),
                        &mut check,
                    )?;
                report.changes.notes += usize::from(notes);
                report.changes.events += usize::from(events);
                report.changes.settings += usize::from(settings);
                ("changed", notes, events, settings)
            } else {
                report.changes.added += 1;
                ("added", false, false, false)
            };
            if kind == "added" || notes || events || settings {
                report.sampled_tracks += 1;
                if report.samples.len() < MAX_SAMPLES {
                    report.samples.push(Sample {
                        track: t.id(),
                        label: label(t.name())?,
                        kind,
                        notes,
                        events,
                        settings,
                    });
                }
            }
        }
        report.changes.removed = index.len();
        for (i, t) in source.tracks().iter().enumerate() {
            if i % 256 == 0 {
                check()?;
            }
            if index.contains_key(&t.id()) {
                report.sampled_tracks += 1;
                if report.samples.len() < MAX_SAMPLES {
                    report.samples.push(Sample {
                        track: t.id(),
                        label: label(t.name())?,
                        kind: "removed",
                        notes: false,
                        events: false,
                        settings: false,
                    });
                }
            }
        }
        let a = source.clips();
        let b = result.clips();
        report.changes.clips = a.contents().len() != b.contents().len()
            || !equal(a.explicit_placements(), b.explicit_placements(), &mut check)?
            || !equal(a.removed_primaries(), b.removed_primaries(), &mut check)?;
        for (i, t) in b.contents().iter().enumerate() {
            if i % 256 == 0 {
                check()?;
            }
            let same = if let Some(a) = a.content(t.id) {
                a.notes.content_revision() == t.notes.content_revision()
                    && a.name == t.name
                    && a.color == t.color
                    && event_equal(&a.events, &t.events, &mut check)?
            } else {
                false
            };
            report.changes.clips |= !same;
        }
        let a = source.conductor();
        let b = result.conductor();
        report.changes.tempo = !equal(a.tempo_points(), b.tempo_points(), &mut check)?;
        report.changes.meter = !equal(a.time_signatures(), b.time_signatures(), &mut check)?;
        report.changes.markers = !equal(a.markers(), b.markers(), &mut check)?;
        report.changes.master = !equal(a.master_volume(), b.master_volume(), &mut check)?;
        report.changes.groups = !equal(
            source.project().groups.entries(),
            result.project().groups.entries(),
            &mut check,
        )?;
        check()?;
        report.changes.attributes = source.project().metadata != result.project().metadata;
        check()?;
        let a = source.project();
        let b = result.project();
        report.changes.project = source.source_format() != result.source_format()
            || source.division() != result.division()
            || a.name != b.name
            || a.keys != b.keys
            || a.soundfont != b.soundfont
            || a.onion != b.onion
            || a.looping != b.looping
            || a.graphs != b.graphs
            || a.reference != b.reference
            || a.folders != b.folders;
        check()?;
        Ok(report)
    }
}

#[cfg(test)]
#[path = "format_review_tests.rs"]
mod tests;
