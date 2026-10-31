//! Note generators (v0.9 §151, §162, §163, §239, §240, §256;
//! `docs/GENERATORS.md`): the Generator family's shared engine.
//!
//! A generator is an ordinary tool of [`Family::Generator`] that needs no
//! selection: the platform writes its output into the active track (or
//! opened clip) with Preview on by default, Apply / Cancel / Reset,
//! presets and one Undo step, like every tool. What the engine adds:
//!
//! * the **shared controls** every generator has ([`base_specs`], keys
//!   `gen.*`): where (the cursor, the ruler's time selection, the selected
//!   notes' box), length, pitch range, resolution (the grid notes land on —
//!   the Piano Roll snap by default), velocity, gate, subchannel, seed,
//!   keep the selection, and the boundary policies;
//! * the [`Frame`]: the generation box those controls give;
//! * **recipes without storage** (§163): a generator only *describes* its
//!   shapes to a pen ([`Shapes::draw`]); the kernel runs it twice — through
//!   a counting pen for the exact preflight ([`Kernel::estimate_source`],
//!   nothing allocated) and through the emitting pen straight into the
//!   run's output (its cap, boundary policies and cancel token). Before
//!   anything is drawn, [`Shapes::bound`] gives a cheap upper bound
//!   ([`Kernel::estimate`]).

pub mod arcs;
pub mod area;
pub mod circles;
pub mod custom;
pub mod draw;
pub mod growth;
pub mod lines;
pub mod lsystem;
pub mod parametric;
pub mod path;
pub mod pen;
pub mod polygons;
pub mod rect;
pub mod region;
pub mod spirograph;

pub use pen::{Band, Brush, Counter, Draw, Emitter, Fold, Maps, Pen, RowMap, Sink, TimeMap};

use super::boundary::{self, PitchPolicy};
use super::param::*;
use super::registry::{BuildCtx, Family, FunctionSpec, ToolSpec};
use super::tools::{NOTE_VALUES, note_value};
use super::{Ctx, Estimate, Growth, Kernel, Output, Source, SourceStats, TNote, TransformError};
use crate::model::project::KeyRange;

/// The Advanced generators (registry ids; the others are Simple — §151).
pub const ADVANCED: &[&str] = &[
    "gen.custom",
    "gen.parametric",
    "gen.lsystem",
    "gen.spirograph",
    "gen.growth",
];

/// Whether generator `id` is one of the five Advanced ones.
pub fn is_advanced(id: &str) -> bool {
    ADVANCED.contains(&id)
}

/// Where the generation box is.
pub const PLACES: &[&str] = &["At the cursor", "Time selection", "Selected notes' box"];

/// The longest generation box (ticks; about 17.9 million 4/4 bars at 960
/// PPQ): the preflight, not this limit, guards the output.
pub const MAX_LENGTH: u64 = 1 << 36;

/// The shared controls every generator has, before its own (§162).
pub fn base_specs() -> Vec<ParamSpec> {
    let mut v = vec![
        choice("gen.place", "Place", PLACES, 0).help(
            "At the cursor (with the length below), over the ruler's time selection, or over the selected notes' time and pitch box — each falls back to the cursor.",
        ),
        ticks("gen.length", "Length", 1, MAX_LENGTH, 3840)
            .help("How long the generation box is when it is placed at the cursor."),
        pitch("gen.low", "Lowest pitch", 48)
            .help("The generation box's lowest key (the selected notes' box replaces it when Place uses them)."),
        pitch("gen.high", "Highest pitch", 84)
            .help("The generation box's highest key (the selected notes' box replaces it when Place uses them)."),
        choice("gen.step", "Resolution", NOTE_VALUES, 10)
            .help("The grid every generated note starts and ends on (Snap: the Piano Roll's)."),
        ticks("gen.step_ticks", "Custom resolution (ticks)", 1, 1 << 24, 120)
            .help("The grid in ticks when Resolution is Custom ticks."),
        int("gen.vel", "Velocity", 1, 127, 100).help("The velocity of every generated note (functions may vary it)."),
        float("gen.gate", "Gate", 1.0, 100.0, 100.0, "%")
            .help("The part of each generated note that sounds, from its start."),
        int("gen.sub", "Subchannel (position in the track)", 0, 4095, 0)
            .help("Which of the track's subchannels the generated notes go to, by position in its list (0 = the first)."),
        flag("gen.keep", "Keep the selected notes", true)
            .help("On: the selected notes stay and the generated notes are added. Off: the generated notes replace them."),
    ];
    v.extend(boundary::specs(PitchPolicy::Discard));
    v
}

/// The seed control (§162 "Seed where random"): only generators with a
/// function that draws random values add it to their own controls (a seed
/// that changes nothing is not offered — the v0.7 harness checks).
pub fn seed_spec() -> ParamSpec {
    seed("gen.seed")
}

/// A generator's spec: the Generator family, the shared controls, then
/// `own` (base controls, [`seed_spec`] when random, and the functions'
/// switches / controls declared by id in `functions`).
pub fn spec(
    id: &'static str,
    name: &'static str,
    summary: &'static str,
    own: Vec<ParamSpec>,
    functions: &[(&'static str, &'static str, u16)],
) -> ToolSpec {
    let mut params = base_specs();
    params.extend(own);
    ToolSpec {
        id,
        name,
        family: Family::Generator,
        summary,
        params,
        advanced: Vec::new(),
        functions: functions
            .iter()
            .map(|&(id, name, order)| FunctionSpec { id, name, order })
            .collect(),
        needs_selection: false,
        context: false,
    }
}

/// The generation box: ticks `[t0, t1)` and pitches `lo ..= hi`, on a grid
/// of `step` ticks, with what the generators read from the context.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub t0: u64,
    pub t1: u64,
    pub lo: u8,
    pub hi: u8,
    /// The resolution grid (ticks, ≥ 1).
    pub step: u64,
    pub ppq: u64,
    /// The Piano Roll's snap (ticks).
    pub snap: u64,
    pub keys: KeyRange,
    /// `gen.seed` (0 for a generator without one).
    pub seed: u64,
    /// The shared brush: velocity, gate, subchannel position.
    pub brush: Brush,
}

impl Frame {
    /// The box the shared controls give in `ctx` (the selection's statistics
    /// for "Selected notes' box").
    pub fn of(p: &Params, ctx: &Ctx, stats: &SourceStats) -> Frame {
        let top = ctx.keys.max_pitch();
        let clamp_p = |k: &str| p.int(k).clamp(0, i64::from(top)) as u8;
        let (mut lo, mut hi) = (clamp_p("gen.low"), clamp_p("gen.high"));
        if lo > hi {
            std::mem::swap(&mut lo, &mut hi);
        }
        let len = p.ticks("gen.length").clamp(1, MAX_LENGTH);
        let at_cursor = (ctx.cursor, ctx.cursor.saturating_add(len));
        let (t0, t1) = match p.choice("gen.place") {
            1 => ctx.time_range.filter(|&(a, b)| b > a).unwrap_or(at_cursor),
            2 if stats.count > 0 => {
                lo = stats.min_pitch.min(top);
                hi = stats.max_pitch.min(top).max(lo);
                (stats.min_start, stats.max_end.max(stats.min_start + 1))
            }
            _ => at_cursor,
        };
        Frame {
            t0,
            t1: t1.max(t0.saturating_add(1)),
            lo,
            hi,
            step: note_value(p.choice("gen.step"), p.ticks("gen.step_ticks"), ctx).max(1),
            ppq: ctx.ppq.max(1),
            snap: ctx.snap.max(1),
            keys: ctx.keys,
            seed: p.seed("gen.seed"),
            brush: Brush {
                vel: p.int("gen.vel").clamp(1, 127) as u8,
                gate: (p.float("gen.gate") / 100.0).clamp(0.01, 1.0),
                sub: p.int("gen.sub").clamp(0, 4095) as u32,
            },
        }
    }

    /// The box's width in ticks.
    pub fn width(&self) -> f64 {
        (self.t1 - self.t0) as f64
    }

    /// Pitch rows from the lowest to the highest (≥ 1).
    pub fn rows(&self) -> u64 {
        u64::from(self.hi - self.lo) + 1
    }

    /// Tick at `u` (0 = the box's start, 1 = its end).
    #[inline]
    pub fn x(&self, u: f64) -> f64 {
        self.t0 as f64 + u * self.width()
    }

    /// Row coordinate at `v` (0 = the lowest pitch's row centre, 1 = the
    /// highest's).
    #[inline]
    pub fn y(&self, v: f64) -> f64 {
        f64::from(self.lo) + 0.5 + v * f64::from(self.hi - self.lo)
    }

    /// The grid steps across the box (≥ 1).
    pub fn steps(&self) -> u64 {
        (self.t1 - self.t0).div_ceil(self.step).max(1)
    }

    /// The rows the pen draws on.
    pub fn band(&self) -> Band {
        Band::around(self.keys.keys())
    }
}

/// Where the counting pass stops (the platform's hard cap on output notes,
/// `Limits::max_notes`, never exceeds it): beyond, the count is `COUNT_CAP
/// + 1` and the preflight refuses the run.
pub const COUNT_CAP: u64 = 50_000_000;

/// What a generator does: describe its shapes, and bound them.
pub trait Shapes: Send + Sync {
    /// Draws every shape with `pen`, always in the same order; returns false
    /// as soon as the pen does (the output is full or cancelled).
    fn draw(&self, f: &Frame, pen: &mut dyn Draw) -> bool;

    /// An upper bound of the notes [`Shapes::draw`] makes in `f`, without
    /// drawing (cheap: the stats-only preflight on the UI thread).
    fn bound(&self, f: &Frame) -> u64;

    /// A cheap lower bound of the notes [`Shapes::draw`] makes in `f` (0 =
    /// unknown, the default). When it passes [`COUNT_CAP`] the counting
    /// pass refuses at once instead of counting that far (V09-S52: a
    /// sampled line over a huge box).
    fn floor(&self, _f: &Frame) -> u64 {
        0
    }

    /// The pen's maps (row fold / scale snap, time grid pull) for `f`;
    /// identity unless the generator asks (V09-S52). Maps never add notes,
    /// so [`Shapes::bound`] holds with them.
    fn maps(&self, _f: &Frame) -> Maps {
        Maps::default()
    }

    /// The source, before anything is counted or drawn (V09-S61: the
    /// growth's External Track Field reads the auxiliary notes its
    /// preparation collected). Shapes that read it bound themselves for
    /// any source in [`Shapes::bound`], which comes first; an error (a cap
    /// only the source can pass) refuses the run — the estimate counts
    /// nothing for it.
    fn source(&self, _src: &Source, _f: &Frame) -> Result<(), TransformError> {
        Ok(())
    }
}

/// The kernel of every generator: the selection kept (when asked), then the
/// shapes rasterized into the active track.
pub struct GenKernel<S: Shapes> {
    shapes: S,
    frame: Frame,
    keep: bool,
    /// The target track's slot and its subchannel table.
    slot: u16,
    subs: Vec<(u32, u8)>,
    bound: u64,
}

/// Builds the kernel of a generator with `shapes` (the target track: the
/// active one's slot, else the first).
pub fn kernel<S: Shapes + 'static>(
    p: &Params,
    b: &BuildCtx,
    shapes: S,
) -> Result<Box<dyn Kernel>, TransformError> {
    let frame = Frame::of(p, b.ctx, b.stats);
    let slot = b
        .ctx
        .active
        .and_then(|a| b.tracks.iter().position(|t| t.index == a))
        .unwrap_or(0);
    let subs = b
        .tracks
        .get(slot)
        .map(|t| t.subs.clone())
        .unwrap_or_default();
    let bound = shapes.bound(&frame);
    Ok(Box::new(GenKernel {
        shapes,
        frame,
        keep: p.flag("gen.keep"),
        slot: slot as u16,
        subs,
        bound,
    }))
}

impl<S: Shapes> GenKernel<S> {
    /// The notes kept from the selection.
    fn kept(&self, n: u64) -> u64 {
        if self.keep { n } else { 0 }
    }

    /// The exact number of notes the shapes make (the counting pen; past
    /// [`COUNT_CAP`] it stops: any limit refuses that many).
    pub fn count(&self) -> u64 {
        if self.shapes.floor(&self.frame) > COUNT_CAP {
            return COUNT_CAP + 1;
        }
        let mut pen = Pen::new(
            Counter::capped(COUNT_CAP),
            self.frame.step,
            self.frame.band(),
        )
        .with_maps(self.shapes.maps(&self.frame));
        self.shapes.draw(&self.frame, &mut pen);
        pen.finish().notes
    }
}

impl<S: Shapes> Kernel for GenKernel<S> {
    fn growth(&self) -> Growth {
        Growth::Generate(self.bound)
    }

    fn estimate(&self, stats: &SourceStats) -> Estimate {
        let n = stats.count as u64;
        Estimate::for_notes(n, self.kept(n).saturating_add(self.bound))
    }

    fn estimate_source(&self, src: &Source) -> Estimate {
        // A refusal comes from `run`, which calls it again.
        let _ = self.shapes.source(src, &self.frame);
        let n = src.notes.len() as u64;
        Estimate::for_notes(n, self.kept(n).saturating_add(self.count()))
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        if src.tracks.is_empty() {
            return Err(TransformError::Invalid(
                "Choose a track (or open a clip) to generate notes into.".into(),
            ));
        }
        self.shapes.source(src, &self.frame)?;
        if self.keep {
            for n in &src.notes {
                if !out.push(*n) {
                    return out.check();
                }
            }
        }
        let (sub, ch) = self.subs.first().copied().unwrap_or((0, 0));
        let tpl = TNote {
            start: 0,
            dur: 1,
            sub,
            group: 0,
            track: self.slot,
            pitch: 0,
            vel: self.frame.brush.vel,
            rel: 64,
            ch,
            flags: 0,
        };
        let mut pen = Pen::new(
            Emitter {
                out,
                tpl,
                subs: &self.subs,
            },
            self.frame.step,
            self.frame.band(),
        )
        .with_maps(self.shapes.maps(&self.frame));
        self.shapes.draw(&self.frame, &mut pen);
        let em = pen.finish();
        em.out.check()
    }
}

/// The frame's shared brush with velocity `vel` (1 – 127).
pub fn with_vel(b: Brush, vel: f64) -> Brush {
    Brush {
        vel: vel.round().clamp(1.0, 127.0) as u8,
        ..b
    }
}
