//! The shared Remove Note Overlaps service (v0.9 contract §098 function 3,
//! §231): one pass over a run's sorted result, used by every eligible tool
//! instead of each reinventing its own sweep.
//!
//! Modes: exact duplicates, notes with the same pitch and start, and
//! same-pitch notes whose times overlap (the earlier one trimmed to the later
//! one's start, or the later one removed). By default only notes the tool
//! created or changed are touched — a result note identical to a source note
//! is the user's own (an intentional stack stays a stack) — and notes on
//! different subchannels or MIDI channels are different layers unless
//! "across" is chosen. The result is never longer than its input and stays in
//! canonical order.

use super::TNote;
use super::output::sort_notes;
use std::cmp::Ordering;
use std::collections::HashMap;

/// What counts as an overlap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Same start, length, pitch (and layer): all but the loudest removed.
    ExactDuplicates,
    /// Same start and pitch (and layer), any length: the longest (then the
    /// loudest) stays.
    SamePitchStart,
    /// Same pitch (and layer), overlapping in time: the earlier note ends
    /// where the later one starts (same starts: as [`Mode::SamePitchStart`]).
    TrimOverlaps,
    /// Same pitch (and layer), overlapping in time: the later note is removed.
    RemoveOverlaps,
}

impl Mode {
    pub const ALL: [Mode; 4] = [
        Mode::ExactDuplicates,
        Mode::SamePitchStart,
        Mode::TrimOverlaps,
        Mode::RemoveOverlaps,
    ];

    /// Menu names, in [`Mode::ALL`] order.
    pub const NAMES: &'static [&'static str] = &[
        "Exact duplicates",
        "Same pitch and start",
        "Same-pitch overlaps: trim the earlier note",
        "Same-pitch overlaps: remove the later note",
    ];

    pub fn from_index(i: usize) -> Mode {
        Mode::ALL.get(i).copied().unwrap_or(Mode::ExactDuplicates)
    }
}

/// How the service runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub mode: Mode,
    /// Only notes the tool created or changed may be removed or trimmed
    /// (a result note identical to a source note is protected).
    pub generated_only: bool,
    /// Compare notes across subchannels and MIDI channels (otherwise each
    /// (subchannel, channel) is its own layer).
    pub across: bool,
}

/// What the service did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub removed: u64,
    pub trimmed: u64,
}

/// Marks the result notes that are not identical to a source note
/// (multiset: one source note protects one result note). `notes` must be in
/// canonical order ([`sort_notes`]); `source` may be in any order.
pub fn generated(notes: &[TNote], source: &[TNote]) -> Vec<bool> {
    let mut src = source.to_vec();
    sort_notes(&mut src);
    let mut out = vec![true; notes.len()];
    let (mut i, mut j) = (0, 0);
    while i < notes.len() && j < src.len() {
        match canonical(&notes[i], &src[j]) {
            Ordering::Less => i += 1,
            Ordering::Greater => j += 1,
            Ordering::Equal => {
                out[i] = false;
                i += 1;
                j += 1;
            }
        }
    }
    out
}

/// The canonical order of [`sort_notes`] as a comparison.
fn canonical(a: &TNote, b: &TNote) -> Ordering {
    (
        a.track, a.start, a.pitch, a.dur, a.sub, a.vel, a.ch, a.rel, a.group, a.flags,
    )
        .cmp(&(
            b.track, b.start, b.pitch, b.dur, b.sub, b.vel, b.ch, b.rel, b.group, b.flags,
        ))
}

/// The layer two notes must share to overlap.
#[inline]
fn layer(n: &TNote, across: bool) -> (u32, u8) {
    if across { (0, 0) } else { (n.sub, n.ch) }
}

/// Removes / trims overlaps in `notes` (canonical order, as a run leaves
/// them; kept in canonical order). `source`: the run's source notes, for
/// [`Policy::generated_only`]; `protected`: notes never touched whatever the
/// policy (locked group notes).
pub fn apply(notes: &mut Vec<TNote>, source: &[TNote], protected: &[TNote], p: Policy) -> Report {
    apply_inner(notes, source, protected, &[], p, false)
}

/// [`apply`] with `context` (canonical order): notes that stay in the
/// document beside the result (B1-0117). They count as overlap partners but
/// are never removed, trimmed or returned — the prevention is
/// non-destructive. A result overlapped by a later note it may not touch is
/// removed too (the later note stays), so no same-key overlap is left with
/// a context note either way.
pub fn apply_with_context(
    notes: &mut Vec<TNote>,
    source: &[TNote],
    protected: &[TNote],
    context: &[TNote],
    p: Policy,
) -> Report {
    apply_inner(notes, source, protected, context, p, true)
}

fn apply_inner(
    notes: &mut Vec<TNote>,
    source: &[TNote],
    protected: &[TNote],
    context: &[TNote],
    p: Policy,
    general: bool,
) -> Report {
    let mut report = Report::default();
    if notes.is_empty() || notes.len() + context.len() < 2 {
        return report;
    }
    let mut alterable = if p.generated_only {
        generated(notes, source)
    } else {
        vec![true; notes.len()]
    };
    if !protected.is_empty() {
        for (a, g) in alterable.iter_mut().zip(generated(notes, protected)) {
            *a &= g;
        }
    }
    if !context.is_empty() {
        // Merge the context in (canonical order); its notes are never
        // alterable and never returned.
        let mut all = Vec::with_capacity(notes.len() + context.len());
        let mut alt = Vec::with_capacity(all.capacity());
        let mut is_ctx = Vec::with_capacity(all.capacity());
        let (mut i, mut j) = (0, 0);
        while i < notes.len() || j < context.len() {
            let take_note = j >= context.len()
                || (i < notes.len() && canonical(&notes[i], &context[j]) != Ordering::Greater);
            if take_note {
                all.push(notes[i]);
                alt.push(alterable[i]);
                is_ctx.push(false);
                i += 1;
            } else {
                all.push(context[j]);
                alt.push(false);
                is_ctx.push(true);
                j += 1;
            }
        }
        let mut keep = vec![true; all.len()];
        settle_all(&all, p, &alt, &mut keep, &mut report);
        if matches!(p.mode, Mode::TrimOverlaps | Mode::RemoveOverlaps) {
            sweep_overlaps(&mut all, p, &alt, &mut keep, &mut report, general);
        }
        notes.clear();
        notes.extend(
            all.iter()
                .zip(keep.iter().zip(&is_ctx))
                .filter(|(_, (k, c))| **k && !**c)
                .map(|(n, _)| *n),
        );
        if report.trimmed > 0 {
            sort_notes(notes);
        }
        return report;
    }
    let keep = settle_and_sweep(notes, &alterable, p, &mut report, general);
    if report.removed > 0 {
        let mut k = keep.iter();
        notes.retain(|_| *k.next().unwrap());
    }
    if report.trimmed > 0 {
        sort_notes(notes);
    }
    report
}

/// Steps 1 and 2 of [`apply`]: which notes stay (`notes` canonical; trims
/// are written into their lengths in place, the order is left as it is).
fn settle_and_sweep(
    notes: &mut [TNote],
    alterable: &[bool],
    p: Policy,
    report: &mut Report,
    general: bool,
) -> Vec<bool> {
    let mut keep = vec![true; notes.len()];
    settle_all(notes, p, alterable, &mut keep, report);
    // 2. Overlaps in time between different starts.
    if matches!(p.mode, Mode::TrimOverlaps | Mode::RemoveOverlaps) {
        sweep_overlaps(notes, p, alterable, &mut keep, report, general);
    }
    keep
}

/// v1.0-beta2 B2-F01: [`apply`]'s decisions without removing anything —
/// for each note of `notes` (canonical order) whether it stays; trimmed
/// lengths are written in place. The platform runs Remove Note Overlaps
/// this way in the middle of a run when the user moves it before an offset
/// driver (`Output::overlaps_since`).
pub fn decide(
    notes: &mut [TNote],
    source: &[TNote],
    protected: &[TNote],
    p: Policy,
) -> (Vec<bool>, Report) {
    let mut report = Report::default();
    if notes.len() < 2 {
        return (vec![true; notes.len()], report);
    }
    let mut alterable = if p.generated_only {
        generated(notes, source)
    } else {
        vec![true; notes.len()]
    };
    if !protected.is_empty() {
        for (a, g) in alterable.iter_mut().zip(generated(notes, protected)) {
            *a &= g;
        }
    }
    let keep = settle_and_sweep(notes, &alterable, p, &mut report, false);
    (keep, report)
}

/// The canonical order of [`apply`] (track, start, pitch, then the rest).
pub fn canonical_order(a: &TNote, b: &TNote) -> Ordering {
    canonical(a, b)
}

/// Step 1: groups of equal (track, start, pitch) are adjacent in canonical
/// order, so duplicates and same-start notes are settled inside them.
fn settle_all(
    notes: &[TNote],
    p: Policy,
    alterable: &[bool],
    keep: &mut [bool],
    report: &mut Report,
) {
    let mut a = 0;
    while a < notes.len() {
        let mut b = a + 1;
        while b < notes.len()
            && (notes[b].track, notes[b].start, notes[b].pitch)
                == (notes[a].track, notes[a].start, notes[a].pitch)
        {
            b += 1;
        }
        if b - a >= 2 {
            settle_group(notes, a..b, p, alterable, keep, report);
        }
        a = b;
    }
}

/// Within one (track, start, pitch) group: keeps one note per layer (and
/// per length for exact duplicates), removing only alterable notes.
fn settle_group(
    notes: &[TNote],
    g: std::ops::Range<usize>,
    p: Policy,
    alterable: &[bool],
    keep: &mut [bool],
    report: &mut Report,
) {
    let exact = p.mode == Mode::ExactDuplicates;
    // Which note of a set stays: the longest (unless exact), then the
    // loudest, then the first; a protected note always stays.
    let better = |i: usize, j: usize| -> bool {
        let (x, y) = (&notes[i], &notes[j]);
        (!alterable[i], x.dur, x.vel, std::cmp::Reverse(i))
            > (!alterable[j], y.dur, y.vel, std::cmp::Reverse(j))
    };
    for i in g.clone() {
        if !keep[i] {
            continue;
        }
        for j in (i + 1)..g.end {
            if !keep[j] || layer(&notes[i], p.across) != layer(&notes[j], p.across) {
                continue;
            }
            if exact && notes[i].dur != notes[j].dur {
                continue;
            }
            let lose = if better(j, i) { i } else { j };
            if !alterable[lose] {
                continue; // both protected: an intentional stack stays
            }
            keep[lose] = false;
            report.removed += 1;
            if lose == i {
                break;
            }
        }
    }
}

/// Same-pitch overlaps across different starts, in time order per
/// (track, pitch, layer). `general` (B1-0117): when the later note may not
/// be touched but the earlier one may, Remove Overlaps removes the earlier.
fn sweep_overlaps(
    notes: &mut [TNote],
    p: Policy,
    alterable: &[bool],
    keep: &mut [bool],
    report: &mut Report,
    general: bool,
) {
    // The last kept note of each (track, pitch, layer) seen so far.
    let mut last: HashMap<(u16, u8, u32, u8), usize> = HashMap::new();
    for i in 0..notes.len() {
        if !keep[i] {
            continue;
        }
        let n = notes[i];
        let (sub, ch) = layer(&n, p.across);
        let key = (n.track, n.pitch, sub, ch);
        if let Some(&j) = last.get(&key) {
            let prev = notes[j];
            if prev.start < n.start && prev.start.saturating_add(prev.dur) > n.start {
                match p.mode {
                    Mode::TrimOverlaps if alterable[j] => {
                        notes[j].dur = n.start - prev.start;
                        report.trimmed += 1;
                    }
                    Mode::RemoveOverlaps | Mode::TrimOverlaps if alterable[i] => {
                        // Trim impossible (the earlier note is protected):
                        // the later, generated note goes.
                        keep[i] = false;
                        report.removed += 1;
                        continue;
                    }
                    Mode::RemoveOverlaps if general && alterable[j] => {
                        // The later note stays (protected or kept as it
                        // is): the earlier, generated one goes.
                        keep[j] = false;
                        report.removed += 1;
                    }
                    _ => {}
                }
            }
            // The note that reaches furthest is the one later notes meet.
            let reach = |k: usize| notes[k].start.saturating_add(notes[k].dur);
            if reach(i) < reach(j) && keep[j] {
                continue;
            }
        }
        last.insert(key, i);
    }
}

// ============================================ B1-0117 General Note Overlap Prevention

/// B1-0117: the General Note Overlap Prevention — this service, optional
/// (off by default), run in the supported creation / manipulation contexts
/// (tool runs, scale gestures, Scale X / Y, Paste, Draw) with the notes that
/// stay in the track as overlap partners ([`apply_with_context`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct General {
    pub policy: Policy,
    /// Results are compared with the notes that stay in the track (never
    /// changed); off: only with each other.
    pub existing: bool,
}

impl Default for General {
    /// Same-pitch overlaps, the later note goes; only notes the operation
    /// created or changed; layers (subchannel, channel) apart; against the
    /// notes already there.
    fn default() -> Self {
        General {
            policy: Policy {
                mode: Mode::RemoveOverlaps,
                generated_only: true,
                across: false,
            },
            existing: true,
        }
    }
}

/// The notes of `store` that stay beside `results` (canonical order, all of
/// one track slot): all of them, or the unselected ones when the results
/// replace the selection; only those that can meet a result — inside the
/// results' time span, on their keys — minus `erase` (notes the operation
/// deletes, canonical order). As notes of slot `slot`, canonical order.
pub fn context_of(
    store: &crate::model::NoteStore,
    slot: u16,
    results: &[TNote],
    unselected_only: bool,
    erase: &[TNote],
) -> Vec<TNote> {
    let Some(t0) = results.iter().map(|n| n.start).min() else {
        return Vec::new();
    };
    let t1 = results
        .iter()
        .map(|n| n.start.saturating_add(n.dur))
        .max()
        .unwrap_or(t0);
    let mut keys = [false; 256];
    for n in results {
        keys[usize::from(n.pitch)] = true;
    }
    let mut out = Vec::new();
    store.query(t0, t1, |_, v| {
        if (unselected_only && v.selected) || !keys[usize::from(v.attrs.pitch())] {
            return;
        }
        out.push(TNote::from_record(&v.record(), slot));
    });
    sort_notes(&mut out);
    if !erase.is_empty() {
        // Multiset difference (both canonical).
        let mut kept = Vec::with_capacity(out.len());
        let mut j = 0;
        for n in out {
            while j < erase.len() && canonical(&erase[j], &n) == Ordering::Less {
                j += 1;
            }
            if j < erase.len() && canonical(&erase[j], &n) == Ordering::Equal {
                j += 1;
                continue;
            }
            kept.push(n);
        }
        out = kept;
    }
    out
}

/// Runs the General Note Overlap Prevention on a tool run (preview and
/// Apply alike): per track slot, the results against each other and — with
/// [`General::existing`] — the notes that stay in the track. `src` is the
/// run's source (the notes it replaces: [`Policy::generated_only`]; locked
/// group notes stay). Adds to the run's overlap statistics.
pub fn prevent_in_run(
    doc: &crate::model::PlumDocument,
    src: &super::Source,
    run: &mut super::TransformRun,
    g: General,
) -> Report {
    let mut total = Report::default();
    if run.notes.is_empty() {
        return total;
    }
    // The source, locked and erased notes by slot (one pass each).
    let by_slot = |v: &[TNote]| -> Vec<Vec<TNote>> {
        let mut out: Vec<Vec<TNote>> = vec![Vec::new(); run.tracks.len().max(1)];
        for n in v {
            if let Some(list) = out.get_mut(usize::from(n.track)) {
                list.push(*n);
            }
        }
        out
    };
    let sources = if g.policy.generated_only {
        by_slot(&src.notes)
    } else {
        Vec::new()
    };
    let locked = by_slot(&src.locked);
    let erased = by_slot(&run.erase);
    let none: Vec<TNote> = Vec::new();
    let mut out = Vec::with_capacity(run.notes.len());
    let mut a = 0;
    while a < run.notes.len() {
        let slot = run.notes[a].track;
        let b = a + run.notes[a..].partition_point(|n| n.track == slot);
        let mut notes = run.notes[a..b].to_vec();
        let of = |v: &'_ Vec<Vec<TNote>>| v.get(usize::from(slot)).unwrap_or(&none).clone();
        let source = of(&sources);
        let locked = of(&locked);
        let context = match run.tracks.get(usize::from(slot)) {
            Some(t) if g.existing && (t.insert_only || !t.all) => {
                let mut erase = of(&erased);
                sort_notes(&mut erase);
                doc.notes_of(t.host())
                    .map(|store| {
                        if t.frame == 0 {
                            return context_of(store, slot, &notes, !t.insert_only, &erase);
                        }
                        // B2-F16: a destination clip elsewhere in the song —
                        // compared in its own ticks, the context brought back
                        // onto the edited notes' ticks.
                        let host = |v: &[TNote]| -> Vec<TNote> {
                            v.iter()
                                .filter_map(|n| {
                                    let start =
                                        u64::try_from(i128::from(n.start) - t.frame).ok()?;
                                    Some(TNote { start, ..*n })
                                })
                                .collect()
                        };
                        let mut ctx =
                            context_of(store, slot, &host(&notes), !t.insert_only, &host(&erase));
                        // A note that began before the edited notes' tick 0
                        // keeps the part from there on.
                        ctx.retain_mut(|n| {
                            let s = i128::from(n.start) + t.frame;
                            let e = s + i128::from(n.dur);
                            if e <= 0 {
                                return false;
                            }
                            n.start = u64::try_from(s.max(0)).unwrap_or(u64::MAX);
                            n.dur = u64::try_from(e - s.max(0)).unwrap_or(u64::MAX);
                            true
                        });
                        ctx
                    })
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        };
        let r = apply_with_context(&mut notes, &source, &locked, &context, g.policy);
        total.removed += r.removed;
        total.trimmed += r.trimmed;
        out.extend(notes);
        a = b;
    }
    if total.removed + total.trimmed > 0 {
        run.notes = out;
        run.stats.overlaps_removed += total.removed;
        run.stats.overlaps_trimmed += total.trimmed;
    }
    total
}

/// Runs the General Note Overlap Prevention on a prepared note edit (Paste,
/// Scale X / Y, a replacing edit): its records against each other and — with
/// [`General::existing`] — the notes that stay in its host (the unselected
/// notes of a replacing edit, all notes of an insert). Other edits are left
/// alone. The records stay sorted by start.
pub fn prevent_in_edit(
    doc: &crate::model::PlumDocument,
    edit: &mut crate::editor::PreparedEdit,
    g: General,
) -> Report {
    use crate::editor::PreparedEdit as E;
    if let E::RemapSelectedPitch {
        host,
        base_revision,
        table,
        ..
    } = edit
    {
        // An in-place pitch remap (Scale Y): as the records it writes; it
        // becomes a replacing edit only when something has to go.
        let (host, base_revision) = (*host, *base_revision);
        let Some(store) = doc.notes_of(host) else {
            return Report::default();
        };
        let mut records = Vec::with_capacity(store.selected_count());
        store.for_each_selected(|v| {
            let mut r = v.record();
            let p = table[usize::from(r.attrs.pitch())];
            r.attrs = r.attrs.with_pitch_wide(p).unwrap_or(r.attrs);
            records.push(r);
        });
        let mut replace = E::ReplaceSelected {
            host,
            base_revision,
            records,
            shift: None,
        };
        let r = prevent_in_edit(doc, &mut replace, g);
        if r.removed + r.trimmed > 0 {
            *edit = replace;
        }
        return r;
    }
    let (host, records, replacing, erase) = match edit {
        E::ReplaceSelected { host, records, .. } => (*host, records, true, &[][..]),
        E::ReplaceSelectedErasing {
            host,
            records,
            erase,
            ..
        } => (*host, records, true, &erase[..]),
        E::InsertSelected { host, records, .. } => (*host, records, false, &[][..]),
        _ => return Report::default(),
    };
    let Some(store) = doc.notes_of(host) else {
        return Report::default();
    };
    let mut notes: Vec<TNote> = records.iter().map(|r| TNote::from_record(r, 0)).collect();
    sort_notes(&mut notes);
    let mut source = Vec::new();
    let mut locked = Vec::new();
    if replacing {
        let any_locked = super::source::any_locked(doc);
        store.for_each_selected(|v| {
            let r = v.record();
            let n = TNote::from_record(&r, 0);
            if any_locked && super::source::is_locked(doc, &r) {
                locked.push(n);
            }
            source.push(n);
        });
    }
    let context = if g.existing {
        let mut er: Vec<TNote> = erase.iter().map(|r| TNote::from_record(r, 0)).collect();
        sort_notes(&mut er);
        context_of(store, 0, &notes, replacing, &er)
    } else {
        Vec::new()
    };
    let r = apply_with_context(&mut notes, &source, &locked, &context, g.policy);
    if r.removed + r.trimmed > 0 {
        *records = notes.iter().map(TNote::to_record).collect();
        records.sort_by_key(|r| r.start_tick);
        if let E::ReplaceSelected { shift, .. } = edit {
            // No longer the selection moved as a whole.
            *shift = None;
        }
    }
    r
}

/// Runs the General Note Overlap Prevention on one note about to be drawn
/// into `store` (every note there stays): the note as it may be placed
/// (shortened by Trim Overlaps), or `None` when it would overlap.
pub fn prevent_note(
    store: &crate::model::NoteStore,
    record: &crate::model::NoteRecord,
    g: General,
) -> Option<crate::model::NoteRecord> {
    let mut notes = vec![TNote::from_record(record, 0)];
    let context = if g.existing {
        context_of(store, 0, &notes, false, &[])
    } else {
        return Some(*record);
    };
    let p = Policy {
        // A drawn note is the operation's own note.
        generated_only: false,
        ..g.policy
    };
    apply_with_context(&mut notes, &[], &[], &context, p);
    notes.first().map(TNote::to_record)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(start: u64, dur: u64, pitch: u8) -> TNote {
        TNote {
            start,
            dur,
            sub: 0,
            group: 0,
            track: 0,
            pitch,
            vel: 100,
            rel: 64,
            ch: 0,
            flags: 0,
        }
    }

    fn run(
        mut v: Vec<TNote>,
        src: &[TNote],
        mode: Mode,
        gen_only: bool,
        across: bool,
    ) -> (Vec<TNote>, Report) {
        sort_notes(&mut v);
        let r = apply(
            &mut v,
            src,
            &[],
            Policy {
                mode,
                generated_only: gen_only,
                across,
            },
        );
        (v, r)
    }

    #[test]
    fn exact_duplicates_keep_the_loudest_and_leave_other_lengths() {
        let mut quiet = n(0, 100, 60);
        quiet.vel = 40;
        let v = vec![
            n(0, 100, 60),
            quiet,
            n(0, 100, 60),
            n(0, 50, 60),
            n(0, 100, 61),
        ];
        let (out, r) = run(v, &[], Mode::ExactDuplicates, false, false);
        assert_eq!(
            r,
            Report {
                removed: 2,
                trimmed: 0
            }
        );
        assert_eq!(out.len(), 3);
        assert!(
            out.iter()
                .filter(|x| x.dur == 100 && x.pitch == 60)
                .all(|x| x.vel == 100)
        );
        // Same pitch and start keeps only the longest.
        let v = vec![n(0, 100, 60), n(0, 50, 60), n(0, 100, 61)];
        let (out, r) = run(v, &[], Mode::SamePitchStart, false, false);
        assert_eq!((r.removed, out.len()), (1, 2));
        assert!(out.iter().any(|x| x.pitch == 60 && x.dur == 100));
    }

    #[test]
    fn layers_stay_apart_unless_across() {
        let mut other = n(0, 100, 60);
        other.sub = 1;
        let v = vec![n(0, 100, 60), other];
        let (out, _) = run(v.clone(), &[], Mode::ExactDuplicates, false, false);
        assert_eq!(out.len(), 2, "different subchannels are layers");
        let (out, _) = run(v, &[], Mode::ExactDuplicates, false, true);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn source_stacks_are_protected_and_only_generated_copies_go() {
        // The user's own stacked pair, and the tool added a third copy.
        let src = vec![n(0, 100, 60), n(0, 100, 60)];
        let v = vec![n(0, 100, 60), n(0, 100, 60), n(0, 100, 60)];
        let (out, r) = run(v.clone(), &src, Mode::ExactDuplicates, true, false);
        assert_eq!((out.len(), r.removed), (2, 1));
        // Without the protection the stack collapses.
        let (out, _) = run(v, &src, Mode::ExactDuplicates, false, false);
        assert_eq!(out.len(), 1);
        // generated(): one source note protects one result note.
        let mut res = vec![n(0, 100, 60), n(0, 100, 60), n(5, 1, 1)];
        sort_notes(&mut res);
        assert_eq!(generated(&res, &[n(0, 100, 60)]), vec![false, true, true]);
    }

    #[test]
    fn overlaps_are_trimmed_or_removed_per_pitch_and_layer() {
        let v = vec![n(0, 100, 60), n(50, 100, 60), n(60, 10, 61), n(200, 10, 60)];
        let (out, r) = run(v.clone(), &[], Mode::TrimOverlaps, false, false);
        assert_eq!(
            r,
            Report {
                removed: 0,
                trimmed: 1
            }
        );
        assert_eq!(out.iter().find(|x| x.start == 0).unwrap().dur, 50);
        assert_eq!(out.len(), 4);
        let (out, r) = run(v, &[], Mode::RemoveOverlaps, false, false);
        assert_eq!(
            r,
            Report {
                removed: 1,
                trimmed: 0
            }
        );
        assert!(out.iter().all(|x| x.start != 50));
        // A long note covering several later ones: each later one is met.
        let v = vec![n(0, 1000, 60), n(100, 10, 60), n(300, 10, 60)];
        let (out, r) = run(v, &[], Mode::RemoveOverlaps, false, false);
        assert_eq!((out.len(), r.removed), (1, 2));
        // Touching (end == next start) is not an overlap.
        let (out, r) = run(
            vec![n(0, 100, 60), n(100, 100, 60)],
            &[],
            Mode::TrimOverlaps,
            false,
            false,
        );
        assert_eq!((out.len(), r), (2, Report::default()));
    }

    #[test]
    fn a_protected_earlier_note_is_never_trimmed() {
        let src = vec![n(0, 100, 60)];
        let v = vec![n(0, 100, 60), n(50, 100, 60)];
        let (out, r) = run(v, &src, Mode::TrimOverlaps, true, false);
        assert_eq!(
            r,
            Report {
                removed: 1,
                trimmed: 0
            },
            "the generated note goes"
        );
        assert_eq!(out, vec![n(0, 100, 60)]);
        // Both from the source: nothing happens.
        let src = vec![n(0, 100, 60), n(50, 100, 60)];
        let (out, r) = run(src.clone(), &src, Mode::RemoveOverlaps, true, false);
        assert_eq!((out.len(), r), (2, Report::default()));
    }

    #[test]
    fn tracks_never_mix_and_the_result_stays_canonical() {
        let mut t1 = n(0, 100, 60);
        t1.track = 1;
        let v = vec![n(0, 100, 60), t1, n(10, 100, 60), n(5, 1, 61)];
        let (out, r) = run(v, &[], Mode::TrimOverlaps, false, true);
        assert_eq!(r.trimmed, 1);
        let mut sorted = out.clone();
        sort_notes(&mut sorted);
        assert_eq!(out, sorted);
        assert_eq!(out.iter().filter(|x| x.track == 1).count(), 1);
    }

    /// Random results: without protection, no overlap of the chosen kind
    /// survives in any layer, nothing is invented, and lengths only shrink.
    #[test]
    fn random_results_end_without_overlaps_of_the_chosen_kind() {
        let mut s = 0x2545_F491_4F6C_DD1Du64;
        let mut r = |m: u64| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s % m
        };
        for round in 0..300 {
            let len = 1 + r(40) as usize;
            let v: Vec<TNote> = (0..len)
                .map(|_| {
                    let mut x = n(r(20) * 10, 1 + r(4) * 10, 60 + r(3) as u8);
                    x.sub = r(2) as u32;
                    x.vel = 1 + r(3) as u8;
                    x.track = r(2) as u16;
                    x
                })
                .collect();
            for mode in Mode::ALL {
                for across in [false, true] {
                    let (out, rep) = run(v.clone(), &[], mode, false, across);
                    assert_eq!(out.len() as u64 + rep.removed, v.len() as u64, "{round}");
                    // Every result note comes from an input note (same
                    // start, pitch, layer, track; never longer).
                    for o in &out {
                        assert!(
                            v.iter().any(|i| (i.track, i.start, i.pitch, i.sub, i.vel)
                                == (o.track, o.start, o.pitch, o.sub, o.vel)
                                && o.dur <= i.dur
                                && o.dur >= 1),
                            "{round} {mode:?}"
                        );
                    }
                    for (a, x) in out.iter().enumerate() {
                        for y in &out[a + 1..] {
                            if x.track != y.track
                                || x.pitch != y.pitch
                                || layer(x, across) != layer(y, across)
                            {
                                continue;
                            }
                            let clash = match mode {
                                Mode::ExactDuplicates => x.start == y.start && x.dur == y.dur,
                                Mode::SamePitchStart => x.start == y.start,
                                _ => x.start < y.start + y.dur && y.start < x.start + x.dur,
                            };
                            assert!(!clash, "{round} {mode:?} {across}: {x:?} {y:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn protected_notes_are_never_touched_even_without_the_source_rule() {
        let mut v = vec![n(0, 100, 60), n(0, 100, 60)];
        let r = apply(
            &mut v,
            &[],
            &[n(0, 100, 60), n(0, 100, 60)],
            Policy {
                mode: Mode::ExactDuplicates,
                generated_only: false,
                across: false,
            },
        );
        assert_eq!((v.len(), r), (2, Report::default()));
    }

    /// B1-0117: context notes take part but never leave or change; no clash
    /// is left among the results or with the context, in every mode and
    /// layer setting (random sets).
    #[test]
    fn context_notes_are_partners_never_removed_or_returned() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut rnd = |m: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % m
        };
        for round in 0..300 {
            for mode in Mode::ALL {
                for across in [false, true] {
                    let mk = |rnd: &mut dyn FnMut(u64) -> u64| {
                        let mut x = n(rnd(400), 1 + rnd(120), 60 + rnd(3) as u8);
                        x.sub = rnd(2) as u32;
                        x
                    };
                    let mut res: Vec<TNote> = (0..1 + rnd(12)).map(|_| mk(&mut rnd)).collect();
                    let mut ctx: Vec<TNote> = (0..rnd(10)).map(|_| mk(&mut rnd)).collect();
                    for c in &mut ctx {
                        c.vel = 7; // tells a context note apart
                    }
                    sort_notes(&mut res);
                    sort_notes(&mut ctx);
                    let before = res.len();
                    let p = Policy {
                        mode,
                        generated_only: false,
                        across,
                    };
                    let r = apply_with_context(&mut res, &[], &[], &ctx, p);
                    assert!(
                        res.iter().all(|x| x.vel != 7),
                        "{round}: a context note came out"
                    );
                    assert_eq!(res.len() as u64 + r.removed, before as u64, "{round}");
                    let clash = |x: &TNote, y: &TNote| {
                        x.pitch == y.pitch
                            && layer(x, across) == layer(y, across)
                            && match mode {
                                Mode::ExactDuplicates => x.start == y.start && x.dur == y.dur,
                                Mode::SamePitchStart => x.start == y.start,
                                _ => x.start < y.start + y.dur && y.start < x.start + x.dur,
                            }
                    };
                    for (a, x) in res.iter().enumerate() {
                        for y in res[a + 1..].iter().chain(&ctx) {
                            assert!(!clash(x, y), "{round} {mode:?} {across}: {x:?} {y:?}");
                        }
                    }
                }
            }
        }
    }

    /// B1-0117 generated-only: a result identical to a source note is the
    /// user's own and stays even beside a context note it overlaps; an
    /// earlier generated note that a kept note overlaps goes.
    #[test]
    fn generated_only_keeps_the_users_own_notes() {
        let own = n(0, 100, 60);
        let mut res = vec![own, n(150, 100, 60)];
        let ctx = vec![n(50, 10, 60), n(200, 100, 60)];
        let p = Policy {
            mode: Mode::RemoveOverlaps,
            generated_only: true,
            across: false,
        };
        let r = apply_with_context(&mut res, &[own], &[], &ctx, p);
        assert_eq!(res, vec![own]);
        assert_eq!(r.removed, 1);
        // Without context the v0.9 service is unchanged.
        let mut v = vec![n(0, 100, 60), n(50, 100, 60)];
        let r = apply(&mut v, &[n(50, 100, 60)], &[], p);
        assert_eq!((v.len(), r.removed), (2, 0), "the later note is protected");
    }
}
