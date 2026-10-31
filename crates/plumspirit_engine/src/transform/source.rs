//! The immutable source of a transformation: the notes in scope, copied once
//! into a flat array (32 bytes per note, sorted by time) together with per-track
//! context (subchannel table, revision for staleness checks) and statistics.
//!
//! Pointwise tools never need the copy for previewing: [`SourceStats`] alone
//! (one pass, no allocation per note) is enough to build their kernel, and the
//! viewport evaluates the visible notes directly from the document.

use super::{TNote, TransformError};
use crate::editor::{HostView, NoteTarget};
use crate::model::PlumDocument;
use crate::model::clip::{ContentId, ContentRef};
use crate::model::note::NoteRecord;
use crate::model::placed::{PlacedLane, PlacedNotes};
use crate::model::track::TrackId;

/// Which notes of a track are in scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackScope {
    /// The track (for an owned content: the track it is edited on, whose
    /// subchannels apply).
    pub index: usize,
    /// Every note of the track (otherwise the selected notes).
    pub all: bool,
    /// An owned content edited on track `index` (v0.9 V09-S11), else the
    /// track's own notes.
    pub content: Option<ContentId>,
}

impl TrackScope {
    /// The scope of a note target's notes.
    pub fn of(target: NoteTarget, all: bool) -> TrackScope {
        match target {
            NoteTarget::Track(index) => TrackScope {
                index,
                all,
                content: None,
            },
            NoteTarget::Content { host, content } => TrackScope {
                index: host,
                all,
                content: Some(content),
            },
        }
    }

    pub fn target(self) -> NoteTarget {
        match self.content {
            None => NoteTarget::Track(self.index),
            Some(content) => NoteTarget::Content {
                host: self.index,
                content,
            },
        }
    }
}

/// The notes of `s`, if its track (and content) exist.
fn scope_view(doc: &PlumDocument, s: TrackScope) -> Option<HostView<'_>> {
    crate::editor::note_view(doc, s.target()).ok()
}

/// One track taking part in a transformation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceTrack {
    /// The track (for an owned content: the track it is edited on).
    pub id: TrackId,
    pub index: usize,
    /// The owned content whose notes take part, else the track's own.
    pub content: Option<ContentId>,
    /// Note-store revision when the source was taken (stale check at Apply).
    pub revision: u64,
    /// The track's subchannels: (id, MIDI channel 0–15), in table order.
    pub subs: Vec<(u32, u8)>,
    pub all: bool,
    /// A destination only: its notes are not replaced, results are added.
    pub insert_only: bool,
    /// v1.0-beta2 B2-F16: where this host's content tick 0 lies on the
    /// edited notes' ticks (the host clip's song shift − [`Ctx::shift`]):
    /// the slot's notes are read at `content tick + frame`, its results
    /// stored at `tick − frame` ([`SourceTrack::host_record`]). 0 for the
    /// tracks in scope; a track another track's tool reads or writes
    /// ([`Source::placed_layer_slots`], [`placed_destination`]) carries its
    /// clip's place.
    ///
    /// [`Ctx::shift`]: super::Ctx::shift
    pub frame: i128,
}

impl SourceTrack {
    /// The note host results are written to.
    pub fn host(&self) -> ContentRef {
        match self.content {
            Some(c) => ContentRef::Owned(c),
            None => ContentRef::Track(self.id),
        }
    }

    /// The note target, by the track index the source was taken at.
    pub fn target(&self) -> NoteTarget {
        TrackScope {
            index: self.index,
            all: self.all,
            content: self.content,
        }
        .target()
    }

    /// Note `n` (on the edited notes' ticks) as stored in this host: its start
    /// moved by [`SourceTrack::frame`]. A result before the host's content
    /// tick 0 cannot be stored there and is refused.
    pub fn host_record(&self, n: &TNote) -> Result<NoteRecord, TransformError> {
        let mut r = n.to_record();
        if self.frame != 0 {
            r.start_tick = u64::try_from(i128::from(n.start) - self.frame).map_err(|_| {
                TransformError::Invalid(format!(
                    "A result lies before the start of the clip of track {} it is written to; move that clip or choose another track.",
                    self.index + 1
                ))
            })?;
        }
        Ok(r)
    }

    /// Position of subchannel `id` in the table.
    pub fn sub_index(&self, id: u32) -> Option<usize> {
        self.subs.iter().position(|s| s.0 == id)
    }

    /// The subchannel `steps` table positions after `id` (wrapping, signed).
    pub fn sub_offset(&self, id: u32, steps: i64) -> (u32, u8) {
        if self.subs.is_empty() {
            return (id, 0);
        }
        let n = self.subs.len() as i64;
        let i = self.sub_index(id).unwrap_or(0) as i64;
        self.subs[(i + steps).rem_euclid(n) as usize]
    }

    /// MIDI channel of subchannel `id` (None when not in the table).
    pub fn sub_channel(&self, id: u32) -> Option<u8> {
        self.subs.iter().find(|s| s.0 == id).map(|s| s.1)
    }
}

/// One-pass statistics of the notes in scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceStats {
    pub count: usize,
    pub min_start: u64,
    pub max_start: u64,
    pub max_end: u64,
    pub min_pitch: u8,
    pub max_pitch: u8,
    pub max_dur: u64,
    /// Sum of all lengths (bounds for generators: steps over every note).
    pub sum_dur: u128,
    pub vel_hist: [u32; 128],
    /// Notes per pitch, 0–255 (v0.9 256-Key Mode; 1 KB per run, never per note).
    pub pitch_hist: [u32; 256],
    /// Smallest positive distance between consecutive starts in time order
    /// (`u64::MAX`: none; 1 when unknown, e.g. statistics from a scan). Bounds
    /// generators that step by the notes' own inter-onset intervals.
    pub min_gap: u64,
}

impl Default for SourceStats {
    fn default() -> Self {
        SourceStats {
            count: 0,
            min_start: u64::MAX,
            max_start: 0,
            max_end: 0,
            // Above every pitch until a note is added.
            min_pitch: u8::MAX,
            max_pitch: 0,
            max_dur: 0,
            sum_dur: 0,
            vel_hist: [0; 128],
            pitch_hist: [0; 256],
            min_gap: u64::MAX,
        }
    }
}

impl SourceStats {
    #[inline]
    pub fn add(&mut self, n: &TNote) {
        self.count += 1;
        self.min_start = self.min_start.min(n.start);
        self.max_start = self.max_start.max(n.start);
        self.max_end = self.max_end.max(n.end());
        self.min_pitch = self.min_pitch.min(n.pitch);
        self.max_pitch = self.max_pitch.max(n.pitch);
        self.max_dur = self.max_dur.max(n.dur);
        self.sum_dur += u128::from(n.dur);
        self.vel_hist[usize::from(n.vel.min(127))] += 1;
        self.pitch_hist[usize::from(n.pitch)] += 1;
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Time span (0 when empty).
    pub fn span(&self) -> u64 {
        if self.count == 0 {
            0
        } else {
            self.max_end.saturating_sub(self.min_start)
        }
    }

    /// Value at percentile `q` (0–1) of a 128-bin histogram.
    pub fn percentile(hist: &[u32; 128], q: f64) -> u8 {
        let total: u64 = hist.iter().map(|&c| u64::from(c)).sum();
        if total == 0 {
            return 0;
        }
        let target = ((q.clamp(0.0, 1.0) * (total - 1) as f64).round() as u64) + 1;
        let mut acc = 0u64;
        for (v, &c) in hist.iter().enumerate() {
            acc += u64::from(c);
            if acc >= target {
                return v as u8;
            }
        }
        127
    }

    /// Upper bound of `Σ (length / step + 1)` over every note.
    pub fn steps_bound(&self, step: u64) -> u64 {
        let s = self.sum_dur / u128::from(step.max(1)) + self.count as u128;
        u64::try_from(s).unwrap_or(u64::MAX)
    }

    /// Mean velocity.
    pub fn mean_velocity(&self) -> f64 {
        let total: u64 = self.vel_hist.iter().map(|&c| u64::from(c)).sum();
        if total == 0 {
            return 0.0;
        }
        let sum: u64 = self
            .vel_hist
            .iter()
            .enumerate()
            .map(|(v, &c)| v as u64 * u64::from(c))
            .sum();
        sum as f64 / total as f64
    }
}

/// The notes in scope (sorted by start, pitch, track, …) and their tracks.
#[derive(Clone, Debug, Default)]
pub struct Source {
    pub notes: Vec<TNote>,
    pub stats: SourceStats,
    pub tracks: Vec<SourceTrack>,
    /// Unselected notes of the scoped tracks near the selection (tools whose
    /// spec asks for context: legato / articulation "only with selection" off).
    /// Read-only: never replaced by a commit. Shared (`Arc`) so a part of
    /// the source (a Control Sweep segment) costs no copy of it.
    pub context: std::sync::Arc<Vec<TNote>>,
    /// Selected notes of locked groups (v0.7): never given to the kernel; a
    /// replacing commit keeps them unchanged.
    pub locked: Vec<TNote>,
    /// The project's note groups when the source was taken (new group ids that
    /// kernels emit are registered against it).
    pub groups: std::sync::Arc<crate::model::GroupTable>,
    /// Auxiliary note layers a tool asked for (Clipping Mask: layer 0 = the
    /// base track, layer 1 = extra mask tracks). Read-only; shared like
    /// [`Source::context`].
    pub aux: std::sync::Arc<Vec<Vec<TNote>>>,
    /// Slot of the auxiliary destination track in [`Source::tracks`], if any.
    pub aux_slot: Option<u16>,
    /// v0.9 V09-S39: controller points of the scoped tracks a tool's
    /// preparation read (Duplicate n08 copies them along), by slot and in
    /// time order. Read-only; shared like [`Source::context`].
    pub aux_events: std::sync::Arc<Vec<super::output::TEvent>>,
    /// v0.9 V09-S39: (source slot, destination slot) pairs a tool's
    /// preparation resolved (Duplicate n01 pastes onto them).
    pub routes: std::sync::Arc<Vec<(u16, u16)>>,
    /// The project's key range (v0.9 256-Key Mode): the boundary policies'
    /// pitch range.
    pub keys: crate::model::project::KeyRange,
    /// v1.0-beta2 B2-F16: the song tick of the notes' tick 0 when the source
    /// was collected with a run context ([`Ctx::shift`](super::Ctx::shift),
    /// else 0). A track the run creates (a new subtrack) starts at the song's
    /// tick 0, so its notes are placed in song time from it.
    pub shift: i128,
}

/// Most context notes collected.
pub const MAX_CONTEXT: usize = 1 << 20;

/// A track that only receives results (Clipping Mask to base track, …).
pub fn destination_track(doc: &PlumDocument, index: usize) -> Option<SourceTrack> {
    let mut t = source_track(
        doc,
        TrackScope {
            index,
            all: false,
            content: None,
        },
    )?;
    t.insert_only = true;
    Some(t)
}

fn source_track(doc: &PlumDocument, s: TrackScope) -> Option<SourceTrack> {
    let t = scope_view(doc, s)?;
    Some(SourceTrack {
        id: t.track.id(),
        index: s.index,
        content: s.content,
        revision: t.notes.revision(),
        subs: t
            .subchannels()
            .entries()
            .iter()
            .map(|e| (e.id.0, e.midi_channel))
            .collect(),
        all: s.all,
        insert_only: false,
        frame: 0,
    })
}

/// v1.0-beta2 B2-F16: the track the edited notes are shown on
/// ([`Ctx::host`](super::Ctx::host), else [`Ctx::active`](super::Ctx::active),
/// else the first scope's track) and the song tick of their tick 0
/// ([`Ctx::shift`](super::Ctx::shift); 0 without a context). "The track
/// below / above" of the cross-track tools counts from that track.
pub fn edit_place(scopes: &[TrackScope], ctx: Option<&super::Ctx>) -> Option<(usize, i128)> {
    let first = scopes.first()?.index;
    Some(match ctx {
        Some(c) => (c.host.or(c.active).unwrap_or(first), c.shift),
        None => (first, 0),
    })
}

/// The note host and song shift placement `p` shows (a track's own content
/// is that track's notes, an owned content is seen through the clip's
/// track).
fn placement_scope(
    doc: &PlumDocument,
    p: &crate::model::ClipPlacement,
) -> Option<(TrackScope, i128)> {
    let shift = i128::from(p.start) - i128::from(p.offset);
    let scope = match p.content {
        ContentRef::Track(t) => TrackScope {
            index: doc.index_of(t)?,
            all: false,
            content: None,
        },
        ContentRef::Owned(c) => TrackScope {
            index: doc.index_of(p.track)?,
            all: false,
            content: Some(c),
        },
    };
    Some((scope, shift))
}

/// v1.0-beta2 B2-F16: track `index` as a destination (results are added to
/// it), resolved the way the Piano Roll edits it without an opened clip
/// ([`crate::editor::clips::default_pick`]): its own notes, or the content
/// of the clip it plays — with that clip's place on the edited notes' ticks
/// (`shift` = [`Ctx::shift`](super::Ctx::shift)) as [`SourceTrack::frame`].
pub fn placed_destination(doc: &PlumDocument, index: usize, shift: i128) -> Option<SourceTrack> {
    let (scope, song) = match crate::editor::clips::default_pick(doc, index) {
        None => (
            TrackScope {
                index,
                all: false,
                content: None,
            },
            0,
        ),
        Some(p) => placement_scope(doc, &p)?,
    };
    let mut t = source_track(doc, scope)?;
    t.insert_only = true;
    t.frame = song - shift;
    Some(t)
}

/// Whether `r` belongs to a locked group of `doc`.
#[inline]
pub fn is_locked(doc: &PlumDocument, r: &NoteRecord) -> bool {
    r.group != 0 && doc.project().groups.locked(r.group)
}

/// Whether any group of `doc` is locked (fast path: no per-note checks).
pub fn any_locked(doc: &PlumDocument) -> bool {
    doc.project().groups.entries().iter().any(|g| g.locked)
}

/// v1.0-beta2 B2-A02: scope `k`'s frame in `frames` (0 when absent).
fn frame_of(frames: &[i128], k: usize) -> i128 {
    frames.get(k).copied().unwrap_or(0)
}

/// Record `r` of a host whose content tick 0 lies at `frame` on the edited
/// notes' ticks, on those ticks (`None`: before their tick 0).
fn framed(mut r: NoteRecord, frame: i128) -> Option<NoteRecord> {
    if frame != 0 {
        r.start_tick = u64::try_from(i128::from(r.start_tick) + frame).ok()?;
    }
    Some(r)
}

fn visit_scope<F: FnMut(NoteRecord)>(doc: &PlumDocument, s: TrackScope, mut f: F) {
    if let Some(t) = scope_view(doc, s) {
        if s.all {
            for v in t.notes.iter() {
                f(v.record());
            }
        } else {
            t.notes.for_each_selected(|v| f(v.record()));
        }
    }
}

/// v1.0-beta2 B2-F16: visits the notes track `index` shows that sound in
/// `a ..= b` on the edited notes' ticks (song tick − `shift`,
/// [`Ctx::shift`](super::Ctx::shift)) — every placement on it (muted ones
/// too), its content's notes starting in the placement's window — as records
/// on those ticks, placement by placement (callers that need time order
/// sort). With the clips at the identity and `shift` 0: the track's own
/// store, queried in place. Notes before the edited notes' tick 0 are left
/// out.
pub fn placed_query(
    doc: &PlumDocument,
    index: usize,
    shift: i128,
    a: u64,
    b: u64,
    f: &mut dyn FnMut(NoteRecord),
) {
    if shift == 0 && doc.clips_are_identity() {
        if let Some(t) = doc.track(index) {
            t.notes.query(a, b, |_, v| f(v.record()));
        }
        return;
    }
    for pc in doc.placed_contents(index) {
        query_placement(&pc, shift, a, b, f);
    }
}

/// [`placed_query`] of one placement.
fn query_placement(
    pc: &crate::model::PlacedContent<'_>,
    shift: i128,
    a: u64,
    b: u64,
    f: &mut dyn FnMut(NoteRecord),
) {
    let frame = pc.shift() - shift;
    let content = |t: u64| i128::from(t) - frame;
    let (ca, cb) = (content(a), content(b));
    if cb < 0 {
        return;
    }
    let ca = u64::try_from(ca.max(0)).unwrap_or(u64::MAX);
    let cb = u64::try_from(cb).unwrap_or(u64::MAX);
    let (lo, hi) = pc.window();
    pc.notes.query(ca, cb, |_, v| {
        if v.start_tick < lo || v.start_tick >= hi {
            return;
        }
        let Ok(start) = u64::try_from(i128::from(v.start_tick) + frame) else {
            return;
        };
        let mut r = v.record();
        r.start_tick = start;
        f(r);
    });
}

/// The first song tick a frame starting at song tick `shift` can express.
fn from_song(shift: i128) -> u64 {
    u64::try_from(shift.max(0)).unwrap_or(u64::MAX)
}

/// Record `r` (song ticks) as a note of `slot` on a frame starting at song
/// tick `shift`.
fn moved(r: &NoteRecord, slot: u16, shift: i128) -> TNote {
    let mut n = TNote::from_record(r, slot);
    n.start = u64::try_from(i128::from(r.start_tick) - shift).unwrap_or(u64::MAX);
    n
}

/// Canonical time-major order of source notes.
pub fn sort_time_major(v: &mut [TNote]) {
    v.sort_unstable_by(|a, b| {
        (
            a.start, a.pitch, a.track, a.sub, a.dur, a.vel, a.ch, a.rel, a.group, a.flags,
        )
            .cmp(&(
                b.start, b.pitch, b.track, b.sub, b.dur, b.vel, b.ch, b.rel, b.group, b.flags,
            ))
    });
}

impl Source {
    /// Tracks and statistics without copying any note (pointwise previews).
    pub fn scan(doc: &PlumDocument, scopes: &[TrackScope]) -> (Vec<SourceTrack>, SourceStats) {
        Self::scan_framed(doc, scopes, &[])
    }

    /// [`Source::scan`] with each scope on the edited notes' ticks by its
    /// frame ([`Ctx::scope_frames`](super::Ctx::scope_frames); notes before
    /// their tick 0 are not counted).
    pub fn scan_framed(
        doc: &PlumDocument,
        scopes: &[TrackScope],
        frames: &[i128],
    ) -> (Vec<SourceTrack>, SourceStats) {
        let mut tracks = Vec::new();
        let mut stats = SourceStats::default();
        for (k, &s) in scopes.iter().enumerate() {
            let Some(mut st) = source_track(doc, s) else {
                continue;
            };
            let frame = frame_of(frames, k);
            st.frame = frame;
            let slot = tracks.len() as u16;
            tracks.push(st);
            let lock = any_locked(doc);
            visit_scope(doc, s, |r| {
                if let Some(r) = framed(r, frame)
                    && !(lock && is_locked(doc, &r))
                {
                    stats.add(&TNote::from_record(&r, slot));
                }
            });
        }
        // Not measured here (the notes are not merged in time order).
        stats.min_gap = 1;
        (tracks, stats)
    }

    /// Copies the notes in scope (at most `limit`; more is refused before the
    /// copy is made).
    pub fn collect(
        doc: &PlumDocument,
        scopes: &[TrackScope],
        limit: usize,
    ) -> Result<Source, TransformError> {
        Self::collect_framed(doc, scopes, &[], limit)
    }

    /// [`Source::collect`] with each scope on the edited notes' ticks by its
    /// frame (v1.0-beta2 B2-A02, [`Ctx::scope_frames`](super::Ctx::scope_frames);
    /// [`SourceTrack::frame`] takes it, so a commit stores the results back
    /// in the host's ticks). A whole-track scope leaves out its notes before
    /// the edited notes' tick 0 (the commit keeps them as they are); a
    /// selected note there cannot take part and is refused with a sentence.
    pub fn collect_framed(
        doc: &PlumDocument,
        scopes: &[TrackScope],
        frames: &[i128],
        limit: usize,
    ) -> Result<Source, TransformError> {
        let mut total = 0usize;
        for &s in scopes {
            if let Some(t) = scope_view(doc, s) {
                total += if s.all {
                    t.notes.len()
                } else {
                    t.notes.selected_count()
                };
            }
        }
        if total > limit {
            return Err(TransformError::TooLarge(format!(
                "{} notes are in scope; this tool can process at most {} at once.",
                crate::midi::group_digits(total as u64),
                crate::midi::group_digits(limit as u64)
            )));
        }
        let mut notes = Vec::new();
        notes.try_reserve_exact(total).map_err(|_| {
            TransformError::TooLarge(format!(
                "Not enough memory to process {} notes.",
                crate::midi::group_digits(total as u64)
            ))
        })?;
        let mut tracks = Vec::new();
        let mut stats = SourceStats::default();
        let lock = any_locked(doc);
        let mut locked = Vec::new();
        for (k, &s) in scopes.iter().enumerate() {
            let Some(mut st) = source_track(doc, s) else {
                continue;
            };
            let frame = frame_of(frames, k);
            st.frame = frame;
            let index = st.index;
            let slot = tracks.len() as u16;
            tracks.push(st);
            let mut before = false;
            visit_scope(doc, s, |r| {
                let Some(r) = framed(r, frame) else {
                    before |= !s.all;
                    return;
                };
                let n = TNote::from_record(&r, slot);
                if lock && is_locked(doc, &r) {
                    locked.push(n);
                    return;
                }
                stats.add(&n);
                notes.push(n);
            });
            if before {
                return Err(TransformError::Invalid(format!(
                    "Track {} has selected notes before the start of the clip being edited; they cannot take part. Edit them from their own track, or select only notes from the clip's start on.",
                    index + 1
                )));
            }
        }
        sort_time_major(&mut notes);
        stats.min_gap = notes
            .windows(2)
            .filter_map(|w| w[1].start.checked_sub(w[0].start).filter(|&g| g > 0))
            .min()
            .unwrap_or(u64::MAX);
        Ok(Source {
            notes,
            stats,
            tracks,
            context: Default::default(),
            locked,
            groups: if doc.project().groups.is_empty() {
                Default::default()
            } else {
                std::sync::Arc::new(doc.project().groups.clone())
            },
            aux: Default::default(),
            aux_slot: None,
            aux_events: Default::default(),
            routes: Default::default(),
            keys: doc.project().keys,
            shift: 0,
        })
    }

    /// Adds the unselected notes of the scoped tracks starting between the
    /// selection's first start and its end plus its span (capped).
    pub fn with_context(self, doc: &PlumDocument) -> Source {
        if self.stats.is_empty() {
            return self;
        }
        let a = self.stats.min_start;
        let b = self.stats.max_end.saturating_add(self.stats.span());
        self.with_context_between(doc, a, b)
    }

    /// Adds the unselected notes of the scoped tracks starting in `a ..= b`
    /// (capped at [`MAX_CONTEXT`]), in time order.
    pub fn with_context_between(self, doc: &PlumDocument, a: u64, b: u64) -> Source {
        self.context_in(doc, a, b, false)
    }

    /// Adds the unselected notes of the scoped tracks sounding anywhere in
    /// `a ..= b` (also those that started before `a`; capped), in time
    /// order.
    pub fn with_context_sounding(self, doc: &PlumDocument, a: u64, b: u64) -> Source {
        self.context_in(doc, a, b, true)
    }

    fn context_in(mut self, doc: &PlumDocument, a: u64, b: u64, sounding: bool) -> Source {
        let mut ctx = Vec::new();
        for (slot, t) in self.tracks.iter().enumerate() {
            if t.all || t.insert_only {
                continue;
            }
            // B2-A02: the host's ticks of `a ..= b` (its frame).
            let (ca, cb) = (i128::from(a) - t.frame, i128::from(b) - t.frame);
            if cb < 0 {
                continue;
            }
            let ca = u64::try_from(ca.max(0)).unwrap_or(u64::MAX);
            let cb = u64::try_from(cb).unwrap_or(u64::MAX);
            if let Ok(tr) = crate::editor::note_view(doc, t.target()) {
                tr.notes.query(ca, cb, |_, v| {
                    if v.selected || ctx.len() >= MAX_CONTEXT {
                        return;
                    }
                    let Some(r) = framed(v.record(), t.frame) else {
                        return;
                    };
                    if sounding || r.start_tick >= a {
                        ctx.push(TNote::from_record(&r, slot as u16));
                    }
                });
            }
        }
        sort_time_major(&mut ctx);
        self.context = std::sync::Arc::new(ctx);
        self
    }

    /// Every note of track `index` as an auxiliary layer (`slot` = the track's
    /// slot when it is also a destination, else `u16::MAX`). Capped.
    pub fn track_layer(doc: &PlumDocument, index: usize, slot: u16, cap: usize) -> Vec<TNote> {
        let mut v = Vec::new();
        if let Some(t) = doc.track(index) {
            for n in t.notes.iter().take(cap) {
                v.push(TNote::from_record(&n.record(), slot));
            }
        }
        v
    }

    /// v1.0-beta2 B2-F16: the notes track `index` shows — every placement on
    /// it (muted ones too: what the track shows is what a mask or a map
    /// reads), its content's notes in the placement's window at their song
    /// ticks — on the edited notes' ticks (song tick − `shift`,
    /// [`Ctx::shift`](super::Ctx::shift)), by start; `slot` as in
    /// [`Source::track_layer`]. Notes before the edited notes' tick 0 cannot
    /// be expressed and are left out. Capped. With the clips at the identity
    /// and `shift` 0 this is [`Source::track_layer`]. Nothing but the result
    /// is copied.
    pub fn placed_layer(
        doc: &PlumDocument,
        index: usize,
        slot: u16,
        cap: usize,
        shift: i128,
    ) -> Vec<TNote> {
        if shift == 0 && doc.clips_are_identity() {
            return Self::track_layer(doc, index, slot, cap);
        }
        let lanes: Vec<PlacedLane> = doc
            .placed_contents(index)
            .iter()
            .map(PlacedLane::of)
            .collect();
        PlacedNotes::range(&lanes, from_song(shift), u64::MAX)
            .take(cap)
            .map(|r| moved(&r, slot, shift))
            .collect()
    }

    /// v1.0-beta2 B2-F16: [`Source::placed_layer`] of a track whose notes a
    /// tool may also write or erase (Clipping Mask's base, Displacement Map's
    /// map track): each note keeps its origin — one destination slot per
    /// placement (content host and place) on the track, added to
    /// [`Source::tracks`] (a slot of the same host at the same place is
    /// reused) — so a result or an erase of that note lands in its own clip.
    /// Refused when the track shows the notes being edited at another place
    /// (a linked copy of the edited clip): the roles would swap. In time
    /// order; capped.
    pub fn placed_layer_slots(
        &mut self,
        doc: &PlumDocument,
        index: usize,
        cap: usize,
        shift: i128,
    ) -> Result<Vec<TNote>, TransformError> {
        let mut out = Vec::new();
        for (pc, slot) in self.origins(doc, index, shift)? {
            let lane = [PlacedLane::of(&pc)];
            for r in PlacedNotes::range(&lane, from_song(shift), u64::MAX) {
                if out.len() >= cap {
                    break;
                }
                out.push(moved(&r, slot, shift));
            }
        }
        sort_time_major(&mut out);
        Ok(out)
    }

    /// v1.0-beta2 B2-F16: [`Source::placed_layer_slots`] limited to the notes
    /// sounding in `a ..= b` on the edited notes' ticks (the notes a brush
    /// can reach on another visible track). In time order; capped.
    pub fn placed_window_slots(
        &mut self,
        doc: &PlumDocument,
        index: usize,
        (a, b): (u64, u64),
        cap: usize,
        shift: i128,
    ) -> Result<Vec<TNote>, TransformError> {
        let mut out = Vec::new();
        for (pc, slot) in self.origins(doc, index, shift)? {
            query_placement(&pc, shift, a, b, &mut |r| {
                if out.len() < cap {
                    out.push(TNote::from_record(&r, slot));
                }
            });
        }
        sort_time_major(&mut out);
        Ok(out)
    }

    /// The placements on track `index` (each content at each place once)
    /// with the destination slot of each: a slot of the same host at the
    /// same place is reused, else one is added. Refused when the track shows
    /// the notes being edited at another place (the roles would swap).
    fn origins<'d>(
        &mut self,
        doc: &'d PlumDocument,
        index: usize,
        shift: i128,
    ) -> Result<Vec<(crate::model::PlacedContent<'d>, u16)>, TransformError> {
        let mut out = Vec::new();
        let mut seen: Vec<(ContentRef, i128, (u64, u64))> = Vec::new();
        for pc in doc.placed_contents(index) {
            let key = (pc.placement.content, pc.shift(), pc.window());
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            let Some((scope, song)) = placement_scope(doc, &pc.placement) else {
                continue;
            };
            let Some(mut dest) = source_track(doc, scope) else {
                continue;
            };
            dest.insert_only = true;
            dest.frame = song - shift;
            let same = |t: &SourceTrack| t.host() == dest.host();
            let slot = if let Some(s) = self
                .tracks
                .iter()
                .position(|t| same(t) && t.frame == dest.frame)
            {
                s
            } else if self.tracks.iter().any(|t| same(t) && !t.insert_only) {
                return Err(TransformError::Invalid(format!(
                    "Track {} shows the notes being edited (a linked copy of this clip) at another place; choose another track.",
                    index + 1
                )));
            } else {
                self.tracks.push(dest);
                self.tracks.len() - 1
            };
            let slot = u16::try_from(slot)
                .map_err(|_| TransformError::Invalid("Too many tracks take part.".into()))?;
            out.push((pc, slot));
        }
        Ok(out)
    }

    /// A source from ready-made notes (tests, scores, generators).
    pub fn from_notes(mut notes: Vec<TNote>, tracks: Vec<SourceTrack>) -> Source {
        let mut stats = SourceStats::default();
        for n in &notes {
            stats.add(n);
        }
        sort_time_major(&mut notes);
        Source {
            notes,
            stats,
            tracks,
            context: Default::default(),
            locked: Vec::new(),
            groups: Default::default(),
            aux: Default::default(),
            aux_slot: None,
            aux_events: Default::default(),
            routes: Default::default(),
            keys: Default::default(),
            shift: 0,
        }
    }

    pub fn track(&self, slot: u16) -> Option<&SourceTrack> {
        self.tracks.get(usize::from(slot))
    }

    /// Indices of the notes of one track slot, in time order.
    pub fn indices_of_track(&self, slot: u16) -> Vec<usize> {
        self.notes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.track == slot)
            .map(|(i, _)| i)
            .collect()
    }

    /// Groups of notes that start together (within `tolerance` ticks) per track:
    /// `(first index, count)` ranges over `notes` — chords for strum / arpeggiate.
    pub fn onset_groups(notes: &[TNote], tolerance: u64) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < notes.len() {
            let s = notes[i].start;
            let mut j = i + 1;
            while j < notes.len() && notes[j].start.saturating_sub(s) <= tolerance {
                j += 1;
            }
            out.push((i, j - i));
            i = j;
        }
        out
    }
}
