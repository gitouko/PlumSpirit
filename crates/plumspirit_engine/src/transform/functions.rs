//! The four mandatory advanced functions of every eligible tool (v0.9
//! contract §098, §231–§236), applied by the platform around the tool's own
//! kernel — never copied into 58 tools:
//!
//! 1. **Subchannel Offset Driver** — the shared driver ([`super::drivers`])
//!    moves each result note's subchannel (Paint V2's wrap policies);
//! 2. **Pitch Offset Driver** — the same driver moves its pitch (through the
//!    tool's boundary policy);
//! 3. **Remove Note Overlaps** — the shared service ([`super::overlap`]) on
//!    the finished result;
//! 4. **Control Sweep** — the tool's declared sweep targets, evaluated by
//!    [`super::sweep`]: the notes are grouped by their place along each
//!    enabled graph and every group is processed with its parameters.
//!
//! Composition order (§235, §236), shown in Preview: the Control Sweep
//! chooses each group's parameters → the tool runs with its own functions in
//! their documented order → pitch offset → subchannel offset → overlaps.
//! v1.0-beta2 B2-F01: the last three form the Output group, run in the order
//! the user chooses (`order::OUTPUT`; declared = the order above).
//! With every mandatory function off a tool runs exactly as before (its own
//! kernel, unwrapped).

use super::definition::{DriverDim, SweepTarget};
use super::drivers::{Driver, Frame, Input, Mapping, Place, SUB_WRAPS, SubWrap, move_sub};
use super::overlap::{Mode, Policy};
use super::param::{
    Curve, ParamSet, ParamSpec, ParamValue, Params, choice, curve, flag, int, list, ticks,
};
use super::preflight::{Estimate, Growth};
use super::registry::{BuildCtx, BuildFn, ToolSpec};
use super::source::{Source, SourceStats, SourceTrack};
use super::sweep::{self, Sweep};
use super::tools::{NOTE_VALUES, note_value};
use super::{Ctx, Kernel, Output, PointKernel, TNote, TransformError};
use std::sync::Arc;

/// Switch of the Subchannel Offset Driver.
pub const SUB: &str = "m.sub";
/// Switch of the Pitch Offset Driver.
pub const PITCH: &str = "m.pitch";
/// Switch of Remove Note Overlaps.
pub const OVERLAPS: &str = "m.ovl";
/// Switch of the Control Sweep.
pub const SWEEP: &str = "m.sweep";

/// (stable id, switch, name) of the mandatory functions, in window and
/// composition order (catalog `order` 900–930).
pub const MANDATORY: [(&str, &str, &str); 4] = [
    ("m_sub_offset", SUB, "Subchannel Offset Driver"),
    ("m_pitch_offset", PITCH, "Pitch Offset Driver"),
    ("m_overlaps", OVERLAPS, "Remove Note Overlaps"),
    ("m_sweep", SWEEP, "Control Sweep"),
];

/// How a driver's input becomes an offset (menu).
pub const MAPS: &[&str] = &["Step every …", "Cycle a list", "Graph"];

/// The parameters of a driver function under `pre` (`m.sub` / `m.pitch`);
/// `dim`: the tool's own dimension, the seventh input (V09-0143).
fn driver_specs(pre: &'static str, n: u8, pitch: bool, dim: Option<&DriverDim>) -> Vec<ParamSpec> {
    // Keys are spelled out (static) per driver.
    let k = |s: &'static str, p: &'static str| if pitch { p } else { s };
    let by = choice(
        k("m.sub.by", "m.pitch.by"),
        "Driven by",
        dim.map_or(Input::NAMES, |d| d.menu),
        0,
    );
    let mut v = vec![
        match dim {
            Some(d) => by.help(d.help),
            None => by.help(
                "What sets each note's offset: its time after the first selected note, its pitch above the lowest one, its place in start order on its track, its velocity, its subchannel's place in the track's list, or its time within the pattern period.",
            ),
        },
        choice(k("m.sub.map", "m.pitch.map"), "Offset", MAPS, 0).help(
            "How the input becomes an offset: Step every adds one step each time the input grows by the step size; Cycle a list takes the listed offsets in turn, one per step; Graph maps the input's range (first to last note, lowest to highest) through the graph.",
        ),
        choice(
            k("m.sub.every", "m.pitch.every"),
            "Step every (time, pattern phase)",
            NOTE_VALUES,
            2,
        )
        .help("Driven by Time or Pattern phase: how much later a note must start for the offset to take one more step (Custom ticks: the value below)."),
        ticks(
            k("m.sub.every.ticks", "m.pitch.every.ticks"),
            "Custom step (ticks)",
            1,
            1 << 40,
            960,
        )
        .help("The step length in ticks when Step every is Custom ticks."),
        int(
            k("m.sub.every.n", "m.pitch.every.n"),
            "Step every (semitones, notes, velocity, subchannels, the tool's dimension)",
            1,
            1_000_000,
            1,
        )
        .help("Driven by anything but time: how many semitones, notes, velocity levels, subchannels or units of the tool's dimension make one step."),
        choice(
            k("m.sub.period", "m.pitch.period"),
            "Pattern period",
            NOTE_VALUES,
            0,
        )
        .help("Driven by Pattern phase: the length of the repeating pattern; every period the phase starts again at 0 (Custom ticks: the value below)."),
        ticks(
            k("m.sub.period.ticks", "m.pitch.period.ticks"),
            "Custom period (ticks)",
            1,
            1 << 40,
            3840,
        )
        .help("The pattern period in ticks when Pattern period is Custom ticks."),
    ];
    if pitch {
        v.extend([
            int("m.pitch.step", "Semitones per step (+/−)", -127, 127, 12)
                .keyed()
                .help("Semitones added at each step (negative: down)."),
            list(
                "m.pitch.cycle",
                "Semitones in turn (+/−)",
                -127,
                127,
                64,
                &[0, 12, 7],
            )
            .keyed()
            .help("Transpositions in semitones taken in turn, one per step, starting over after the last."),
            curve("m.pitch.graph", "Graph", Curve::line(0.0, 1.0)).help(
                "The transposition across the input's range: left is the first / lowest input, right the last / highest; the bottom is Graph from, the top Graph to.",
            ),
            int("m.pitch.lo", "Graph from (semitones)", -127, 127, -12)
                .keyed()
                .help("The transposition where the graph is at its bottom."),
            int("m.pitch.hi", "Graph to (semitones)", -127, 127, 12)
                .keyed()
                .help("The transposition where the graph is at its top."),
        ]);
    } else {
        v.extend([
            int("m.sub.step", "Subchannels per step (+/−)", -4096, 4096, 1)
                .help("Subchannels moved at each step (negative: toward the first)."),
            list(
                "m.sub.cycle",
                "Subchannel offsets in turn (+/−)",
                -4096,
                4096,
                64,
                &[0, 1, 2, 3],
            )
            .help("Subchannel offsets taken in turn, one per step, starting over after the last."),
            curve("m.sub.graph", "Graph", Curve::line(0.0, 1.0)).help(
                "The subchannel offset across the input's range: left is the first / lowest input, right the last / highest; the bottom is Graph from, the top Graph to.",
            ),
            int("m.sub.lo", "Graph from (subchannels)", -4096, 4096, 0)
                .help("The subchannel offset where the graph is at its bottom."),
            int("m.sub.hi", "Graph to (subchannels)", -4096, 4096, 3)
                .help("The subchannel offset where the graph is at its top."),
            choice("m.sub.wrap", "Subchannels", SUB_WRAPS, 0).help(
                "Where a moved note lands: around the track's own subchannels in their order, wrapped, reflected or clamped inside the range below, or anywhere (subchannels that do not exist yet are made).",
            ),
            int("m.sub.range.lo", "Range: first subchannel", 1, 1 << 24, 1)
                .help("The first subchannel of the range (Wrap, Reflect and Clamp)."),
            int("m.sub.range.n", "Range: subchannels", 1, 4096, 4)
                .help("How many subchannels the range holds."),
            flag(
                "m.sub.set_channel",
                "Also set the subchannel's MIDI channel",
                false,
            )
            .help("On: a moved note also takes its new subchannel's MIDI channel. Off: every note keeps its own channel."),
        ]);
    }
    let name = if pitch {
        "Pitch Offset Driver"
    } else {
        "Subchannel Offset Driver"
    };
    let help = if pitch {
        "Transposes each note of the result by an offset that something about the note drives (its time, pitch, order, velocity, subchannel or pattern phase). Runs after the tool; notes pushed outside the keys follow Outside the key range."
    } else {
        "Moves each note of the result to another subchannel by an offset that something about the note drives (its time, pitch, order, velocity, subchannel or pattern phase). Runs after the tool — after the pitch offset unless the execution order says otherwise."
    };
    let mut out = vec![flag(pre, name, false).help(help).adv(n)];
    out.extend(v.into_iter().map(|p| p.adv(n)));
    out
}

/// The parameters of the mandatory functions of a tool whose own functions
/// number `n0`, with its declared sweep targets (no targets: no Control
/// Sweep function) and its driver dimension (none: the generic inputs).
pub fn specs(n0: u8, sweeps: &[SweepTarget], dim: Option<&DriverDim>) -> Vec<ParamSpec> {
    let mut v = driver_specs(SUB, n0 + 1, false, dim);
    v.extend(driver_specs(PITCH, n0 + 2, true, dim));
    let n = n0 + 3;
    v.extend([
        flag(OVERLAPS, "Remove Note Overlaps", false)
            .help("Cleans the result of duplicate or overlapping notes; runs last unless the execution order puts it before an offset driver.")
            .adv(n),
        choice("m.ovl.mode", "Remove", Mode::NAMES, 0)
            .help("Exact duplicates: same start, length and pitch, the loudest stays. Same pitch and start: the longest stays. Same-pitch overlaps: the earlier note ends where the later one starts, or the later note goes.")
            .adv(n),
        flag(
            "m.ovl.generated",
            "Only notes the tool created or changed",
            true,
        )
        .help("On: notes the tool left untouched always stay as they are; only the tool's own results are trimmed or removed.")
        .adv(n),
        flag(
            "m.ovl.across",
            "Across subchannels and MIDI channels",
            false,
        )
        .help("On: notes on different subchannels or MIDI channels count as overlapping too. Off: each subchannel and channel is its own layer.")
        .adv(n),
    ]);
    if !sweeps.is_empty() {
        let n = n0 + 4;
        v.push(
            flag(SWEEP, "Control Sweep", false)
                .help("Lets graphs drive the tool's controls along the notes: the notes are grouped by where they fall on each switched-on graph, and every group runs with the values its graphs give.")
                .adv(n),
        );
        for s in sweeps {
            let (on, graph) = sweep_help(s, dim);
            v.push(flag(s.on, s.label, false).help(on).adv(n));
            v.push(
                curve(s.graph, s.graph_label, Curve::line(0.0, 1.0))
                    .help(graph)
                    .adv(n),
            );
        }
    }
    v
}

/// The help texts of one Control Sweep target (its switch, its graph),
/// built from what the target declares (B1-0066): the control, the
/// domain, the range and how the swept value meets the control's own.
fn sweep_help(s: &SweepTarget, dim: Option<&DriverDim>) -> (&'static str, &'static str) {
    let along = match s.domain {
        Input::Time => "time (first to last note)".to_owned(),
        Input::Pitch => "pitch (lowest to highest note)".to_owned(),
        Input::NoteIndex => "note order (first to last note on each track)".to_owned(),
        Input::Velocity => "velocity (1 to 127)".to_owned(),
        Input::Subchannel => "subchannel (the track's first to last)".to_owned(),
        Input::PatternPhase => "the pattern phase (start to end of each period)".to_owned(),
        Input::Tool => match dim {
            Some(d) => format!("{} (first to last)", d.name.to_lowercase()),
            None => "the tool's dimension".to_owned(),
        },
    };
    let num = |x: f64| {
        if x.fract() == 0.0 {
            format!("{x:.0}")
        } else {
            format!("{x}")
        }
    };
    let (lo, hi) = (num(s.range.0), num(s.range.1));
    let how = match s.combine {
        super::definition::Combine::Replace => "replaces the control's own value",
        super::definition::Combine::Add => "is added to the control's own value",
        super::definition::Combine::Multiply => "multiplies the control's own value",
    };
    let on = super::param::intern(format!(
        "Sweeps {} along {along}: the graph's value ({lo} at the bottom, {hi} at the top) {how}.",
        s.label
    ));
    let graph = super::param::intern(format!(
        "{} along {along}: left to right is the start to the end of the domain, bottom to top {lo} to {hi}.",
        s.label
    ));
    (on, graph)
}

/// A driver from its parameters under `pre`.
fn driver(p: &Params, pre: &str, ctx: &Ctx) -> Driver {
    let key = |s: &str| format!("{pre}.{s}");
    let input = Input::from_index(p.choice(&key("by")));
    let every = match input {
        Input::Time | Input::PatternPhase => {
            note_value(p.choice(&key("every")), p.ticks(&key("every.ticks")), ctx)
        }
        _ => p.int(&key("every.n")).max(1) as u64,
    };
    let mapping = match p.choice(&key("map")) {
        0 => Mapping::Step(p.int(&key("step"))),
        1 => Mapping::Cycle(p.list(&key("cycle"))),
        _ => Mapping::Graph {
            curve: p.curve(&key("graph")),
            lo: p.int(&key("lo")),
            hi: p.int(&key("hi")),
        },
    };
    Driver {
        input,
        every: every.max(1),
        period: note_value(p.choice(&key("period")), p.ticks(&key("period.ticks")), ctx).max(1),
        mapping,
    }
}

fn on(set: &ParamSet, key: &str) -> bool {
    matches!(set.get_raw(key), Some(ParamValue::Bool(true)))
}

/// The tool's kernel with its mandatory functions applied (the kernel itself
/// when none is on or the tool is not eligible).
pub fn wrap(
    id: &'static str,
    build: BuildFn,
    spec: &ToolSpec,
    set: &ParamSet,
    b: &BuildCtx,
    inner: Box<dyn Kernel>,
) -> Result<Box<dyn Kernel>, TransformError> {
    if ![SUB, PITCH, OVERLAPS, SWEEP].iter().any(|k| on(set, k))
        || !super::definition::eligible(id, spec.family)
    {
        return Ok(inner);
    }
    let targets = super::definition::sweep_targets(id);
    let n0 = spec.own_functions() as u8;
    let mut all = spec.params.clone();
    all.extend(specs(n0, targets, super::definition::driver_dimension(id)));
    let p = Params::new(&all, set).with_keys(b.ctx.keys);
    let sub = on(set, SUB).then(|| {
        (
            driver(&p, SUB, b.ctx),
            SubWrap::from_choice(
                p.choice("m.sub.wrap"),
                p.int("m.sub.range.lo").max(1) - 1,
                p.int("m.sub.range.n"),
            ),
            p.flag("m.sub.set_channel"),
        )
    });
    let pitch = on(set, PITCH).then(|| driver(&p, PITCH, b.ctx));
    let overlaps = on(set, OVERLAPS).then(|| Policy {
        mode: Mode::from_index(p.choice("m.ovl.mode")),
        generated_only: p.flag("m.ovl.generated"),
        across: p.flag("m.ovl.across"),
    });
    let mut sweeps: Vec<Sweep> = if on(set, SWEEP) {
        targets
            .iter()
            .filter(|t| p.flag(t.on))
            .map(|t| Sweep {
                target: *t,
                graph: p.curve(t.graph),
            })
            .collect()
    } else {
        Vec::new()
    };
    // B2-F01: the Output group in the order the values ask for (declared:
    // pitch offset → subchannel offset → overlaps).
    let post: Vec<Post> = p
        .sequence(super::order::OUTPUT.members)
        .into_iter()
        .filter_map(|id| match id {
            "m_pitch_offset" if pitch.is_some() => Some(Post::Pitch),
            "m_sub_offset" if sub.is_some() => Some(Post::Sub),
            "m_overlaps" if overlaps.is_some() => Some(Post::Overlaps),
            _ => None,
        })
        .collect();
    let mut inner = inner;
    if !sweeps.is_empty() && super::definition::kernel_sweeps(id) {
        // V09-0143: the kernel evaluates the sweeps per unit; rebuild it
        // with them (the source is not split by levels).
        let specs = Arc::new(spec.params.clone());
        let unit = sweep::UnitSweeps::new(
            std::mem::take(&mut sweeps),
            specs.clone(),
            Frame::of(b.stats),
            b.ctx.bar(),
            b.ctx.keys,
        );
        let mut ctx = b.ctx.clone();
        ctx.sweeps = Some(Arc::new(unit));
        let bb = BuildCtx {
            ctx: &ctx,
            stats: b.stats,
            tracks: b.tracks,
        };
        inner = build(&Params::new(&specs, set).with_keys(b.ctx.keys), &bb)?;
        if sub.is_none() && pitch.is_none() && overlaps.is_none() {
            return Ok(inner);
        }
    }
    Ok(Box::new(Composite {
        build,
        specs: Arc::new(spec.params.clone()),
        base: set.clone(),
        inner,
        sweeps,
        sub,
        pitch,
        overlaps,
        post,
        ctx: b.ctx.clone(),
        frame: Frame::of(b.stats),
        tracks: b.tracks.to_vec(),
    }))
}

/// A stage of the platform's Output group (B2-F01).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Post {
    Pitch,
    Sub,
    Overlaps,
}

/// A tool's kernel with its mandatory functions.
struct Composite {
    /// The tool's kernel builder (the Control Sweep rebuilds it per group).
    build: BuildFn,
    /// The tool's own parameter specs (for the swept parameter sets).
    specs: Arc<Vec<ParamSpec>>,
    base: ParamSet,
    inner: Box<dyn Kernel>,
    sweeps: Vec<Sweep>,
    sub: Option<(Driver, SubWrap, bool)>,
    pitch: Option<Driver>,
    overlaps: Option<Policy>,
    /// The switched-on Output stages in run order (B2-F01).
    post: Vec<Post>,
    ctx: Ctx,
    frame: Frame,
    tracks: Vec<SourceTrack>,
}

impl Composite {
    fn drives(&self) -> bool {
        self.sub.is_some() || self.pitch.is_some()
    }

    /// A driver is driven by the tool's dimension (V09-0143).
    fn by_dim(&self) -> bool {
        self.sub.as_ref().is_some_and(|s| s.0.input == Input::Tool)
            || self.pitch.as_ref().is_some_and(|d| d.input == Input::Tool)
    }

    fn pointwise_drivers(&self) -> bool {
        self.sub.as_ref().is_none_or(|s| s.0.is_pointwise())
            && self.pitch.as_ref().is_none_or(Driver::is_pointwise)
    }

    /// The offset drivers among `stages`, in that order (declared: pitch
    /// offset, then subchannel offset — §235), on every note emitted since
    /// `mark`.
    fn drive(&self, out: &mut Output, mark: super::output::Mark, stages: &[Post]) {
        if !self.drives() || !stages.iter().any(|s| matches!(s, Post::Pitch | Post::Sub)) {
            return;
        }
        let need_place = !self.pointwise_drivers();
        let (places, counts) = if need_place {
            out.places_since(mark)
        } else {
            (Vec::new(), Default::default())
        };
        let dims = if self.by_dim() {
            out.dims_since(mark)
        } else {
            Vec::new()
        };
        let dim_max = dims.iter().copied().max().unwrap_or(0);
        out.adjust_since(mark, |i, n, bound, stats| {
            let tr = self.tracks.get(usize::from(n.track));
            let place = Place {
                index: places.get(i).copied().unwrap_or(0),
                count: counts.get(&n.track).copied().unwrap_or(1),
                sub_pos: tr.and_then(|t| t.sub_index(n.sub)).unwrap_or(0),
                subs: tr.map_or(1, |t| t.subs.len().max(1)),
                dim: dims.get(i).copied().unwrap_or(0),
                dim_max,
            };
            for stage in stages {
                match stage {
                    Post::Pitch => {
                        let Some(d) = &self.pitch else { continue };
                        let off = d.offset(n, place, &self.frame);
                        if off != 0 {
                            let (p, changed) = bound.pitch(i64::from(n.pitch).saturating_add(off));
                            match p {
                                Some(p) => {
                                    if changed {
                                        stats.pitch_changed += 1;
                                    }
                                    n.pitch = p;
                                }
                                None => {
                                    stats.pitch_dropped += 1;
                                    return false;
                                }
                            }
                        }
                    }
                    Post::Sub => {
                        let Some((d, wrap, set_channel)) = &self.sub else {
                            continue;
                        };
                        let off = d.offset(n, place, &self.frame);
                        if off != 0 {
                            let (id, ch) = move_sub(*wrap, *set_channel, tr, n, off, None);
                            n.sub = id;
                            n.ch = ch;
                        }
                    }
                    Post::Overlaps => {}
                }
            }
            true
        });
    }

    /// The Output group after the tool (B2-F01): runs of offset drivers in
    /// one pass each; Remove Note Overlaps in place when a driver follows it
    /// — listed last it runs on the finished result ([`Kernel::overlaps`]),
    /// as before.
    fn post_steps(&self, src: &Source, out: &mut Output, mark: super::output::Mark) {
        let mut i = 0;
        while i < self.post.len() {
            if self.post[i] == Post::Overlaps {
                if i + 1 < self.post.len()
                    && let Some(policy) = self.overlaps
                {
                    out.overlaps_since(mark, &src.notes, &src.locked, policy);
                }
                i += 1;
                continue;
            }
            let j = (i..self.post.len())
                .find(|&k| self.post[k] == Post::Overlaps)
                .unwrap_or(self.post.len());
            self.drive(out, mark, &self.post[i..j]);
            i = j;
        }
    }

    /// The Control Sweep's groups of `src`: each group's parameter set and
    /// the indices of its notes.
    fn plan(&self, src: &Source) -> Vec<(ParamSet, Vec<usize>)> {
        // Each note's place on its track (time-major source order).
        let mut per_track: std::collections::HashMap<u16, usize> = Default::default();
        let mut places: Vec<Place> = src
            .notes
            .iter()
            .map(|n| {
                let c = per_track.entry(n.track).or_insert(0);
                let index = *c;
                *c += 1;
                let tr = src.track(n.track);
                Place {
                    index,
                    count: 0,
                    sub_pos: tr.and_then(|t| t.sub_index(n.sub)).unwrap_or(0),
                    subs: tr.map_or(1, |t| t.subs.len().max(1)),
                    ..Place::default()
                }
            })
            .collect();
        for (p, n) in places.iter_mut().zip(&src.notes) {
            p.count = per_track.get(&n.track).copied().unwrap_or(1);
        }
        let frame = Frame::of(&src.stats);
        let (q, groups) = sweep::group(&self.sweeps, &src.notes, &places, &frame, self.ctx.bar());
        groups
            .into_iter()
            .map(|(k, idx)| {
                (
                    sweep::params_for(&self.base, &self.specs, &self.sweeps, k, q, self.ctx.keys),
                    idx,
                )
            })
            .collect()
    }

    /// The part of `src` made of the notes `idx` (its own statistics; the
    /// tracks, context and auxiliary layers shared).
    fn segment(src: &Source, idx: &[usize]) -> Source {
        let notes: Vec<TNote> = idx.iter().map(|&i| src.notes[i]).collect();
        let mut stats = SourceStats::default();
        for n in &notes {
            stats.add(n);
        }
        stats.min_gap = notes
            .windows(2)
            .filter_map(|w| w[1].start.checked_sub(w[0].start).filter(|&g| g > 0))
            .min()
            .unwrap_or(u64::MAX);
        Source {
            notes,
            stats,
            tracks: src.tracks.clone(),
            context: src.context.clone(),
            locked: Vec::new(),
            groups: src.groups.clone(),
            aux: src.aux.clone(),
            aux_slot: src.aux_slot,
            aux_events: src.aux_events.clone(),
            routes: src.routes.clone(),
            keys: src.keys,
            shift: src.shift,
        }
    }

    /// The tool's kernel for parameter set `set`, built against the whole
    /// source (geometry boxes and spans stay those of the selection).
    fn build(&self, set: &ParamSet, src: &Source) -> Result<Box<dyn Kernel>, TransformError> {
        let b = BuildCtx {
            ctx: &self.ctx,
            stats: &src.stats,
            tracks: &src.tracks,
        };
        let p = Params::new(&self.specs, set).with_keys(self.ctx.keys);
        (self.build)(&p, &b)
    }

    fn sweeping(&self, src: &Source) -> bool {
        !self.sweeps.is_empty() && !src.notes.is_empty()
    }
}

impl Kernel for Composite {
    fn growth(&self) -> Growth {
        self.inner.growth()
    }

    fn estimate(&self, stats: &SourceStats) -> Estimate {
        self.inner.estimate(stats)
    }

    fn estimate_source(&self, src: &Source) -> Estimate {
        if !self.sweeping(src) {
            return self.inner.estimate_source(src);
        }
        // Each group runs the tool on its part: the sum bounds the result.
        let mut total = Estimate::default();
        for (set, idx) in self.plan(src) {
            let part = Self::segment(src, &idx);
            let e = match self.build(&set, src) {
                Ok(k) => k.estimate_source(&part),
                Err(_) => continue, // reported by the run
            };
            total.notes = total.notes.saturating_add(e.notes);
            total.temp_bytes = total.temp_bytes.max(e.temp_bytes);
        }
        total
    }

    fn as_point(&self) -> Option<&dyn PointKernel> {
        if !self.sweeps.is_empty() || self.overlaps.is_some() || !self.pointwise_drivers() {
            return None;
        }
        self.inner.as_point().map(|_| self as &dyn PointKernel)
    }

    fn overlaps(&self) -> Option<Policy> {
        // Moved before a driver it ran inside the run (`post_steps`).
        if self.post.last() == Some(&Post::Overlaps) {
            self.overlaps
        } else {
            None
        }
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let mark = out.mark();
        if self.by_dim() {
            out.enable_dims();
        }
        if self.sweeping(src) {
            for (set, idx) in self.plan(src) {
                let part = Self::segment(src, &idx);
                out.set_dim(0);
                self.build(&set, src)?.run(&part, out)?;
                out.check()?;
            }
        } else {
            self.inner.run(src, out)?;
        }
        self.post_steps(src, out, mark);
        out.check()
    }
}

impl PointKernel for Composite {
    fn map(&self, n: &TNote, out: &mut Output) {
        let Some(p) = self.inner.as_point() else {
            return;
        };
        let mark = out.mark();
        p.map(n, out);
        self.drive(out, mark, &self.post);
    }

    fn source_window(&self, t0: u64, t1: u64) -> (u64, u64) {
        self.inner
            .as_point()
            .map_or((t0, t1), |p| p.source_window(t0, t1))
    }
}
