//! Pulse Field's v0.9 kernel (V09-S41, `docs/TRANSFORM_ENGINE.md` "Pulse
//! Field"): the functions declared by id beside the v0.8 kernel (`xfield.rs`
//! `pulse_build`, untouched — it hands over when a new switch or a Control
//! Sweep is on), with Control Sweeps per note.
//!
//! v0.8's rule per note (its middle (mu, mv) in the selection's box) and
//! echo copy k: the phase of layer c is θ = 2π·c·pos + ph (pos = mu, or the
//! radial distance from the box's centre), the pulse w the layers' mean of
//! the wave, the scale s = 1 + amplitude·w·envelope about the box's centre.
//! n01 counts the cycles on the song's beats or bars, n12 pulses the inside
//! of a split radius against the outside, n09 makes the pulse square with a
//! duty that moves across the selection, n02 replaces the periodic wave by
//! an envelope fired at every onset of reference notes; n08 adds a growth
//! per completed cycle, n07 gives time and pitch their own amplitudes and a
//! phase offset, n05 rounds the pitch displacement to semitones, octaves or
//! the key's degrees; n03 lets velocity or gate follow the pulse, n06 thins
//! the contracted phase, n10 strobes, n11 colours notes by their phase.

use super::super::geom::{Geom, rem1};
use super::super::param::*;
use super::super::registry::BuildCtx;
use super::super::rng::{Rng, hash, note_key};
use super::super::shared::metric_position::{bar_place, metric_position};
use super::super::shared::reference::reference_notes;
use super::super::shared::scale_lattice::Lattice;
use super::super::shared::seeded_stream::salt_of;
use super::super::sweep::{PerLevel, Rebuild};
use super::super::theory::Scale;
use super::super::{Ctx, Growth, Kernel, Output, Source, SourceTrack, TNote, TransformError};
use super::geometry::{sub_at, sub_pos};
use super::xgeom::{mid_unit, vel_scaled};
use std::f64::consts::{FRAC_1_SQRT_2, PI, TAU};
use std::sync::Arc;

/// The switches of the v0.9 functions (n04 was retired in the design gate).
pub(super) const NEW: [&str; 11] = [
    "n01", "n02", "n03", "n05", "n06", "n07", "n08", "n09", "n10", "n11", "n12",
];
pub(super) const UNITS: &[&str] = &["Per beat", "Per bar"];
pub(super) const REFERENCES: &[&str] = &["A track", "A subchannel"];
pub(super) const TARGETS: &[&str] = &["Velocity", "Gate"];
pub(super) const STEPS: &[&str] = &["Semitone", "Octave", "Scale degree"];

/// The beyond-the-keyboard bound of a quantized point (rows).
const FAR_ROWS: f64 = 1e9;

#[derive(Clone, Debug)]
struct PCfg {
    // v0.8 (`pulse_build`).
    amp: f64,
    radial: bool,
    ax: bool,
    ay: bool,
    /// 0 triangle, 1 sine, 2 square, 3 custom.
    shape: u8,
    custom: Curve,
    layers: Arc<Vec<f64>>,
    phases: Arc<Vec<f64>>,
    env: Option<Curve>,
    /// a10: count, phase step (rad), velocity per echo.
    echoes: Option<(usize, f64, f64)>,
    // v0.9.
    /// n01: per bar (else per beat), cycles per unit.
    tempo: Option<(bool, f64)>,
    /// n02: the envelope over one decay, the decay (ticks).
    trigger: Option<(Curve, f64)>,
    /// n03: gate (else velocity), depth.
    dynamics: Option<(bool, f64)>,
    /// n05: 0 semitone, 1 octave, 2 the key's degrees.
    quant: Option<(u8, Lattice)>,
    /// n06: the share kept at the smallest point.
    thin: Option<f64>,
    /// n07: time amplitude, pitch amplitude, pitch's phase offset (rad).
    ellipse: Option<(f64, f64, f64)>,
    /// n08: growth per completed cycle.
    ratchet: f64,
    /// n09: square duty at the selection's start and end.
    duty: Option<(f64, f64)>,
    /// n10: duty, phase (cycles).
    strobe: Option<(f64, f64)>,
    /// n11: bands, the palette (subchannel positions; empty = the band).
    colours: Option<(u32, Arc<Vec<i64>>)>,
    /// n12: split radius (unit), counter phase (rad).
    counter: Option<(f64, f64)>,
}

impl PCfg {
    fn from(p: &Params, _b: &BuildCtx) -> PCfg {
        let (xo, yo) = (p.adv(2), p.adv(3));
        let (ax, ay) = if xo == yo { (true, true) } else { (xo, yo) };
        let shape = if p.adv(6) {
            3
        } else if p.adv(5) {
            2
        } else if p.adv(4) {
            1
        } else {
            0
        };
        let layers: Vec<f64> = if p.adv(7) {
            p.list("a7.cycles").iter().map(|&c| c as f64).collect()
        } else {
            vec![p.float("cycles")]
        };
        let phases: Vec<f64> = if p.adv(8) {
            p.list("a8.phases")
                .iter()
                .map(|&d| d as f64 * PI / 180.0)
                .collect()
        } else {
            Vec::new()
        };
        let pc = |k: &str| p.float(k) / 100.0;
        PCfg {
            amp: pc("amp"),
            radial: p.adv(1),
            ax,
            ay,
            shape,
            custom: p.curve("a6.curve"),
            layers: Arc::new(layers),
            phases: Arc::new(phases),
            env: p.adv(9).then(|| p.curve("a9.curve")),
            echoes: p.adv(10).then(|| {
                (
                    p.int("a10.count").clamp(1, 8) as usize,
                    p.float("a10.phase") * PI / 180.0,
                    p.float("a10.decay") / 100.0,
                )
            }),
            tempo: p.flag("n01").then(|| {
                (
                    p.choice("n01.unit") == 1,
                    p.float("n01.cycles").clamp(0.125, 16.0),
                )
            }),
            trigger: p
                .flag("n02")
                .then(|| (p.curve("n02.envelope"), p.ticks("n02.decay").max(1) as f64)),
            dynamics: p
                .flag("n03")
                .then(|| (p.choice("n03.target") == 1, pc("n03.depth").clamp(0.0, 1.0))),
            quant: p.flag("n05").then(|| {
                let step = p.choice("n05.step").min(2) as u8;
                let lat =
                    Lattice::of_scale(&Scale::new(p.choice("n05.key"), p.choice("n05.scale")));
                (step, lat)
            }),
            thin: p.flag("n06").then(|| pc("n06.keep").clamp(0.0, 1.0)),
            ellipse: p.flag("n07").then(|| {
                (
                    pc("n07.x").clamp(0.0, 2.0),
                    pc("n07.y").clamp(0.0, 2.0),
                    p.float("n07.offset") * PI / 180.0,
                )
            }),
            ratchet: if p.flag("n08") {
                pc("n08.growth").clamp(-0.5, 0.5)
            } else {
                0.0
            },
            duty: p.flag("n09").then(|| {
                (
                    pc("n09.start").clamp(0.01, 0.99),
                    pc("n09.end").clamp(0.01, 0.99),
                )
            }),
            strobe: p.flag("n10").then(|| {
                (
                    pc("n10.duty").clamp(0.01, 0.99),
                    pc("n10.phase").clamp(0.0, 1.0),
                )
            }),
            colours: p.flag("n11").then(|| {
                (
                    p.int("n11.bands").clamp(2, 64) as u32,
                    Arc::new(p.list("n11.palette")),
                )
            }),
            counter: p.flag("n12").then(|| {
                (
                    pc("n12.split").clamp(0.01, 1.0),
                    p.float("n12.phase") * PI / 180.0,
                )
            }),
        }
    }

    fn copies(&self) -> usize {
        1 + self.echoes.map_or(0, |e| e.0)
    }

    /// v0.8's wave at phase `x` (rad); n09's square of duty `duty`.
    #[inline]
    fn wave(&self, x: f64, duty: Option<f64>) -> f64 {
        let t = rem1(x / TAU);
        if let Some(d) = duty {
            return if t < d { 1.0 } else { -1.0 };
        }
        match self.shape {
            1 => x.sin(),
            2 => {
                if t < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            3 => (self.custom.eval(t) - 0.5) * 2.0,
            _ => {
                if t < 0.25 {
                    4.0 * t
                } else if t < 0.75 {
                    2.0 - 4.0 * t
                } else {
                    4.0 * t - 4.0
                }
            }
        }
    }
}

/// A note copy's pulse: the scales along time and pitch, the pulse value
/// and envelope (for n03 / n06), the cycles so far (ψ, for n08 / n10 / n11).
#[derive(Clone, Copy, Debug)]
struct Pulse {
    sx: f64,
    sy: f64,
    w: f64,
    e: f64,
    psi: f64,
}

pub(super) struct Pulse9 {
    base: PCfg,
    rebuild: Option<Arc<Rebuild>>,
    g: Geom,
    ctx: Ctx,
    tracks: Vec<SourceTrack>,
    growth: Growth,
}

pub(super) fn pulse9_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    let g = Geom::from_params(p, b.stats);
    let base = PCfg::from(p, b);
    let rebuild = Rebuild::of(p, b);
    let corners = match &rebuild {
        Some(r) => r.corners(b.ctx, PCfg::from),
        None => vec![base.clone()],
    };
    // Each echo rasterized; n06 / n10 only remove copies.
    let per = corners
        .iter()
        .map(|c| c.copies() as u64)
        .max()
        .unwrap_or(1)
        .saturating_mul(g.per_note());
    Ok(Box::new(Pulse9 {
        base,
        rebuild,
        g,
        ctx: b.ctx.clone(),
        tracks: b.tracks.to_vec(),
        growth: if per > 1 {
            Growth::Expand(per)
        } else {
            Growth::Map
        },
    }))
}

/// n02: reads the reference notes' onsets (layer 0 of `Source::aux`) over
/// the selection and one decay before it.
pub fn prepare(
    doc: &crate::model::PlumDocument,
    p: &Params,
    src: &mut Source,
    limit: usize,
) -> Result<(), TransformError> {
    if !p.flag("n02") || src.stats.is_empty() {
        return Ok(());
    }
    let w0 = src.stats.min_start.saturating_sub(p.ticks("n02.decay"));
    let notes = reference_notes(
        doc,
        src,
        p.choice("n02.ref") == 1,
        p.int("n02.num"),
        w0,
        src.stats.max_end,
        limit,
        "Onset trigger",
    )?;
    src.aux = Arc::new(vec![notes]);
    Ok(())
}

impl Pulse9 {
    fn levels(&self, src: &Source) -> PerLevel<PCfg> {
        match &self.rebuild {
            Some(r) => r.levels(&self.ctx, src, PCfg::from),
            None => PerLevel::one(self.base.clone()),
        }
    }

    /// n01: the song's beats or bars at `tick` (fractional).
    fn units(&self, tick: f64, bars: bool) -> f64 {
        let t = tick.clamp(0.0, u64::MAX as f64) as u64;
        let frac = tick - t as f64;
        let meter = &self.ctx.meter;
        if bars {
            let (bar, start, len) = bar_place(meter, t);
            return bar as f64 + ((t - start) as f64 + frac) / len.max(1) as f64;
        }
        let m = metric_position(meter, t);
        let len = m.beat_len.max(1) as f64;
        match meter.beat_at(t) {
            Some((index, off)) => index as f64 + (off as f64 + frac) / len,
            None => tick / len,
        }
    }

    /// The pulse of copy `k` of note `n` with settings `c`.
    fn pulse(&self, c: &PCfg, n: &TNote, k: usize, onsets: &[u64]) -> Pulse {
        let g = &self.g;
        let (mu, mv) = mid_unit(g, n);
        let r = ((mu - 0.5).powi(2) + (mv - 0.5).powi(2)).sqrt() / FRAC_1_SQRT_2;
        let pos = if c.radial { r } else { mu };
        let mut ph = c
            .phases
            .get(sub_pos(&self.tracks, n) % c.phases.len().max(1))
            .copied()
            .filter(|_| !c.phases.is_empty())
            .unwrap_or(0.0);
        if let Some((_, step, _)) = c.echoes {
            ph += step * k as f64;
        }
        if let Some((split, counter)) = c.counter
            && r < split
        {
            ph += counter;
        }
        let duty = c.duty.map(|(a, b)| a + (b - a) * mu.clamp(0.0, 1.0));
        let e = c.env.as_ref().map_or(1.0, |cv| cv.eval(mu.clamp(0.0, 1.0)));
        let c0 = c.layers.first().copied().unwrap_or(1.0);
        let mid = (n.start as f64 + n.end() as f64) / 2.0;
        // The phase of layer `cyc` (rad) without ph.
        let arg = |cyc: f64| -> f64 {
            match c.tempo {
                Some((bars, per)) => {
                    let m = self.units(mid, bars);
                    let ring = if c.radial { cyc * r } else { 0.0 };
                    TAU * (ring + per * (cyc / c0.max(1e-9)) * m)
                }
                None => TAU * cyc * pos,
            }
        };
        // The pulse at phase offset `off`: the layers' mean, or n02's
        // envelope fired at the last onset (delayed by the phase); and ψ.
        let at = |off: f64| -> (f64, f64) {
            if let Some((curve, decay)) = &c.trigger {
                let te = mid - (ph + off) / TAU * decay;
                let i = onsets.partition_point(|&o| (o as f64) <= te);
                if i == 0 {
                    return (0.0, 0.0);
                }
                let x = (te - onsets[i - 1] as f64) / decay;
                let w = if (0.0..1.0).contains(&x) {
                    curve.eval(x)
                } else {
                    0.0
                };
                return (w, (i - 1) as f64 + x.min(1.0));
            }
            let w = c
                .layers
                .iter()
                .map(|&cyc| c.wave(arg(cyc) + ph + off, duty))
                .sum::<f64>()
                / c.layers.len().max(1) as f64;
            (w, (arg(c0) + ph + off) / TAU)
        };
        let (w, psi) = at(0.0);
        let grow = c.ratchet * psi.floor();
        let (sx, sy) = match c.ellipse {
            Some((xa, ya, off)) => {
                let (w2, _) = if off == 0.0 { (w, psi) } else { at(off) };
                (1.0 + xa * w * e + grow, 1.0 + ya * w2 * e + grow)
            }
            None => {
                let s = 1.0 + c.amp * w * e + grow;
                (s, s)
            }
        };
        Pulse { sx, sy, w, e, psi }
    }

    /// Whether n06 / n10 keep copy `k` of note `n`.
    fn kept(c: &PCfg, n: &TNote, k: usize, p: &Pulse) -> bool {
        if let Some((duty, phase)) = c.strobe
            && rem1(p.psi + phase) >= duty
        {
            return false;
        }
        if let Some(keep) = c.thin {
            let (amp, s) = match c.ellipse {
                Some((xa, ya, _)) => ((xa + ya) / 2.0, (p.sx + p.sy) / 2.0),
                None => (c.amp, p.sx),
            };
            if s < 1.0 && amp > 0.0 {
                let depth = ((1.0 - s) / amp).clamp(0.0, 1.0);
                let share = 1.0 - (1.0 - keep) * depth;
                let u = Rng::new(hash(&[note_key(n), k as u64, salt_of("pulse_field.n06")])).unit();
                if u >= share {
                    return false;
                }
            }
        }
        true
    }

    /// n05: the point (x, y) with its pitch displacement from (u, v)
    /// rounded to whole steps.
    #[inline]
    fn quantized(&self, c: &PCfg, u: f64, v: f64, x: f64, y: f64) -> (f64, f64) {
        let Some((step, lat)) = c.quant else {
            return (x, y);
        };
        let b = &self.g.bbox;
        let (_, y0) = b.from_unit(u, v);
        let (_, y1) = b.from_unit(x, y);
        if !(y0.is_finite() && y1.is_finite()) || y1.abs() > FAR_ROWS || y0.abs() > FAR_ROWS {
            return (x, y);
        }
        let dy = y1 - y0;
        let row = match step {
            0 => y0 + dy.round(),
            1 => y0 + (dy / 12.0).round() * 12.0,
            _ => lat.nearest(y1.floor() as i64) as f64 + (y0 - y0.floor()),
        };
        (x, b.to_unit(0.0, row).1)
    }

    fn run_all(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let lv = self.levels(src);
        let onsets: Vec<u64> = if lv.all().iter().any(|c| c.trigger.is_some()) {
            let mut v: Vec<u64> = src
                .aux
                .first()
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .map(|n| n.start)
                .collect();
            v.sort_unstable();
            v.dedup();
            v
        } else {
            Vec::new()
        };
        let g = &self.g;
        for (i, n) in src.notes.iter().enumerate() {
            let c = lv.at(i);
            for k in 0..c.copies() {
                // V09-0143: the echo (0 = the note itself).
                out.set_dim(u32::try_from(k).unwrap_or(u32::MAX));
                let p = self.pulse(c, n, k, &onsets);
                if !Self::kept(c, n, k, &p) {
                    continue;
                }
                let mut t = *n;
                if let Some((_, _, d)) = c.echoes {
                    t.vel = vel_scaled(n, d.powi(k as i32));
                }
                if let Some((gate, depth)) = c.dynamics {
                    let f = (1.0 + depth * p.w * p.e).max(0.0);
                    if gate {
                        t.dur = ((t.dur as f64) * f).round().clamp(1.0, u64::MAX as f64) as u64;
                    } else {
                        t.vel = vel_scaled(&t, f);
                    }
                }
                if let Some((bands, palette)) = &c.colours {
                    let band =
                        ((rem1(p.psi) * f64::from(*bands)) as i64).min(i64::from(*bands) - 1);
                    let pos = if palette.is_empty() {
                        band
                    } else {
                        palette[band as usize % palette.len()]
                    };
                    (t.sub, t.ch) = sub_at(&self.tracks, n, pos);
                }
                let (sx, sy) = (p.sx, p.sy);
                let map = |u: f64, v: f64| {
                    let x = if c.ax { 0.5 + (u - 0.5) * sx } else { u };
                    let y = if c.ay { 0.5 + (v - 0.5) * sy } else { v };
                    self.quantized(c, u, v, x, y)
                };
                if !g.rasterize(&t, &map, &self.tracks, out) {
                    return out.check();
                }
            }
            out.check()?;
        }
        Ok(())
    }
}

impl Kernel for Pulse9 {
    fn growth(&self) -> Growth {
        self.growth
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        self.run_all(src, out)?;
        out.check()
    }
}
