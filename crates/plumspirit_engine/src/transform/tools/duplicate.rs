//! Duplicate's v0.9 kernel (V09-S39 step 7, `docs/TRANSFORM_ENGINE.md`
//! "Duplicate"): the functions declared by id beside the v0.8 kernel
//! (`edit.rs` `dup_build`, untouched — a module of its own so the v0.8
//! closure compiles as before), with Control Sweeps per note.
//!
//! The copies come from the first placement function that is on — n03 At
//! positions, n11 Offset list, n05 Along a path, n02 Canon — else n01's
//! layers (the selection's own time), else v0.8's count and step. n01 sends
//! them to other tracks, n10 puts a stutter before each, n04 / n12 / n07
//! change their pitches, n09 thins them, n08 copies the controllers with
//! them. v0.8's per-copy changes (pitch and scale steps, decay, subchannel
//! advance, mirror, omission) apply to every copy by its index.

use super::super::output::{Lane, MAX_EVENTS, TEvent};
use super::super::param::*;
use super::super::registry::BuildCtx;
use super::super::rng::{Rng, hash, note_key, salt};
use super::super::shared::chord_groups::ChordGroups;
use super::super::shared::metric_position::{bar_bounds, bar_place};
use super::super::shared::scale_lattice::Lattice;
use super::super::sweep::{PerLevel, Rebuild};
use super::super::theory::{Scale, parse_chord};
use super::super::{
    Ctx, Estimate, Growth, Kernel, Output, PointKernel, Source, TNote, TransformError,
};
use std::sync::Arc;

pub(super) const DUP_TARGETS: &[&str] = &["Next tracks", "Listed tracks", "Subtracks"];
pub(super) const DUP_CANON: &[&str] = &["Augmentation", "Diminution", "Inversion", "Retrograde"];
pub(super) const DUP_AT: &[&str] = &[
    "Every marker",
    "Every bar of the time selection",
    "Reference chord changes",
    "Listed ticks",
];
pub(super) const DUP_MOTION: &[&str] = &["Fifths up", "Fourths up", "Thirds down", "Chromatic"];
pub(super) const DUP_WITHIN: &[&str] = &["Chromatic", "The key"];
pub(super) const DUP_FIT: &[&str] = &["Transpose by the root", "Snap to chord tones"];
pub(super) const DUP_INCLUDE: &[&str] = &[
    "Notes only",
    "Notes and controllers",
    "Notes, controllers and pitch bend",
];
pub(super) const DUP_THIN: &[&str] = &["Outline", "Loudest", "Evenly spaced"];

/// Most positions n03 pastes at.
pub const MAX_POSITIONS: usize = 4096;

/// Duplicate's v0.8 settings (the v0.8 kernel's closure reads the same
/// values; [`Dup9`] re-reads them here, unchanged).
#[derive(Clone, Debug)]
struct Dup8 {
    /// Ticks from one copy to the next (≥ 1).
    step: i128,
    count: u64,
    pitch_step: i64,
    decay: Option<f64>,
    sub_step: i64,
    mirror: bool,
    scale: Option<(Scale, i64)>,
    omit: Option<f64>,
    seed: u64,
    s0: u64,
    e0: u64,
}

impl Dup8 {
    fn from(p: &Params, b: &BuildCtx) -> Dup8 {
        let snap = b.ctx.snap.max(1);
        let bar = b.ctx.bar().max(1);
        let span = b.stats.max_end.saturating_sub(b.stats.min_start).max(1);
        let mut offset = if p.flag("grid") {
            span.div_ceil(snap).saturating_mul(snap)
        } else {
            span
        };
        if p.adv(6) {
            offset = offset.div_ceil(bar).saturating_mul(bar);
        }
        let extra = if p.adv(2) { p.int("a2.ticks") } else { 0 };
        let step = (i128::from(offset) + i128::from(extra)).max(1);
        let mut count = if p.adv(1) {
            p.int("a1.count") as u64
        } else {
            1
        };
        if p.adv(10) {
            let until = p.ticks("a10.until");
            let room = i128::from(until) - i128::from(b.stats.max_end);
            count = if room > 0 {
                (room / step + 1).min(1 << 16) as u64
            } else {
                0
            };
        }
        Dup8 {
            step,
            count,
            pitch_step: if p.adv(3) { p.int("a3.step") } else { 0 },
            decay: p.adv(4).then(|| p.float("a4.decay") / 100.0),
            sub_step: if p.adv(5) { p.int("a5.step") } else { 0 },
            mirror: p.adv(7),
            scale: p.adv(8).then(|| {
                (
                    Scale::new(p.choice("a8.key"), p.choice("a8.scale")),
                    p.int("a8.degrees"),
                )
            }),
            omit: p.adv(9).then(|| p.float("a9.prob") / 100.0),
            seed: p.seed("seed"),
            s0: b.stats.min_start,
            e0: b.stats.max_end,
        }
    }
}

/// Duplicate's settings of the functions declared by id (V09-S39 step 7),
/// per Control Sweep level ([`PerLevel`]).
#[derive(Clone, Debug, Default)]
pub(super) struct DupNew {
    /// n01: targets (0 next tracks, 1 listed, 2 subtracks), copies on each.
    tracks: Option<(usize, u64)>,
    /// n02: kind (0 augmentation, 1 diminution, 2 inversion, 3
    /// retrograde), factor (a fraction), entry delay, entries.
    canon: Option<(usize, f64, u64, u64)>,
    /// n03: kind (0 markers, 1 bars of the time selection, 2 reference
    /// chord changes, 3 listed ticks).
    at: Option<usize>,
    /// n04: rotation per copy (positions).
    rotate: Option<i64>,
    /// n05: the path, pitch range, spacing, copies.
    path: Option<(Curve, i64, u64, u64)>,
    /// n12: semitones per copy, scale degrees per copy, the key (within
    /// it).
    sequence: Option<(i64, i64, Option<Scale>)>,
    /// n07: snap to the chord tones (else transpose by the root).
    fit: Option<bool>,
    /// B2-F01: n07's fit before n12's sequence (`order::DUPLICATE_PITCH`;
    /// declared: the sequence first).
    fit_first: bool,
    /// n08: include (0 notes only, 1 controllers, 2 and pitch bend), the
    /// controller numbers (empty = all).
    events: Option<(usize, Vec<i64>)>,
    /// n09: keep per copy (a fraction), keep the (0 outline, 1 loudest, 2
    /// evenly spaced).
    thin: Option<(f64, usize)>,
    /// n10: stutter notes, times.
    stutter: Option<(u64, u64)>,
    /// n11: the offsets (ticks).
    offsets: Option<Vec<i64>>,
}

impl DupNew {
    pub(super) fn from(p: &Params, _ctx: &Ctx) -> DupNew {
        DupNew {
            tracks: p.flag("n01").then(|| {
                (
                    p.choice("n01.targets"),
                    p.int("n01.count").clamp(1, 64) as u64,
                )
            }),
            canon: p.flag("n02").then(|| {
                (
                    p.choice("n02.kind"),
                    p.float("n02.factor") / 100.0,
                    p.ticks("n02.delay"),
                    p.int("n02.entries").clamp(1, 16) as u64,
                )
            }),
            at: p.flag("n03").then(|| p.choice("n03.at")),
            rotate: p.flag("n04").then(|| p.int("n04.rotate")),
            path: p.flag("n05").then(|| {
                (
                    p.curve("n05.path"),
                    p.int("n05.range").max(1),
                    p.ticks("n05.spacing").max(1),
                    p.int("n05.copies").clamp(1, 256) as u64,
                )
            }),
            sequence: p.flag("n12").then(|| {
                let (semis, degrees) = match p.choice("n12.motion") {
                    0 => (7, 4),
                    1 => (5, 3),
                    2 => (-3, -2),
                    _ => (1, 1),
                };
                let key = (p.choice("n12.within") == 1)
                    .then(|| Scale::new(p.choice("n12.key"), p.choice("n12.scale")));
                (semis, degrees, key)
            }),
            fit: p.flag("n07").then(|| p.choice("n07.fit") == 1),
            fit_first: p.sequence(crate::transform::order::DUPLICATE_PITCH.members)[0] == "n07",
            events: p
                .flag("n08")
                .then(|| (p.choice("n08.include"), p.list("n08.ccs"))),
            thin: p.flag("n09").then(|| {
                (
                    (p.float("n09.keep") / 100.0).clamp(0.0, 1.0),
                    p.choice("n09.by"),
                )
            }),
            stutter: p.flag("n10").then(|| {
                (
                    p.int("n10.notes").clamp(1, 64) as u64,
                    p.int("n10.times").clamp(1, 16) as u64,
                )
            }),
            offsets: p.flag("n11").then(|| {
                p.list("n11.offsets")
                    .into_iter()
                    .map(|o| o.max(0))
                    .collect()
            }),
        }
    }

    pub(super) fn any(&self) -> bool {
        self.tracks.is_some()
            || self.canon.is_some()
            || self.at.is_some()
            || self.rotate.is_some()
            || self.path.is_some()
            || self.sequence.is_some()
            || self.fit.is_some()
            || self.events.is_some()
            || self.thin.is_some()
            || self.stutter.is_some()
            || self.offsets.is_some()
    }

    /// Whether every function that is on works note by note (lazy viewport
    /// preview): n01's destinations, n03's reference, n08's controllers,
    /// n02's inversion (the first note), n04's rotation, n09's ranks and
    /// n10's fragment read the whole selection or prepared layers.
    fn per_note(&self) -> bool {
        self.tracks.is_none()
            && self.rotate.is_none()
            && self.thin.is_none()
            && self.stutter.is_none()
            && self.canon.is_none_or(|c| c.0 != 2)
            && self.at != Some(2)
            && self.events.as_ref().is_none_or(|e| e.0 == 0)
    }
}

/// Where the copies come from (the first function on, in this order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Provider {
    /// n03.
    Positions,
    /// n11.
    Offsets,
    /// n05.
    Path,
    /// n02.
    Canon,
    /// n01 alone: layers at the selection's own time.
    Layers,
    /// v0.8: count and step.
    V08,
}

impl Provider {
    fn of(c: &DupNew) -> Provider {
        if c.at.is_some() {
            Provider::Positions
        } else if c.offsets.is_some() {
            Provider::Offsets
        } else if c.path.is_some() {
            Provider::Path
        } else if c.canon.is_some() {
            Provider::Canon
        } else if c.tracks.is_some() {
            Provider::Layers
        } else {
            Provider::V08
        }
    }

    /// Whether copies follow one another by a step (n10's stutter widens
    /// it); the others keep their positions and the stutter goes before
    /// them.
    fn stepped(self) -> bool {
        matches!(self, Provider::Path | Provider::Canon | Provider::V08)
    }
}

/// One copy's placement: its index, the anchor's offset, the time scale
/// around the selection's start, retrograde, inversion and n05's pitch
/// offset.
#[derive(Clone, Copy, Debug)]
struct Place {
    k: u64,
    dt: i128,
    sigma: f64,
    retro: bool,
    invert: bool,
    dp: i64,
}

/// What the whole-source run reads across notes (empty for the lazy
/// per-note path): n09's ranks, n04's positions and pitch sequences, n02's
/// first pitch, n10's fragment ends, n01's routes, the tracks'
/// subchannels and n03's positions.
#[derive(Clone, Debug, Default)]
struct World {
    rank: Vec<u32>,
    pos: Vec<u32>,
    seq: Vec<Vec<u8>>,
    first: i64,
    /// Starts of the notes in time order and the largest end of the first
    /// k + 1 notes (n10).
    starts: Vec<u64>,
    prefix_end: Vec<u64>,
    /// Destination slots by source slot (n01).
    routes: Vec<Vec<u16>>,
    subs: Vec<Vec<(u32, u8)>>,
    positions: Arc<Vec<u64>>,
}

impl World {
    /// The slots copies of a note of slot `track` go to.
    fn targets(&self, track: u16, routed: bool) -> &[u16] {
        if routed {
            self.routes
                .get(usize::from(track))
                .map_or(&[][..], |v| v.as_slice())
        } else {
            std::slice::from_ref(&ROUTE_SELF[0])
        }
    }
}

/// Placeholder slot meaning "the note's own track" ([`World::targets`]).
static ROUTE_SELF: [u16; 1] = [u16::MAX];

/// The v0.9 Duplicate kernel (V09-S39 step 7): the v0.8 rule re-read from
/// the same parameters, with the functions declared by id at their stages
/// and the Control Sweeps evaluated per note inside the kernel.
pub(super) struct Dup9 {
    v8: Dup8,
    new: DupNew,
    provider: Provider,
    rebuild: Option<Arc<Rebuild>>,
    ctx: Ctx,
    /// n07's chords (root, pitch classes), cycled per bar, and the root of
    /// the selection's bar.
    chords: Arc<Vec<(i64, u16)>>,
    fit_from: i64,
    /// The per-note path's world (positions, subchannels).
    fixed: World,
    /// Lazy viewport preview: (reach before, reach after) when every
    /// function on works per note and no sweep is on.
    point: Option<(u64, u64)>,
    growth: Growth,
}

/// n07's chords: one per bar, cycled.
fn parse_chords(text: &str) -> Result<Vec<(i64, u16)>, TransformError> {
    let mut v = Vec::new();
    for tok in text
        .split([' ', ',', ';', '|'])
        .filter(|t| !t.trim().is_empty())
    {
        let c = parse_chord(tok.trim()).ok_or_else(|| {
            TransformError::Invalid(format!(
                "Progression fit: “{}” is not a chord name (for example C, Am, F#m7, Bbmaj7).",
                tok.trim()
            ))
        })?;
        v.push((i64::from(c.root), c.pitch_classes()));
    }
    if v.is_empty() {
        return Err(TransformError::Invalid(
            "Progression fit: type at least one chord, for example “C Am F G”.".into(),
        ));
    }
    Ok(v)
}

/// n03's positions from the project (markers, the time selection's bars)
/// or the list; the reference's come from its prepared layer at run time.
fn positions_of(p: &Params, ctx: &Ctx, kind: usize) -> Result<Vec<u64>, TransformError> {
    let v: Vec<u64> = match kind {
        0 => {
            let v: Vec<u64> = ctx
                .markers
                .iter()
                .filter_map(|&m| u64::try_from(i128::from(m) - ctx.shift).ok())
                .collect();
            if v.is_empty() {
                return Err(TransformError::Invalid(
                    "At positions: the project has no markers. Add markers, or paste at listed ticks instead.".into(),
                ));
            }
            v
        }
        1 => {
            let Some((a, b)) = ctx.time_range else {
                return Err(TransformError::Invalid(
                    "At positions: make a time selection on the ruler first; a copy goes to the start of each of its bars.".into(),
                ));
            };
            let mut v = Vec::new();
            let (mut t, _) = bar_bounds(&ctx.meter, a);
            while t < b && v.len() <= MAX_POSITIONS {
                v.push(t);
                let (s, len) = bar_bounds(&ctx.meter, t);
                t = s.saturating_add(len.max(1));
            }
            v
        }
        2 => Vec::new(),
        _ => {
            let v: Vec<u64> = p
                .list("n03.ticks")
                .into_iter()
                .map(|t| t.max(0) as u64)
                .collect();
            if v.is_empty() {
                return Err(TransformError::Invalid(
                    "At positions: list at least one tick to paste at.".into(),
                ));
            }
            v
        }
    };
    Ok(v)
}

/// Sorted, without duplicates and without the selection's own start (a
/// copy there would lie on the originals), at most [`MAX_POSITIONS`].
fn tidy_positions(mut v: Vec<u64>, s0: u64) -> Vec<u64> {
    v.sort_unstable();
    v.dedup();
    v.retain(|&t| t != s0);
    v.truncate(MAX_POSITIONS);
    v
}

/// The reference's chord changes: the onsets (in its time order) whose
/// pitch classes differ from the previous onset's.
fn chord_changes(notes: &[TNote]) -> Vec<u64> {
    let idx: Vec<usize> = (0..notes.len()).collect();
    let groups = ChordGroups::of(notes, &idx, 0);
    let mut out = Vec::new();
    let mut last: Option<u16> = None;
    for g in groups.iter() {
        let mask = g.iter().fold(0u16, |m, &i| m | 1 << (notes[i].pitch % 12));
        if last != Some(mask) {
            out.push(notes[g[0]].start);
            last = Some(mask);
        }
    }
    out
}

/// The van der Corput order of `m` items: the items taken in bit-reversed
/// order — any prefix of it is spread evenly over the items (n09's evenly
/// spaced share, and its tie-breaks). O(m), no sort.
pub(super) fn spread_order(m: usize) -> Vec<u32> {
    if m == 0 {
        return Vec::new();
    }
    let bits = usize::BITS - (m - 1).leading_zeros();
    let mut order = Vec::with_capacity(m);
    for r in 0..(1usize << bits) {
        let j = if bits == 0 {
            0
        } else {
            r.reverse_bits() >> (usize::BITS - bits)
        };
        if j < m {
            order.push(j as u32);
        }
    }
    order
}

/// Each note's role in its onset (per track; `notes` in time order, an
/// onset's notes by pitch): 0 its top note, 1 its bottom, 2 an inner voice
/// (the outline first: n09 here, Score Stretch n04).
pub(super) fn onset_roles(notes: &[TNote]) -> Vec<u8> {
    let n = notes.len();
    let mut role = vec![2u8; n];
    // (track, its lowest, its highest note) of one onset.
    let mut seen: Vec<(u16, usize, usize)> = Vec::new();
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j < n && notes[j].start == notes[i].start {
            j += 1;
        }
        seen.clear();
        for (k, x) in notes.iter().enumerate().take(j).skip(i) {
            match seen.iter_mut().find(|s| s.0 == x.track) {
                Some(s) => s.2 = k,
                None => seen.push((x.track, k, k)),
            }
        }
        for &(_, bottom, top) in &seen {
            role[bottom] = 1;
            role[top] = 0;
        }
        i = j;
    }
    role
}

/// Each item's place in [`spread_order`].
#[cfg(test)]
fn spread_ranks(m: usize) -> Vec<u32> {
    let mut rank = vec![0u32; m];
    for (k, &j) in spread_order(m).iter().enumerate() {
        rank[j as usize] = k as u32;
    }
    rank
}

impl Dup9 {
    pub(super) fn build(
        p: &Params,
        b: &BuildCtx,
        new: DupNew,
    ) -> Result<Box<dyn Kernel>, TransformError> {
        let v8 = Dup8::from(p, b);
        let provider = Provider::of(&new);
        let chords = if new.fit.is_some() {
            parse_chords(&p.text("n07.chords"))?
        } else {
            Vec::new()
        };
        let positions = match new.at {
            Some(kind) => tidy_positions(positions_of(p, b.ctx, kind)?, v8.s0),
            None => Vec::new(),
        };
        let rebuild = Rebuild::of(p, b);
        let fixed = World {
            subs: b.tracks.iter().map(|t| t.subs.clone()).collect(),
            positions: Arc::new(positions),
            ..World::default()
        };
        let mut k = Dup9 {
            v8,
            new,
            provider,
            rebuild,
            ctx: b.ctx.clone(),
            chords: Arc::new(chords),
            fit_from: 0,
            fixed,
            point: None,
            growth: Growth::Map,
        };
        // The bound per note over every swept corner: the note, then each
        // copy on each target with its stutter.
        let targets = if k.new.tracks.is_some() {
            b.tracks.len().max(1) as u64
        } else {
            1
        };
        let cfgs: Vec<DupNew> = match &k.rebuild {
            Some(r) => r.corners(b.ctx, |p, b| DupNew::from(p, b.ctx)),
            None => vec![k.new.clone()],
        };
        let mut per = 1u64;
        for c in &cfgs {
            let count = k.count_bound(c);
            let reps = c.stutter.map_or(0, |s| s.1);
            per = per.max(
                1u64.saturating_add(
                    count
                        .saturating_mul(targets)
                        .saturating_mul(1u64.saturating_add(reps)),
                ),
            );
        }
        k.growth = Growth::Expand(per);
        if !k.chords.is_empty() {
            k.fit_from = k.chord_at(i128::from(k.v8.s0)).0;
        }
        if k.rebuild.is_none() && k.new.per_note() {
            k.point = Some(k.reach());
        }
        Ok(Box::new(k))
    }

    /// The most copies a note gets from the provider (n03's reference
    /// positions are known only at run time: at most [`MAX_POSITIONS`]).
    fn count_bound(&self, c: &DupNew) -> u64 {
        match self.provider {
            Provider::Positions if c.at == Some(2) => MAX_POSITIONS as u64,
            _ => self.count(c, &self.fixed),
        }
    }

    /// The number of copies of a note with settings `c`.
    fn count(&self, c: &DupNew, w: &World) -> u64 {
        match self.provider {
            Provider::Positions => w.positions.len() as u64,
            Provider::Offsets => c.offsets.as_ref().map_or(0, |o| o.len() as u64),
            Provider::Path => c.path.as_ref().map_or(0, |p| p.3),
            Provider::Canon => c.canon.map_or(0, |c| c.3),
            Provider::Layers => c.tracks.map_or(0, |t| t.1),
            Provider::V08 => self.v8.count,
        }
    }

    /// Copy `k`'s placement with settings `c` (`widen`: n10's stutter
    /// length added to the step of step-placed copies).
    fn place(&self, c: &DupNew, w: &World, k: u64, widen: i128) -> Place {
        let ki = i128::from(k);
        let mut pl = Place {
            k,
            dt: 0,
            sigma: 1.0,
            retro: false,
            invert: false,
            dp: 0,
        };
        match self.provider {
            Provider::Positions => {
                let t = w.positions.get(k as usize - 1).copied().unwrap_or(0);
                pl.dt = i128::from(t) - i128::from(self.v8.s0);
            }
            Provider::Offsets => {
                let o = c
                    .offsets
                    .as_ref()
                    .and_then(|o| o.get(k as usize - 1))
                    .copied()
                    .unwrap_or(0);
                pl.dt = i128::from(o);
            }
            Provider::Path => {
                if let Some((path, range, spacing, copies)) = &c.path {
                    pl.dt = ki * (i128::from(*spacing) + widen);
                    let u = k as f64 / (*copies).max(1) as f64;
                    pl.dp = ((path.eval(u) - path.eval(0.0)) * *range as f64).round() as i64;
                }
            }
            Provider::Canon => {
                if let Some((kind, f, delay, _)) = c.canon {
                    pl.dt = ki * (i128::from(delay) + widen);
                    let e = k.min(64) as i32;
                    match kind {
                        0 => pl.sigma = f.max(1e-6).powi(e),
                        1 => pl.sigma = (1.0 / f.max(1e-6)).powi(e),
                        2 => pl.invert = true,
                        _ => pl.retro = true,
                    }
                }
            }
            Provider::Layers => {}
            Provider::V08 => pl.dt = self.v8.step * ki + ki * widen,
        }
        pl
    }

    /// The copy's start and length (n02's retrograde and time scale, v0.8's
    /// mirror of odd copies, the anchor's offset).
    #[inline]
    fn time(&self, x: &TNote, pl: &Place) -> (i128, i128) {
        let (s0, e0) = (i128::from(self.v8.s0), i128::from(self.v8.e0));
        let mut t = i128::from(x.start);
        let mut d = i128::from(x.dur);
        if pl.retro {
            t = s0 + e0 - (t + d);
        }
        if self.v8.mirror && pl.k % 2 == 1 {
            t = s0 + e0 - (t + d);
        }
        if pl.sigma != 1.0 {
            t = s0 + ((t - s0) as f64 * pl.sigma).round() as i128;
            d = ((d as f64 * pl.sigma).round() as i128).max(1);
        }
        (t + pl.dt, d)
    }

    /// The chord of the bar holding `tick` (n07).
    fn chord_at(&self, tick: i128) -> (i64, u16) {
        let t = tick.clamp(0, i128::from(u64::MAX)) as u64;
        let (bar, _, _) = bar_place(&self.ctx.meter, t);
        self.chords[(bar % self.chords.len() as u64) as usize]
    }

    /// The copy's pitch: n04's rotation, n02's inversion, v0.8's pitch and
    /// scale steps, n05's path, n12's sequence, n07's fit.
    fn pitch(&self, i: usize, x: &TNote, c: &DupNew, w: &World, pl: &Place, start: i128) -> i64 {
        let k = pl.k as i64;
        let mut p = match c.rotate {
            Some(r) if !w.pos.is_empty() => {
                let seq = &w.seq[usize::from(x.track)];
                let m = seq.len() as i64;
                let j = (i64::from(w.pos[i]) + r * k).rem_euclid(m.max(1));
                i64::from(seq[j as usize])
            }
            _ => i64::from(x.pitch),
        };
        if pl.invert {
            p = 2 * w.first - p;
        }
        p += self.v8.pitch_step * k;
        if let Some((sc, deg)) = &self.v8.scale {
            let (d, off) = sc.degree_of(p);
            p = sc.pitch_of(d + deg * k) + off;
        }
        p += pl.dp;
        // B2-F01: n12 and n07 in the user's order (declared: n12 first).
        let sequence = |p: i64| match &c.sequence {
            Some((_, degrees, Some(sc))) => {
                let (d, off) = sc.degree_of(p);
                sc.pitch_of(d + degrees * k) + off
            }
            Some((semis, _, None)) => p + semis * k,
            None => p,
        };
        let fit = |p: i64| match c.fit {
            Some(true) => Lattice::from_mask(self.chord_at(start).1).nearest(p),
            Some(false) => {
                let to = self.chord_at(i128::from(self.v8.s0) + pl.dt).0;
                let d = (to - self.fit_from).rem_euclid(12);
                p + if d > 6 { d - 12 } else { d }
            }
            None => p,
        };
        if c.fit_first {
            sequence(fit(p))
        } else {
            fit(sequence(p))
        }
    }

    /// The copy's template on slot `dest` (`u16::MAX`: the note's own):
    /// v0.8's subchannel advance by the copy index, a note pasted onto
    /// another track taking the subchannel at its position there.
    #[inline]
    fn template(&self, x: &TNote, dest: u16, k: u64, w: &World) -> TNote {
        let mut tpl = *x;
        let own = dest == u16::MAX || dest == x.track;
        let step = self.v8.sub_step;
        if own {
            if step != 0
                && let Some(t) = w.subs.get(usize::from(x.track))
                && !t.is_empty()
            {
                let i = t.iter().position(|s| s.0 == x.sub).unwrap_or(0) as i64;
                let (id, ch) = t[(i + step * k as i64).rem_euclid(t.len() as i64) as usize];
                tpl.sub = id;
                tpl.ch = ch;
            }
            return tpl;
        }
        tpl.track = dest;
        let pos = w
            .subs
            .get(usize::from(x.track))
            .and_then(|t| t.iter().position(|s| s.0 == x.sub))
            .unwrap_or(0) as i64;
        if let Some(t) = w.subs.get(usize::from(dest))
            && !t.is_empty()
        {
            if step != 0 {
                let (id, ch) = t[(pos + step * k as i64).rem_euclid(t.len() as i64) as usize];
                tpl.sub = id;
                tpl.ch = ch;
            } else {
                tpl.sub = t[(pos as usize) % t.len()].0;
            }
        }
        tpl
    }

    /// n10: the note's stutter (times, fragment length) when it is one of
    /// the first notes of the selection, and the stutter's length that
    /// widens a step.
    fn stutter(&self, i: usize, c: &DupNew, w: &World) -> (Option<(u64, i128)>, i128) {
        let Some((kf, times)) = c.stutter else {
            return (None, 0);
        };
        let n = w.starts.len();
        if n == 0 {
            return (None, 0);
        }
        let kf = (kf as usize).min(n);
        let s0 = i128::from(self.v8.s0);
        let mut lf = if kf < n {
            i128::from(w.starts[kf]) - s0
        } else {
            0
        };
        if lf <= 0 {
            lf = (i128::from(w.prefix_end[kf - 1]) - s0).max(1);
        }
        let widen = if self.provider.stepped() {
            i128::from(times) * lf
        } else {
            0
        };
        ((i < kf).then_some((times, lf)), widen)
    }

    /// Whether copy `k` keeps note `i` (n09's share, v0.8's seeded
    /// omission).
    #[inline]
    fn kept(&self, i: usize, x: &TNote, c: &DupNew, w: &World, k: u64) -> bool {
        if let Some(pr) = self.v8.omit
            && Rng::new(hash(&[self.v8.seed, salt::KEEP, note_key(x), k])).chance(pr)
        {
            return false;
        }
        if let Some((q, _)) = c.thin
            && !w.rank.is_empty()
        {
            let n = w.rank.len() as f64;
            let keep = (n * q.powi(k.min(1 << 20) as i32)).round();
            return f64::from(w.rank[i]) < keep;
        }
        true
    }

    /// The note and its copies (`i`: its index in time order, `usize::MAX`
    /// on the per-note path). False once the output is full.
    fn emit_note(&self, i: usize, x: &TNote, c: &DupNew, w: &World, out: &mut Output) -> bool {
        // V09-0143: the driver dimension is the copy (0 = the original).
        out.set_dim(0);
        if !out.push(*x) {
            return false;
        }
        let count = self.count(c, w);
        if count == 0 {
            return true;
        }
        let routed = c.tracks.is_some();
        let targets = w.targets(x.track, routed);
        if targets.is_empty() {
            return true;
        }
        let (reps, widen) = self.stutter(i, c, w);
        for k in 1..=count {
            if !self.kept(i, x, c, w, k) {
                continue;
            }
            out.set_dim(k.min(u64::from(u32::MAX)) as u32);
            let pl = self.place(c, w, k, widen);
            let (start, dur) = self.time(x, &pl);
            let pitch = self.pitch(i, x, c, w, &pl, start);
            let vel = match self.v8.decay {
                Some(dk) => (f64::from(x.vel) * dk.powi(k as i32)).round() as i64,
                None => i64::from(x.vel),
            };
            for &t in targets {
                let tpl = self.template(x, t, k, w);
                if !out.emit(start, dur, pitch, vel, &tpl) {
                    return false;
                }
                if let Some((times, lf)) = reps {
                    for r in 0..times {
                        let s = start - i128::from(times - r) * lf;
                        if !out.emit(s, dur, pitch, vel, &tpl) {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    /// Conservative reach of the per-note path (source window): copies end
    /// at most `before` ticks after the note's end / start at most `after`
    /// ticks before its start.
    fn reach(&self) -> (u64, u64) {
        let span = self.v8.e0.saturating_sub(self.v8.s0) as i128;
        let (mut lo, mut hi, mut sigma) = (0i128, 0i128, 1.0f64);
        let count = self.count(&self.new, &self.fixed);
        for k in 1..=count.min(1 << 16) {
            let pl = self.place(&self.new, &self.fixed, k, 0);
            lo = lo.min(pl.dt);
            hi = hi.max(pl.dt);
            sigma = sigma.max(pl.sigma);
        }
        let far = |v: f64| -> u64 { v.clamp(0.0, (u64::MAX >> 2) as f64) as u64 };
        let behind = far(hi as f64 + span as f64 * sigma + span as f64);
        let ahead = far((-lo) as f64 + span as f64);
        (behind, ahead)
    }

    /// The settings of every note.
    fn levels(&self, src: &Source) -> PerLevel<DupNew> {
        match &self.rebuild {
            Some(r) => r.levels(&self.ctx, src, |p, b| DupNew::from(p, b.ctx)),
            None => PerLevel::one(self.new.clone()),
        }
    }

    /// The whole-source world.
    fn world(&self, src: &Source, lv: &PerLevel<DupNew>) -> World {
        let notes = &src.notes;
        let n = notes.len();
        let any = |f: fn(&DupNew) -> bool| lv.all().iter().any(f);
        let mut w = World {
            subs: src.tracks.iter().map(|t| t.subs.clone()).collect(),
            positions: self.fixed.positions.clone(),
            first: notes.first().map_or(0, |x| i64::from(x.pitch)),
            ..World::default()
        };
        if self.new.at == Some(2) {
            let refs = src.aux.first().map_or(&[][..], |v| v.as_slice());
            w.positions = Arc::new(tidy_positions(chord_changes(refs), self.v8.s0));
        }
        if any(|c| c.rotate.is_some()) {
            w.pos = vec![0; n];
            w.seq = vec![Vec::new(); src.tracks.len().max(1)];
            for (i, x) in notes.iter().enumerate() {
                let s = &mut w.seq[usize::from(x.track)];
                w.pos[i] = s.len() as u32;
                s.push(x.pitch);
            }
        }
        if any(|c| c.stutter.is_some()) {
            w.starts = notes.iter().map(|x| x.start).collect();
            let mut m = 0u64;
            w.prefix_end = notes
                .iter()
                .map(|x| {
                    m = m.max(x.end());
                    m
                })
                .collect();
        }
        if let Some((_, by)) = lv.first().thin {
            w.rank = self.ranks(src, by);
        }
        if any(|c| c.tracks.is_some()) {
            w.routes = vec![Vec::new(); src.tracks.len()];
            for &(s, d) in src.routes.iter() {
                if let Some(r) = w.routes.get_mut(usize::from(s))
                    && usize::from(d) < src.tracks.len()
                    && !r.contains(&d)
                {
                    r.push(d);
                }
            }
        }
        w
    }

    /// n09's priority of every note (0 = kept longest): loudest by
    /// velocity, outline the top then the bottom note of each onset before
    /// the inner voices, evenly spaced by time — ties and every class in
    /// the van der Corput order of time, so any share is spread evenly.
    fn ranks(&self, src: &Source, by: usize) -> Vec<u32> {
        let notes = &src.notes;
        let n = notes.len();
        let order = spread_order(n);
        let mut rank = vec![0u32; n];
        if by == 2 {
            for (r, &i) in order.iter().enumerate() {
                rank[i as usize] = r as u32;
            }
            return rank;
        }
        // The classes, highest priority first: 127 − velocity (loudest), or
        // 0 the top of its onset (per track), 1 the bottom, 2 inner
        // (outline). A stable counting sort of the spread order by class:
        // O(n).
        let (classes, class): (usize, Vec<u8>) = if by == 1 {
            (128, notes.iter().map(|x| 127 - x.vel.min(127)).collect())
        } else {
            (3, onset_roles(notes))
        };
        let mut next = vec![0u32; classes + 1];
        for &c in &class {
            next[usize::from(c) + 1] += 1;
        }
        for c in 1..=classes {
            next[c] += next[c - 1];
        }
        for &i in &order {
            let c = usize::from(class[i as usize]);
            rank[i as usize] = next[c];
            next[c] += 1;
        }
        rank
    }

    /// n08: the controllers of each track's span, with every copy of the
    /// track's first note (its placement) on each of its targets.
    fn copy_events(
        &self,
        src: &Source,
        lv: &PerLevel<DupNew>,
        w: &World,
        out: &mut Output,
    ) -> Result<(), TransformError> {
        let ev = &src.aux_events;
        if ev.is_empty() {
            return Ok(());
        }
        let (s0, e0) = (i128::from(self.v8.s0), i128::from(self.v8.e0));
        let mut first: Vec<Option<usize>> = vec![None; src.tracks.len()];
        for (i, x) in src.notes.iter().enumerate() {
            let f = &mut first[usize::from(x.track)];
            if f.is_none() {
                *f = Some(i);
            }
        }
        let mut written = 0u64;
        for (slot, f) in first.iter().enumerate() {
            let Some(i) = *f else {
                continue;
            };
            let c = lv.at(i);
            let Some((include, ccs)) = c.events.as_ref().filter(|e| e.0 > 0) else {
                continue;
            };
            let x = &src.notes[i];
            let count = self.count(c, w);
            let targets = w.targets(x.track, c.tracks.is_some());
            let (_, widen) = self.stutter(i, c, w);
            let mine: Vec<&TEvent> = ev
                .iter()
                .filter(|e| usize::from(e.track) == slot)
                .filter(|e| match e.lane {
                    Lane::Cc(n) => ccs.is_empty() || ccs.contains(&i64::from(n)),
                    Lane::PitchBend => *include == 2,
                })
                .collect();
            let per = mine.len() as u64;
            written = written.saturating_add(
                per.saturating_mul(count)
                    .saturating_mul(targets.len() as u64),
            );
            if written > MAX_EVENTS as u64 {
                return Err(TransformError::TooLarge(format!(
                    "Copy controllers: the copies would write more than {} controller points; copy fewer controllers or make fewer copies.",
                    crate::midi::group_digits(MAX_EVENTS as u64)
                )));
            }
            for k in 1..=count {
                let pl = self.place(c, w, k, widen);
                for e in &mine {
                    let mut t = i128::from(e.tick);
                    if pl.retro {
                        t = s0 + e0 - t;
                    }
                    if self.v8.mirror && k % 2 == 1 {
                        t = s0 + e0 - t;
                    }
                    if pl.sigma != 1.0 {
                        t = s0 + ((t - s0) as f64 * pl.sigma).round() as i128;
                    }
                    for &d in targets {
                        let d = if d == u16::MAX { x.track } else { d };
                        if !out.emit_event(d, e.lane, e.ch, t + pl.dt, i64::from(e.value)) {
                            return out.check();
                        }
                    }
                }
                out.check()?;
            }
        }
        Ok(())
    }

    fn run_all(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let notes = &src.notes;
        if notes.is_empty() {
            return Ok(());
        }
        let lv = self.levels(src);
        let w = self.world(src, &lv);
        if self.new.at == Some(2) && w.positions.is_empty() {
            out.message =
                Some("At positions: the reference track has no chord changes to paste at.".into());
        }
        for (i, x) in notes.iter().enumerate() {
            if !self.emit_note(i, x, lv.at(i), &w, out) {
                return out.check();
            }
            if i & 0xFFF == 0 {
                out.check()?;
            }
        }
        if lv
            .all()
            .iter()
            .any(|c| c.events.as_ref().is_some_and(|e| e.0 > 0))
        {
            self.copy_events(src, &lv, &w, out)?;
        }
        out.check()
    }

    /// The exact number of notes before omission, thinning and the
    /// boundary policies (the preflight's bound with the source at hand).
    fn exact(&self, src: &Source) -> u64 {
        let lv = self.levels(src);
        let w = self.world(src, &lv);
        let mut total = 0u64;
        for (i, x) in src.notes.iter().enumerate() {
            let c = lv.at(i);
            let targets = w.targets(x.track, c.tracks.is_some()).len() as u64;
            let (reps, _) = self.stutter(i, c, &w);
            let per = 1 + reps.map_or(0, |r| r.0);
            total = total.saturating_add(1).saturating_add(
                self.count(c, &w)
                    .saturating_mul(targets)
                    .saturating_mul(per),
            );
        }
        total
    }
}

impl Kernel for Dup9 {
    fn growth(&self) -> Growth {
        self.growth
    }

    fn estimate_source(&self, src: &Source) -> Estimate {
        let n = self.exact(src).min(self.estimate(&src.stats).notes);
        Estimate::for_notes(src.notes.len() as u64, n)
    }

    fn as_point(&self) -> Option<&dyn PointKernel> {
        if self.point.is_some() {
            Some(self)
        } else {
            None
        }
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        if self.point.is_some() {
            let w = World {
                subs: src.tracks.iter().map(|t| t.subs.clone()).collect(),
                positions: self.fixed.positions.clone(),
                ..World::default()
            };
            for x in &src.notes {
                if !self.emit_note(usize::MAX, x, &self.new, &w, out) {
                    return out.check();
                }
            }
            return out.check();
        }
        self.run_all(src, out)
    }
}

impl PointKernel for Dup9 {
    fn map(&self, n: &TNote, out: &mut Output) {
        self.emit_note(usize::MAX, n, &self.new, &self.fixed, out);
    }

    fn source_window(&self, t0: u64, t1: u64) -> (u64, u64) {
        let (behind, ahead) = self.point.unwrap_or((0, 0));
        (t0.saturating_sub(behind), t1.saturating_add(ahead))
    }
}

/// Duplicate's source preparation (V09-S39 step 7): n01's destination
/// tracks (the tracks after each selected track at the listed offsets, the
/// listed tracks by number, or the track's subtracks) as routes; n03's
/// reference notes (a track by number, 0 = the active track); n08's
/// controller points in the selection's span on each selected track.
pub fn duplicate_prepare(
    doc: &crate::model::PlumDocument,
    p: &Params,
    src: &mut Source,
    limit: usize,
    ctx: Option<&super::super::Ctx>,
) -> Result<(), TransformError> {
    if src.notes.is_empty() {
        return Ok(());
    }
    let scoped: Vec<(u16, super::super::SourceTrack)> = src
        .tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| !t.insert_only)
        .map(|(s, t)| (s as u16, t.clone()))
        .collect();
    // B2-F16: the edited notes' place in the song, and the track the edited
    // clip sits on (offsets and subtracks count from it).
    let shift = ctx.map_or(0, |c| c.shift);
    let host = ctx.and_then(|c| c.host);
    if p.flag("n01") {
        let kind = p.choice("n01.targets");
        let list = p.list("n01.tracks");
        let mut routes: Vec<(u16, u16)> = Vec::new();
        for (slot, t) in &scoped {
            let base = match host {
                Some(h) if *slot == 0 => h,
                _ => t.index,
            };
            let want: Vec<usize> = match kind {
                0 => list
                    .iter()
                    .filter_map(|&o| usize::try_from(o).ok().filter(|&o| o >= 1))
                    .map(|o| base + o)
                    .collect(),
                1 => list
                    .iter()
                    .filter_map(|&n| usize::try_from(n - 1).ok())
                    .collect(),
                _ => doc.children_of(base),
            };
            for ti in want {
                if doc.track(ti).is_none() {
                    continue;
                }
                // The clip the target track plays, at its place (B2-F16).
                let Some(dest) = super::super::source::placed_destination(doc, ti, shift) else {
                    continue;
                };
                let d = match src
                    .tracks
                    .iter()
                    .position(|x| x.host() == dest.host() && x.frame == dest.frame)
                {
                    Some(d) => d,
                    None if src
                        .tracks
                        .iter()
                        .any(|x| x.host() == dest.host() && !x.insert_only) =>
                    {
                        return Err(TransformError::Invalid(format!(
                            "To tracks: track {} shows the notes being edited (a linked copy of this clip) at another place; choose another track.",
                            ti + 1
                        )));
                    }
                    None => {
                        src.tracks.push(dest);
                        src.tracks.len() - 1
                    }
                } as u16;
                if !routes.contains(&(*slot, d)) {
                    routes.push((*slot, d));
                }
            }
        }
        if routes.is_empty() {
            return Err(TransformError::Invalid(
                match kind {
                    0 => "To tracks: no track follows at the listed offsets (1 = the next track).",
                    1 => "To tracks: none of the listed tracks exists. List tracks by their number in the track list.",
                    _ => "To tracks: the track has no subtracks to paste onto.",
                }
                .into(),
            ));
        }
        src.routes = Arc::new(routes);
    }
    if p.flag("n03") && p.choice("n03.at") == 2 {
        let num = p.int("n03.ref");
        let Some((_, active)) = scoped.first() else {
            return Ok(());
        };
        let mut v = Vec::new();
        if num == 0 {
            if let Ok(t) = crate::editor::note_view(doc, active.target()) {
                for n in t.notes.iter().take(limit) {
                    v.push(TNote::from_record(&n.record(), u16::MAX));
                }
            }
        } else {
            let i = usize::try_from(num - 1)
                .ok()
                .filter(|&i| doc.track(i).is_some())
                .ok_or_else(|| {
                    TransformError::Invalid(format!(
                        "At positions: track {num} does not exist. Choose the reference track by its number in the track list (0 = this track)."
                    ))
                })?;
            // B2-F16: the reference track as the song shows it.
            v = Source::placed_layer(doc, i, u16::MAX, limit, shift);
        }
        super::super::source::sort_time_major(&mut v);
        src.aux = Arc::new(vec![v]);
    }
    if p.flag("n08") && p.choice("n08.include") > 0 {
        let bend = p.choice("n08.include") == 2;
        let ccs = p.list("n08.ccs");
        let (a, b) = (src.stats.min_start, src.stats.max_end);
        let mut ev: Vec<TEvent> = Vec::new();
        for (slot, t) in &scoped {
            let Ok(view) = crate::editor::note_view(doc, t.target()) else {
                continue;
            };
            for e in view.events().events() {
                if e.tick < a || e.tick >= b {
                    continue;
                }
                let crate::model::events::RawKind::Channel { status, data } = e.kind else {
                    continue;
                };
                let ch = status & 0x0F;
                let (lane, value) = match status & 0xF0 {
                    0xB0 if data[0] < 120
                        && (ccs.is_empty() || ccs.contains(&i64::from(data[0]))) =>
                    {
                        (Lane::Cc(data[0]), u16::from(data[1]))
                    }
                    0xE0 if bend => (
                        Lane::PitchBend,
                        u16::from(data[0] & 0x7F) | u16::from(data[1] & 0x7F) << 7,
                    ),
                    _ => continue,
                };
                if ev.len() >= MAX_EVENTS {
                    break;
                }
                ev.push(TEvent {
                    track: *slot,
                    lane,
                    ch,
                    tick: e.tick,
                    value,
                });
            }
        }
        src.aux_events = Arc::new(ev);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spread_ranks_take_every_prefix_evenly() {
        assert_eq!(spread_ranks(1), vec![0]);
        // 0 4 2 6 1 5 3 7: the first two are 0 and 4, the first four 0 2 4 6.
        assert_eq!(spread_ranks(8), vec![0, 4, 2, 6, 1, 5, 3, 7]);
        let r = spread_ranks(5);
        let mut s = r.clone();
        s.sort_unstable();
        assert_eq!(s, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn chord_changes_are_onsets_with_new_pitch_classes() {
        let n = |start: u64, pitch: u8| TNote {
            start,
            dur: 10,
            pitch,
            vel: 90,
            ..TNote::default()
        };
        // C E at 0, C E an octave up at 100 (the same classes), D at 200.
        let v = vec![n(0, 60), n(0, 64), n(100, 72), n(100, 76), n(200, 62)];
        assert_eq!(chord_changes(&v), vec![0, 200]);
        assert_eq!(tidy_positions(vec![200, 0, 200, 50], 50), vec![0, 200]);
    }
}
