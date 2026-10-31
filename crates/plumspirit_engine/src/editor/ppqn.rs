//! Change PPQN (Experimental) — v1.0-beta1 B1-0103 / B1-0104 (contract §014-B).
//!
//! [`prepare_change_ppqn`] reads the document and computes a complete new
//! [`Timebase`]: the division, every track's and owned content's note and
//! event stores, the track properties that hold ticks (tick correction,
//! explicit end), the Conductor, the clip placements and the project's tick
//! settings (loop range, named views, the Track List view). Nothing in the
//! document changes; the history swaps the prepared parts in as one undo step
//! ([`crate::history::Operation::SetTimebase`]) and swaps them back on undo.
//!
//! Two modes:
//!
//! * [`PpqnMode::Beats`] — every tick × `to / from`, rounded half up in
//!   `u128` (a tick that does not fit 64 bits is refused); tempos are kept, so
//!   bars, beats and time stay within half a new tick. Note ends are rescaled
//!   like starts (so notes that touched still touch) and a duration that
//!   rounds to zero becomes 1 tick. Entries of the Conductor that land on one
//!   tick are merged (the later one is kept); markers keep their order.
//! * [`PpqnMode::Seconds`] — ticks are kept and every tempo is multiplied by
//!   `to / from` (microseconds per quarter note), so every event keeps its
//!   time in seconds and the beat grid changes. Rounded tempos are counted.
//!
//! The map `tick → round_half_up(tick × to / from)` is monotone: notes and
//! events stay in their order (equal new starts keep the original order) and
//! a note's selection is kept by rank. [`PpqnReport`] counts what the
//! rounding did; it is the dialog's Preview.

use crate::model::clip::{ClipId, ClipPlacement, ContentId, ContentRef};
use crate::model::conductor::Conductor;
use crate::model::document::PlumDocument;
use crate::model::events::EventStore;
use crate::model::note_store::{NoteStore, NoteStoreBuilder};
use crate::model::project::{Bpm, TrackListView, ZoomRegion};
use crate::model::tempo::TempoValue;
use crate::model::timing::{Division, MAX_NATIVE_PPQN, MAX_SMF_PPQN, TimingKind};
use crate::model::track::{MAX_TICK_CORRECTION, TrackId, TrackProps};

/// What a PPQN change keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PpqnMode {
    /// Rescale every tick; tempos unchanged (bars, beats and seconds kept).
    Beats,
    /// Keep the ticks; rescale every tempo (seconds kept, the beat grid moves).
    Seconds,
}

impl PpqnMode {
    pub fn label(self) -> &'static str {
        match self {
            PpqnMode::Beats => "Preserve beats (rescale ticks)",
            PpqnMode::Seconds => "Preserve seconds (keep ticks, rescale tempos)",
        }
    }
}

/// What a prepared PPQN change does (the Preview).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PpqnReport {
    pub from: u32,
    pub to: u32,
    pub seconds_mode: bool,
    /// Notes rescaled (tracks and owned clip contents).
    pub notes: u64,
    /// Preserved events rescaled.
    pub events: u64,
    /// Tick values that were not exact and were rounded (half up).
    pub rounded_ticks: u64,
    /// Notes whose start or end was rounded.
    pub rounded_notes: u64,
    /// Notes whose length rounded to zero and became 1 tick.
    pub lengthened: u64,
    /// Notes that now start on the same tick, key and subchannel as a note that
    /// started on another tick before (kept, in their original order).
    pub collisions: u64,
    /// Events that now share a tick with events of another tick.
    pub merged_events: u64,
    /// Tempo changes, time signatures and Master Volume points merged with an
    /// entry that landed on the same tick (the later one is kept).
    pub merged_conductor: u64,
    /// Notes that entered or left a clip's window because its edge was rounded.
    pub window_changes: u64,
    /// Tempos that could not be scaled exactly (Seconds mode).
    pub rounded_tempos: u64,
    /// Largest rounding error, in new ticks: `max_error_num / from` (≤ ½).
    pub max_error_num: u64,
    /// Memory of the new stores (the undo step keeps the same amount for the
    /// previous ones).
    pub bytes: u64,
}

impl PpqnReport {
    /// Nothing was rounded, merged or lengthened: the change is exact.
    pub fn is_exact(&self) -> bool {
        self.rounded_ticks == 0
            && self.lengthened == 0
            && self.collisions == 0
            && self.merged_events == 0
            && self.merged_conductor == 0
            && self.window_changes == 0
            && self.rounded_tempos == 0
    }

    /// Largest rounding error in new ticks.
    pub fn max_error_ticks(&self) -> f64 {
        self.max_error_num as f64 / f64::from(self.from.max(1))
    }

    /// One line per finding, in American English (the dialog's Preview).
    pub fn lines(&self) -> Vec<String> {
        use crate::midi::group_digits as g;
        let mut out = Vec::new();
        if self.seconds_mode {
            out.push(format!(
                "Ticks stay where they are; every tempo is multiplied by {} / {}, so every note keeps its time in seconds and the bars move.",
                g(u64::from(self.to)),
                g(u64::from(self.from))
            ));
            if self.rounded_tempos > 0 {
                out.push(format!(
                    "{} tempos cannot be scaled exactly and are rounded (MIDI tempos are whole microseconds).",
                    g(self.rounded_tempos)
                ));
            }
        } else {
            out.push(format!(
                "{} notes and {} events are rescaled; the tempos stay, so bars, beats and seconds are kept.",
                g(self.notes),
                g(self.events)
            ));
            if self.rounded_ticks > 0 {
                out.push(format!(
                    "{} positions are not exact at {} PPQN and are rounded to the nearest tick (largest error {:.3} tick), {} of them note starts or ends.",
                    g(self.rounded_ticks),
                    g(u64::from(self.to)),
                    self.max_error_ticks(),
                    g(self.rounded_notes)
                ));
            }
            if self.lengthened > 0 {
                out.push(format!(
                    "{} notes would be shorter than one tick and become 1 tick long.",
                    g(self.lengthened)
                ));
            }
            if self.collisions > 0 {
                out.push(format!(
                    "{} notes now start on the same tick and key as another note; they are kept in their original order.",
                    g(self.collisions)
                ));
            }
            if self.merged_events > 0 {
                out.push(format!(
                    "{} events now share a tick with events that were apart; their order is kept.",
                    g(self.merged_events)
                ));
            }
            if self.merged_conductor > 0 {
                out.push(format!(
                    "{} tempo changes, time signatures or Master Volume points land on the same tick as a later one and are merged into it.",
                    g(self.merged_conductor)
                ));
            }
            if self.window_changes > 0 {
                out.push(format!(
                    "{} notes enter or leave a clip because the clip's edge is rounded.",
                    g(self.window_changes)
                ));
            }
        }
        if self.bytes >= 1 << 20 {
            out.push(format!(
                "It uses about {} MB more memory: Undo keeps the previous notes.",
                g(self.bytes >> 20)
            ));
        }
        if self.to > MAX_SMF_PPQN {
            out.push(format!(
                "{} PPQN is above the 32,767 a MIDI file can store: the project keeps it, and MIDI export rescales with a notice.",
                g(u64::from(self.to))
            ));
        }
        if self.is_exact() {
            out.push("The change is exact: nothing is rounded.".into());
        }
        out
    }
}

/// The parts of a document a PPQN change replaces. Prepared by
/// [`prepare_change_ppqn`]; after the history swaps it in, it holds the
/// previous parts (undo swaps them back).
#[derive(Debug)]
pub struct Timebase {
    pub(crate) division: Division,
    pub(crate) conductor: Conductor,
    /// Per track in document order; empty in Seconds mode.
    pub(crate) tracks: Vec<TrackPart>,
    pub(crate) contents: Vec<ContentPart>,
    /// The explicit clip placements (`None`: unchanged).
    pub(crate) placements: Option<Vec<ClipPlacement>>,
    /// The project's tick settings (`None`: unchanged).
    pub(crate) project: Option<ProjectPart>,
}

#[derive(Debug)]
pub(crate) struct TrackPart {
    pub(crate) id: TrackId,
    pub(crate) notes: NoteStore,
    pub(crate) events: EventStore,
    pub(crate) props: TrackProps,
    pub(crate) end: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct ContentPart {
    pub(crate) id: ContentId,
    pub(crate) notes: NoteStore,
    pub(crate) events: EventStore,
}

#[derive(Debug)]
pub(crate) struct ProjectPart {
    pub(crate) loop_range: Option<(u64, u64)>,
    pub(crate) zoom_regions: Vec<ZoomRegion>,
    pub(crate) track_list: Option<TrackListView>,
}

impl Timebase {
    /// The division this timebase holds.
    pub fn division(&self) -> Division {
        self.division
    }

    /// Undo memory of the parts held (the stores that are not in the document).
    pub fn heap_bytes(&self) -> usize {
        self.conductor.heap_bytes()
            + self
                .tracks
                .iter()
                .map(|t| t.notes.heap_bytes() + t.events.heap_bytes() + 64)
                .sum::<usize>()
            + self
                .contents
                .iter()
                .map(|c| c.notes.heap_bytes() + c.events.heap_bytes() + 32)
                .sum::<usize>()
            + self.placements.as_ref().map_or(0, |p| {
                p.len() * std::mem::size_of::<ClipPlacement>()
                    + p.iter()
                        .map(|q| q.name.as_ref().map_or(0, String::capacity))
                        .sum::<usize>()
            })
            + self.project.as_ref().map_or(0, |p| {
                p.zoom_regions.len() * std::mem::size_of::<ZoomRegion>()
            })
    }

    /// Preserve beats: the ticks themselves are rescaled (Preserve seconds
    /// keeps them).
    pub fn retimes_ticks(&self) -> bool {
        self.placements.is_some()
    }

    /// Ids of the tracks whose stores this timebase holds.
    pub(crate) fn track_ids(&self) -> Vec<TrackId> {
        let mut v: Vec<TrackId> = self.tracks.iter().map(|t| t.id).collect();
        v.sort_unstable();
        v
    }
}

/// The PPQN of a metrical document (`Err`: SMPTE or zero PPQN).
pub fn current_ppqn(doc: &PlumDocument) -> Result<u32, String> {
    match doc.division().kind() {
        TimingKind::Ppqn(p) if p > 0 => Ok(p),
        TimingKind::Ppqn(_) => Err(
            "Change PPQN needs a project with PPQN timing; this one has a zero division.".into(),
        ),
        TimingKind::Smpte { .. } => Err(format!(
            "Change PPQN needs a project with PPQN timing; this one uses {}.",
            doc.division().describe()
        )),
    }
}

/// The exact tick map and its statistics.
struct Scale {
    from: u128,
    to: u128,
    rounded: u64,
    max_err: u128,
}

impl Scale {
    fn new(from: u32, to: u32) -> Scale {
        Scale {
            from: u128::from(from),
            to: u128::from(to),
            rounded: 0,
            max_err: 0,
        }
    }

    /// `round_half_up(t × to / from)`, refused above 64 bits. Counts rounding.
    fn tick(&mut self, t: u64) -> Result<u64, String> {
        let p = u128::from(t) * self.to;
        let q = (2 * p + self.from) / (2 * self.from);
        let err = (q * self.from).abs_diff(p);
        if err != 0 {
            self.rounded += 1;
            self.max_err = self.max_err.max(err);
        }
        u64::try_from(q).map_err(|_| overflow(t))
    }

    /// Like [`Scale::tick`] without statistics (window edge lookups).
    fn peek(&self, t: u64) -> u128 {
        (2 * u128::from(t) * self.to + self.from) / (2 * self.from)
    }

    /// The smallest old tick whose new tick is at least `v` (`v ≥ 1`):
    /// `t·to·2 + from ≥ 2·v·from` ⇔ `t ≥ ⌈(2v − 1)·from / (2·to)⌉`.
    fn first_reaching(&self, v: u128) -> u128 {
        if v == 0 {
            return 0;
        }
        let num = (2 * v - 1) * self.from;
        num.div_ceil(2 * self.to)
    }

    fn signed(&mut self, x: i64) -> Result<i64, String> {
        let m = self.tick(x.unsigned_abs())?;
        let m = i64::try_from(m)
            .ok()
            .filter(|&m| m <= MAX_TICK_CORRECTION)
            .ok_or_else(|| {
                "A track's tick correction would be larger than PlumSpirit can store. Nothing was changed."
                    .to_string()
            })?;
        Ok(if x < 0 { -m } else { m })
    }
}

fn overflow(t: u64) -> String {
    format!(
        "Tick {} would be beyond the largest tick PlumSpirit can store at the new PPQN. Nothing was changed.",
        crate::midi::group_digits(t)
    )
}

/// Collision counter for one note store: notes grouped by new start.
#[derive(Default)]
struct Collisions {
    start: Option<u64>,
    group: Vec<(u64, u64)>,
    count: u64,
}

impl Collisions {
    fn push(&mut self, new_start: u64, key: u64, old_start: u64) {
        if self.start != Some(new_start) {
            self.flush();
            self.start = Some(new_start);
        }
        self.group.push((key, old_start));
    }

    fn flush(&mut self) {
        if self.group.len() > 1 {
            self.group.sort_unstable();
            let mut first = self.group[0];
            for &(k, o) in &self.group[1..] {
                if k != first.0 {
                    first = (k, o);
                } else if o != first.1 {
                    self.count += 1;
                }
            }
        }
        self.group.clear();
    }
}

fn retime_notes(
    store: &NoteStore,
    s: &mut Scale,
    report: &mut PpqnReport,
) -> Result<NoteStore, String> {
    let mut b = NoteStoreBuilder::new();
    let mut coll = Collisions::default();
    for v in store.iter() {
        let before = s.rounded;
        let start = s.tick(v.start_tick)?;
        let end = s.tick(v.end_tick())?;
        if s.rounded != before {
            report.rounded_notes += 1;
        }
        let mut dur = end - start;
        if dur == 0 {
            dur = 1;
            report.lengthened += 1;
            if start == u64::MAX {
                return Err(overflow(v.start_tick));
            }
        }
        coll.push(
            start,
            (u64::from(v.sub.0) << 8) | u64::from(v.attrs.pitch()),
            v.start_tick,
        );
        let i = b.push(start, v.attrs).map_err(|e| e.to_string())?;
        b.set_duration(i, dur).map_err(|e| e.to_string())?;
        if v.sub.0 != 0 {
            b.set_sub(i, v.sub);
        }
        if v.group != 0 {
            b.set_group(i, v.group);
        }
    }
    coll.flush();
    report.collisions += coll.count;
    report.notes += store.len() as u64;
    let mut out = b.finish(&[]);
    if store.selected_count() > 0 {
        out.select_ranks(&store.selected_ranks())
            .map_err(|e| e.to_string())?;
    }
    Ok(out)
}

fn retime_events(
    events: &EventStore,
    s: &mut Scale,
    report: &mut PpqnReport,
) -> Result<EventStore, String> {
    let mut last: Option<(u64, u64)> = None;
    let mut merged = 0u64;
    let out = events.retimed(|t| {
        let n = s.tick(t)?;
        if let Some((lo, ln)) = last
            && ln == n
            && lo != t
        {
            merged += 1;
        }
        last = Some((t, n));
        Ok::<u64, String>(n)
    })?;
    report.events += events.len() as u64;
    report.merged_events += merged;
    Ok(out)
}

/// Rescaled conductor lists; entries landing on one tick merge (the later one
/// is kept, it is the one in effect from that tick on).
fn retime_conductor(
    c: &Conductor,
    s: &mut Scale,
    report: &mut PpqnReport,
) -> Result<Conductor, String> {
    fn merge<T>(v: Vec<T>, tick: impl Fn(&T) -> u64, merged: &mut u64) -> Vec<T> {
        let mut out: Vec<T> = Vec::with_capacity(v.len());
        for x in v {
            if out.last().is_some_and(|p| tick(p) == tick(&x)) {
                out.pop();
                *merged += 1;
            }
            out.push(x);
        }
        out
    }
    let mut tempo = c.tempo_points().to_vec();
    for p in &mut tempo {
        p.tick = s.tick(p.tick)?;
    }
    let mut meter = c.time_signatures().to_vec();
    for m in &mut meter {
        m.tick = s.tick(m.tick)?;
    }
    let mut master = c.master_volume().to_vec();
    for m in &mut master {
        m.tick = s.tick(m.tick)?;
    }
    let mut markers = c.markers().to_vec();
    for m in &mut markers {
        m.start = s.tick(m.start)?;
        if let Some(e) = m.end {
            let e = s.tick(e)?;
            m.end = Some(if e > m.start { e } else { m.start + 1 });
        }
    }
    markers.sort_by_key(|m| (m.start, m.id));
    let mut merged = 0u64;
    let tempo = merge(tempo, |p| p.tick, &mut merged);
    let meter = merge(meter, |m| m.tick, &mut merged);
    let master = merge(master, |m| m.tick, &mut merged);
    report.merged_conductor += merged;
    Conductor::from_parts_with_master(c.scope(), tempo, meter, markers, master, c.next_id())
        .map_err(|m| format!("The rescaled tempo map is not valid ({m}). Nothing was changed."))
}

/// `b × num / den` as a BPM, rounded to the representable digits; `true` when exact.
fn scale_bpm(b: Bpm, num: u128, den: u128) -> Option<(Bpm, bool)> {
    let (n, d) = b.as_ratio();
    let n2 = n.checked_mul(num)?;
    let d2 = d.checked_mul(den)?;
    let top = n2.checked_mul(1_000_000_000_000)?;
    let mut m = top.checked_mul(2)?.checked_add(d2)? / d2.checked_mul(2)?;
    let mut exact = m.checked_mul(d2)? == top;
    let mut e: i32 = -12;
    while m > 0 && m.is_multiple_of(10) {
        m /= 10;
        e += 1;
    }
    while m >= 1_000_000_000_000_000_000 {
        exact &= m.is_multiple_of(10);
        m = (m + 5) / 10;
        e += 1;
    }
    Some((Bpm::new(u64::try_from(m).ok()?, e).ok()?, exact))
}

fn scale_tempos(
    c: &Conductor,
    from: u32,
    to: u32,
    report: &mut PpqnReport,
) -> Result<Conductor, String> {
    let range = |tick: u64| {
        format!(
            "At tick {} the tempo would be outside what a MIDI file can store (3.5762788 – 60,000,000 BPM). Nothing was changed.",
            crate::midi::group_digits(tick)
        )
    };
    let mut tempo = c.tempo_points().to_vec();
    for p in &mut tempo {
        p.value = match p.value {
            TempoValue::Micros(us) => {
                let top = u128::from(us) * u128::from(to);
                let f = u128::from(from);
                let q = (2 * top + f) / (2 * f);
                if q * f != top {
                    report.rounded_tempos += 1;
                }
                if !(1..=0xFF_FFFF).contains(&q) {
                    return Err(range(p.tick));
                }
                TempoValue::Micros(q as u32)
            }
            TempoValue::Bpm(b) => {
                let (nb, exact) =
                    scale_bpm(b, u128::from(from), u128::from(to)).ok_or_else(|| range(p.tick))?;
                nb.check_midi().map_err(|_| range(p.tick))?;
                if !exact {
                    report.rounded_tempos += 1;
                }
                TempoValue::Bpm(nb)
            }
        };
        // The scaled default is not the implicit 120 BPM any more.
        p.implicit = false;
    }
    Conductor::from_parts_with_master(
        c.scope(),
        tempo,
        c.time_signatures().to_vec(),
        c.markers().to_vec(),
        c.master_volume().to_vec(),
        c.next_id(),
    )
    .map_err(|m| format!("The rescaled tempo map is not valid ({m}). Nothing was changed."))
}

/// Notes of the content a placement shows that enter or leave its window
/// when the window's edges are rounded (only when the PPQN goes down).
fn window_changes(doc: &PlumDocument, p: &ClipPlacement, s: &Scale) -> u64 {
    if s.to >= s.from {
        return 0;
    }
    let notes = match p.content {
        ContentRef::Track(t) => doc.track_by_id(t).map(|t| &t.notes),
        ContentRef::Owned(c) => doc.clips().content(c).map(|c| &c.notes),
    };
    let Some(notes) = notes else {
        return 0;
    };
    let mut n = 0u64;
    // A start before the window whose new tick reaches the new first tick.
    let mut edge = |old: u64| {
        let v = s.peek(old);
        let a = s.first_reaching(v);
        if a < u128::from(old) {
            n += notes.count_starting_in(a as u64, old - 1) as u64;
        }
    };
    if p.offset > 0 {
        edge(p.offset);
    }
    if let Some(l) = p.length
        && let Some(end) = p.offset.checked_add(l)
    {
        edge(end);
    }
    n
}

/// Computes a PPQN change from the document's PPQN to `to` (1 – 2,147,483,647).
pub fn prepare_change_ppqn(
    doc: &PlumDocument,
    to: u32,
    mode: PpqnMode,
) -> Result<(Timebase, PpqnReport), String> {
    let from = current_ppqn(doc)?;
    if to == 0 || to > MAX_NATIVE_PPQN {
        return Err(format!(
            "Enter a PPQN from 1 to {}.",
            crate::midi::group_digits(u64::from(MAX_NATIVE_PPQN))
        ));
    }
    if to == from {
        return Err(format!(
            "The project already uses {} PPQN. Nothing was changed.",
            crate::midi::group_digits(u64::from(from))
        ));
    }
    let mut report = PpqnReport {
        from,
        to,
        seconds_mode: mode == PpqnMode::Seconds,
        ..PpqnReport::default()
    };
    let division = Division::ppqn(to);
    if mode == PpqnMode::Seconds {
        if !doc.conductor().timing_editable() {
            return Err(
                "Preserve seconds needs one tempo map; a Format 2 project keeps a tempo per sequence. Use Preserve beats."
                    .into(),
            );
        }
        let conductor = scale_tempos(doc.conductor(), from, to, &mut report)?;
        return Ok((
            Timebase {
                division,
                conductor,
                tracks: Vec::new(),
                contents: Vec::new(),
                placements: None,
                project: None,
            },
            report,
        ));
    }
    let mut s = Scale::new(from, to);
    let mut tracks = Vec::with_capacity(doc.tracks().len());
    for t in doc.tracks() {
        let notes = retime_notes(&t.notes, &mut s, &mut report)?;
        let events = retime_events(&t.events, &mut s, &mut report)?;
        let mut props = t.props().clone();
        props.tick_correction = s.signed(props.tick_correction)?;
        let end = t.end_of_track_tick.map(|e| s.tick(e)).transpose()?;
        tracks.push(TrackPart {
            id: t.id(),
            notes,
            events,
            props,
            end,
        });
    }
    let mut contents = Vec::with_capacity(doc.clips().contents().len());
    for c in doc.clips().contents() {
        contents.push(ContentPart {
            id: c.id,
            notes: retime_notes(&c.notes, &mut s, &mut report)?,
            events: retime_events(&c.events, &mut s, &mut report)?,
        });
    }
    let mut placements = Vec::with_capacity(doc.clips().explicit_placements().len());
    for p in doc.clips().explicit_placements() {
        report.window_changes += window_changes(doc, p, &s);
        let mut q = p.clone();
        q.start = s.tick(p.start)?;
        q.offset = s.tick(p.offset)?;
        if let Some(l) = p.length {
            let end = p.offset.checked_add(l).ok_or_else(|| overflow(p.offset))?;
            q.length = Some(s.tick(end)?.saturating_sub(q.offset).max(1));
        }
        let implicit = matches!(q.id, ClipId::Primary(_))
            && q.is_identity()
            && q.name.is_none()
            && q.color.is_none();
        if !implicit {
            placements.push(q);
        }
    }
    let conductor = retime_conductor(doc.conductor(), &mut s, &mut report)?;
    let info = doc.project();
    let loop_range = match info.looping.range {
        Some((a, b)) => {
            let (a, b) = (s.tick(a)?, s.tick(b)?);
            Some((a, b.max(a + 1)))
        }
        None => None,
    };
    let ppt = |p: f64| crate::view::clamp_zoom(p * f64::from(from) / f64::from(to));
    let mut zoom_regions = info.zoom_regions.clone();
    for z in &mut zoom_regions {
        z.origin_tick = s.tick(z.origin_tick)?;
        z.ppt_bits = ppt(z.pixels_per_tick()).to_bits();
    }
    let track_list = match info.track_list {
        Some(v) => Some(TrackListView {
            origin_tick: s.tick(v.origin_tick)?,
            ppt_bits: ppt(v.pixels_per_tick()).to_bits(),
            row_bits: v.row_bits,
        }),
        None => None,
    };
    report.rounded_ticks = s.rounded;
    report.max_error_num = u64::try_from(s.max_err).unwrap_or(u64::MAX);
    let tb = Timebase {
        division,
        conductor,
        tracks,
        contents,
        placements: Some(placements),
        project: Some(ProjectPart {
            loop_range,
            zoom_regions,
            track_list,
        }),
    };
    report.bytes = tb.heap_bytes() as u64;
    Ok((tb, report))
}

/// Swaps `tb` with the document's parts (apply = undo = redo). Checks that
/// `tb` belongs to this document first; on error nothing changed.
pub(crate) fn swap_timebase(doc: &mut PlumDocument, tb: &mut Timebase) -> Result<(), String> {
    if !tb.tracks.is_empty() {
        if tb.tracks.len() != doc.tracks().len()
            || tb
                .tracks
                .iter()
                .zip(doc.tracks())
                .any(|(p, t)| p.id != t.id())
        {
            return Err("the tracks changed since the PPQN change was prepared".into());
        }
        if tb.contents.len() != doc.clips().contents().len()
            || tb
                .contents
                .iter()
                .any(|c| doc.clips().content(c.id).is_none())
        {
            return Err("the clips changed since the PPQN change was prepared".into());
        }
    }
    let old = doc.division();
    doc.set_division(tb.division);
    tb.division = old;
    std::mem::swap(doc.conductor_mut(), &mut tb.conductor);
    doc.conductor_mut().continue_revisions(&tb.conductor);
    for (i, p) in tb.tracks.iter_mut().enumerate() {
        let t = doc.track_mut(i).expect("checked above");
        std::mem::swap(&mut t.notes, &mut p.notes);
        t.notes.continue_revision(p.notes.revision());
        std::mem::swap(&mut t.events, &mut p.events);
        t.bump_events_revision();
        let (props, end) = (t.props().clone(), t.end_of_track_tick);
        t.set_props(
            std::mem::replace(&mut p.props, props),
            std::mem::replace(&mut p.end, end),
        );
    }
    for p in &mut tb.contents {
        let c = doc.clips_mut().content_mut(p.id).expect("checked above");
        std::mem::swap(&mut c.notes, &mut p.notes);
        c.notes.continue_revision(p.notes.revision());
        std::mem::swap(&mut c.events, &mut p.events);
        c.touch_events();
    }
    if let Some(pl) = tb.placements.as_mut() {
        doc.clips_mut().swap_placements(pl);
    }
    if let Some(pp) = tb.project.as_mut() {
        let info = doc.project_mut();
        std::mem::swap(&mut info.looping.range, &mut pp.loop_range);
        std::mem::swap(&mut info.zoom_regions, &mut pp.zoom_regions);
        std::mem::swap(&mut info.track_list, &mut pp.track_list);
    }
    doc.touch_layout();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{History, Operation};
    use crate::model::note::{NoteAttrs, NoteOffKind, NoteRecord};

    fn rec(s: u64, d: u64, p: u8) -> NoteRecord {
        NoteRecord::new(
            s,
            d,
            NoteAttrs::new(p, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
        )
    }

    fn doc(ppqn: u32, notes: &[(u64, u64, u8)]) -> PlumDocument {
        let mut d = PlumDocument::new_project("t".into(), Bpm::from_integer(120).unwrap(), ppqn);
        let recs: Vec<NoteRecord> = notes.iter().map(|&(s, l, p)| rec(s, l, p)).collect();
        d.track_mut(0).unwrap().notes = NoteStore::from_sorted_records(&recs, false).unwrap();
        d
    }

    fn notes(d: &PlumDocument) -> Vec<(u64, u64, u8)> {
        d.tracks()[0]
            .notes
            .iter()
            .map(|v| (v.start_tick, v.duration_ticks, v.attrs.pitch()))
            .collect()
    }

    fn apply(d: &mut PlumDocument, h: &mut History, to: u32, mode: PpqnMode) -> PpqnReport {
        let (tb, r) = prepare_change_ppqn(d, to, mode).unwrap();
        h.apply(
            d,
            Operation::SetTimebase {
                timebase: Box::new(tb),
                label: "Change PPQN",
            },
        )
        .unwrap();
        r
    }

    #[test]
    fn going_up_to_30720_and_back_to_960_is_the_identity() {
        let n = [(0, 240, 60), (240, 1, 61), (961, 959, 62), (3840, 7, 63)];
        let mut d = doc(960, &n);
        let fp = d.state_fingerprint();
        let mut h = History::default();
        let r = apply(&mut d, &mut h, 30_720, PpqnMode::Beats);
        assert!(r.is_exact(), "{r:?}");
        assert_eq!(d.division(), Division::ppqn(30_720));
        assert_eq!(notes(&d)[2], (961 * 32, 959 * 32, 62));
        let r = apply(&mut d, &mut h, 960, PpqnMode::Beats);
        assert!(r.is_exact(), "{r:?}");
        assert_eq!(notes(&d), n.to_vec());
        assert_eq!(d.state_fingerprint(), fp);
        h.undo(&mut d).unwrap();
        h.undo(&mut d).unwrap();
        assert_eq!(d.state_fingerprint(), fp);
        h.redo(&mut d).unwrap();
        assert_eq!(d.division(), Division::ppqn(30_720));
    }

    #[test]
    fn going_down_to_480_rounds_half_up_and_reports_it() {
        // 1 → 0.5 → 1 (half up), 3 → 1.5 → 2; a 1-tick note at 2 → start 1, end 2.
        let mut d = doc(960, &[(1, 1, 60), (2, 1, 60), (3, 2, 61), (960, 960, 62)]);
        let (tb, r) = prepare_change_ppqn(&d, 480, PpqnMode::Beats).unwrap();
        assert!(!r.is_exact());
        assert_eq!(r.notes, 4);
        // Starts 1 → 1, 2 → 1: same key 60 → one collision; ends 2 → 1, 3 → 2.
        assert_eq!(r.collisions, 1);
        assert_eq!(r.lengthened, 1);
        assert_eq!(r.max_error_num, 480);
        assert!(r.max_error_ticks() <= 0.5);
        let mut h = History::default();
        h.apply(
            &mut d,
            Operation::SetTimebase {
                timebase: Box::new(tb),
                label: "Change PPQN",
            },
        )
        .unwrap();
        assert_eq!(
            notes(&d),
            vec![(1, 1, 60), (1, 1, 60), (2, 1, 61), (480, 480, 62)]
        );
        d.tracks()[0].notes.validate().unwrap();
        d.validate_clips().unwrap();
    }

    #[test]
    fn conductor_entries_rescale_and_merge_on_one_tick() {
        let mut d = doc(960, &[(0, 960, 60)]);
        let c = d.conductor();
        let e = c
            .prepare_set_tempo(
                None,
                1920,
                TempoValue::Bpm(Bpm::from_integer(90).unwrap()),
                Default::default(),
            )
            .unwrap();
        d.conductor_mut().apply(&e).unwrap();
        let e = d
            .conductor()
            .prepare_set_tempo(
                None,
                1921,
                TempoValue::Bpm(Bpm::from_integer(150).unwrap()),
                Default::default(),
            )
            .unwrap();
        d.conductor_mut().apply(&e).unwrap();
        let (tb, r) = prepare_change_ppqn(&d, 1, PpqnMode::Beats).unwrap();
        // 1920 → 2, 1921 → 2.0010… → 2: merged, the later (150) kept.
        assert_eq!(r.merged_conductor, 1);
        let t = tb.conductor.tempo_points();
        assert_eq!(t.len(), 2);
        assert_eq!(t[1].tick, 2);
        assert_eq!(t[1].value, TempoValue::Bpm(Bpm::from_integer(150).unwrap()));
    }

    #[test]
    fn seconds_mode_keeps_ticks_and_seconds_and_scales_tempos() {
        let mut d = doc(960, &[(960, 960, 60)]);
        let before = crate::model::tempo::TempoMap::for_document(&d, 48_000).seconds_at_tick(960);
        let mut h = History::default();
        let r = apply(&mut d, &mut h, 480, PpqnMode::Seconds);
        assert!(r.is_exact(), "{r:?}");
        assert_eq!(notes(&d), vec![(960, 960, 60)]);
        assert_eq!(
            d.conductor().tempo_points()[0].value,
            TempoValue::Bpm(Bpm::from_integer(240).unwrap())
        );
        let after = crate::model::tempo::TempoMap::for_document(&d, 48_000).seconds_at_tick(960);
        assert!((before - after).abs() < 1e-9, "{before} {after}");
        h.undo(&mut d).unwrap();
        assert_eq!(d.division(), Division::ppqn(960));
        assert_eq!(
            d.conductor().tempo_points()[0].value,
            TempoValue::Bpm(Bpm::from_integer(120).unwrap())
        );
    }

    #[test]
    fn selection_is_kept_by_rank_and_refusals_change_nothing() {
        let mut d = doc(960, &[(0, 10, 60), (5, 10, 61), (9, 10, 62)]);
        d.track_mut(0).unwrap().notes.select_range(4, 6, false);
        let mut h = History::default();
        apply(&mut d, &mut h, 7, PpqnMode::Beats);
        let sel: Vec<bool> = d.tracks()[0].notes.iter().map(|v| v.selected).collect();
        assert_eq!(sel, vec![false, true, false]);
        assert!(prepare_change_ppqn(&d, 7, PpqnMode::Beats).is_err());
        assert!(prepare_change_ppqn(&d, 0, PpqnMode::Beats).is_err());
        let huge = doc(1, &[(u64::MAX / 4, 1, 60)]);
        assert!(prepare_change_ppqn(&huge, 960, PpqnMode::Beats).is_err());
    }

    #[test]
    fn scale_bpm_is_exact_when_the_decimal_ends() {
        let b = Bpm::from_integer(120).unwrap();
        assert_eq!(
            scale_bpm(b, 960, 480).unwrap(),
            (Bpm::from_integer(240).unwrap(), true)
        );
        let (x, exact) = scale_bpm(b, 1, 3).unwrap();
        assert!(exact);
        assert_eq!(x.to_string(), "40");
        let (y, exact) = scale_bpm(Bpm::from_integer(100).unwrap(), 1, 3).unwrap();
        assert!(!exact);
        assert!(y.to_string().starts_with("33.333333333333"));
    }
}
