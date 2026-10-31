//! Live keyboard colours (v0.6): which piano keys are sounding at the playhead,
//! in the colours of the notes on them.
//!
//! State is kept for the 256 possible pitches only (v0.9: 0–255, so a
//! 256-Key Mode project or a key correction above 127 never indexes past the
//! table): per pitch, how many sounding notes
//! there are of each *visual identity* — (track, subchannel), v0.8 §124 –
//! §126 — with their velocities summed. A key's colours come from the
//! canonical note-colour resolver (`theme::NoteTint`, the same as the Piano
//! Roll): the active track in its subchannel colours, every other track in
//! its Track colour turned to each subchannel's hue, velocity as saturation
//! (the group's mean velocity). So Track A / Subchannel 1 and Track B /
//! Subchannel 1 on one key stay two colours. Sequential playback updates it from note
//! boundaries — notes starting since the last frame are added, notes whose end
//! has passed are removed (a min-heap of end ticks) — so a frame costs only the
//! notes that started or ended. A seek, a loop wrap, a large jump or any
//! document change rebuilds the state from the notes sounding at the new tick
//! (an exact interval query, never a replay from the start).
//!
//! What a key shows is bounded (§127): up to [`MAX_BANDS`] colour bands — the
//! identities with the most notes (ties by track, then subchannel, so the
//! order never flickers), the rest blended into one band weighted by note
//! count in linear light.

use crate::app::progressor::ScopeSetting;
use crate::ui::theme::{NoteColors, NoteTint};
use egui::Color32;
use plumspirit_engine::analysis::{
    AnalysisIndex, for_each_sounding_placed, for_each_starting_placed, next_start_bound_placed,
};
use plumspirit_engine::model::correction::Correction;
use plumspirit_engine::model::{NoteRecord, PlumDocument, TrackId};
use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap, HashMap};

/// Colour bands drawn on one key at most.
pub const MAX_BANDS: usize = 4;
/// Note starts processed incrementally per update at most; beyond that a jump
/// rebuilds from the sounding notes instead.
const MAX_INCREMENTAL: u64 = 50_000;
/// L-S45: notes one [`RollKeys::update`] reads to complete key masks (a few
/// ms; measured 700 tracks × 100,000 notes: first update 212 ms reading
/// every mask, 11 ms with 2M a update; a 224M-note project's masks complete
/// over ~225 updates).
const MASK_NOTES_PER_UPDATE: usize = 1_000_000;

/// What one key shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyDisplay {
    /// Notes sounding on the key.
    pub total: u32,
    pub bands: [Color32; MAX_BANDS],
    pub n: u8,
}

/// Sounding notes of one identity on one key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Group {
    n: u32,
    velocity_sum: u32,
}

/// Visual identity of a note: (track index in the document, subchannel).
fn ident(track: usize, sub: u32) -> u64 {
    (track as u64) << 32 | u64::from(sub)
}

#[derive(Default)]
struct Pitch {
    groups: HashMap<u64, Group>,
    total: u32,
    dirty: bool,
}

/// Inputs that, when changed, invalidate the accumulated state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeysKey {
    pub notes: crate::app::analysis::NotesKey,
    pub dark: bool,
    pub tracks: Option<BTreeSet<TrackId>>,
    /// The track the Piano Roll draws in subchannel colours (v0.8: keys use
    /// the roll's colours); `None`: every track in its Track colour.
    pub active: Option<TrackId>,
    /// The note palette (v0.9 §068): keys use the roll's colours.
    pub palette: std::sync::Arc<crate::ui::theme::NotePalette>,
    /// v0.9 (V09-S17): the MIDI Player's keyboard — every track in its
    /// Player identity colours (`NoteTint::Played`, track × subchannel)
    /// instead of the roll's.
    pub played: bool,
}

/// Keys the tracker covers: every pitch a note word can hold (0–255).
pub const KEYS: usize = 256;

pub struct KeyboardTracker {
    pitches: Vec<Pitch>,
    display: [KeyDisplay; KEYS],
    /// (end tick, pitch, identity, velocity) of every sounding note.
    ends: BinaryHeap<Reverse<(u64, u8, u64, u8)>>,
    /// Per track (document order): no note starts before this tick after the
    /// position (`u64::MAX`: none / out of scope). Lets playback skip the
    /// tracks with nothing new this frame.
    next: Vec<u64>,
    at: Option<u64>,
    key: Option<KeysKey>,
    /// Note tint of every track (document order), from the last rebuild.
    tints: Vec<NoteTint>,
    colors: NoteColors,
    /// Rebuilds since start (tests, diagnostics).
    pub rebuilds: u64,
}

impl Default for KeyboardTracker {
    fn default() -> Self {
        KeyboardTracker {
            pitches: (0..KEYS).map(|_| Pitch::default()).collect(),
            display: [KeyDisplay::default(); KEYS],
            ends: BinaryHeap::new(),
            next: Vec::new(),
            at: None,
            key: None,
            tints: Vec::new(),
            colors: NoteColors::default(),
            rebuilds: 0,
        }
    }
}

/// L-G02: the tracks the Piano Roll shows — `active` (`None`: the
/// Conductor, which shows no track's notes) and its subtracks, plus the
/// Onion ghosts `ghosts` (track indices; empty while the Onion is off).
pub fn shown_tracks(
    doc: &PlumDocument,
    active: Option<usize>,
    ghosts: &[usize],
) -> BTreeSet<TrackId> {
    let own = active
        .into_iter()
        .flat_map(|a| a..doc.subtree_end(a).max(a + 1));
    own.chain(ghosts.iter().copied())
        .filter_map(|i| doc.track(i).map(|t| t.id()))
        .collect()
}

/// Tracks a scope covers (`None`: all).
pub fn scope_tracks(
    doc: &PlumDocument,
    scope: ScopeSetting,
    active: usize,
) -> Option<BTreeSet<TrackId>> {
    match scope {
        ScopeSetting::Project => None,
        ScopeSetting::Audible => Some(
            doc.tracks()
                .iter()
                .zip(plumspirit_engine::playback::eligibility(doc))
                .filter(|(_, e)| *e)
                .map(|(t, _)| t.id())
                .collect(),
        ),
        ScopeSetting::ActiveTrack => Some(
            (active..doc.subtree_end(active).max(active + 1))
                .filter_map(|i| doc.track(i).map(|t| t.id()))
                .collect(),
        ),
    }
}

impl KeyboardTracker {
    /// Forgets the state (playback stopped, feature off).
    pub fn clear(&mut self) {
        if self.at.is_none() && self.ends.is_empty() {
            return;
        }
        for p in &mut self.pitches {
            p.groups.clear();
            p.total = 0;
            p.dirty = false;
        }
        self.display = [KeyDisplay::default(); KEYS];
        self.ends.clear();
        self.next.clear();
        self.at = None;
        self.key = None;
    }

    /// What every key shows (index = pitch 0–255).
    pub fn display(&self) -> &[KeyDisplay; KEYS] {
        &self.display
    }

    /// Active notes on `pitch` (tests).
    #[cfg(test)]
    pub fn count(&self, pitch: u8) -> u32 {
        self.pitches[usize::from(pitch)].total
    }

    /// Brings the state to `tick`.
    pub fn update(
        &mut self,
        doc: &PlumDocument,
        corrections: &[Option<Correction>],
        index: Option<&AnalysisIndex>,
        tick: u64,
        key: KeysKey,
    ) {
        let same = self.key.as_ref() == Some(&key);
        match self.at {
            Some(at) if same && at == tick => return,
            Some(at) if same && tick > at && small_step(index, &key, at, tick) => {
                self.advance(doc, corrections, at, tick);
            }
            _ => self.rebuild(doc, corrections, tick, &key),
        }
        self.at = Some(tick);
        self.key = Some(key);
        self.refresh_display();
    }

    fn add(&mut self, n: &NoteRecord, track: usize) {
        let id = ident(track, n.sub.0);
        let vel = n.attrs.velocity();
        let p = &mut self.pitches[usize::from(n.attrs.pitch())];
        let g = p.groups.entry(id).or_default();
        g.n += 1;
        g.velocity_sum += u32::from(vel);
        p.total += 1;
        p.dirty = true;
        self.ends.push(Reverse((
            n.start_tick + n.duration_ticks,
            n.attrs.pitch(),
            id,
            vel,
        )));
    }

    fn remove(&mut self, pitch: u8, id: u64, vel: u8) {
        let p = &mut self.pitches[usize::from(pitch)];
        if let Some(g) = p.groups.get_mut(&id) {
            g.n -= 1;
            g.velocity_sum -= u32::from(vel);
            if g.n == 0 {
                p.groups.remove(&id);
            }
            p.total -= 1;
            p.dirty = true;
        }
    }

    fn rebuild(
        &mut self,
        doc: &PlumDocument,
        corr: &[Option<Correction>],
        tick: u64,
        key: &KeysKey,
    ) {
        self.rebuilds += 1;
        for p in &mut self.pitches {
            p.groups.clear();
            p.total = 0;
            p.dirty = true;
        }
        self.ends.clear();
        self.next.clear();
        self.tints = if key.played {
            crate::player::track_bases(doc)
                .into_iter()
                .map(|(base, count)| NoteTint::Played { base, count })
                .collect()
        } else {
            doc.tracks()
                .iter()
                .map(|t| {
                    key.palette.track_tint(
                        t.id().0,
                        t.props().onion_color,
                        key.active == Some(t.id()),
                        key.dark,
                    )
                })
                .collect()
        };
        let mut notes = Vec::new();
        for (i, t) in doc.tracks().iter().enumerate() {
            if key.tracks.as_ref().is_some_and(|s| !s.contains(&t.id())) {
                self.next.push(u64::MAX);
                continue;
            }
            let c = corr.get(i).and_then(Option::as_ref);
            // v0.9: what the track's clips play (its own notes without clips).
            for_each_sounding_placed(doc, i, c, tick, |n| notes.push((n, i)));
            self.next
                .push(next_start_bound_placed(doc, i, c, tick).unwrap_or(u64::MAX));
        }
        for (n, i) in &notes {
            self.add(n, *i);
        }
    }

    fn advance(&mut self, doc: &PlumDocument, corr: &[Option<Correction>], from: u64, to: u64) {
        // Notes that ended by `to` (half-open: a note ending at `to` is off).
        while let Some(Reverse((end, pitch, id, vel))) = self.ends.peek().copied() {
            if end > to {
                break;
            }
            self.ends.pop();
            self.remove(pitch, id, vel);
        }
        let mut started = Vec::new();
        let tracks = doc.tracks();
        for (i, next) in self.next.iter_mut().enumerate() {
            // Out of scope, or nothing starts by `to` (the common case).
            if *next > to {
                continue;
            }
            if tracks.get(i).is_none() {
                continue;
            }
            let c = corr.get(i).and_then(Option::as_ref);
            for_each_starting_placed(doc, i, c, Some(from), to, |n| {
                if n.start_tick + n.duration_ticks > to {
                    started.push((n, i));
                }
            });
            *next = next_start_bound_placed(doc, i, c, to).unwrap_or(u64::MAX);
        }
        for (n, i) in &started {
            self.add(n, *i);
        }
    }

    fn refresh_display(&mut self) {
        let dark = self.key.as_ref().is_some_and(|k| k.dark);
        let palette = self
            .key
            .as_ref()
            .map(|k| k.palette.clone())
            .unwrap_or_default();
        let mut v = Vec::new();
        for (i, p) in self.pitches.iter_mut().enumerate() {
            if !p.dirty {
                continue;
            }
            p.dirty = false;
            v.clear();
            for (&id, g) in &p.groups {
                let track = (id >> 32) as usize;
                let sub = id as u32;
                let tint = self
                    .tints
                    .get(track)
                    .copied()
                    .unwrap_or(NoteTint::Subchannels);
                let vel = ((g.velocity_sum + g.n / 2) / g.n.max(1)).min(127) as u8;
                let c = self.colors.get(&palette, tint, sub, vel, dark);
                v.push((id, u32::from_be_bytes(c.to_array()), g.n));
            }
            self.display[i] = summarize(&mut v, p.total);
        }
    }
}

/// v1.0-beta1 B1-0110 (contract §016): the Piano Roll keyboard shows one
/// colour per key — the note the roll shows in front at the playhead. Layers
/// as the roll draws them: the active track over the Onion ghosts in their
/// drawing order, then the other tracks of the keyboard's scope in track
/// order (the later track in front); within one track the latest-starting
/// sounding note (the note drawn last), then the later clip, then the later
/// stored note — `tiles::foremost_keys`. Notes sound as playback plays them
/// (clips, corrections, muted notes silent), as the counts of
/// [`KeyboardTracker`] (kept for the hover text). The selection never
/// changes a key's colour. O(a few notes per key) per frame with the
/// tracks' key masks (cached per content revision).
pub struct RollKeys {
    masks: HashMap<(usize, u64), [u64; 4]>,
    /// L-S45: masks still being read — (pitches so far, next chunk). A store
    /// without its complete mask is read with every key (a superset gives
    /// the same keys, only without the early stop).
    partial: HashMap<(usize, u64), ([u64; 4], usize)>,
    /// Notes the next [`RollKeys::update`] may still read for masks.
    mask_budget: usize,
    colors: NoteColors,
    out: Vec<Option<(usize, plumspirit_engine::model::NoteView)>>,
    display: Box<[KeyDisplay; KEYS]>,
}

impl Default for RollKeys {
    fn default() -> Self {
        RollKeys {
            masks: HashMap::new(),
            partial: HashMap::new(),
            mask_budget: MASK_NOTES_PER_UPDATE,
            colors: NoteColors::default(),
            out: Vec::new(),
            display: Box::new([KeyDisplay::default(); KEYS]),
        }
    }
}

impl RollKeys {
    /// The keys at `tick`: `scope` (`None`: every track), the edited track
    /// `active` (`None`: the Conductor), the Onion layers `onion` back to
    /// front (track ids, repeated for clips), `totals` the tracker's counts.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        doc: &PlumDocument,
        corrections: &[Option<Correction>],
        scope: Option<&BTreeSet<TrackId>>,
        active: Option<usize>,
        onion: &[TrackId],
        tick: u64,
        totals: &[KeyDisplay; KEYS],
        palette: &crate::ui::theme::NotePalette,
        dark: bool,
    ) -> &[KeyDisplay; KEYS] {
        use plumspirit_engine::model::NoteStore;
        use plumspirit_engine::view::tiles::{self, Reading, SourcePlace};
        let front: HashMap<TrackId, u32> = onion
            .iter()
            .enumerate()
            .map(|(p, &id)| (id, p as u32))
            .collect();
        let identity = doc.clips_are_identity();
        let mut stores: Vec<&NoteStore> = Vec::new();
        let mut places = Vec::new();
        let mut readings = Vec::new();
        let mut ranks = Vec::new();
        let mut tracks = Vec::new();
        for (i, t) in doc.tracks().iter().enumerate() {
            if scope.is_some_and(|s| !s.contains(&t.id())) {
                continue;
            }
            let rank = if active == Some(i) {
                2_000_000
            } else if let Some(p) = front.get(&t.id()) {
                1_000_000 + p
            } else {
                i as u32
            };
            let reading = Reading::Played(corrections.get(i).copied().flatten());
            let sources: Vec<(&NoteStore, SourcePlace)> = if identity {
                vec![(&t.notes, SourcePlace::IDENTITY)]
            } else {
                doc.placed_contents(i)
                    .into_iter()
                    .filter(|pc| !pc.placement.muted)
                    .map(|pc| (pc.notes, SourcePlace::of(&pc.placement)))
                    .collect()
            };
            for (st, pl) in sources {
                stores.push(st);
                places.push(pl);
                readings.push(reading);
                ranks.push(rank);
                tracks.push(i);
            }
        }
        if self.masks.len() > 4096 {
            self.masks.clear();
        }
        if self.partial.len() > 4096 {
            self.partial.clear();
        }
        // L-S45 (E0075): a store's key mask costs a pass over its notes.
        // Switching the Onion to All Other Tracks of a 224M-note project
        // asked for 699 of them in one frame (373 ms on Windows); now each
        // update reads at most `mask_budget` notes and the stores not read
        // yet use every key until their mask is complete.
        let mut budget = std::mem::replace(&mut self.mask_budget, MASK_NOTES_PER_UPDATE);
        let masks: Vec<[u64; 4]> = stores
            .iter()
            .zip(&readings)
            .map(|(s, r)| match r {
                Reading::Played(Some(_)) => [u64::MAX; 4],
                _ => {
                    let key = (*s as *const NoteStore as usize, s.content_revision());
                    if let Some(m) = self.masks.get(&key) {
                        return *m;
                    }
                    if budget == 0 {
                        return [u64::MAX; 4];
                    }
                    let (mut m, from) = self.partial.remove(&key).unwrap_or(([0; 4], 0));
                    let (next, read) = s.pitch_mask_part(&mut m, from, budget);
                    budget = budget.saturating_sub(read.max(1));
                    if next >= s.chunk_count() {
                        self.masks.insert(key, m);
                        m
                    } else {
                        self.partial.insert(key, (m, next));
                        [u64::MAX; 4]
                    }
                }
            })
            .collect();
        tiles::foremost_keys(
            &stores,
            &places,
            &readings,
            &ranks,
            &masks,
            tick,
            KEYS,
            &mut self.out,
        );
        for (k, lit) in self.out.iter().enumerate() {
            let total = totals[k].total;
            self.display[k] = match lit {
                Some((si, v)) => {
                    let t = &doc.tracks()[tracks[*si]];
                    let tint = palette.track_tint(
                        t.id().0,
                        t.props().onion_color,
                        active == Some(tracks[*si]),
                        dark,
                    );
                    let c = self
                        .colors
                        .get(palette, tint, v.sub.0, v.attrs.velocity(), dark);
                    let mut d = KeyDisplay {
                        total: total.max(1),
                        n: 1,
                        ..KeyDisplay::default()
                    };
                    d.bands[0] = c;
                    d
                }
                None => KeyDisplay {
                    total,
                    ..KeyDisplay::default()
                },
            };
        }
        &self.display
    }
}

/// Whether moving forward from `at` to `tick` is cheaper incrementally.
fn small_step(index: Option<&AnalysisIndex>, key: &KeysKey, at: u64, tick: u64) -> bool {
    match index {
        Some(idx) => {
            let scope = match &key.tracks {
                None => plumspirit_engine::analysis::Scope::Project,
                Some(s) => plumspirit_engine::analysis::Scope::Tracks(s.clone()),
            };
            idx.starts_in(&scope, Some(at), tick).0 <= MAX_INCREMENTAL
        }
        // Without an index, only short steps (about a frame at fast tempos).
        None => tick - at <= 4_000,
    }
}

fn lin(c: u8) -> f32 {
    let x = f32::from(c) / 255.0;
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

fn enc(x: f32) -> u8 {
    let x = x.clamp(0.0, 1.0);
    let s = if x <= 0.003_130_8 {
        x * 12.92
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}

/// Bounded display of one key (§127) from its identities `(identity, colour,
/// notes)`: the identities with the most notes first (ties by identity —
/// track, then subchannel — so the order never flickers), the rest blended.
pub fn summarize(v: &mut [(u64, u32, u32)], total: u32) -> KeyDisplay {
    let mut d = KeyDisplay {
        total,
        ..KeyDisplay::default()
    };
    if total == 0 || v.is_empty() {
        return d;
    }
    v.sort_unstable_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    let rgb = |c: u32| {
        let [r, g, b, _] = c.to_be_bytes();
        Color32::from_rgb(r, g, b)
    };
    if v.len() <= MAX_BANDS {
        for (i, (_, c, _)) in v.iter().enumerate() {
            d.bands[i] = rgb(*c);
        }
        d.n = v.len() as u8;
        return d;
    }
    for (band, (_, c, _)) in d.bands.iter_mut().zip(&v[..MAX_BANDS - 1]) {
        *band = rgb(*c);
    }
    // Remainder: count-weighted average in linear light (no muddy darkening).
    let (mut r, mut g, mut b, mut w) = (0f32, 0f32, 0f32, 0f32);
    for (_, c, n) in &v[MAX_BANDS - 1..] {
        let [cr, cg, cb, _] = c.to_be_bytes();
        let n = *n as f32;
        r += lin(cr) * n;
        g += lin(cg) * n;
        b += lin(cb) * n;
        w += n;
    }
    d.bands[MAX_BANDS - 1] = Color32::from_rgb(enc(r / w), enc(g / w), enc(b / w));
    d.n = MAX_BANDS as u8;
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use plumspirit_engine::analysis::document_corrections;
    use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, SubchannelId};

    fn rec(start: u64, dur: u64, pitch: u8, vel: u8, sub: u32) -> NoteRecord {
        NoteRecord {
            start_tick: start,
            duration_ticks: dur,
            attrs: NoteAttrs::new(pitch, vel, 0, 0, NoteOffKind::NoteOff).unwrap(),
            sub: SubchannelId(sub),
            group: 0,
        }
    }

    fn doc(tracks: &[Vec<NoteRecord>]) -> PlumDocument {
        let mut d = PlumDocument::new_project("k".into(), Bpm::DEFAULT, 960);
        for (i, v) in tracks.iter().enumerate() {
            if i > 0 {
                d.add_track().unwrap();
            }
            let mut v = v.clone();
            v.sort_by_key(|n| n.start_tick);
            d.track_mut(i)
                .unwrap()
                .notes
                .insert_sorted_batch(&v, false)
                .unwrap();
        }
        d
    }

    fn key(d: &PlumDocument) -> KeysKey {
        KeysKey {
            notes: crate::app::analysis::NotesKey::of(d, 1),
            dark: true,
            tracks: None,
            active: None,
            palette: Default::default(),
            played: false,
        }
    }

    #[test]
    fn keys_follow_note_boundaries_and_rebuild_on_seek() {
        let d = doc(&[
            vec![rec(0, 100, 60, 100, 0), rec(50, 100, 62, 1, 3)],
            vec![rec(10, 500, 60, 127, 2)],
        ]);
        let corr = document_corrections(&d);
        let mut k = KeyboardTracker::default();
        k.update(&d, &corr, None, 0, key(&d));
        assert_eq!((k.count(60), k.count(62)), (1, 0));
        k.update(&d, &corr, None, 60, key(&d));
        assert_eq!(
            (k.count(60), k.count(62)),
            (2, 1),
            "track 2's note lights C4 too"
        );
        assert_eq!(k.display()[60].n, 2, "two colors on one key");
        k.update(&d, &corr, None, 100, key(&d));
        assert_eq!(k.count(60), 1, "a note ending at 100 is off at 100");
        let rebuilds = k.rebuilds;
        k.update(&d, &corr, None, 30, key(&d)); // backwards: rebuild
        assert_eq!(k.rebuilds, rebuilds + 1);
        assert_eq!((k.count(60), k.count(62)), (2, 0));
        k.update(&d, &corr, None, 1000, key(&d));
        assert_eq!(k.display().iter().map(|d| d.total).sum::<u32>(), 0);
    }

    #[test]
    fn identical_pitches_count_and_many_colours_stay_bounded() {
        let mut v = Vec::new();
        for i in 0..1000u32 {
            v.push(rec(0, 100, 72, (i % 127 + 1) as u8, i % 50));
        }
        let d = doc(&[v]);
        let corr = document_corrections(&d);
        let mut k = KeyboardTracker::default();
        k.update(&d, &corr, None, 10, key(&d));
        assert_eq!(k.count(72), 1000);
        let disp = k.display()[72];
        assert_eq!(disp.total, 1000);
        assert_eq!(
            usize::from(disp.n),
            MAX_BANDS,
            "bounded however many colors"
        );
        // Deterministic: the same state gives the same bands, idle frames
        // change nothing.
        let rebuilds = k.rebuilds;
        for _ in 0..50 {
            k.update(&d, &corr, None, 10, key(&d));
            assert_eq!(k.display()[72], disp);
        }
        assert_eq!(k.rebuilds, rebuilds);
    }

    #[test]
    fn incremental_updates_equal_a_fresh_rebuild() {
        let mut s = 99u64;
        let mut r = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let mut tracks = Vec::new();
        for _ in 0..3 {
            tracks.push(
                (0..3000)
                    .map(|_| {
                        rec(
                            r() % 20_000,
                            r() % 900,
                            (r() % 40 + 40) as u8,
                            (r() % 127 + 1) as u8,
                            (r() % 5) as u32,
                        )
                    })
                    .collect::<Vec<_>>(),
            );
        }
        // Sparse tracks too (most frames skip them).
        for k in 0..5u64 {
            tracks.push(
                (0..40)
                    .map(|i| rec(i * 500 + k * 37, 120, 60, 90, 0))
                    .collect(),
            );
        }
        let mut d = doc(&tracks);
        // Corrections: shift back from measure 2 with a gate, shift on.
        use plumspirit_engine::history::{History, Operation};
        use plumspirit_engine::model::track::{GatePercent, TrackProps};
        let mut h = History::default();
        for (ti, props) in [
            (
                1usize,
                TrackProps {
                    tick_correction: -500,
                    correction_from_measure: 2,
                    gate: GatePercent::parse("150").unwrap(),
                    ..TrackProps::default()
                },
            ),
            (
                4,
                TrackProps {
                    tick_correction: 300,
                    key_range: (0, 100),
                    ..TrackProps::default()
                },
            ),
        ] {
            let id = d.track(ti).unwrap().id();
            h.apply(
                &mut d,
                Operation::TrackProps {
                    track: id,
                    props,
                    end: None,
                    label: "p",
                },
            )
            .unwrap();
        }
        let corr = document_corrections(&d);
        assert!(corr[1].is_some() && corr[4].is_some());
        let mut inc = KeyboardTracker::default();
        let mut t = 0;
        while t < 21_000 {
            inc.update(&d, &corr, None, t, key(&d));
            let mut fresh = KeyboardTracker::default();
            fresh.update(&d, &corr, None, t, key(&d));
            assert_eq!(inc.display(), fresh.display(), "at {t}");
            t += 7 + r() % 300;
        }
        assert!(
            inc.rebuilds <= 2,
            "sequential play is incremental ({})",
            inc.rebuilds
        );
    }

    /// §227: Track A / Subchannel 0, Track B / Subchannel 0 and Track C /
    /// Subchannel 3 all sound C5: the key keeps three source colours (the
    /// same colours the Piano Roll gives those notes), in a deterministic
    /// order, unchanged over idle frames.
    #[test]
    fn keyboard_multicolour_keeps_tracks_with_the_same_subchannel_apart() {
        let c5 = 72u8;
        let d = doc(&[
            vec![rec(0, 1_000, c5, 100, 0)],
            vec![rec(0, 1_000, c5, 100, 0)],
            vec![rec(0, 1_000, c5, 100, 3)],
        ]);
        let corr = document_corrections(&d);
        let ids: Vec<TrackId> = d.tracks().iter().map(|t| t.id()).collect();
        let mut nc = NoteColors::default();
        for active in [Some(ids[0]), Some(ids[1]), None] {
            let k_of = |d: &PlumDocument| KeysKey { active, ..key(d) };
            let mut k = KeyboardTracker::default();
            k.update(&d, &corr, None, 10, k_of(&d));
            let disp = k.display()[usize::from(c5)];
            assert_eq!(disp.total, 3);
            assert_eq!(disp.n, 3, "three sources, three bands ({active:?})");
            let bands = &disp.bands[..3];
            assert!(
                bands[0] != bands[1] && bands[1] != bands[2] && bands[0] != bands[2],
                "{bands:?}"
            );
            // Equal counts: track order (A, B, C), each in the roll's colour.
            for (i, (t, sub)) in [(0usize, 0u32), (1, 0), (2, 3)].into_iter().enumerate() {
                let tr = d.track(t).unwrap();
                let tint = NoteTint::of(tr.id().0, None, active == Some(tr.id()), true);
                assert_eq!(
                    bands[i],
                    nc.get(&Default::default(), tint, sub, 100, true),
                    "band {i} ({active:?})"
                );
            }
            // Idle frames: no flicker, no rebuild.
            let rebuilds = k.rebuilds;
            for _ in 0..200 {
                k.update(&d, &corr, None, 10, k_of(&d));
                assert_eq!(k.display()[usize::from(c5)], disp);
            }
            assert_eq!(k.rebuilds, rebuilds);
        }
        // Many tracks on one key: bounded, deterministic; the busiest leads.
        let mut tracks: Vec<Vec<NoteRecord>> =
            (0..40).map(|_| vec![rec(0, 500, c5, 90, 0)]).collect();
        tracks[17].extend((0..5).map(|_| rec(0, 500, c5, 90, 0)));
        let d = doc(&tracks);
        let corr = document_corrections(&d);
        let mut a = KeyboardTracker::default();
        a.update(&d, &corr, None, 1, key(&d));
        let disp = a.display()[usize::from(c5)];
        assert_eq!(usize::from(disp.n), MAX_BANDS);
        let lead = NoteTint::of(d.track(17).unwrap().id().0, None, false, true);
        assert_eq!(
            disp.bands[0],
            nc.get(&Default::default(), lead, 0, 90, true)
        );
        let mut b = KeyboardTracker::default();
        b.update(&d, &corr, None, 1, key(&d));
        assert_eq!(b.display()[usize::from(c5)], disp);
    }

    #[test]
    fn velocity_one_stays_coloured_and_blends_are_not_muddy() {
        let mut g = Vec::new();
        let red = u32::from_be_bytes([230, 30, 30, 255]);
        let green = u32::from_be_bytes([30, 200, 60, 255]);
        let blue = u32::from_be_bytes([40, 60, 230, 255]);
        let pale = u32::from_be_bytes([180, 170, 200, 255]);
        let yellow = u32::from_be_bytes([230, 220, 40, 255]);
        for (i, (c, n)) in [(red, 5), (green, 4), (blue, 3), (pale, 2), (yellow, 2)]
            .into_iter()
            .enumerate()
        {
            g.push((i as u64, c, n));
        }
        let d = summarize(&mut g, 16);
        assert_eq!(d.n as usize, MAX_BANDS);
        let rest = d.bands[3];
        let lum = |c: Color32| 0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b());
        assert!(lum(rest) > 0.2, "the blend is not darker than its parts");
    }

    /// Per-frame cost of the live features on a 1M-note project: the
    /// Progressor query (+ text) and the keyboard tracker, while playing
    /// (60 fps steps of song time) and on random seeks. Also run with the same
    /// notes spread over 512 tracks.
    #[test]
    #[ignore = "measurement: run with --release --ignored --nocapture"]
    fn live_frame_cost() {
        use crate::app::analysis::{AnalysisService, NotesKey, Peaks};
        use crate::app::progressor::{ProgressorSettings, Readout, Template, render_text};
        use plumspirit_engine::analysis::{DEFAULT_WINDOW_NS, Scope, Timing};
        use plumspirit_engine::fixtures::{FixtureShape, generate};
        use std::time::{Duration, Instant};
        let dense = generate(FixtureShape::Dense, 1_000_000, 960, 42);
        let mut spread = PlumDocument::new_project("s".into(), Bpm::DEFAULT, 960);
        let mut per: Vec<Vec<NoteRecord>> = vec![Vec::new(); 512];
        for (i, v) in dense.track(1).unwrap().notes.iter().enumerate() {
            per[i % 512].push(v.record());
        }
        for (k, recs) in per.iter().enumerate() {
            let ti = if k == 0 {
                0
            } else {
                spread.add_track().unwrap()
            };
            spread
                .track_mut(ti)
                .unwrap()
                .notes
                .insert_sorted_batch(recs, false)
                .unwrap();
        }
        for (name, doc) in [
            ("1M notes, 1 track", &dense),
            ("1M notes, 512 tracks", &spread),
        ] {
            let mut svc = AnalysisService::default();
            let t0 = Instant::now();
            while svc.index().is_none() {
                let _ = svc.sync(doc, 1);
                std::thread::sleep(Duration::from_millis(2));
            }
            let built = t0.elapsed();
            let corr = document_corrections(doc);
            let timing = Timing::for_document(doc);
            let end = doc.content_end_tick();
            let tpl = Template::parse(crate::app::progressor::DEFAULT_TEMPLATE);
            let settings = ProgressorSettings::default();
            let key = || KeysKey {
                notes: NotesKey::of(doc, 1),
                dark: true,
                tracks: None,
                active: None,
                palette: Default::default(),
                played: false,
            };
            let mut keys = KeyboardTracker::default();
            let frame = |tick: u64, keys: &mut KeyboardTracker, svc: &mut AnalysisService| {
                let t = Instant::now();
                let idx = svc.index().cloned();
                keys.update(doc, &corr, idx.as_deref(), tick, key());
                let snap = svc
                    .query(tick, &Scope::Project, DEFAULT_WINDOW_NS, Peaks::default())
                    .unwrap();
                let m = snap.metrics;
                let text = render_text(
                    &tpl,
                    &Readout {
                        notes: m.notes_passed,
                        total: m.total_notes,
                        nps: m.nps,
                        poly: m.poly,
                        pps: m.pps,
                        ..Readout::default()
                    },
                    &settings,
                );
                assert!(!text.is_empty());
                t.elapsed()
            };
            let report = |what: &str, v: &mut Vec<Duration>| {
                v.sort();
                println!(
                    "live ({name}), {what}: median {:.1} µs, p95 {:.1} µs, max {:.1} µs ({} frames)",
                    v[v.len() / 2].as_secs_f64() * 1e6,
                    v[v.len() * 95 / 100].as_secs_f64() * 1e6,
                    v[v.len() - 1].as_secs_f64() * 1e6,
                    v.len()
                );
            };
            println!(
                "live ({name}): analysis index ready after {:.1} ms",
                built.as_secs_f64() * 1e3
            );
            // Playing from mid-song at 60 fps.
            let mut tick = end / 2;
            frame(tick, &mut keys, &mut svc);
            let mut v = Vec::new();
            for _ in 0..600 {
                let ns = timing.nanos_at(tick) + 16_666_667;
                tick = timing.tick_at_nanos(ns).max(tick + 1);
                v.push(frame(tick, &mut keys, &mut svc));
            }
            report("playing (keys + Progressor per frame)", &mut v);
            let rebuilds = keys.rebuilds;
            // Random seeks.
            let mut s = 99u64;
            let mut v = Vec::new();
            for _ in 0..300 {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                v.push(frame((s >> 11) % end, &mut keys, &mut svc));
            }
            report("random seek (keys rebuild + Progressor)", &mut v);
            // Scrubbing: the playhead dragged across the whole song in 300 frames.
            let mut v = Vec::new();
            for k in 0..300u64 {
                v.push(frame(end * k / 300, &mut keys, &mut svc));
            }
            report("scrub across the song (300 frames)", &mut v);
            println!(
                "live ({name}): keyboard rebuilds while playing {}, after seeks {}",
                rebuilds, keys.rebuilds
            );
        }
    }

    /// v0.9 §068: the keys light up in the roll's colours of the current note
    /// palette, and a palette change re-colours them.
    #[test]
    fn keys_follow_the_note_palette() {
        use crate::ui::theme::{NoteColorMode, NoteColorPrefs, NotePalette};
        use std::sync::Arc;
        let c5 = 72u8;
        let d = doc(&[vec![rec(0, 1_000, c5, 100, 2)]]);
        let corr = document_corrections(&d);
        let id = d.track(0).unwrap().id();
        let manual = Arc::new(NotePalette::new(NoteColorPrefs {
            mode: NoteColorMode::Manual,
            seed: 0,
            manual: vec![[250, 10, 10], [10, 250, 10], [10, 10, 250]],
        }));
        let mut nc = NoteColors::default();
        let mut k = KeyboardTracker::default();
        let mut seen = Vec::new();
        for palette in [
            Arc::new(NotePalette::default()),
            manual.clone(),
            Arc::default(),
        ] {
            let kk = KeysKey {
                active: Some(id),
                palette: palette.clone(),
                ..key(&d)
            };
            k.update(&d, &corr, None, 10, kk);
            let band = k.display()[usize::from(c5)].bands[0];
            assert_eq!(band, nc.get(&palette, NoteTint::Subchannels, 2, 100, true));
            seen.push(band);
        }
        assert_ne!(seen[0], seen[1]);
        assert_eq!(seen[0], seen[2]);
    }
}

#[cfg(test)]
mod roll_keys_tests {
    use super::*;
    use plumspirit_engine::analysis::document_corrections;
    use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, SubchannelId};

    fn rec(start: u64, dur: u64, pitch: u8, sub: u32) -> NoteRecord {
        NoteRecord {
            start_tick: start,
            duration_ticks: dur,
            attrs: NoteAttrs::new_wide(pitch, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
            sub: SubchannelId(sub),
            group: 0,
        }
    }

    /// Three tracks on key 60 (and one note on key 200): track 0 holds
    /// 0..1,000 and 500..700 (subchannel 1), track 1 holds 0..2,000, track 2
    /// holds 100..3,000; a muted note on track 2 at 3,000..4,000.
    fn doc() -> PlumDocument {
        let mut d = PlumDocument::new_project("roll keys".into(), Bpm::DEFAULT, 960);
        d.add_track().unwrap();
        d.add_track().unwrap();
        let mut muted = rec(3_000, 1_000, 60, 0);
        muted.attrs = muted.attrs.with_muted(true);
        for (i, v) in [
            vec![rec(0, 1_000, 60, 0), rec(500, 200, 60, 1)],
            vec![rec(0, 2_000, 60, 0)],
            vec![rec(100, 2_900, 60, 0), muted, rec(10, 50, 200, 0)],
        ]
        .into_iter()
        .enumerate()
        {
            let mut v = v;
            v.sort_by_key(|n| n.start_tick);
            d.track_mut(i)
                .unwrap()
                .notes
                .insert_sorted_batch(&v, false)
                .unwrap();
        }
        d
    }

    /// The colour the roll gives a note of track `t` (active or a ghost).
    fn colour(d: &PlumDocument, t: usize, active: bool, sub: u32) -> Color32 {
        let tr = &d.tracks()[t];
        let tint = NoteTint::of(tr.id().0, tr.props().onion_color, active, true);
        NoteColors::default().get(
            &crate::ui::theme::NotePalette::default(),
            tint,
            sub,
            100,
            true,
        )
    }

    fn at(
        rk: &mut RollKeys,
        d: &PlumDocument,
        scope: Option<&BTreeSet<TrackId>>,
        active: Option<usize>,
        onion: &[usize],
        tick: u64,
    ) -> Vec<(usize, Color32, u8)> {
        let ids: Vec<TrackId> = onion.iter().map(|&i| d.tracks()[i].id()).collect();
        let totals = [KeyDisplay::default(); KEYS];
        let palette = crate::ui::theme::NotePalette::default();
        let shown = rk.update(
            d,
            &document_corrections(d),
            scope,
            active,
            &ids,
            tick,
            &totals,
            &palette,
            true,
        );
        shown
            .iter()
            .enumerate()
            .filter(|(_, k)| k.n > 0)
            .map(|(i, k)| (i, k.bands[0], k.n))
            .collect()
    }

    /// L-G02 (contract §013; "if there's no onion, the keyboard should also
    /// be the same"): following the roll, the keys show the active track
    /// only while the Onion is off — never another track's colour — and
    /// the ghosts in the roll's layers while it is on; the Conductor (no
    /// track's notes in the roll) lights nothing without the Onion.
    #[test]
    fn the_keyboard_never_keeps_ghost_colours_with_onion_off() {
        let d = doc();
        let mut rk = RollKeys::default();
        // Onion off, track 0 active: key 60 is track 0's (not track 2's in
        // front of it in track order).
        let shown = shown_tracks(&d, Some(0), &[]);
        assert_eq!(shown.len(), 1);
        let k = at(&mut rk, &d, Some(&shown), Some(0), &[], 600);
        assert_eq!(k, vec![(60, colour(&d, 0, true, 1), 1)]);
        // Key 200 belongs to track 2 only: dark while track 2 is no ghost.
        let k = at(&mut rk, &d, Some(&shown), Some(0), &[], 20);
        assert!(k.iter().all(|&(key, ..)| key != 200), "{k:?}");
        // Onion on with track 2 as the ghost: key 200 lights in its ghost colour.
        let shown = shown_tracks(&d, Some(0), &[2]);
        let k = at(&mut rk, &d, Some(&shown), Some(0), &[2], 20);
        assert!(k.contains(&(200, colour(&d, 2, false, 0), 1)), "{k:?}");
        // Off again: gone — no stale ghost colour.
        let shown = shown_tracks(&d, Some(0), &[]);
        let k = at(&mut rk, &d, Some(&shown), Some(0), &[], 20);
        assert!(k.iter().all(|&(key, ..)| key != 200), "{k:?}");
        // The Conductor without the Onion: no track's notes, no key.
        let shown = shown_tracks(&d, None, &[]);
        assert!(shown.is_empty());
        assert!(at(&mut rk, &d, Some(&shown), None, &[], 600).is_empty());
    }

    /// v1.0-beta1 B1-0110 (contract §016): one colour per key — the note in
    /// front in the roll's layers at the playhead; never bands.
    #[test]
    fn a_roll_key_shows_the_foremost_note_in_the_rolls_layers() {
        let d = doc();
        let mut rk = RollKeys::default();
        // The active track is in front of every ghost; within it the later
        // start (subchannel 1 from 500) wins.
        assert_eq!(
            at(&mut rk, &d, None, Some(0), &[1, 2], 300),
            vec![(60, colour(&d, 0, true, 0), 1)]
        );
        assert_eq!(
            at(&mut rk, &d, None, Some(0), &[1, 2], 600),
            vec![(60, colour(&d, 0, true, 1), 1)]
        );
        // Note Off is exclusive: at 1,000 the active track is silent there,
        // so the front ghost (track 2, last in the Onion order) shows.
        assert_eq!(
            at(&mut rk, &d, None, Some(0), &[1, 2], 1_000),
            vec![(60, colour(&d, 2, false, 0), 1)]
        );
        // The Onion order decides between ghosts (track 1 in front now).
        assert_eq!(
            at(&mut rk, &d, None, Some(0), &[2, 1], 1_000),
            vec![(60, colour(&d, 1, false, 0), 1)]
        );
        // Tracks of the scope not drawn as ghosts: track order, later in front.
        assert_eq!(
            at(&mut rk, &d, None, Some(0), &[], 1_000),
            vec![(60, colour(&d, 2, false, 0), 1)]
        );
        // Scope: only the active track lights keys.
        let only0: BTreeSet<TrackId> = [d.tracks()[0].id()].into();
        assert_eq!(
            at(&mut rk, &d, Some(&only0), Some(0), &[1, 2], 1_500),
            vec![]
        );
        // Muted notes never sound; the 256-key range lights key 200.
        assert_eq!(at(&mut rk, &d, None, Some(0), &[1, 2], 3_500), vec![]);
        assert_eq!(
            at(&mut rk, &d, None, Some(0), &[1, 2], 20),
            vec![
                (60, colour(&d, 0, true, 0), 1),
                (200, colour(&d, 2, false, 0), 1)
            ]
        );
        // The Conductor edited: no active track, the ghosts' order alone.
        assert_eq!(
            at(&mut rk, &d, None, None, &[0, 1, 2], 300),
            vec![(60, colour(&d, 2, false, 0), 1)]
        );
        // Paused frames (and a seek back) give the same keys.
        let a = at(&mut rk, &d, None, Some(0), &[1, 2], 600);
        let _ = at(&mut rk, &d, None, Some(0), &[1, 2], 2_500);
        assert_eq!(at(&mut rk, &d, None, Some(0), &[1, 2], 600), a);
    }

    /// L-S45 (E0075): key masks are read a budget of notes per update — the
    /// first frame of the Onion over every track of a 224M-note project no
    /// longer reads all of them — and a store whose mask is not complete yet
    /// lights exactly the same keys (read with every key until then).
    #[test]
    fn key_masks_are_read_in_bounded_pieces_and_never_change_the_keys() {
        let mut d = doc();
        // Tracks 0 and 1 over several chunks (4,096 notes each).
        for (t, n, k0) in [(0usize, 10_000u64, 30u8), (1, 9_000, 40)] {
            let mut v: Vec<NoteRecord> = (0..n)
                .map(|i| rec(5_000 + i * 3, 2 + i % 5, k0 + (i % 50) as u8, 0))
                .collect();
            v.sort_by_key(|r| r.start_tick);
            d.track_mut(t)
                .unwrap()
                .notes
                .insert_sorted_batch(&v, false)
                .unwrap();
        }
        assert!(d.tracks()[0].notes.chunk_count() >= 3);
        let mut exact = RollKeys::default();
        let mut small = RollKeys::default();
        let ticks = [20, 600, 1_000, 5_001, 9_000, 17_000, 25_000, 33_000, 600];
        let mut dense_keys = 0;
        for (step, &t) in ticks.iter().cycle().take(40).enumerate() {
            small.mask_budget = 1_000;
            let want = at(&mut exact, &d, None, Some(0), &[1, 2], t);
            let got = at(&mut small, &d, None, Some(0), &[1, 2], t);
            assert_eq!(got, want, "update {step} at {t}");
            dense_keys += got
                .iter()
                .filter(|&&(k, ..)| (30..90).contains(&k) && k != 60)
                .count();
        }
        assert!(dense_keys > 0, "the multi-chunk tracks light keys");
        // Every mask completed piece by piece, and equals the whole read.
        assert!(small.partial.is_empty());
        assert_eq!(small.masks.len(), exact.masks.len());
        for (k, m) in &exact.masks {
            assert_eq!(small.masks.get(k), Some(m));
        }
        // One update reads about its budget: a 1-note budget leaves the
        // multi-chunk tracks partial after the first update.
        let mut one = RollKeys {
            mask_budget: 1,
            ..RollKeys::default()
        };
        let _ = at(&mut one, &d, None, Some(0), &[1, 2], 9_000);
        assert_eq!(one.masks.len(), 0, "{:?}", one.masks.len());
        assert_eq!(one.partial.len(), 1);
    }

    /// L-S45 measurement: the first keyboard update after the Onion turns
    /// on for every other track (env PLUMSPIRIT_BENCH_KEYS_TRACKS /
    /// _NOTES, default 700 × 100,000): every mask at once (before L-S45)
    /// against the per-update budget, then the updates until every mask is
    /// complete.
    #[test]
    #[ignore = "measurement: run with --release --ignored --nocapture"]
    fn first_onion_keyboard_update_cost() {
        let env = |k: &str, d: u64| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d)
        };
        let (tracks, per) = (
            env("PLUMSPIRIT_BENCH_KEYS_TRACKS", 700) as usize,
            env("PLUMSPIRIT_BENCH_KEYS_NOTES", 100_000),
        );
        let mut d = PlumDocument::new_project("keys".into(), Bpm::DEFAULT, 960);
        for _ in 1..tracks {
            d.add_track().unwrap();
        }
        for t in 0..tracks {
            let v: Vec<NoteRecord> = (0..per)
                .map(|i| rec(i * 4 + (t as u64 % 4), 3, (i % 128) as u8, 0))
                .collect();
            d.track_mut(t)
                .unwrap()
                .notes
                .insert_sorted_batch(&v, false)
                .unwrap();
        }
        let others: Vec<usize> = (1..tracks).collect();
        let tick = per * 2;
        for (name, budget) in [
            ("all masks at once", usize::MAX),
            ("budgeted", MASK_NOTES_PER_UPDATE),
        ] {
            let mut rk = RollKeys::default();
            let _ = at(&mut rk, &d, None, Some(0), &[], tick);
            rk.mask_budget = budget;
            let t0 = std::time::Instant::now();
            let first = at(&mut rk, &d, None, Some(0), &others, tick);
            let first_t = t0.elapsed();
            let mut n = 1;
            let t1 = std::time::Instant::now();
            let mut worst = first_t;
            while !rk.partial.is_empty() || rk.masks.len() < tracks {
                rk.mask_budget = budget;
                let u = std::time::Instant::now();
                assert_eq!(at(&mut rk, &d, None, Some(0), &others, tick), first);
                worst = worst.max(u.elapsed());
                n += 1;
                assert!(n < 100_000);
            }
            let u = std::time::Instant::now();
            let _ = at(&mut rk, &d, None, Some(0), &others, tick);
            println!(
                "{tracks} tracks x {per} notes, {name}: first update {:.1} ms, {n} updates until every mask ({:.1} ms more, worst {:.1} ms), then {:.2} ms",
                first_t.as_secs_f64() * 1e3,
                t1.elapsed().as_secs_f64() * 1e3,
                worst.as_secs_f64() * 1e3,
                u.elapsed().as_secs_f64() * 1e3
            );
        }
    }
}
