//! Score Stretch's v0.9 kernel (V09-S39 step 8, `docs/TRANSFORM_ENGINE.md`
//! "Score Stretch"): the functions declared by id beside the v0.8 kernel
//! (`edit.rs` `stretch_build`, untouched — a module of its own so the v0.8
//! closure compiles as before), with Control Sweeps per note.
//!
//! One time map moves every note — n05's rests, n03's anchors, n09's bars or
//! n10's two factors when one is on, else v0.8's (a9 segment, a5 curve, a8
//! seconds, or the factor around the pivot; n01 sets the factor and the
//! pivot) — then n06 keeps the groove, v0.8's a3 / a4 / a7 / a10 and n02
//! shape the lengths, onsets and keys, n08 slows the end down, n07 couples
//! the keys to the stretch, n11 caps the lengths and n04 thins the result.

use super::super::param::*;
use super::super::registry::BuildCtx;
use super::super::shared::density_window::{sweep_budget_keep, trailing_counts};
use super::super::shared::metric_position::bar_bounds;
use super::super::shared::phrase_detect::Phrases;
use super::super::shared::tempo_time::TempoTime;
use super::super::sweep::{PerLevel, Rebuild};
use super::super::{Ctx, Growth, Kernel, Output, Source, TNote, TransformError};
use super::duplicate::{onset_roles, spread_order};
use super::{next_onsets, note_value};
use std::sync::Arc;

pub(super) const STRETCH_NPS: &[&str] = &["The original", "A limit"];
pub(super) const STRETCH_KEEP: &[&str] = &["Loudest", "Outline"];
pub(super) const STRETCH_CAP: &[&str] = &["Next onset of the same key", "Next onset"];

/// The canonical NPS window (one second, as the Inspector measures).
const NPS_WINDOW_US: u64 = 1_000_000;

/// Samples of n08's slow-down table.
const STOP_STEPS: usize = 1024;

/// Score Stretch's v0.8 settings (the v0.8 kernel's closure reads the same
/// values; [`Stretch9`] re-reads them here, unchanged).
#[derive(Clone, Debug)]
struct Stretch8 {
    s0: u64,
    e0: u64,
    f: f64,
    pivot: f64,
    gates_only: bool,
    onsets_only: bool,
    curve: Option<Curve>,
    factors: Vec<i64>,
    snap: Option<u64>,
    seconds: bool,
    segment: Option<(u64, u64)>,
    pitch_f: Option<f64>,
    lo: u8,
    hi: u8,
}

impl Stretch8 {
    fn from(p: &Params, b: &BuildCtx) -> Stretch8 {
        let s0 = if b.stats.is_empty() {
            0
        } else {
            b.stats.min_start
        };
        let e0 = b.stats.max_end.max(s0.saturating_add(1));
        let mut f = p.float("factor") / 100.0;
        if p.adv(1) {
            f = p.ticks("a1.len") as f64 / (e0 - s0) as f64;
        }
        let pivot = if p.adv(2) {
            match p.choice("a2.pivot") {
                0 => s0 as f64,
                1 => e0 as f64,
                3 => b.ctx.cursor as f64,
                _ => (s0 as f64 + e0 as f64) / 2.0,
            }
        } else {
            s0 as f64
        };
        Stretch8 {
            s0,
            e0,
            f,
            pivot,
            gates_only: p.adv(3),
            onsets_only: p.adv(4),
            curve: p.adv(5).then(|| p.curve("a5.curve")),
            factors: if p.adv(6) {
                p.list("a6.factors")
            } else {
                Vec::new()
            },
            snap: p.adv(7).then_some(b.ctx.snap.max(1)),
            seconds: p.adv(8),
            segment: p.adv(9).then(|| {
                let (a, c) = (p.ticks("a9.from"), p.ticks("a9.to"));
                (a.min(c), a.max(c).max(a.min(c) + 1))
            }),
            pitch_f: p.adv(10).then(|| p.float("a10.factor") / 100.0),
            lo: b.stats.min_pitch,
            hi: b.stats.max_pitch,
        }
    }
}

/// Score Stretch's settings of the functions declared by id (V09-S39 step
/// 8), per Control Sweep level ([`PerLevel`]).
#[derive(Clone, Debug, Default)]
pub(super) struct StretchNew {
    /// n01: from / to beats per bar.
    meter: Option<(i64, i64)>,
    /// n02: keep the lengths of notes shorter than (ticks).
    short: Option<u64>,
    /// n03: anchors (from ‰, to ‰), sorted by from, with the ends.
    anchors: Option<Vec<(f64, f64)>>,
    /// n04: keep NPS at (0 the original, 1 a limit), the limit, keep the
    /// (0 loudest, 1 outline).
    nps: Option<(usize, u64, usize)>,
    /// n05: phrase split rest (ticks), rest factor.
    rests: Option<(u64, f64)>,
    /// n06: grid (ticks), keep (a fraction of the offset).
    groove: Option<(u64, f64)>,
    /// n07: semitones per doubling.
    tape: Option<f64>,
    /// n08: stop length (ticks), pitch drop (semitones).
    stop: Option<(u64, i64)>,
    /// n09: factors per bar (fractions), cycled.
    bars: Option<Vec<f64>>,
    /// n10: factors before / after the pivot.
    asym: Option<(f64, f64)>,
    /// n11: cap at the next onset of any key (else of the same key).
    cap: Option<bool>,
}

/// n03's anchor points: (0, 0), the listed pairs by `from` (the last of
/// equal ones wins), (1, 1); an anchor at an end replaces it.
fn anchors_of(list: &[i64]) -> Vec<(f64, f64)> {
    let mut pts: Vec<(i64, i64)> = vec![(0, 0), (1000, 1000)];
    for w in list.chunks_exact(2) {
        let (a, b) = (w[0].clamp(0, 1000), w[1].clamp(0, 1000));
        match pts.iter_mut().find(|p| p.0 == a) {
            Some(p) => p.1 = b,
            None => pts.push((a, b)),
        }
    }
    pts.sort_unstable_by_key(|p| p.0);
    pts.iter()
        .map(|&(a, b)| (a as f64 / 1000.0, b as f64 / 1000.0))
        .collect()
}

/// The piecewise-linear anchor map at `u` (0 – 1).
fn anchor_eval(pts: &[(f64, f64)], u: f64) -> f64 {
    let k = pts.partition_point(|p| p.0 <= u).clamp(1, pts.len() - 1);
    let (a, b) = (pts[k - 1], pts[k]);
    if b.0 <= a.0 {
        return b.1;
    }
    a.1 + (u - a.0) / (b.0 - a.0) * (b.1 - a.1)
}

impl StretchNew {
    pub(super) fn from(p: &Params, ctx: &Ctx) -> StretchNew {
        StretchNew {
            meter: p
                .flag("n01")
                .then(|| (p.int("n01.from").max(1), p.int("n01.to").max(1))),
            short: p.flag("n02").then(|| p.ticks("n02.short")),
            anchors: p.flag("n03").then(|| anchors_of(&p.list("n03.anchors"))),
            nps: p.flag("n04").then(|| {
                (
                    p.choice("n04.at"),
                    p.int("n04.limit").max(1) as u64,
                    p.choice("n04.keep"),
                )
            }),
            rests: p
                .flag("n05")
                .then(|| (p.ticks("n05.rest"), p.float("n05.factor") / 100.0)),
            groove: p.flag("n06").then(|| {
                (
                    note_value(p.choice("n06.grid"), 0, ctx),
                    (p.float("n06.keep") / 100.0).clamp(0.0, 1.0),
                )
            }),
            tape: p.flag("n07").then(|| p.float("n07.semis")),
            stop: p
                .flag("n08")
                .then(|| (p.ticks("n08.len").max(1), p.int("n08.drop").max(0))),
            bars: p.flag("n09").then(|| {
                let v: Vec<f64> = p
                    .list("n09.factors")
                    .iter()
                    .map(|&f| f.max(1) as f64 / 100.0)
                    .collect();
                if v.is_empty() { vec![1.0] } else { v }
            }),
            asym: p
                .flag("n10")
                .then(|| (p.float("n10.before") / 100.0, p.float("n10.after") / 100.0)),
            cap: p.flag("n11").then(|| p.choice("n11.at") == 1),
        }
    }

    pub(super) fn any(&self) -> bool {
        self.meter.is_some()
            || self.short.is_some()
            || self.anchors.is_some()
            || self.nps.is_some()
            || self.rests.is_some()
            || self.groove.is_some()
            || self.tape.is_some()
            || self.stop.is_some()
            || self.bars.is_some()
            || self.asym.is_some()
            || self.cap.is_some()
    }

    /// Whether a new time map replaces v0.8's.
    fn maps(&self) -> bool {
        self.rests.is_some() || self.anchors.is_some() || self.bars.is_some() || self.asym.is_some()
    }
}

/// One output note in the making: its start and end (as v0.8 computes them,
/// rounded only when emitted), key, the source note and whether it stays.
#[derive(Clone, Copy, Debug)]
struct Img {
    s: f64,
    e: f64,
    pitch: i64,
    i: u32,
    gone: bool,
}

impl Img {
    /// The start and length as emitted (v0.8's rounding).
    fn ticks(&self) -> (i128, i128) {
        (self.s.round() as i128, (self.e - self.s).round() as i128)
    }
}

/// The v0.9 Score Stretch kernel (V09-S39 step 8): the v0.8 rule re-read
/// from the same parameters, with the functions declared by id at their
/// stages and the Control Sweeps evaluated per note inside the kernel.
pub(super) struct Stretch9 {
    v8: Stretch8,
    new: StretchNew,
    rebuild: Option<Arc<Rebuild>>,
    ctx: Ctx,
    subs: Vec<Vec<(u32, u8)>>,
    /// n08's slow-down: S(y) = ∫₀^y (1 − c) at `STOP_STEPS + 1` points, and
    /// the curve itself (the pitch drop) at the same points.
    stop: Option<(Vec<f64>, Vec<f64>)>,
    /// The bar line at or before the selection's start (n01, n09).
    bar0: u64,
    growth: Growth,
}

impl Stretch9 {
    pub(super) fn build(
        p: &Params,
        b: &BuildCtx,
        new: StretchNew,
    ) -> Result<Box<dyn Kernel>, TransformError> {
        let v8 = Stretch8::from(p, b);
        let stop = new.stop.is_some().then(|| {
            let c = p.curve("n08.curve");
            let mut s = Vec::with_capacity(STOP_STEPS + 1);
            let mut cv = Vec::with_capacity(STOP_STEPS + 1);
            let mut acc = 0.0;
            let mut prev = c.eval(0.0).clamp(0.0, 1.0);
            s.push(0.0);
            cv.push(prev);
            for k in 1..=STOP_STEPS {
                let y = k as f64 / STOP_STEPS as f64;
                let v = c.eval(y).clamp(0.0, 1.0);
                acc += (2.0 - prev - v) / 2.0 / STOP_STEPS as f64;
                s.push(acc);
                cv.push(v);
                prev = v;
            }
            (s, cv)
        });
        if let Some((s, _)) = &stop
            && s[STOP_STEPS] < 0.05
        {
            return Err(TransformError::Invalid(
                "Tape stop: the slow-down curve stops the tape too early (it must leave at least 5 % of the speed on average); lower the curve.".into(),
            ));
        }
        let growth = if new.nps.is_some() {
            Growth::Thin
        } else {
            Growth::Map
        };
        Ok(Box::new(Stretch9 {
            bar0: bar_bounds(&b.ctx.meter, v8.s0).0,
            v8,
            new,
            rebuild: Rebuild::of(p, b),
            ctx: b.ctx.clone(),
            subs: b.tracks.iter().map(|t| t.subs.clone()).collect(),
            stop,
            growth,
        }))
    }

    /// v0.8's map of tick `t` with factor `f` (and the pivot, n01's when on).
    #[inline]
    fn map8(&self, t: f64, f: f64, pivot: f64) -> f64 {
        let v = &self.v8;
        let (s0, e0) = (v.s0, v.e0);
        if let Some((a, c)) = v.segment {
            let (a, c) = (a as f64, c as f64);
            return if t <= a {
                t
            } else if t <= c {
                a + (t - a) * f
            } else {
                a + (c - a) * f + (t - c)
            };
        }
        if let Some(cv) = &v.curve {
            let u = ((t - s0 as f64) / (e0 - s0) as f64).clamp(0.0, 1.0);
            return s0 as f64 + cv.eval(u) * (e0 - s0) as f64 * f;
        }
        if v.seconds {
            let ps = self.ctx.seconds_at(pivot.max(0.0) as u64);
            let ts = self.ctx.seconds_at(t.max(0.0) as u64);
            return self.ctx.tick_at_seconds(ps + (ts - ps) * f) as f64;
        }
        pivot + (t - pivot) * f
    }

    /// The note's factor (v0.8: a6 by subchannel, else the factor; n01 the
    /// meter's ratio) and pivot.
    fn factor(&self, x: &TNote, c: &StretchNew) -> (f64, f64) {
        let v = &self.v8;
        let (mut f, mut pivot) = (v.f, v.pivot);
        if let Some((from, to)) = c.meter {
            f = to as f64 / from as f64;
            pivot = self.bar0 as f64;
        }
        if !v.factors.is_empty() {
            let si = self
                .subs
                .get(usize::from(x.track))
                .and_then(|t| t.iter().position(|s| s.0 == x.sub))
                .unwrap_or(0);
            f = v.factors[si % v.factors.len()] as f64 / 100.0;
        }
        (f, pivot)
    }

    /// The time map of the note's settings at tick `t`: n03's anchors, n09's
    /// bars, n10's two factors, else v0.8's (n05's rests translate whole
    /// phrases: `shift`).
    fn map(&self, t: f64, f: f64, pivot: f64, c: &StretchNew, w: &World, shift: f64) -> f64 {
        let v = &self.v8;
        if c.rests.is_some() {
            return t + shift;
        }
        if let Some(pts) = &c.anchors {
            let span = (v.e0 - v.s0) as f64;
            let u = ((t - v.s0 as f64) / span).clamp(0.0, 1.0);
            return v.s0 as f64 + anchor_eval(pts, u) * span * f;
        }
        if let Some(fs) = &c.bars {
            return self.bar_map(t, fs, w);
        }
        if let Some((before, after)) = c.asym {
            return pivot + (t - pivot) * if t < pivot { before } else { after };
        }
        self.map8(t, f, pivot)
    }

    /// n09: bar k (from the bar holding the selection's start) stretched by
    /// the k-th factor (cycled), the bars laid end to end.
    fn bar_map(&self, t: f64, fs: &[f64], w: &World) -> f64 {
        let b0 = self.bar0 as f64;
        if t <= b0 || w.bars.is_empty() {
            return b0 + (t - b0) * fs[0];
        }
        // The bar holding t (the last bar runs on), after the stretched
        // bars before it.
        let k = w.bars.partition_point(|&(s, _)| (s as f64) <= t).max(1) - 1;
        b0 + w.cum[k] + (t - w.bars[k].0 as f64) * fs[k % fs.len()]
    }

    /// n08: the slowed-down offset of `x` ticks into a stop of `len` ticks,
    /// and the curve there (the pitch drop's share).
    fn stopped(&self, x: f64, len: f64) -> (f64, f64) {
        let Some((s, cv)) = &self.stop else {
            return (x, 0.0);
        };
        let total = s[STOP_STEPS];
        let target = (x / len).clamp(0.0, 1.0) * total;
        // S is non-decreasing: the first sample at or past the target.
        let k = s.partition_point(|&v| v < target).clamp(1, STOP_STEPS);
        let (a, b) = (s[k - 1], s[k]);
        let frac = if b > a { (target - a) / (b - a) } else { 1.0 };
        let y = ((k - 1) as f64 + frac) / STOP_STEPS as f64;
        let c = cv[k - 1] + (cv[k] - cv[k - 1]) * frac;
        // T = len / S(1): the stop's real length.
        (y * len / total, c)
    }

    fn levels(&self, src: &Source) -> PerLevel<StretchNew> {
        match &self.rebuild {
            Some(r) => r.levels(&self.ctx, src, |p, b| StretchNew::from(p, b.ctx)),
            None => PerLevel::one(self.new.clone()),
        }
    }

    fn run_all(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let notes = &src.notes;
        let n = notes.len();
        if n == 0 {
            return Ok(());
        }
        let lv = self.levels(src);
        let any = |f: fn(&StretchNew) -> bool| lv.all().iter().any(f);
        let w = self.world(src, &lv);
        let v = &self.v8;
        // ---- The time map, n06's groove, v0.8's lengths and onsets, n02,
        // v0.8's snap and keys, n07's tape coupling.
        let mut imgs: Vec<Img> = Vec::with_capacity(n);
        for (i, x) in notes.iter().enumerate() {
            let c = lv.at(i);
            let (fx, pivot) = self.factor(x, c);
            let shift = w.shift.get(i).copied().unwrap_or(0.0);
            let m = |t: f64| self.map(t, fx, pivot, c, &w, shift);
            let (s, e) = (x.start as f64, x.end() as f64);
            let (ms, me) = (m(s), m(e));
            let (mut ns, mut ne) = (ms, me);
            if let Some((grid, keep)) = c.groove.filter(|g| g.1 > 0.0) {
                // The nearest grid line in the note's bar; its offset kept
                // in ticks (a share).
                let (bs, _) = bar_bounds(&self.ctx.meter, x.start);
                let g = grid as f64;
                let gp = bs as f64 + ((s - bs as f64) / g).round() * g;
                let off = s - gp;
                let mg = m(gp);
                let to = mg + keep * off + (1.0 - keep) * (ms - mg);
                ne += to - ns;
                ns = to;
            }
            if v.gates_only {
                ns = s;
                ne = if c.maps() {
                    s + (me - ms)
                } else {
                    s + (e - s) * fx
                };
            } else if v.onsets_only {
                ne = ns + (e - s);
            }
            if let Some(short) = c.short
                && x.dur < short
            {
                ne = ns + (e - s);
            }
            if let Some(g) = v.snap {
                let q = (ns / g as f64).round() * g as f64;
                ne += q - ns;
                ns = q;
            }
            let mut pitch = match v.pitch_f {
                Some(pf) => {
                    let ctr = (f64::from(v.lo) + f64::from(v.hi)) / 2.0;
                    (ctr + (f64::from(x.pitch) - ctr) * pf).round() as i64
                }
                None => i64::from(x.pitch),
            };
            if let Some(semis) = c.tape {
                let local = (me - ms) / (e - s).max(1e-9);
                if local > 0.0 {
                    pitch += (-semis * local.log2()).round() as i64;
                }
            }
            imgs.push(Img {
                s: ns,
                e: ne,
                pitch,
                i: i as u32,
                gone: false,
            });
        }
        // ---- n08: the end slowed to a stop.
        if any(|c| c.stop.is_some()) {
            let end = imgs.iter().map(|m| m.e).fold(f64::NEG_INFINITY, f64::max);
            for m in imgs.iter_mut() {
                let Some((len, drop)) = lv.at(m.i as usize).stop else {
                    continue;
                };
                let len = len as f64;
                let r = end - len;
                if m.e <= r {
                    continue;
                }
                if m.s >= r {
                    let (ts, c) = self.stopped(m.s - r, len);
                    let (te, _) = self.stopped(m.e - r, len);
                    m.pitch -= (drop as f64 * c).round() as i64;
                    m.s = r + ts;
                    m.e = r + te;
                } else {
                    m.e = r + self.stopped(m.e - r, len).0;
                }
            }
        }
        // ---- n11: lengths capped at the next onset.
        if any(|c| c.cap.is_some()) {
            self.cap(notes, &mut imgs, &lv);
        }
        out.check()?;
        // ---- n04: thinned to the original NPS or a limit.
        if any(|c| c.nps.is_some()) {
            self.thin(src, &mut imgs, &lv);
        }
        let mut removed = 0usize;
        for m in &imgs {
            if m.gone {
                removed += 1;
                continue;
            }
            let x = &notes[m.i as usize];
            let (s, d) = m.ticks();
            if !out.emit(s, d, m.pitch, i64::from(x.vel), x) {
                return out.check();
            }
        }
        if removed > 0 {
            out.message = Some(format!(
                "NPS-safe compression: {} note(s) removed to keep the notes per second.",
                crate::midi::group_digits(removed as u64)
            ));
        }
        out.check()
    }

    /// n11: per track, every image's end at most at the next later start
    /// (of the same key, or any) among the images — unless its source note
    /// already reached past its own next onset (an overlap the selection
    /// had is not a new one).
    fn cap(&self, notes: &[TNote], imgs: &mut [Img], lv: &PerLevel<StretchNew>) {
        let starts: Vec<TNote> = imgs
            .iter()
            .map(|m| {
                let x = &notes[m.i as usize];
                TNote {
                    start: m.ticks().0.clamp(0, i128::from(u64::MAX >> 2)) as u64,
                    pitch: m.pitch.clamp(0, 255) as u8,
                    ..*x
                }
            })
            .collect();
        let any_key = lv.all().iter().any(|c| c.cap == Some(true));
        let same = lv.all().iter().any(|c| c.cap == Some(false));
        let by_key_out = same.then(|| next_onsets(&starts, &[], |x| (x.track, x.pitch)));
        let any_out = any_key.then(|| next_onsets(&starts, &[], |x| x.track));
        let by_key_src = same.then(|| next_onsets(notes, &[], |x| (x.track, x.pitch)));
        let any_src = any_key.then(|| next_onsets(notes, &[], |x| x.track));
        for (k, m) in imgs.iter_mut().enumerate() {
            let i = m.i as usize;
            let Some(anyk) = lv.at(i).cap else {
                continue;
            };
            let (next_out, next_src) = if anyk {
                (any_out.as_ref().unwrap()[k], any_src.as_ref().unwrap()[i])
            } else {
                (
                    by_key_out.as_ref().unwrap()[k],
                    by_key_src.as_ref().unwrap()[i],
                )
            };
            if next_src.is_some_and(|ns| notes[i].end() > ns) {
                continue;
            }
            let Some(next) = next_out else {
                continue;
            };
            let (s, d) = m.ticks();
            let room = i128::from(next) - s;
            if d > room && room > 0 {
                m.e = m.s + room as f64;
            }
        }
    }

    /// n04: in time order, while the one-second window ending at an image's
    /// start holds more starts than its budget — the original's NPS at that
    /// image's source note (the window keeps its source's density), or the
    /// limit — the window's kept image of lowest priority goes: the
    /// quietest (or the inner voices, then the bottoms), ties spread evenly
    /// (`density_window::sweep_budget_keep`).
    fn thin(&self, src: &Source, imgs: &mut [Img], lv: &PerLevel<StretchNew>) {
        let notes = &src.notes;
        let n = imgs.len();
        let mut tt = TempoTime::new(&self.ctx);
        // Time order of the images.
        let mut by_start: Vec<(i128, u32)> = imgs
            .iter()
            .enumerate()
            .map(|(k, m)| (m.ticks().0, k as u32))
            .collect();
        by_start.sort_unstable();
        let order: Vec<usize> = by_start.iter().map(|x| x.1 as usize).collect();
        drop(by_start);
        let tick = |m: &Img| m.ticks().0.clamp(0, i128::from(u64::MAX >> 2)) as u64;
        let times: Vec<u64> = order
            .iter()
            .map(|&k| tt.micros_at(tick(&imgs[k])))
            .collect();
        // The original's NPS at each source note's start (only when a note
        // keeps the original).
        let original = lv
            .all()
            .iter()
            .any(|c| c.nps.is_some_and(|p| p.0 == 0))
            .then(|| {
                let src_times: Vec<u64> = notes.iter().map(|x| tt.micros_at(x.start)).collect();
                trailing_counts(&src_times, NPS_WINDOW_US)
            });
        // Priority (higher stays): velocity, or the onset role (tops, then
        // bottoms, inner voices last); then the van der Corput order.
        let mut rank = vec![0u32; n];
        for (r, &k) in spread_order(n).iter().enumerate() {
            rank[k as usize] = r as u32;
        }
        let roles = lv
            .all()
            .iter()
            .any(|c| c.nps.is_some_and(|p| p.2 == 1))
            .then(|| onset_roles(notes));
        let mut priority = Vec::with_capacity(n);
        let mut removable = Vec::with_capacity(n);
        for &k in &order {
            let i = imgs[k].i as usize;
            let (on, keep) = lv.at(i).nps.map_or((false, 0), |p| (true, p.2));
            let class = if keep == 1 {
                u64::from(2 - roles.as_ref().map_or(2, |r| r[i]).min(2))
            } else {
                u64::from(notes[i].vel)
            };
            priority.push((class << 32) | u64::from(u32::MAX - rank[k]));
            removable.push(on);
        }
        let cap_of = |q: usize| -> usize {
            let i = imgs[order[q]].i as usize;
            match lv.at(i).nps {
                Some((0, _, _)) => original
                    .as_ref()
                    .map_or(usize::MAX, |o| o[i] as usize)
                    .max(1),
                Some((_, limit, _)) => limit.min(usize::MAX as u64) as usize,
                None => usize::MAX,
            }
        };
        let keep = sweep_budget_keep(&times, NPS_WINDOW_US, cap_of, &priority, &removable);
        for (q, &k) in order.iter().enumerate() {
            if !keep[q] {
                imgs[k].gone = true;
            }
        }
    }

    /// The whole-source world: n05's phrase shifts, n09's bars.
    fn world(&self, src: &Source, lv: &PerLevel<StretchNew>) -> World {
        let notes = &src.notes;
        let mut w = World::default();
        if let Some((rest, _)) = lv.first().rests {
            // Phrases of the whole selection in time order; each rest takes
            // the factor of the phrase's first note.
            let idx: Vec<usize> = (0..notes.len()).collect();
            let ph = Phrases::of(notes, &idx, rest, None);
            w.shift = vec![0.0; notes.len()];
            let mut shift = 0.0f64;
            let mut until: Option<u64> = None;
            for p in ph.iter(&idx) {
                let first = p[0];
                if let Some(u) = until {
                    let gap = notes[first].start.saturating_sub(u) as f64;
                    let rf = lv.at(first).rests.map_or(1.0, |r| r.1);
                    shift += gap * (rf - 1.0);
                }
                let mut end = until.unwrap_or(0);
                for &i in p {
                    w.shift[i] = shift;
                    end = end.max(notes[i].end());
                }
                until = Some(end);
            }
        }
        if let Some(fs) = &lv.first().bars {
            let mut t = self.bar0;
            let end = self.v8.e0;
            let mut at = 0.0f64;
            while t < end && w.bars.len() < 1 << 20 {
                let (s, len) = bar_bounds(&self.ctx.meter, t);
                let len = len.max(1);
                w.cum.push(at);
                at += len as f64 * fs[w.bars.len() % fs.len()];
                w.bars.push((s, len));
                t = s.saturating_add(len);
            }
        }
        w
    }
}

/// What the run reads across notes: each note's n05 phrase shift, n09's
/// bars (start, length) from the selection's first bar and the stretched
/// length of the bars before each.
#[derive(Clone, Debug, Default)]
struct World {
    shift: Vec<f64>,
    bars: Vec<(u64, u64)>,
    cum: Vec<f64>,
}

impl Kernel for Stretch9 {
    fn growth(&self) -> Growth {
        self.growth
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        self.run_all(src, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_take_the_ends_and_the_last_equal_entry() {
        let a = anchors_of(&[500, 250, 1000, 800, 500, 300]);
        assert_eq!(a, vec![(0.0, 0.0), (0.5, 0.3), (1.0, 0.8)]);
        assert!((anchor_eval(&a, 0.25) - 0.15).abs() < 1e-12);
        assert!((anchor_eval(&a, 0.75) - 0.55).abs() < 1e-12);
        assert_eq!(anchor_eval(&a, 1.0), 0.8);
    }
}
