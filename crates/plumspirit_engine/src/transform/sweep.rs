//! The Control Sweep engine (v0.9 contract §099, §210, §234).
//!
//! A tool declares its sweep targets ([`SweepTarget`]: the parameter, the
//! domain, the range, how the swept value meets the parameter's own value);
//! the user draws a graph per target. This engine evaluates the graphs and
//! turns them into parameter sets: every note's place along each enabled
//! sweep's domain is quantized into levels, the notes are grouped by their
//! levels, and each group is processed with the parameters at its levels —
//! so any tool's parameter can be swept without the tool changing.
//!
//! Levels are exact at both ends of a domain (level 0 = position 0, the last
//! level = position 1), at most [`MAX_LEVELS`] per sweep and [`MAX_GROUPS`]
//! groups per run (the resolution halves until the groups fit). Everything
//! is deterministic: the same notes and graphs give the same groups.

use super::TNote;
use super::definition::{Combine, SweepTarget};
use super::drivers::{Frame, Place, measure};
use super::param::{Curve, ParamKind, ParamSet, ParamSpec, ParamValue};
use crate::model::project::KeyRange;

/// Most levels one sweep is quantized into.
pub const MAX_LEVELS: u32 = 64;
/// Most groups (kernel builds) one run uses.
pub const MAX_GROUPS: usize = 256;

/// An enabled sweep: a declared target and the user's graph over its domain.
#[derive(Clone, Debug, PartialEq)]
pub struct Sweep {
    pub target: SweepTarget,
    pub graph: Curve,
}

impl Sweep {
    /// The graph's value at domain position `u` (0 … 1), in the target's range.
    pub fn value(&self, u: f64) -> f64 {
        let (lo, hi) = self.target.range;
        lo + self.graph.eval(u.clamp(0.0, 1.0)) * (hi - lo)
    }
}

/// The parameter value for swept value `v` combined with `base`, inside the
/// parameter's range in key range `keys` (integers rounded). Kinds that are
/// not numeric keep `base`.
pub fn combine(
    spec: &ParamSpec,
    base: &ParamValue,
    v: f64,
    how: Combine,
    keys: KeyRange,
) -> ParamValue {
    let mix = |b: f64| match how {
        Combine::Replace => v,
        Combine::Add => b + v,
        Combine::Multiply => b * v,
    };
    match (&spec.kind, base) {
        (ParamKind::Int { .. } | ParamKind::Pitch, ParamValue::Int(b)) => {
            let (lo, hi) = spec.range_in(keys).unwrap_or((i64::MIN, i64::MAX));
            let r = mix(*b as f64).round();
            ParamValue::Int(if r.is_nan() {
                *b
            } else {
                (r.clamp(lo as f64, hi as f64)) as i64
            })
        }
        (ParamKind::Ticks { min, max }, ParamValue::Int(b)) => {
            let r = mix(*b as f64).round();
            let (lo, hi) = (*min as f64, (*max).min(i64::MAX as u64) as f64);
            ParamValue::Int(if r.is_nan() {
                *b
            } else {
                r.clamp(lo, hi) as i64
            })
        }
        (ParamKind::Float { min, max, .. }, ParamValue::Float(b)) => {
            let r = mix(*b);
            ParamValue::Float(if r.is_nan() { *b } else { r.clamp(*min, *max) })
        }
        _ => base.clone(),
    }
}

/// Level of domain position `u` among `q` levels (exact ends).
#[inline]
pub fn level(u: f64, q: u32) -> u32 {
    if q <= 1 {
        return 0;
    }
    ((u.clamp(0.0, 1.0) * f64::from(q - 1)).round() as u32).min(q - 1)
}

/// Domain position of level `l` among `q` levels.
#[inline]
pub fn position(l: u32, q: u32) -> f64 {
    if q <= 1 {
        0.0
    } else {
        f64::from(l.min(q - 1)) / f64::from(q - 1)
    }
}

/// The group key of a note: its level along each sweep, packed (6 bits per
/// sweep — at most ten sweeps).
#[inline]
pub fn key(sweeps: &[Sweep], n: &TNote, place: Place, f: &Frame, bar: u64, q: u32) -> u64 {
    let mut k = 0u64;
    for (i, s) in sweeps.iter().enumerate() {
        let (_, u) = measure(s.target.domain, n, place, f, bar);
        k |= u64::from(level(u, q)) << (6 * i);
    }
    k
}

/// The levels packed in `key`.
pub fn levels(key: u64, sweeps: usize) -> Vec<u32> {
    (0..sweeps)
        .map(|i| ((key >> (6 * i)) & 63) as u32)
        .collect()
}

/// The parameter set for the group `key`: `base` with every swept target
/// combined with the graph's value at its level.
pub fn params_for(
    base: &ParamSet,
    specs: &[ParamSpec],
    sweeps: &[Sweep],
    key: u64,
    q: u32,
    keys: KeyRange,
) -> ParamSet {
    let mut set = base.clone();
    for (s, l) in sweeps.iter().zip(levels(key, sweeps.len())) {
        let Some(spec) = specs.iter().find(|p| p.key == s.target.param) else {
            continue;
        };
        let cur = set
            .get_raw(spec.key)
            .cloned()
            .unwrap_or_else(|| spec.default_in(keys).into_owned());
        let v = s.value(position(l, q));
        set.set(spec.key, combine(spec, &cur, v, s.target.combine, keys));
    }
    set
}

/// Groups `notes` (with their places) by level key, halving the resolution
/// until at most [`MAX_GROUPS`] groups remain. Returns the resolution used
/// and, per group in key order, the key and the indices of its notes.
pub fn group(
    sweeps: &[Sweep],
    notes: &[TNote],
    places: &[Place],
    f: &Frame,
    bar: u64,
) -> (u32, Vec<(u64, Vec<usize>)>) {
    let mut q = MAX_LEVELS;
    loop {
        let mut keyed: Vec<(u64, usize)> = notes
            .iter()
            .zip(places)
            .enumerate()
            .map(|(i, (n, p))| (key(sweeps, n, *p, f, bar, q), i))
            .collect();
        keyed.sort_unstable();
        let mut groups: Vec<(u64, Vec<usize>)> = Vec::new();
        for (k, i) in keyed {
            match groups.last_mut() {
                Some((gk, v)) if *gk == k => v.push(i),
                _ => groups.push((k, vec![i])),
            }
        }
        if groups.len() <= MAX_GROUPS || q <= 2 {
            return (q, groups);
        }
        q /= 2;
    }
}

/// Control Sweeps a tool evaluates itself, once per unit it processes
/// (V09-0143: tools whose output does not come note by note from the
/// selection — Group Notes' groups; later Draw / Paint strokes). The
/// platform does not split the source for them (that would cut the units
/// apart): [`super::functions::wrap`] builds the kernel with these sweeps in
/// [`super::Ctx::sweeps`], and the kernel asks [`UnitSweeps::int`] /
/// [`UnitSweeps::float`] for a parameter's value at each unit, measured at
/// the unit's representative note (a group's first note) and place (Note
/// index = the unit's place among the units). The graph is read at the
/// exact domain position — no levels, as no kernel is rebuilt per value.
#[derive(Clone, Debug, PartialEq)]
pub struct UnitSweeps {
    sweeps: Vec<Sweep>,
    specs: std::sync::Arc<Vec<ParamSpec>>,
    frame: Frame,
    bar: u64,
    keys: KeyRange,
}

/// The level groups of [`UnitSweeps::levels`]: per group, its key (the
/// levels packed as [`key`] packs them) and the indices of its notes.
#[derive(Clone, Debug, PartialEq)]
pub struct SweepLevels {
    sweeps: Vec<Sweep>,
    q: u32,
    pub groups: Vec<(u64, Vec<usize>)>,
}

/// Each note's place for a per-note sweep inside a kernel (time-major index
/// and count on its track slot, its subchannel's position in the track's
/// list) — what the platform's note-level sweep measures, for kernels that
/// evaluate their sweeps per note ([`super::definition::kernel_sweeps`]).
pub fn note_places(src: &super::Source) -> Vec<Place> {
    places_of(src, &src.notes)
}

/// [`note_places`] of any note list on `src`'s track slots in time-major
/// order (V09-S38 step 3: Clipping Mask's Vice Versa art is the base
/// track's notes).
pub fn places_of(src: &super::Source, notes: &[TNote]) -> Vec<Place> {
    let slots = notes
        .iter()
        .map(|n| usize::from(n.track) + 1)
        .max()
        .unwrap_or(0);
    let mut count = vec![0usize; slots];
    for n in notes {
        count[usize::from(n.track)] += 1;
    }
    let mut seen = vec![0usize; slots];
    notes
        .iter()
        .map(|n| {
            let t = usize::from(n.track);
            let tr = src.track(n.track);
            let place = Place {
                index: seen[t],
                count: count[t],
                sub_pos: tr.and_then(|x| x.sub_index(n.sub)).unwrap_or(0),
                subs: tr.map_or(1, |x| x.subs.len().max(1)),
                ..Place::default()
            };
            seen[t] += 1;
            place
        })
        .collect()
}

impl UnitSweeps {
    /// The enabled `sweeps` of a tool with parameter `specs`, measured in
    /// `frame` (the selection's) with pattern periods of `bar` ticks.
    pub fn new(
        sweeps: Vec<Sweep>,
        specs: std::sync::Arc<Vec<ParamSpec>>,
        frame: Frame,
        bar: u64,
        keys: KeyRange,
    ) -> UnitSweeps {
        UnitSweeps {
            sweeps,
            specs,
            frame,
            bar,
            keys,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.sweeps.is_empty()
    }

    /// The same sweeps measured in `frame` (a kernel whose units are not
    /// the selection's notes).
    pub fn in_frame(&self, frame: Frame) -> UnitSweeps {
        UnitSweeps {
            frame,
            ..self.clone()
        }
    }

    /// For parameters a kernel cannot take per note because each value
    /// rebuilds a structure (V09-S38 step 3: Clipping Mask's mask): `notes`
    /// (at `places`) grouped by the levels of the enabled sweeps of `keys`
    /// — the platform's quantization ([`group`]: at most [`MAX_LEVELS`]
    /// levels per sweep, at most [`MAX_GROUPS`] groups). `None` when no
    /// sweep drives any of `keys`.
    pub fn levels(&self, keys: &[&str], notes: &[TNote], places: &[Place]) -> Option<SweepLevels> {
        let sweeps: Vec<Sweep> = self
            .sweeps
            .iter()
            .filter(|s| keys.contains(&s.target.param))
            .cloned()
            .collect();
        if sweeps.is_empty() {
            return None;
        }
        let (q, groups) = group(&sweeps, notes, places, &self.frame, self.bar);
        Some(SweepLevels { sweeps, q, groups })
    }

    /// Parameter `key`'s value for level key `group` of `lv`: `base`
    /// combined with each of `lv`'s sweeps of `key` at its level (the rule
    /// of [`params_for`]; `base` when none of them drives `key`).
    pub fn level_value(
        &self,
        lv: &SweepLevels,
        group: u64,
        key: &str,
        base: ParamValue,
    ) -> ParamValue {
        let mut v = base;
        let Some(spec) = self.specs.iter().find(|p| p.key == key) else {
            return v;
        };
        for (s, l) in lv.sweeps.iter().zip(levels(group, lv.sweeps.len())) {
            if s.target.param == key {
                v = combine(
                    spec,
                    &v,
                    s.value(position(l, lv.q)),
                    s.target.combine,
                    self.keys,
                );
            }
        }
        v
    }

    /// [`UnitSweeps::level_value`] of an Int / Ticks parameter.
    pub fn level_int(&self, lv: &SweepLevels, group: u64, key: &str, base: i64) -> i64 {
        match self.level_value(lv, group, key, ParamValue::Int(base)) {
            ParamValue::Int(v) => v,
            _ => base,
        }
    }

    /// [`UnitSweeps::level_value`] of a Float parameter.
    pub fn level_float(&self, lv: &SweepLevels, group: u64, key: &str, base: f64) -> f64 {
        match self.level_value(lv, group, key, ParamValue::Float(base)) {
            ParamValue::Float(v) => v,
            _ => base,
        }
    }

    /// Whether an enabled sweep drives parameter `key`.
    pub fn drives(&self, key: &str) -> bool {
        self.sweeps.iter().any(|s| s.target.param == key)
    }

    /// Parameter `key`'s value for the unit represented by note `n` at
    /// `place`: `base` combined with each enabled sweep of `key` in turn
    /// (within the parameter's range; `base` when nothing sweeps it).
    pub fn value(&self, key: &str, base: ParamValue, n: &TNote, place: Place) -> ParamValue {
        let mut v = base;
        for s in self.sweeps.iter().filter(|s| s.target.param == key) {
            let Some(spec) = self.specs.iter().find(|p| p.key == key) else {
                continue;
            };
            let (_, u) = measure(s.target.domain, n, place, &self.frame, self.bar);
            v = combine(spec, &v, s.value(u), s.target.combine, self.keys);
        }
        v
    }

    /// [`UnitSweeps::value`] of an Int / Ticks parameter.
    pub fn int(&self, key: &str, base: i64, n: &TNote, place: Place) -> i64 {
        match self.value(key, ParamValue::Int(base), n, place) {
            ParamValue::Int(v) => v,
            _ => base,
        }
    }

    /// [`UnitSweeps::value`] of a Float parameter.
    pub fn float(&self, key: &str, base: f64, n: &TNote, place: Place) -> f64 {
        match self.value(key, ParamValue::Float(base), n, place) {
            ParamValue::Float(v) => v,
            _ => base,
        }
    }

    /// `notes` (at `places`) grouped by the levels of every enabled sweep —
    /// the platform's quantization ([`group`]) — each group with its
    /// parameter set from `base` ([`params_for`]): the values a note-level
    /// sweep gives each note, for a kernel that reads them per note while
    /// its passes see the whole selection ([`PerLevel`]).
    pub fn level_sets(&self, base: &ParamSet, notes: &[TNote], places: &[Place]) -> LevelSets {
        let (q, groups) = group(&self.sweeps, notes, places, &self.frame, self.bar);
        let mut group_of = vec![0u32; notes.len()];
        let mut sets = Vec::with_capacity(groups.len());
        for (g, (k, idx)) in groups.into_iter().enumerate() {
            for i in idx {
                group_of[i] = g as u32;
            }
            sets.push(params_for(base, &self.specs, &self.sweeps, k, q, self.keys));
        }
        LevelSets { group_of, sets }
    }

    /// The range of values the enabled sweeps of `key` can give it from
    /// `base` (`base` alone when none drives it): each sweep's value lies
    /// in its target's range (graphs stay within 0 … 1), combined in turn
    /// (interval arithmetic) and clamped to `[min, max]` — the bound a
    /// kernel's estimate uses.
    fn bounds(&self, key: &str, base: f64, min: f64, max: f64) -> (f64, f64) {
        fold_bounds(
            self.sweeps
                .iter()
                .map(|s| &s.target)
                .filter(|t| t.param == key),
            base,
            min,
            max,
        )
    }

    /// [`UnitSweeps::bounds`] of an Int / Ticks parameter (rounded
    /// outward).
    pub fn int_bounds(&self, key: &str, base: i64) -> (i64, i64) {
        if !self.drives(key) {
            return (base, base);
        }
        let Some(spec) = self.specs.iter().find(|p| p.key == key) else {
            return (base, base);
        };
        let (min, max) = match spec.kind {
            ParamKind::Ticks { min, max } => (min as f64, max.min(i64::MAX as u64) as f64),
            _ => {
                let (lo, hi) = spec.range_in(self.keys).unwrap_or((i64::MIN, i64::MAX));
                (lo as f64, hi as f64)
            }
        };
        let (a, b) = self.bounds(key, base as f64, min, max);
        (a.floor() as i64, b.ceil() as i64)
    }

    /// [`UnitSweeps::bounds`] of a Float parameter.
    pub fn float_bounds(&self, key: &str, base: f64) -> (f64, f64) {
        if !self.drives(key) {
            return (base, base);
        }
        match self.specs.iter().find(|p| p.key == key).map(|p| &p.kind) {
            Some(ParamKind::Float { min, max, .. }) => self.bounds(key, base, *min, *max),
            _ => (base, base),
        }
    }

    /// Every combination of the swept parameters at the ends of their
    /// ranges ([`UnitSweeps::int_bounds`] / `float_bounds`) on `base`: a
    /// kernel whose bound grows or shrinks monotonically with each takes
    /// the largest over these as its estimate (2^k sets for k swept
    /// parameters, k ≤ ten).
    pub fn corner_sets(&self, base: &ParamSet) -> Vec<ParamSet> {
        let mut keys: Vec<&'static str> = self.sweeps.iter().map(|s| s.target.param).collect();
        keys.sort_unstable();
        keys.dedup();
        let mut ends: Vec<(&'static str, ParamValue, ParamValue)> = Vec::new();
        for k in keys {
            let Some(spec) = self.specs.iter().find(|p| p.key == k) else {
                continue;
            };
            let cur = base
                .get_raw(k)
                .cloned()
                .unwrap_or_else(|| spec.default_in(self.keys).into_owned());
            match cur {
                ParamValue::Int(v) => {
                    let (a, b) = self.int_bounds(k, v);
                    ends.push((k, ParamValue::Int(a), ParamValue::Int(b)));
                }
                ParamValue::Float(v) => {
                    let (a, b) = self.float_bounds(k, v);
                    ends.push((k, ParamValue::Float(a), ParamValue::Float(b)));
                }
                _ => {}
            }
        }
        let mut out = Vec::with_capacity(1 << ends.len());
        for mask in 0u32..(1 << ends.len()) {
            let mut set = base.clone();
            for (j, (k, a, b)) in ends.iter().enumerate() {
                set.set(
                    k,
                    if mask >> j & 1 == 0 {
                        a.clone()
                    } else {
                        b.clone()
                    },
                );
            }
            out.push(set);
        }
        out
    }
}

/// The range `targets` (each driving one parameter in turn) can give a
/// parameter from `base`: each sweep's value lies in its target's range
/// (graphs stay within 0 … 1), combined in turn (interval arithmetic) and
/// clamped to `[min, max]`.
fn fold_bounds<'a>(
    targets: impl Iterator<Item = &'a SweepTarget>,
    base: f64,
    min: f64,
    max: f64,
) -> (f64, f64) {
    let (mut a, mut b) = (base, base);
    for t in targets {
        let (r0, r1) = t.range;
        let (lo, hi) = (r0.min(r1), r0.max(r1));
        (a, b) = match t.combine {
            Combine::Replace => (lo, hi),
            Combine::Add => (a + lo, b + hi),
            Combine::Multiply => {
                let c = [a * lo, a * hi, b * lo, b * hi];
                (
                    c.iter().copied().fold(f64::INFINITY, f64::min),
                    c.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                )
            }
        };
        (a, b) = (a.clamp(min, max), b.clamp(min, max));
    }
    (a, b)
}

/// The largest value the tool's enabled Control Sweeps can give the Ticks
/// parameter `key` (its own value without one): for a tool's `prepare`,
/// which sizes the context from the parameters before the kernel sees the
/// sweeps (E0422).
pub fn swept_ticks_max(p: &super::param::Params, id: &str, key: &str) -> u64 {
    let base = p.ticks(key);
    if !p.flag(super::functions::SWEEP) {
        return base;
    }
    let (min, max) = match p.specs.iter().find(|s| s.key == key).map(|s| &s.kind) {
        Some(ParamKind::Ticks { min, max }) => (*min as f64, *max as f64),
        _ => return base,
    };
    let targets = super::definition::sweep_targets(id)
        .iter()
        .filter(|t| t.param == key && p.flag(t.on));
    let (_, b) = fold_bounds(targets, base as f64, min, max);
    (b.ceil() as u64).max(base)
}

/// A generator's Control Sweeps over its own units (E0422: Chord
/// Progression's chords, Riff Machine's notes, the Randomizer's generated
/// notes — their output does not come from the selected notes, so the
/// platform's levels of the selection would run the generator once per
/// level). Each unit is measured at its place among the units: its index
/// (Note index), its start and pitch within the units' span and range
/// (Time, Pattern phase, Pitch), its own velocity; one subchannel.
#[derive(Clone, Debug)]
pub struct UnitPlaces {
    sw: UnitSweeps,
    units: Vec<TNote>,
}

impl UnitSweeps {
    /// These sweeps over the generated `units` (representative notes in
    /// time order), measured in their own frame.
    pub fn over(&self, units: Vec<TNote>) -> UnitPlaces {
        UnitPlaces {
            sw: self.in_frame(Frame::of_notes(&units)),
            units,
        }
    }
}

impl UnitPlaces {
    pub fn len(&self) -> usize {
        self.units.len()
    }

    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    /// `base` with every swept parameter at unit `i`'s value (each sweep
    /// in turn, as [`params_for`] combines them, at the unit's exact
    /// place).
    pub fn set_at(&self, base: &ParamSet, i: usize) -> ParamSet {
        let n = &self.units[i];
        let place = Place {
            index: i,
            count: self.units.len(),
            sub_pos: 0,
            subs: 1,
            ..Place::default()
        };
        let mut set = base.clone();
        for s in &self.sw.sweeps {
            let Some(spec) = self.sw.specs.iter().find(|p| p.key == s.target.param) else {
                continue;
            };
            let cur = set
                .get_raw(spec.key)
                .cloned()
                .unwrap_or_else(|| spec.default_in(self.sw.keys).into_owned());
            let (_, u) = measure(s.target.domain, n, place, &self.sw.frame, self.sw.bar);
            set.set(
                spec.key,
                combine(spec, &cur, s.value(u), s.target.combine, self.sw.keys),
            );
        }
        set
    }
}

/// A kernel's own parameters with its Control Sweeps evaluated at any unit
/// it makes (E0422: generators measure their units — a slot, a bar, a
/// burst, an echo — at their own note and place, in the selection's frame
/// or one of their own, [`SweptParams::in_frame`]).
#[derive(Clone, Debug)]
pub struct SweptParams {
    sw: std::sync::Arc<UnitSweeps>,
    set: ParamSet,
    specs: std::sync::Arc<Vec<ParamSpec>>,
}

impl SweptParams {
    /// `Some` when `b`'s context carries Control Sweeps.
    pub fn of(p: &super::param::Params, b: &super::registry::BuildCtx) -> Option<SweptParams> {
        let sw = b.ctx.sweeps.clone().filter(|s| !s.is_empty())?;
        Some(SweptParams {
            sw,
            set: p.set.clone(),
            specs: std::sync::Arc::new(p.specs.to_vec()),
        })
    }

    /// The same, measured in `frame`.
    pub fn in_frame(&self, frame: Frame) -> SweptParams {
        SweptParams {
            sw: std::sync::Arc::new(self.sw.in_frame(frame)),
            ..self.clone()
        }
    }

    /// Whether an enabled sweep drives `key`.
    pub fn drives(&self, key: &str) -> bool {
        self.sw.drives(key)
    }

    fn params(&self) -> super::param::Params<'_> {
        super::param::Params::new(&self.specs, &self.set).with_keys(self.sw.keys)
    }

    /// Float parameter `key` for the unit represented by `n` at `place`
    /// (its own value when no sweep drives it).
    pub fn float(&self, key: &str, n: &TNote, place: Place) -> f64 {
        let base = self.params().float(key);
        if self.sw.drives(key) {
            self.sw.float(key, base, n, place)
        } else {
            base
        }
    }

    /// Int / Ticks parameter `key` for the unit represented by `n` at
    /// `place`.
    pub fn int(&self, key: &str, n: &TNote, place: Place) -> i64 {
        let base = self.params().int(key);
        if self.sw.drives(key) {
            self.sw.int(key, base, n, place)
        } else {
            base
        }
    }

    /// The range the sweeps can give Float `key` ([`UnitSweeps::float_bounds`]).
    pub fn float_bounds(&self, key: &str) -> (f64, f64) {
        self.sw.float_bounds(key, self.params().float(key))
    }

    /// The range the sweeps can give Int / Ticks `key`.
    pub fn int_bounds(&self, key: &str) -> (i64, i64) {
        self.sw.int_bounds(key, self.params().int(key))
    }
}

/// [`UnitSweeps::level_sets`]: per note its group, per group its parameter
/// set.
#[derive(Clone, Debug, PartialEq)]
pub struct LevelSets {
    pub group_of: Vec<u32>,
    pub sets: Vec<ParamSet>,
}

/// A tool's own settings per note under Control Sweeps (V09-0143, E0422):
/// the tools whose passes read across notes — next onsets, chords,
/// phrases, windows, walks, ranks, seeded streams by index — run once over
/// the whole selection ([`super::definition::kernel_sweeps`]: the platform
/// does not split it by levels, which would leave each pass one level of
/// notes) and take each note's settings here, built by the tool from the
/// parameter set of the note's level ([`UnitSweeps::level_sets`]: the same
/// values the platform's note-level sweep gave). Without sweeps there is
/// one setting for every note.
#[derive(Clone, Debug)]
pub struct PerLevel<C> {
    group_of: Vec<u32>,
    cfgs: Vec<C>,
}

impl<C> PerLevel<C> {
    /// One setting for every note.
    pub fn one(c: C) -> PerLevel<C> {
        PerLevel {
            group_of: Vec::new(),
            cfgs: vec![c],
        }
    }

    /// `src`'s notes' settings under `ctx`'s sweeps: `make` builds a
    /// setting from each level's parameters (`base` and `specs`: the
    /// tool's), one setting from `base` without sweeps.
    pub fn of(
        ctx: &super::Ctx,
        src: &super::Source,
        base: &ParamSet,
        specs: &[ParamSpec],
        make: impl FnMut(&super::param::Params) -> C,
    ) -> PerLevel<C> {
        Self::of_notes(ctx, src, &src.notes, base, specs, make)
    }

    /// [`PerLevel::of`] for another note list on `src`'s track slots (in
    /// time-major order, like the source's).
    pub fn of_notes(
        ctx: &super::Ctx,
        src: &super::Source,
        notes: &[TNote],
        base: &ParamSet,
        specs: &[ParamSpec],
        mut make: impl FnMut(&super::param::Params) -> C,
    ) -> PerLevel<C> {
        let keys = ctx.keys;
        match ctx.sweeps.as_deref().filter(|s| !s.is_empty()) {
            Some(sw) if !notes.is_empty() => {
                let places = places_of(src, notes);
                let ls = sw.level_sets(base, notes, &places);
                PerLevel {
                    group_of: ls.group_of,
                    cfgs: ls
                        .sets
                        .iter()
                        .map(|s| make(&super::param::Params::new(specs, s).with_keys(keys)))
                        .collect(),
                }
            }
            _ => PerLevel::one(make(
                &super::param::Params::new(specs, base).with_keys(keys),
            )),
        }
    }

    /// Note `i`'s setting.
    #[inline]
    pub fn at(&self, i: usize) -> &C {
        let g = self.group_of.get(i).map_or(0, |&g| g as usize);
        &self.cfgs[g]
    }

    /// Note `i`'s group (0 without sweeps).
    #[inline]
    pub fn group(&self, i: usize) -> usize {
        self.group_of.get(i).map_or(0, |&g| g as usize)
    }

    /// Every distinct setting (one per level group).
    pub fn all(&self) -> &[C] {
        &self.cfgs
    }

    /// The first group's setting (the only one without sweeps).
    pub fn first(&self) -> &C {
        &self.cfgs[0]
    }

    /// Whether the settings differ by note.
    pub fn swept(&self) -> bool {
        self.cfgs.len() > 1
    }
}

/// What a kernel keeps to rebuild its settings per Control Sweep level
/// ([`PerLevel`], E0422): the tool's parameters and the selection it was
/// built for.
#[derive(Clone, Debug)]
pub struct Rebuild {
    pub set: ParamSet,
    pub specs: Vec<ParamSpec>,
    pub stats: super::SourceStats,
    pub tracks: Vec<super::SourceTrack>,
}

impl Rebuild {
    /// `Some` when `b`'s context carries Control Sweeps.
    pub fn of(
        p: &super::param::Params,
        b: &super::registry::BuildCtx,
    ) -> Option<std::sync::Arc<Rebuild>> {
        b.ctx.sweeps.as_ref().filter(|s| !s.is_empty())?;
        Some(std::sync::Arc::new(Rebuild {
            set: p.set.clone(),
            specs: p.specs.to_vec(),
            stats: b.stats.clone(),
            tracks: b.tracks.to_vec(),
        }))
    }

    /// `src`'s notes' settings: `make` builds one from each level's
    /// parameters and this selection's build context.
    pub fn levels<C>(
        &self,
        ctx: &super::Ctx,
        src: &super::Source,
        mut make: impl FnMut(&super::param::Params, &super::registry::BuildCtx) -> C,
    ) -> PerLevel<C> {
        let b = super::registry::BuildCtx {
            ctx,
            stats: &self.stats,
            tracks: &self.tracks,
        };
        PerLevel::of(ctx, src, &self.set, &self.specs, |p| make(p, &b))
    }

    /// The settings at every corner of the swept ranges
    /// ([`UnitSweeps::corner_sets`]): what an estimate takes its largest
    /// bound over.
    pub fn corners<C>(
        &self,
        ctx: &super::Ctx,
        mut make: impl FnMut(&super::param::Params, &super::registry::BuildCtx) -> C,
    ) -> Vec<C> {
        let b = super::registry::BuildCtx {
            ctx,
            stats: &self.stats,
            tracks: &self.tracks,
        };
        let sets = match ctx.sweeps.as_deref() {
            Some(sw) => sw.corner_sets(&self.set),
            None => vec![self.set.clone()],
        };
        sets.iter()
            .map(|s| {
                make(
                    &super::param::Params::new(&self.specs, s).with_keys(ctx.keys),
                    &b,
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transform::definition::SweepDomain;
    use crate::transform::param::{float, int, ticks};

    fn target(
        param: &'static str,
        domain: SweepDomain,
        lo: f64,
        hi: f64,
        how: Combine,
    ) -> SweepTarget {
        SweepTarget {
            id: "t",
            on: "m.sweep.t",
            graph: "m.sweep.t.graph",
            label: "T",
            graph_label: "T graph",
            param,
            domain,
            range: (lo, hi),
            combine: how,
        }
    }

    fn note(start: u64, pitch: u8) -> TNote {
        TNote {
            start,
            dur: 10,
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

    #[test]
    fn levels_are_exact_at_both_ends() {
        assert_eq!(level(0.0, 64), 0);
        assert_eq!(level(1.0, 64), 63);
        assert_eq!(level(0.5, 3), 1);
        assert_eq!(level(-1.0, 8), 0);
        assert_eq!(level(9.0, 8), 7);
        assert_eq!(position(0, 64), 0.0);
        assert_eq!(position(63, 64), 1.0);
        assert_eq!(level(0.3, 1), 0);
        for q in [2, 3, 17, 64] {
            for l in 0..q {
                assert_eq!(level(position(l, q), q), l);
            }
        }
    }

    #[test]
    fn swept_values_combine_and_stay_in_range() {
        let i = int("amount", "Amount", -50, 50, 10);
        let f = float("depth", "Depth", 0.0, 2.0, 1.0, "");
        let t = ticks("len", "Length", 1, 960, 240);
        let k = KeyRange::Standard;
        assert_eq!(
            combine(&i, &ParamValue::Int(10), 7.4, Combine::Replace, k),
            ParamValue::Int(7)
        );
        assert_eq!(
            combine(&i, &ParamValue::Int(10), 7.6, Combine::Add, k),
            ParamValue::Int(18)
        );
        assert_eq!(
            combine(&i, &ParamValue::Int(10), 9.0, Combine::Multiply, k),
            ParamValue::Int(50)
        );
        assert_eq!(
            combine(&f, &ParamValue::Float(1.0), 0.5, Combine::Multiply, k),
            ParamValue::Float(0.5)
        );
        assert_eq!(
            combine(&f, &ParamValue::Float(1.0), -3.0, Combine::Add, k),
            ParamValue::Float(0.0)
        );
        assert_eq!(
            combine(&t, &ParamValue::Int(240), 0.0, Combine::Replace, k),
            ParamValue::Int(1)
        );
        assert_eq!(
            combine(&i, &ParamValue::Int(3), f64::NAN, Combine::Replace, k),
            ParamValue::Int(3)
        );
        // A sweep's value follows its graph across the range.
        let s = Sweep {
            target: target("amount", SweepDomain::Time, -20.0, 20.0, Combine::Add),
            graph: Curve::line(0.0, 1.0),
        };
        assert_eq!(
            (s.value(0.0), s.value(0.5), s.value(1.0)),
            (-20.0, 0.0, 20.0)
        );
    }

    #[test]
    fn notes_group_by_their_levels_and_get_the_levels_parameters() {
        let specs = vec![
            int("amount", "Amount", -100, 100, 0),
            int("other", "Other", 0, 9, 4),
        ];
        let sweeps = vec![Sweep {
            target: target("amount", SweepDomain::Time, 0.0, 100.0, Combine::Replace),
            graph: Curve::line(0.0, 1.0),
        }];
        let notes: Vec<TNote> = (0..5).map(|i| note(i * 100, 60)).collect();
        let places = vec![Place::default(); 5];
        let f = Frame::of_notes(&notes);
        let (q, groups) = group(&sweeps, &notes, &places, &f, 3840);
        assert_eq!(q, MAX_LEVELS);
        assert_eq!(groups.len(), 5, "five distinct times");
        let base = ParamSet::defaults(&specs);
        let vals: Vec<i64> = groups
            .iter()
            .map(|(k, _)| {
                match params_for(&base, &specs, &sweeps, *k, q, KeyRange::Standard)
                    .get_raw("amount")
                {
                    Some(ParamValue::Int(v)) => *v,
                    _ => -1,
                }
            })
            .collect();
        assert_eq!(vals.first(), Some(&0));
        assert_eq!(vals.last(), Some(&100));
        assert!(vals.windows(2).all(|w| w[0] < w[1]));
        // Other parameters are untouched.
        let set = params_for(&base, &specs, &sweeps, groups[2].0, q, KeyRange::Standard);
        assert_eq!(set.get_raw("other"), base.get_raw("other"));
        // Every note is in exactly one group.
        let mut all: Vec<usize> = groups.iter().flat_map(|g| g.1.clone()).collect();
        all.sort_unstable();
        assert_eq!(all, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn many_groups_lower_the_resolution_until_they_fit() {
        let mk = |p, d| Sweep {
            target: target(p, d, 0.0, 1.0, Combine::Replace),
            graph: Curve::line(0.0, 1.0),
        };
        let sweeps = vec![mk("a", SweepDomain::Time), mk("b", SweepDomain::Pitch)];
        // 4,096 notes on a 64 × 64 time × pitch lattice: 4,096 groups at 64
        // levels, 256 at 16.
        let notes: Vec<TNote> = (0..4096u64)
            .map(|i| note((i % 64) * 10, 40 + (i / 64) as u8))
            .collect();
        let places = vec![Place::default(); notes.len()];
        let f = Frame::of_notes(&notes);
        let (q, groups) = group(&sweeps, &notes, &places, &f, 3840);
        assert_eq!(q, 16);
        assert!(groups.len() <= MAX_GROUPS);
        assert_eq!(groups.iter().map(|g| g.1.len()).sum::<usize>(), notes.len());
        // Deterministic.
        assert_eq!(group(&sweeps, &notes, &places, &f, 3840), (q, groups));
    }

    #[test]
    fn unit_sweeps_read_the_graph_at_each_units_exact_place() {
        let specs = std::sync::Arc::new(vec![
            int("amount", "Amount", -100, 100, 0),
            float("depth", "Depth", 0.0, 2.0, 1.0, ""),
            int("other", "Other", 0, 9, 4),
        ]);
        let sweeps = vec![
            Sweep {
                target: target(
                    "amount",
                    SweepDomain::NoteIndex,
                    0.0,
                    90.0,
                    Combine::Replace,
                ),
                graph: Curve::line(0.0, 1.0),
            },
            Sweep {
                target: target("depth", SweepDomain::Time, 0.5, 1.5, Combine::Multiply),
                graph: Curve::line(0.0, 1.0),
            },
        ];
        let notes: Vec<TNote> = (0..4).map(|i| note(i * 1000, 60)).collect();
        let u = UnitSweeps::new(
            sweeps,
            specs,
            Frame::of_notes(&notes),
            3840,
            KeyRange::Standard,
        );
        assert!(u.drives("amount") && u.drives("depth") && !u.drives("other"));
        // Unit k of 10 (Note index): 90 k / 9 — exact, no levels.
        let place = |index| Place {
            index,
            count: 10,
            ..Place::default()
        };
        let a: Vec<i64> = (0..10)
            .map(|k| u.int("amount", 5, &notes[0], place(k)))
            .collect();
        assert_eq!(a, (0..10).map(|k| 10 * k).collect::<Vec<_>>());
        // Time: 0.5 at the first start, 1.5 at the last, × the base.
        let d: Vec<f64> = notes
            .iter()
            .map(|n| u.float("depth", 1.0, n, place(0)))
            .collect();
        assert_eq!(d, vec![0.5, 0.5 + 1.0 / 3.0, 0.5 + 2.0 / 3.0, 1.5]);
        // An unswept parameter keeps its base.
        assert_eq!(u.int("other", 7, &notes[2], place(3)), 7);
    }
}
