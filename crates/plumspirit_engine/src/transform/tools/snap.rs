//! Snap to Scale's v0.9 kernel (V09-S39 step 5, `docs/TRANSFORM_ENGINE.md`
//! "Snap to Scale"): the functions declared by id beside the v0.8 kernel
//! (`edit.rs` `snap_build`, untouched), with Control Sweeps per note.

use super::super::param::*;
use super::super::registry::BuildCtx;
use super::super::shared::metric_position::{bar_place, metric_position};
use super::super::sweep::{PerLevel, Rebuild};
use super::super::theory::{KEY_NAMES, Scale, SnapMode, scale_names};
use super::super::{Growth, Kernel, Output, Source, SourceStats, TNote, TransformError};
use super::edit::sub_index;
use super::{note_value, pos};

// ===================================================== Snap to Scale (v0.9)

pub(super) const SNAP_CANDIDATES: &[&str] = &["Major and minor", "All modes", "All scales"];
pub(super) const SNAP_WINDOW: &[&str] = &["The whole selection", "Every bar", "Every N bars"];
pub(super) const SNAP_TUNING: &[&str] = &[
    "Just intonation",
    "24-TET (the listed cents to quarter tones)",
    "Pythagorean",
    "Listed cents",
];
pub(super) const SNAP_CHORD: &[&str] = &[
    "Tonic triad",
    "Degree triad per bar (on its lowest note)",
    "Chord list (one per bar)",
];
pub(super) const SNAP_PER: &[&str] = &["Bar", "Beat", "Chord length"];
pub(super) const SNAP_RULES: &[&str] = &["Melodic minor", "Bebop major", "Bebop dominant"];

/// Snap to Scale's prepared layer in [`Source::aux`] ([`snap_prepare`]):
/// n05's reference notes.
const SNAP_AUX_REF: usize = 0;

/// 5-limit just intonation and Pythagorean ratios by semitones above the
/// root (n03).
const JUST_RATIOS: [(f64, f64); 12] = [
    (1.0, 1.0),
    (16.0, 15.0),
    (9.0, 8.0),
    (6.0, 5.0),
    (5.0, 4.0),
    (4.0, 3.0),
    (45.0, 32.0),
    (3.0, 2.0),
    (8.0, 5.0),
    (5.0, 3.0),
    (9.0, 5.0),
    (15.0, 8.0),
];
const PYTH_RATIOS: [(f64, f64); 12] = [
    (1.0, 1.0),
    (256.0, 243.0),
    (9.0, 8.0),
    (32.0, 27.0),
    (81.0, 64.0),
    (4.0, 3.0),
    (729.0, 512.0),
    (3.0, 2.0),
    (128.0, 81.0),
    (27.0, 16.0),
    (16.0, 9.0),
    (243.0, 128.0),
];

/// Candidate scales of n01 by its choice (menu indices of `scale_names`):
/// major and minor; the seven modes; every scale but the chromatic one.
fn detect_candidates(which: usize) -> &'static [usize] {
    match which {
        0 => &[0, 1],
        1 => &[0, 4, 5, 6, 7, 1, 8],
        _ => &[
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
        ],
    }
}

/// n01: the key and scale fitting the notes best — the largest total
/// length of notes in the scale; on a tie the scale whose root sounds
/// longest, then the one with fewer pitch classes, then the earlier menu
/// entry, then the lower root. `weights[pc]`: total length per pitch class.
fn detect_scale(weights: &[u64; 12], which: usize) -> (usize, usize) {
    // (fit, root's length, −classes, −menu index, −root), (root, scale).
    type Ranked = ((u64, u64, i64, i64, i64), (usize, usize));
    let mut best: Option<Ranked> = None;
    for &si in detect_candidates(which) {
        for root in 0..12usize {
            let mask = Scale::new(root, si).mask();
            let fit: u64 = (0..12)
                .filter(|pc| mask & (1 << pc) != 0)
                .fold(0u64, |a, pc| a.saturating_add(weights[pc]));
            let key = (
                fit,
                weights[root],
                -i64::from(mask.count_ones()),
                -(si as i64),
                -(root as i64),
            );
            if best.as_ref().is_none_or(|b| key > b.0) {
                best = Some((key, (root, si)));
            }
        }
    }
    best.map_or((0, 0), |b| b.1)
}

/// Snap to Scale's v0.8 settings (the v0.8 kernel's closure reads the same
/// values; [`Snap9`] re-reads them here, unchanged).
#[derive(Clone, Debug)]
struct Snap8 {
    root: usize,
    base: Scale,
    weights: Vec<i64>,
    dir: Option<usize>,
    melodic: bool,
    chord: bool,
    zones: Vec<i64>,
    sub_scales: Vec<i64>,
    tie: Option<usize>,
    morph: Option<Scale>,
    bar: u64,
}

impl Snap8 {
    fn from(p: &Params, b: &BuildCtx) -> Snap8 {
        let root = p.choice("key");
        let base = if p.adv(1) {
            let mask = p
                .list("a1.pcs")
                .iter()
                .fold(0u16, |m, &c| m | 1 << c.rem_euclid(12));
            Scale::from_mask(root as u8, mask.max(1))
        } else {
            Scale::new(root, p.choice("scale"))
        };
        Snap8 {
            root,
            base,
            weights: if p.adv(2) {
                p.list("a2.weights")
            } else {
                Vec::new()
            },
            dir: p.adv(3).then(|| p.choice("a3.dir")),
            melodic: p.adv(4),
            chord: p.adv(5),
            zones: if p.adv(6) {
                p.list("a6.scales")
            } else {
                Vec::new()
            },
            sub_scales: if p.adv(7) {
                p.list("a7.scales")
            } else {
                Vec::new()
            },
            tie: p.adv(8).then(|| p.choice("a8.tie")),
            morph: p.adv(9).then(|| Scale::new(root, p.choice("a9.to"))),
            bar: b.ctx.bar(),
        }
    }
}

/// Chord names (`C Am F#m7 …`) as pitch-class masks.
fn chord_masks(what: &str, text: &str) -> Result<Vec<u16>, TransformError> {
    let mut masks = Vec::new();
    for tok in text
        .split([' ', ',', ';', '|'])
        .filter(|t| !t.trim().is_empty())
    {
        let c = super::super::theory::parse_chord(tok.trim()).ok_or_else(|| {
            TransformError::Invalid(format!(
                "{what}: “{}” is not a chord name (for example C, Am, F#m7, Bbmaj7).",
                tok.trim()
            ))
        })?;
        masks.push(c.pitch_classes());
    }
    if masks.is_empty() {
        return Err(TransformError::Invalid(format!(
            "{what}: type at least one chord, for example “Am F C G”."
        )));
    }
    Ok(masks)
}

/// Snap to Scale's settings of the functions declared by id (V09-S39 step
/// 5), per Control Sweep level ([`PerLevel`]), with a10's mark (swept too).
#[derive(Clone, Debug, Default)]
pub(super) struct SnapNew {
    /// n01: candidates, window (0 whole, 1 bar, 2 N bars), N.
    detect: Option<(usize, usize, u64)>,
    /// n02: leave notes shorter than (ticks), leave weak beats.
    passing: Option<(u64, bool)>,
    /// n03: tuning (0 just, 1 24-TET, 2 Pythagorean, 3 listed), cents,
    /// bend range (semitones), allocate free channels.
    tuning: Option<(usize, Vec<i64>, i64, bool)>,
    /// n04: avoid unisons, keep the spacing, chord tolerance (ticks).
    voices: Option<(bool, bool, u64)>,
    /// n05: reference kind (0 track, 1 subchannel), number, per bar, least
    /// occurrences.
    reference: Option<(usize, i64, bool, u64)>,
    /// n06: grid (ticks), chord kind (0 tonic, 1 degree per bar, 2 list),
    /// the list's masks.
    metric: Option<(u64, usize, Vec<u16>)>,
    /// n07: palette (subchannel positions by distance).
    palette: Option<Vec<i64>>,
    /// n08: approach window (ticks).
    approach: Option<u64>,
    /// n09: chords, per (0 bar, 1 beat, 2 chord length), chord length.
    progression: Option<(Vec<u16>, usize, u64)>,
    /// n10: rule (0 melodic minor, 1 bebop major, 2 bebop dominant), key.
    direction: Option<(usize, usize)>,
    /// n11: largest shift (semitones), chord tolerance (ticks).
    shift: Option<(i64, u64)>,
    /// a10 (v0.8): mark with subchannel position.
    mark: Option<i64>,
}

impl SnapNew {
    /// The settings (chord texts that do not parse give no chords: the
    /// build refuses them first, [`SnapNew::check`]).
    pub(super) fn from(p: &Params, ctx: &super::super::Ctx) -> SnapNew {
        SnapNew {
            detect: p.flag("n01").then(|| {
                (
                    p.choice("n01.cands"),
                    p.choice("n01.window"),
                    p.int("n01.bars").max(1) as u64,
                )
            }),
            passing: p
                .flag("n02")
                .then(|| (p.ticks("n02.short"), p.choice("n02.weak") == 1)),
            tuning: p.flag("n03").then(|| {
                (
                    p.choice("n03.tuning"),
                    p.list("n03.cents"),
                    p.int("n03.range").clamp(1, 48),
                    p.choice("n03.channels") == 1,
                )
            }),
            voices: p.flag("n04").then(|| {
                (
                    p.choice("n04.unisons") == 0,
                    p.choice("n04.prefer") == 1,
                    p.ticks("n04.tol"),
                )
            }),
            reference: p.flag("n05").then(|| {
                (
                    p.choice("n05.ref"),
                    p.int("n05.num"),
                    p.choice("n05.window") == 1,
                    p.int("n05.least").max(1) as u64,
                )
            }),
            metric: p.flag("n06").then(|| {
                (
                    note_value(p.choice("n06.grid"), 0, ctx).max(1),
                    p.choice("n06.chord"),
                    chord_masks("Metric hierarchy", &p.text("n06.chords")).unwrap_or_default(),
                )
            }),
            palette: p.flag("n07").then(|| p.list("n07.palette")),
            approach: p.flag("n08").then(|| p.ticks("n08.window")),
            progression: p.flag("n09").then(|| {
                (
                    chord_masks("Chord progression snap", &p.text("n09.chords"))
                        .unwrap_or_default(),
                    p.choice("n09.per"),
                    p.ticks("n09.len").max(1),
                )
            }),
            direction: p
                .flag("n10")
                .then(|| (p.choice("n10.rule"), p.choice("n10.key"))),
            shift: p
                .flag("n11")
                .then(|| (p.int("n11.shift").max(1), p.ticks("n11.tol"))),
            mark: p.adv(10).then(|| p.int("a10.sub")),
        }
    }

    /// Refuses chord lists that do not parse (with a sentence).
    fn check(p: &Params) -> Result<(), TransformError> {
        if p.flag("n06") && p.choice("n06.chord") == 2 {
            chord_masks("Metric hierarchy", &p.text("n06.chords"))?;
        }
        if p.flag("n09") {
            chord_masks("Chord progression snap", &p.text("n09.chords"))?;
        }
        Ok(())
    }

    /// Whether any function declared by id is on (a10 is v0.8's).
    pub(super) fn any(&self) -> bool {
        self.detect.is_some()
            || self.passing.is_some()
            || self.tuning.is_some()
            || self.voices.is_some()
            || self.reference.is_some()
            || self.metric.is_some()
            || self.palette.is_some()
            || self.approach.is_some()
            || self.progression.is_some()
            || self.direction.is_some()
            || self.shift.is_some()
    }
}

/// The v0.9 Snap to Scale kernel (V09-S39 step 5): the v0.8 rule re-read
/// from the same parameters, with the functions declared by id at their
/// stages and the Control Sweeps evaluated per note inside the kernel
/// (n01's windows, n04 / n11's chords, n08's next notes and n10's lines
/// read across notes).
pub(super) struct Snap9 {
    v8: Snap8,
    new: SnapNew,
    rebuild: Option<std::sync::Arc<Rebuild>>,
    ctx: super::super::Ctx,
    stats: SourceStats,
}

/// A distinct scale (root, mask) in a run's table: notes keep its index,
/// not a copy.
#[derive(Default)]
struct ScaleTable {
    scales: Vec<Scale>,
    at: super::super::shared::fast_hash::FastMap<(u8, u16), u32>,
}

impl ScaleTable {
    fn id(&mut self, s: Scale) -> u32 {
        let key = (s.root, s.mask());
        if let Some(&i) = self.at.get(&key) {
            return i;
        }
        let i = self.scales.len() as u32;
        self.scales.push(s);
        self.at.insert(key, i);
        i
    }
}

impl Snap9 {
    pub(super) fn build(
        p: &Params,
        b: &BuildCtx,
        new: SnapNew,
    ) -> Result<Box<dyn Kernel>, TransformError> {
        SnapNew::check(p)?;
        Ok(Box::new(Snap9 {
            v8: Snap8::from(p, b),
            new,
            rebuild: Rebuild::of(p, b),
            ctx: b.ctx.clone(),
            stats: b.stats.clone(),
        }))
    }

    /// The bar index of `tick` from the selection's first bar.
    fn bar_of(&self, tick: u64, first: u64) -> u64 {
        bar_place(&self.ctx.meter, tick).0.saturating_sub(first)
    }

    fn run_all(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let notes = &src.notes;
        let n = notes.len();
        let lv: PerLevel<SnapNew> = match &self.rebuild {
            Some(r) => r.levels(&self.ctx, src, |p, b| SnapNew::from(p, b.ctx)),
            None => PerLevel::one(self.new.clone()),
        };
        let v = &self.v8;
        let stats = &self.stats;
        let first_bar = notes
            .iter()
            .map(|x| x.start)
            .min()
            .map_or(0, |t| bar_place(&self.ctx.meter, t).0);
        let mut table = ScaleTable::default();
        let mut message: Vec<String> = Vec::new();
        // ---- 1. Each note's scale: v0.8's base, or (n01) the detected one
        // of its window; then v0.8's zones (a6), subchannel scales (a7) and
        // morph (a9).
        let mut base_of: Vec<u32> = Vec::with_capacity(n);
        {
            // (window kind, N, window) → detected scale id.
            let mut found: super::super::shared::fast_hash::FastMap<(usize, u64, u64, usize), u32> =
                Default::default();
            let mut weights: super::super::shared::fast_hash::FastMap<
                (usize, u64, u64),
                [u64; 12],
            > = Default::default();
            let window = |cfg: &SnapNew, t: u64| -> Option<(usize, u64, u64, usize)> {
                cfg.detect.map(|(cands, w, bars)| {
                    let bar = self.bar_of(t, first_bar);
                    match w {
                        0 => (0, 0, 0, cands),
                        1 => (1, 1, bar, cands),
                        _ => (2, bars, bar / bars, cands),
                    }
                })
            };
            if (0..lv.all().len()).any(|g| lv.all()[g].detect.is_some()) {
                // Weights per window (each window kind / N a level uses).
                for x in notes {
                    for cfg in lv.all() {
                        if let Some((k, nb, w, _)) = window(cfg, x.start) {
                            let e = weights.entry((k, nb, w)).or_insert([0; 12]);
                            let pc = usize::from(x.pitch % 12);
                            e[pc] = e[pc].saturating_add(x.dur.max(1));
                        }
                    }
                }
            }
            let base_id = table.id(v.base.clone());
            for (i, x) in notes.iter().enumerate() {
                let id = match window(lv.at(i), x.start) {
                    Some(key) => *found.entry(key).or_insert_with(|| {
                        let w = weights
                            .get(&(key.0, key.1, key.2))
                            .copied()
                            .unwrap_or([0; 12]);
                        let (root, si) = detect_scale(&w, key.3);
                        table.id(Scale::new(root, si))
                    }),
                    None => base_id,
                };
                base_of.push(id);
            }
            if !found.is_empty() {
                let mut names: Vec<(u64, u64, String)> = found
                    .iter()
                    .map(|(k, &id)| {
                        let s = &table.scales[id as usize];
                        (k.1, k.2, scale_label(s))
                    })
                    .collect();
                names.sort();
                names.dedup_by(|a, b| a.2 == b.2 && a.0 == b.0);
                let list: Vec<String> = names.iter().take(4).map(|x| x.2.clone()).collect();
                message.push(format!(
                    "Detected {}{}.",
                    list.join(", "),
                    if names.len() > 4 { ", …" } else { "" }
                ));
            }
        }
        let mut sc_of: Vec<u32> = Vec::with_capacity(n);
        for (i, x) in notes.iter().enumerate() {
            let mut sc = table.scales[base_of[i] as usize].clone();
            let changed = !v.zones.is_empty() || !v.sub_scales.is_empty() || v.morph.is_some();
            if !v.zones.is_empty() {
                let z = (x.start.saturating_sub(stats.min_start) / v.bar.max(1)) as usize;
                sc = Scale::new(v.root, v.zones[z % v.zones.len()] as usize);
            }
            if !v.sub_scales.is_empty() {
                let si = sub_index(src, x);
                sc = Scale::new(v.root, v.sub_scales[si % v.sub_scales.len()] as usize);
            }
            if let Some(to) = &v.morph
                && pos(x, stats) >= 0.5
            {
                sc = to.clone();
            }
            sc_of.push(if changed { table.id(sc) } else { base_of[i] });
        }
        // ---- 2. Each note's target keys (a pitch-class mask): the scale's,
        // or v0.8's chord-aware mask (a5); n05's reference classes, n06's
        // chord on strong positions, n09's progression chord, n10's
        // directional scale — the latest that applies wins.
        let mut mask: Vec<u16> = sc_of
            .iter()
            .map(|&s| table.scales[s as usize].mask())
            .collect();
        // The root a note's tuning is taken from (n03).
        let mut root_of: Vec<u8> = sc_of
            .iter()
            .map(|&s| table.scales[s as usize].root)
            .collect();
        if v.chord {
            let mut i = 0;
            while i < n {
                let mut j = i;
                while j < n && notes[j].start == notes[i].start {
                    j += 1;
                }
                let lo = notes[i..j].iter().map(|x| x.pitch).min().unwrap_or(0) as i64;
                let sc = &table.scales[base_of[i] as usize];
                let (d, _) = sc.degree_of(sc.snap(lo, SnapMode::Nearest));
                let m = (0..3).fold(0u16, |m, k| m | 1 << sc.pitch_of(d + 2 * k).rem_euclid(12));
                for x in mask.iter_mut().take(j).skip(i) {
                    *x = m;
                }
                i = j;
            }
        }
        if (0..lv.all().len()).any(|g| lv.all()[g].reference.is_some()) {
            let refs: Vec<TNote> = if src.aux.len() > SNAP_AUX_REF {
                src.aux[SNAP_AUX_REF].clone()
            } else {
                // Not prepared (no document at hand): the source's own notes.
                let (kind, num) = self.new.reference.map_or((0, 0), |r| (r.0, r.1));
                let own = notes.iter().chain(src.context.iter());
                match kind {
                    1 => {
                        let id = src
                            .tracks
                            .first()
                            .and_then(|t| t.subs.get(usize::try_from(num).unwrap_or(usize::MAX)))
                            .map(|s| s.0);
                        own.filter(|x| Some(x.sub) == id).copied().collect()
                    }
                    _ if num == 0 => own.copied().collect(),
                    _ => Vec::new(),
                }
            };
            // Occurrences per pitch class: overall, and per bar.
            let mut all = [0u64; 12];
            let mut by_bar: super::super::shared::fast_hash::FastMap<u64, [u64; 12]> =
                Default::default();
            for r in &refs {
                let pc = usize::from(r.pitch % 12);
                all[pc] += 1;
                by_bar
                    .entry(self.bar_of(r.start, first_bar))
                    .or_insert([0; 12])[pc] += 1;
            }
            let mut empty = 0usize;
            for (i, x) in notes.iter().enumerate() {
                let Some((_, _, per_bar, least)) = lv.at(i).reference else {
                    continue;
                };
                let counts = if per_bar {
                    by_bar
                        .get(&self.bar_of(x.start, first_bar))
                        .copied()
                        .unwrap_or([0; 12])
                } else {
                    all
                };
                let m = (0..12).fold(
                    0u16,
                    |m, pc| if counts[pc] >= least { m | 1 << pc } else { m },
                );
                if m == 0 {
                    empty += 1;
                } else {
                    mask[i] = m;
                }
            }
            if empty > 0 {
                message.push(format!(
                    "Reference scale: no pitch class occurs often enough for {empty} note(s); they keep the scale."
                ));
            }
        }
        if (0..lv.all().len()).any(|g| lv.all()[g].metric.is_some()) {
            // Per bar, its lowest note (degree triads).
            let mut lowest: super::super::shared::fast_hash::FastMap<u64, u8> = Default::default();
            for x in notes {
                let b = self.bar_of(x.start, first_bar);
                let e = lowest.entry(b).or_insert(x.pitch);
                *e = (*e).min(x.pitch);
            }
            for (i, x) in notes.iter().enumerate() {
                let Some((grid, kind, list)) = &lv.at(i).metric else {
                    continue;
                };
                let (_, bar_start, _) = bar_place(&self.ctx.meter, x.start);
                if !(x.start - bar_start).is_multiple_of(*grid) {
                    continue;
                }
                let b = self.bar_of(x.start, first_bar);
                let sc = &table.scales[sc_of[i] as usize];
                let triad = |d: i64| {
                    (0..3).fold(0u16, |m, k| m | 1 << sc.pitch_of(d + 2 * k).rem_euclid(12))
                };
                mask[i] = match kind {
                    0 => triad(0),
                    1 => {
                        let lo = i64::from(lowest.get(&b).copied().unwrap_or(x.pitch));
                        triad(sc.degree_of(sc.snap(lo, SnapMode::Nearest)).0)
                    }
                    _ if list.is_empty() => mask[i],
                    _ => list[(b % list.len() as u64) as usize],
                };
            }
        }
        if (0..lv.all().len()).any(|g| lv.all()[g].progression.is_some()) {
            let t0 = stats.min_start;
            for (i, x) in notes.iter().enumerate() {
                let Some((chords, per, len)) = &lv.at(i).progression else {
                    continue;
                };
                if chords.is_empty() {
                    continue;
                }
                let k = match per {
                    0 => self.bar_of(x.start, first_bar),
                    1 => {
                        let m = metric_position(&self.ctx.meter, x.start);
                        let per_bar = (m.bar_len / m.beat_len.max(1)).max(1);
                        self.bar_of(x.start, first_bar) * per_bar + m.beat
                    }
                    _ => x.start.saturating_sub(t0) / len.max(&1),
                };
                mask[i] = chords[(k % chords.len() as u64) as usize];
            }
        }
        if (0..lv.all().len()).any(|g| lv.all()[g].direction.is_some()) {
            let ups = line_directions(notes);
            for (i, up) in ups.into_iter().enumerate() {
                let Some((rule, key)) = lv.at(i).direction else {
                    continue;
                };
                let k = key as u16;
                let pc = |s: u16| 1u16 << ((k + s) % 12);
                mask[i] = match (rule, up) {
                    (0, true) => Scale::new(key, 3).mask(),
                    (0, false) => Scale::new(key, 1).mask(),
                    (1, true) => Scale::new(key, 0).mask(),
                    (1, false) => Scale::new(key, 0).mask() | pc(8),
                    (_, true) => Scale::new(key, 7).mask(),
                    (_, false) => Scale::new(key, 7).mask() | pc(11),
                };
                root_of[i] = key as u8;
            }
        }
        let ok = |i: usize, q: i64| mask[i] & (1 << q.rem_euclid(12)) != 0;
        // ---- 3. Exceptions kept as written: n02's short notes (and weak
        // beats), n08's approach tones.
        let mut keep = vec![false; n];
        if (0..lv.all().len()).any(|g| lv.all()[g].passing.is_some()) {
            for (i, x) in notes.iter().enumerate() {
                if let Some((short, weak)) = lv.at(i).passing {
                    let off_beat = weak && {
                        let m = metric_position(&self.ctx.meter, x.start);
                        m.beat_start != x.start
                    };
                    if x.dur < short || off_beat {
                        keep[i] = true;
                    }
                }
            }
        }
        if (0..lv.all().len()).any(|g| lv.all()[g].approach.is_some()) {
            // Starts per (track, key), ascending.
            let mut at: super::super::shared::fast_hash::FastMap<(u16, u8), Vec<(u64, usize)>> =
                Default::default();
            for (j, x) in notes.iter().enumerate() {
                at.entry((x.track, x.pitch)).or_default().push((x.start, j));
            }
            for v in at.values_mut() {
                v.sort_unstable();
            }
            for (i, x) in notes.iter().enumerate() {
                let Some(w) = lv.at(i).approach else {
                    continue;
                };
                if keep[i] || ok(i, i64::from(x.pitch)) || w == 0 {
                    continue;
                }
                let hit = [x.pitch.checked_sub(1), x.pitch.checked_add(1)]
                    .into_iter()
                    .flatten()
                    .any(|q| {
                        at.get(&(x.track, q)).is_some_and(|v| {
                            let k = v.partition_point(|e| e.0 <= x.start);
                            v[k..]
                                .iter()
                                .take_while(|e| e.0 <= x.start.saturating_add(w))
                                .any(|e| ok(e.1, i64::from(q)))
                        })
                    });
                if hit {
                    keep[i] = true;
                }
            }
        }
        // ---- 4. The targets: n04's chord voicing or n11's block shift per
        // chord (decided first), else v0.8's rule per note.
        let mut decided: Vec<Option<i64>> = vec![None; n];
        let nearest = |i: usize| -> (i64, i64) {
            let p0 = i64::from(notes[i].pitch);
            if mask[i] == 0 {
                return (p0, p0);
            }
            let mut up = p0;
            while !ok(i, up) {
                up += 1;
            }
            let mut dn = p0;
            while !ok(i, dn) {
                dn -= 1;
            }
            (up, dn)
        };
        let mut no_voicing = 0usize;
        let use_shift = (0..lv.all().len()).any(|g| lv.all()[g].shift.is_some());
        let use_voices = (0..lv.all().len()).any(|g| lv.all()[g].voices.is_some());
        if use_shift || use_voices {
            for idx in super::per_track(src) {
                let mut a = 0;
                while a < idx.len() {
                    let first = idx[a];
                    let cfg = lv.at(first);
                    let tol = match (cfg.shift, cfg.voices) {
                        (Some((_, t)), _) => t,
                        (None, Some((_, _, t))) => t,
                        _ => {
                            a += 1;
                            continue;
                        }
                    };
                    let mut b = a + 1;
                    while b < idx.len()
                        && notes[idx[b]].start <= notes[first].start.saturating_add(tol)
                    {
                        b += 1;
                    }
                    let group = &idx[a..b];
                    if let Some((most, _)) = cfg.shift {
                        // n11: the smallest transposition within ±most putting
                        // the most notes on target keys (down first on a tie).
                        let count = |s: i64| {
                            group
                                .iter()
                                .filter(|&&i| !keep[i] && ok(i, i64::from(notes[i].pitch) + s))
                                .count()
                        };
                        // Shifts 0, −1, +1, −2, +2, …: a later one only when
                        // it puts strictly more notes on target keys.
                        let mut best = (count(0), 0i64);
                        for d in 1..=most {
                            for s in [-d, d] {
                                let c = count(s);
                                if c > best.0 {
                                    best = (c, s);
                                }
                            }
                        }
                        for &i in group {
                            if !keep[i] {
                                decided[i] = Some(i64::from(notes[i].pitch) + best.1);
                            }
                        }
                    } else if let Some((avoid, spacing, _)) = cfg.voices
                        && group.len() > 1
                    {
                        // n04: one dynamic programme over the chord's notes by
                        // key — each out-of-key note down or up, no two
                        // distinct keys meeting (when asked), the least total
                        // movement or spacing change.
                        let mut members: Vec<usize> = group.to_vec();
                        members.sort_by_key(|&i| (notes[i].pitch, i));
                        let cands: Vec<Vec<i64>> = members
                            .iter()
                            .map(|&i| {
                                let p0 = i64::from(notes[i].pitch);
                                if keep[i] || ok(i, p0) {
                                    vec![p0]
                                } else {
                                    let (up, dn) = nearest(i);
                                    vec![dn, up]
                                }
                            })
                            .collect();
                        let solve =
                            |strict: bool| voice_dp(&members, &cands, notes, strict, spacing);
                        let pick = if avoid {
                            solve(true).or_else(|| {
                                no_voicing += 1;
                                solve(false)
                            })
                        } else {
                            solve(false)
                        };
                        if let Some(ts) = pick {
                            for (k, &i) in members.iter().enumerate() {
                                decided[i] = Some(ts[k]);
                            }
                        }
                    }
                    a = b;
                }
            }
        }
        if no_voicing > 0 {
            message.push(format!(
                "Voice-aware snap: {no_voicing} chord(s) could not avoid unisons."
            ));
        }
        // v0.8's rule per note (its melodic memory runs over every note; a
        // map type of its own, so the v0.8 closure's stays inlined there).
        let mut prev: super::super::shared::fast_hash::FastMap<(u16, u32), (i64, i64)> =
            Default::default();
        let mut target: Vec<i64> = Vec::with_capacity(n);
        for (i, x) in notes.iter().enumerate() {
            let p0 = i64::from(x.pitch);
            let key = (x.track, x.sub);
            let pv = prev.get(&key).copied();
            let t = if keep[i] {
                p0
            } else if let Some(t) = decided[i] {
                t
            } else if ok(i, p0) || mask[i] == 0 {
                p0
            } else {
                let sc = &table.scales[sc_of[i] as usize];
                let (up, dn) = nearest(i);
                let (du, dd) = (up - p0, p0 - dn);
                let weight = |q: i64| -> i64 {
                    if v.weights.is_empty() {
                        return 0;
                    }
                    let (d, _) = sc.degree_of(q);
                    v.weights[(d.rem_euclid(sc.steps.len() as i64) as usize) % v.weights.len()]
                };
                if let Some((orig_prev, out_prev)) = pv.filter(|_| v.melodic) {
                    if p0 > orig_prev && dn <= out_prev {
                        up
                    } else if p0 < orig_prev && up >= out_prev {
                        dn
                    } else if du <= dd {
                        up
                    } else {
                        dn
                    }
                } else if let Some(d) = v.dir {
                    if d == 0 { up } else { dn }
                } else if !v.weights.is_empty() && weight(up) != weight(dn) {
                    if weight(up) > weight(dn) { up } else { dn }
                } else if du < dd {
                    up
                } else if dd < du {
                    dn
                } else {
                    match v.tie {
                        Some(0) => up,
                        Some(1) => dn,
                        Some(3) => dn,
                        Some(_) => {
                            if x.start.wrapping_add(i as u64).is_multiple_of(2) {
                                up
                            } else {
                                dn
                            }
                        }
                        None => dn,
                    }
                }
            };
            prev.insert(key, (p0, t));
            target.push(t);
        }
        // ---- 5. Output: a10's mark (v0.8), n07's colours by distance,
        // n03's tuning bends (and channels).
        let mut outs: Vec<(i64, TNote)> = Vec::with_capacity(n);
        for (i, x) in notes.iter().enumerate() {
            let cfg = lv.at(i);
            let p0 = i64::from(x.pitch);
            let t = target[i];
            let mut tpl = *x;
            if let Some(pos_sub) = cfg.mark
                && t != p0
                && let Some(tr) = src.track(x.track)
                && !tr.subs.is_empty()
            {
                let (id, ch) = tr.subs[(pos_sub.max(0) as usize) % tr.subs.len()];
                tpl.sub = id;
                tpl.ch = ch;
            }
            if let Some(pal) = cfg.palette.as_ref().filter(|p| !p.is_empty())
                && t != p0
                && let Some(tr) = src.track(x.track)
                && !tr.subs.is_empty()
            {
                let d = (t - p0).unsigned_abs() as usize;
                let pos = pal[d.min(pal.len()) - 1].max(0) as usize;
                tpl.sub = tr.subs[pos % tr.subs.len()].0;
            }
            let pitch = if cfg.mark.is_some() { p0 } else { t };
            outs.push((pitch, tpl));
        }
        let (rpn, bends) = if (0..lv.all().len()).any(|g| lv.all()[g].tuning.is_some()) {
            self.tune(src, &lv, &root_of, &mut outs, &mut message)
        } else {
            (Vec::new(), Vec::new())
        };
        for (pitch, tpl) in &outs {
            out.emit(
                i128::from(tpl.start),
                i128::from(tpl.dur),
                *pitch,
                i64::from(tpl.vel),
                tpl,
            );
            out.check()?;
        }
        // The bend range (RPN 0: CC 101 / 100 = 0, data entry MSB / LSB) just
        // before the bends that need it.
        for (track, ch, at, r) in rpn {
            let t0 = i128::from(at.saturating_sub(4));
            let lane = super::super::output::Lane::Cc;
            for (k, (cc, value)) in [(101u8, 0i64), (100, 0), (6, r), (38, 0)]
                .into_iter()
                .enumerate()
            {
                out.emit_event(track, lane(cc), ch, t0 + k as i128, value);
            }
        }
        for (track, ch, t, value) in bends {
            if !out.emit_event(
                track,
                super::super::output::Lane::PitchBend,
                ch,
                i128::from(t),
                value,
            ) {
                break;
            }
        }
        out.message = (!message.is_empty()).then(|| message.join(" "));
        out.check()
    }

    /// n03: each note's tuning (cents from equal temperament for its key
    /// above its scale's root) as a pitch-bend value; with "allocate" each
    /// note to a channel where nothing else sounds detuned differently
    /// (its own first, then 0 – 15 without 9; the unselected notes sound
    /// untuned); per channel a bend at each note's start when it changes
    /// (with the bend range — RPN 0 — before the first and whenever the
    /// range changes), and back to the centre after the channel's last note.
    /// Returns the range writes and the pitch-bend points (track, channel,
    /// tick, value).
    #[allow(clippy::type_complexity)]
    fn tune(
        &self,
        src: &Source,
        lv: &PerLevel<SnapNew>,
        root_of: &[u8],
        outs: &mut [(i64, TNote)],
        message: &mut Vec<String>,
    ) -> (Vec<(u16, u8, u64, i64)>, Vec<(u16, u8, u64, i64)>) {
        // (value, range) per note; None = untouched.
        let mut want: Vec<Option<(i64, i64)>> = Vec::with_capacity(outs.len());
        let mut allocate = false;
        for (i, (pitch, _)) in outs.iter().enumerate() {
            let Some((tuning, cents, range, alloc)) = &lv.at(i).tuning else {
                want.push(None);
                continue;
            };
            allocate |= *alloc;
            let iv = (pitch - i64::from(root_of[i])).rem_euclid(12) as usize;
            let listed = |k: usize| -> f64 {
                if cents.is_empty() {
                    0.0
                } else {
                    cents[k % cents.len()] as f64
                }
            };
            let c = match tuning {
                0 => 1200.0 * (JUST_RATIOS[iv].0 / JUST_RATIOS[iv].1).log2() - 100.0 * iv as f64,
                1 => (listed(iv) / 50.0).round() * 50.0,
                2 => 1200.0 * (PYTH_RATIOS[iv].0 / PYTH_RATIOS[iv].1).log2() - 100.0 * iv as f64,
                _ => listed(iv),
            };
            let value = (8192.0 + c / 100.0 / *range as f64 * 8192.0)
                .round()
                .clamp(0.0, 16_383.0) as i64;
            want.push(Some((value, *range)));
        }
        // Channels: with "allocate", greedy in start order per track.
        let mut moved = 0usize;
        let mut stuck = 0usize;
        if allocate {
            // Unselected notes per (track, channel): (start, end) by start
            // with the running latest end (they sound untuned).
            let mut fixed: super::super::shared::fast_hash::FastMap<(u16, u8), Vec<(u64, u64)>> =
                Default::default();
            for x in src.context.iter() {
                fixed
                    .entry((x.track, x.ch))
                    .or_default()
                    .push((x.start, x.end()));
            }
            let mut runs: super::super::shared::fast_hash::FastMap<
                (u16, u8),
                (Vec<u64>, Vec<u64>),
            > = Default::default();
            for (k, mut v) in fixed {
                v.sort_unstable();
                let mut m = 0;
                let ends = v
                    .iter()
                    .map(|x| {
                        m = m.max(x.1);
                        m
                    })
                    .collect();
                runs.insert(k, (v.into_iter().map(|x| x.0).collect(), ends));
            }
            let busy = |track: u16, ch: u8, s: u64, e: u64| {
                runs.get(&(track, ch)).is_some_and(|(st, en)| {
                    let k = st.partition_point(|&x| x < e);
                    k > 0 && en[k - 1] > s
                })
            };
            // Per (track, channel): (value, latest end) of the notes placed.
            let mut state: super::super::shared::fast_hash::FastMap<(u16, u8), (i64, u64)> =
                Default::default();
            let mut order: Vec<usize> = (0..outs.len()).filter(|&i| want[i].is_some()).collect();
            order.sort_by_key(|&i| (outs[i].1.track, outs[i].1.start, i));
            for i in order {
                let (value, _) = want[i].expect("filtered");
                let x = outs[i].1;
                let fits = |ch: u8| {
                    (value == 8192 || !busy(x.track, ch, x.start, x.end()))
                        && state
                            .get(&(x.track, ch))
                            .is_none_or(|&(w, end)| w == value || end <= x.start)
                };
                let pick = std::iter::once(x.ch)
                    .chain((0..16u8).filter(|&c| c != 9 && c != x.ch))
                    .find(|&c| fits(c));
                let ch = match pick {
                    Some(c) => c,
                    None => {
                        stuck += 1;
                        x.ch
                    }
                };
                if ch != x.ch {
                    moved += 1;
                    outs[i].1.ch = ch;
                }
                let e = state.entry((x.track, ch)).or_insert((value, 0));
                *e = (value, e.1.max(x.end()));
            }
        }
        // Bend points per (track, channel) in start order.
        let mut by_ch: super::super::shared::fast_hash::FastMap<(u16, u8), Vec<usize>> =
            Default::default();
        for (i, w) in want.iter().enumerate() {
            if w.is_some() {
                by_ch
                    .entry((outs[i].1.track, outs[i].1.ch))
                    .or_default()
                    .push(i);
            }
        }
        let mut keys: Vec<(u16, u8)> = by_ch.keys().copied().collect();
        keys.sort_unstable();
        let mut points = Vec::new();
        let mut rpn: Vec<(u16, u8, u64, i64)> = Vec::new();
        let mut conflicts = 0usize;
        for key in keys {
            let mut idx = by_ch.remove(&key).unwrap_or_default();
            idx.sort_by_key(|&i| (outs[i].1.start, i));
            let (mut cur, mut range, mut until) = (8192i64, 0i64, 0u64);
            let mut last_end = 0u64;
            for &i in &idx {
                let (value, r) = want[i].expect("collected");
                let x = outs[i].1;
                if value != cur {
                    if x.start < until {
                        conflicts += 1;
                    }
                    if r != range {
                        rpn.push((key.0, key.1, x.start, r));
                        range = r;
                    }
                    points.push((key.0, key.1, x.start, value));
                    cur = value;
                }
                until = until.max(x.end());
                last_end = last_end.max(x.end());
            }
            if cur != 8192 {
                points.push((key.0, key.1, last_end, 8192));
            }
        }
        let bent = points.iter().filter(|p| p.3 != 8192).count();
        if bent > 0 || moved > 0 {
            message.push(format!(
                "Tuning: {bent} pitch bend(s){}{}{}.",
                if moved > 0 {
                    format!(", {moved} note(s) to free channels")
                } else {
                    String::new()
                },
                if stuck > 0 {
                    format!(", {stuck} found no free channel")
                } else {
                    String::new()
                },
                if conflicts > 0 {
                    format!(", {conflicts} overlap a differently tuned note on their channel")
                } else {
                    String::new()
                }
            ));
        }
        (rpn, points)
    }
}

/// n10: whether each note's line goes up — towards its voice's next note
/// (same track and subchannel) of another key, else from the previous one;
/// up when neither differs.
fn line_directions(notes: &[TNote]) -> Vec<bool> {
    let mut by_voice: std::collections::HashMap<(u16, u32), Vec<usize>> = Default::default();
    for (i, x) in notes.iter().enumerate() {
        by_voice.entry((x.track, x.sub)).or_default().push(i);
    }
    let mut up = vec![true; notes.len()];
    for idx in by_voice.values() {
        for (k, &i) in idx.iter().enumerate() {
            let p = notes[i].pitch;
            let next = idx[k + 1..]
                .iter()
                .map(|&j| notes[j].pitch)
                .find(|&q| q != p);
            let prev = idx[..k]
                .iter()
                .rev()
                .map(|&j| notes[j].pitch)
                .find(|&q| q != p);
            up[i] = match (next, prev) {
                (Some(q), _) => q > p,
                (None, Some(q)) => p > q,
                _ => true,
            };
        }
    }
    up
}

/// n04: the chord's targets (members by key; `cands[k]`: the keys member
/// `k` may take) with the least total movement (or spacing change, then
/// movement) — consecutive members of distinct keys strictly rising when
/// `strict`. None when no choice satisfies it.
fn voice_dp(
    members: &[usize],
    cands: &[Vec<i64>],
    notes: &[TNote],
    strict: bool,
    spacing: bool,
) -> Option<Vec<i64>> {
    let m = members.len();
    let p = |k: usize| i64::from(notes[members[k]].pitch);
    // cost[k][c], from[k][c]
    let mut cost: Vec<Vec<Option<i64>>> = Vec::with_capacity(m);
    let mut from: Vec<Vec<usize>> = Vec::with_capacity(m);
    for k in 0..m {
        let mut row = Vec::with_capacity(cands[k].len());
        let mut back = Vec::with_capacity(cands[k].len());
        for &t in &cands[k] {
            let mv = (t - p(k)).abs();
            if k == 0 {
                row.push(Some(mv));
                back.push(0);
                continue;
            }
            let mut best: Option<(i64, usize)> = None;
            for (c, &u) in cands[k - 1].iter().enumerate() {
                let Some(prev) = cost[k - 1][c] else {
                    continue;
                };
                if strict && p(k) > p(k - 1) && t <= u {
                    continue;
                }
                let step = if spacing {
                    ((t - u) - (p(k) - p(k - 1))).abs() * 1000 + mv
                } else {
                    mv
                };
                let total = prev + step;
                if best.is_none_or(|b| total < b.0) {
                    best = Some((total, c));
                }
            }
            row.push(best.map(|b| b.0));
            back.push(best.map_or(0, |b| b.1));
        }
        cost.push(row);
        from.push(back);
    }
    let (mut c, _) = cost[m - 1]
        .iter()
        .enumerate()
        .filter_map(|(c, x)| x.map(|v| (c, v)))
        .fold(None::<(usize, i64)>, |b, (c, v)| match b {
            Some((_, bv)) if bv <= v => b,
            _ => Some((c, v)),
        })?;
    let mut out = vec![0i64; m];
    for k in (0..m).rev() {
        out[k] = cands[k][c];
        c = from[k][c];
    }
    Some(out)
}

/// "D major" for a scale (its root's name and its menu name).
fn scale_label(s: &Scale) -> String {
    let names = scale_names();
    let idx = (0..names.len()).find(|&i| Scale::new(usize::from(s.root), i).mask() == s.mask());
    format!(
        "{} {}",
        KEY_NAMES[usize::from(s.root) % 12],
        idx.map_or("(custom)", |i| names[i])
    )
}

impl Kernel for Snap9 {
    fn growth(&self) -> Growth {
        Growth::Map
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        self.run_all(src, out)
    }
}

/// Snap to Scale's source preparation (V09-S39 step 5): n05's reference
/// notes (a track by its number, 0 = the active track, or the active
/// track's notes on one subchannel position) over the selection's span;
/// n03's "allocate free channels" reads the active track's unselected
/// notes sounding there (they keep their channels).
pub fn snap_prepare(
    doc: &crate::model::PlumDocument,
    p: &Params,
    src: &mut Source,
    limit: usize,
    ctx: Option<&super::super::Ctx>,
) -> Result<(), TransformError> {
    let refs = p.flag("n05");
    let alloc = p.flag("n03") && p.choice("n03.channels") == 1;
    if !(refs || alloc) || src.notes.is_empty() {
        return Ok(());
    }
    let Some(active) = src.tracks.first().cloned() else {
        return Ok(());
    };
    let (w0, w1) = (src.stats.min_start, src.stats.max_end);
    if alloc && src.context.is_empty() {
        let s = std::mem::replace(src, Source::from_notes(Vec::new(), Vec::new()));
        *src = s.with_context_sounding(doc, w0, w1);
    }
    if refs {
        let num = p.int("n05.num");
        let (index, sub) = if p.choice("n05.ref") == 1 {
            let id = usize::try_from(num)
                .ok()
                .and_then(|i| active.subs.get(i))
                .map(|s| s.0)
                .ok_or_else(|| {
                    TransformError::Invalid(format!(
                        "Reference scale: the active track has no subchannel at position {num} (0 = its first)."
                    ))
                })?;
            (active.index, Some(id))
        } else if num == 0 {
            (active.index, None)
        } else {
            let i = usize::try_from(num - 1)
                .ok()
                .filter(|&i| doc.track(i).is_some())
                .ok_or_else(|| {
                    TransformError::Invalid(format!(
                        "Reference scale: track {num} does not exist. Choose the track by its number in the track list (0 = the active track)."
                    ))
                })?;
            (i, None)
        };
        let mut v = Vec::new();
        let mut take = |r: crate::model::NoteRecord| {
            if sub.is_none_or(|s| r.sub.0 == s) && v.len() < limit {
                v.push(TNote::from_record(&r, u16::MAX));
            }
        };
        if index == active.index {
            if let Ok(t) = crate::editor::note_view(doc, active.target()) {
                t.notes.query(w0, w1, |_, n| take(n.record()));
            }
        } else {
            // B2-F16: another track as the song shows it, on these ticks.
            let shift = ctx.map_or(0, |c| c.shift);
            super::super::source::placed_query(doc, index, shift, w0, w1, &mut take);
        }
        super::super::source::sort_time_major(&mut v);
        src.aux = std::sync::Arc::new(vec![v]);
    }
    Ok(())
}
