//! Curve generation for the event graphs: turns a curve gesture (two anchors or
//! a free-hand stroke) plus the curve settings into the events it writes, and
//! the same computation (with a small budget) into the live preview.
//!
//! What a curve writes, by graph target:
//!
//! | Target | Events |
//! |---|---|
//! | Velocity | the note-on velocity of each note starting in the curve's range (the selected ones, or all), sampled at the note's start — never a separate stream |
//! | Pitch Bend / CC | channel events at every interval step (or, with *Only Before the Note*, at the start ticks of the notes in scope, ordered before the notes of that tick) |
//! | Master Volume | Conductor Master Volume points at every interval step |
//! | BPM | Conductor tempo points (a Line becomes one exact linear ramp; other shapes are steps at every interval step) |
//!
//! Existing events: *Replace* removes the target's events (same track, channel
//! and type only) between the first and last new event; *Merge* keeps them and
//! only takes over the ticks the curve writes to. The number of events is
//! checked against a budget derived from the undo memory before anything is
//! built; too many is refused with a message, never truncated.

use plumspirit_engine::editor::EditError;
use plumspirit_engine::editor::controllers::{
    self, ControllerKind, EventsEdit, WritePolicy, graph_controller,
};
use plumspirit_engine::editor::curves::{
    self, Bend, CurveSpec, Interval, Shape, distinct_ticks, interval_ticks, notes_in_scope,
};
use plumspirit_engine::editor::expr::Expr;
use plumspirit_engine::history::Operation;
use plumspirit_engine::midi::group_digits;
use plumspirit_engine::model::conductor::{ConductorEdit, Interp};
use plumspirit_engine::model::{Bpm, GraphTarget, PlumDocument, RankSet, TempoValue};
use plumspirit_engine::view::grid::MeterMap;

use crate::piano_roll::graph::{CurvePreview, target_limits};

/// Events the preview computes at most (beyond that it draws the shape only).
pub const PREVIEW_BUDGET: usize = 20_000;

/// How far apart generated events are (as chosen in the Curve menu).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IntervalSetting {
    /// The snap grid (1/16 note when Snap is off).
    Snap,
    /// A note value: `num / den` quarter notes.
    Note(u64, u64),
    Ticks(u64),
    Beat,
    Measure,
}

impl IntervalSetting {
    pub const PRESETS: [(IntervalSetting, &'static str); 10] = [
        (IntervalSetting::Snap, "Snap grid"),
        (IntervalSetting::Note(1, 1), "1/4"),
        (IntervalSetting::Note(1, 2), "1/8"),
        (IntervalSetting::Note(1, 4), "1/16"),
        (IntervalSetting::Note(1, 8), "1/32"),
        (IntervalSetting::Note(1, 16), "1/64"),
        (IntervalSetting::Note(1, 3), "1/8 triplet"),
        (IntervalSetting::Note(1, 6), "1/16 triplet"),
        (IntervalSetting::Beat, "Every beat"),
        (IntervalSetting::Measure, "Every measure"),
    ];

    pub fn label(self) -> String {
        match self {
            IntervalSetting::Ticks(t) => format!("{} ticks", group_digits(t)),
            other => Self::PRESETS
                .iter()
                .find(|(p, _)| *p == other)
                .map_or_else(|| "Custom".to_owned(), |(_, l)| (*l).to_owned()),
        }
    }

    pub fn key(self) -> String {
        match self {
            IntervalSetting::Snap => "snap".into(),
            IntervalSetting::Note(n, d) => format!("note:{n}/{d}"),
            IntervalSetting::Ticks(t) => format!("ticks:{t}"),
            IntervalSetting::Beat => "beat".into(),
            IntervalSetting::Measure => "measure".into(),
        }
    }

    pub fn from_key(s: &str) -> Option<IntervalSetting> {
        match s {
            "snap" => Some(IntervalSetting::Snap),
            "beat" => Some(IntervalSetting::Beat),
            "measure" => Some(IntervalSetting::Measure),
            _ => {
                if let Some(t) = s.strip_prefix("ticks:") {
                    return t
                        .parse()
                        .ok()
                        .filter(|&t: &u64| t >= 1)
                        .map(IntervalSetting::Ticks);
                }
                let (n, d) = s.strip_prefix("note:")?.split_once('/')?;
                let (n, d): (u64, u64) = (n.parse().ok()?, d.parse().ok()?);
                (n >= 1 && d >= 1 && n <= 64 && d <= 256).then_some(IntervalSetting::Note(n, d))
            }
        }
    }

    /// The engine interval for a project (`snap` is the snap grid in ticks).
    pub fn resolve(self, ppqn: Option<u64>, snap: Option<u64>) -> Interval {
        let note = |n: u64, d: u64| match ppqn {
            Some(p) => Interval::Ticks(((p * n * 2 + d) / (2 * d)).max(1)),
            None => Interval::Ticks(120),
        };
        match self {
            IntervalSetting::Snap => match snap {
                Some(u) if u > 0 => Interval::Ticks(u),
                _ => note(1, 4),
            },
            IntervalSetting::Note(n, d) => note(n, d),
            IntervalSetting::Ticks(t) => Interval::Ticks(t.max(1)),
            IntervalSetting::Beat if ppqn.is_some() => Interval::Beat,
            IntervalSetting::Measure if ppqn.is_some() => Interval::Measure,
            _ => Interval::Ticks(120),
        }
    }
}

/// A named custom function (user settings).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurvePreset {
    pub name: String,
    pub expr: String,
}

/// The examples every installation starts with.
pub fn default_presets() -> Vec<CurvePreset> {
    [
        ("Linear", "t"),
        ("Square", "t*t"),
        ("Square root", "sqrt(t)"),
        ("Quarter sine", "sin(pi*t/2)"),
        ("Cosine ease", "(1-cos(pi*t))/2"),
        ("Smoothstep", "t*t*(3-2*t)"),
        ("Wobble", "t + 0.1*sin(8*pi*t)"),
    ]
    .into_iter()
    .map(|(n, e)| CurvePreset {
        name: n.into(),
        expr: e.into(),
    })
    .collect()
}

/// Which notes Velocity curves and "Only Before the Note" apply to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum NoteScope {
    /// The selected notes inside the curve's range (all notes there when none
    /// is selected).
    #[default]
    SelectedOrAll,
    /// Every note starting inside the curve's range.
    All,
}

/// Curve options (persisted in the user settings).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurveSettings {
    pub bend: Bend,
    pub interval: IntervalSetting,
    pub before_notes: bool,
    pub scope: NoteScope,
    /// Only Before the Note: only notes on the graph's channel.
    pub same_channel: bool,
    pub policy: WritePolicy,
    pub seed: u64,
    /// Use a new seed after each applied Random curve.
    pub advance_seed: bool,
    /// Name of the chosen custom function.
    pub custom: String,
    pub clamp: bool,
    pub normalize: bool,
    pub anchors: bool,
}

impl Default for CurveSettings {
    fn default() -> Self {
        CurveSettings {
            bend: Bend::EaseOut,
            interval: IntervalSetting::Note(1, 4),
            before_notes: false,
            scope: NoteScope::SelectedOrAll,
            same_channel: true,
            policy: WritePolicy::Replace,
            seed: 1,
            advance_seed: true,
            custom: "Cosine ease".into(),
            clamp: false,
            normalize: false,
            anchors: false,
        }
    }
}

/// What a curve writes.
#[derive(Clone, Debug, PartialEq)]
pub enum Generated {
    Controller {
        kind: ControllerKind,
        channel: u8,
        points: Vec<(u64, u16)>,
    },
    Velocities {
        ranks: RankSet,
        starts: Vec<u64>,
        values: Vec<u8>,
    },
    Master(Vec<(u64, u16)>),
    Tempo(Vec<(u64, TempoValue, Interp)>),
}

impl Generated {
    pub fn len(&self) -> usize {
        match self {
            Generated::Controller { points, .. } => points.len(),
            Generated::Velocities { values, .. } => values.len(),
            Generated::Master(p) => p.len(),
            Generated::Tempo(p) => p.len(),
        }
    }

    /// (tick, value) for the preview.
    pub fn preview_points(&self) -> Vec<(u64, f64)> {
        match self {
            Generated::Controller { points, .. } | Generated::Master(points) => {
                points.iter().map(|&(t, v)| (t, f64::from(v))).collect()
            }
            Generated::Velocities { starts, values, .. } => starts
                .iter()
                .zip(values)
                .map(|(&t, &v)| (t, f64::from(v)))
                .collect(),
            Generated::Tempo(p) => p
                .iter()
                .map(|(t, v, _)| {
                    (
                        *t,
                        match v {
                            TempoValue::Bpm(b) => b.to_f64(),
                            TempoValue::Micros(us) => 60_000_000.0 / f64::from((*us).max(1)),
                        },
                    )
                })
                .collect(),
        }
    }
}

/// Everything a curve computation reads.
pub struct CurveContext<'a> {
    pub doc: &'a PlumDocument,
    /// The notes and events the curves write to (an opened clip's content
    /// on the active track, else the track's own; V09-S11).
    pub track: Option<plumspirit_engine::editor::HostView<'a>>,
    pub track_index: plumspirit_engine::editor::NoteTarget,
    pub channel: u8,
    pub meter: &'a MeterMap,
    pub settings: &'a CurveSettings,
    /// The chosen custom function, parsed (or why it does not parse).
    pub custom: Option<&'a Result<Expr, String>>,
    pub snap: Option<u64>,
}

/// Why a curve cannot be generated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurveError {
    pub message: String,
    /// More events than the budget allows (the shape can still be previewed).
    pub too_many: bool,
}

impl From<String> for CurveError {
    fn from(message: String) -> Self {
        CurveError {
            message,
            too_many: false,
        }
    }
}

impl From<&str> for CurveError {
    fn from(message: &str) -> Self {
        CurveError::from(message.to_owned())
    }
}

impl std::fmt::Display for CurveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

fn err(e: EditError) -> CurveError {
    CurveError {
        too_many: matches!(e, EditError::TooManyEvents { .. }),
        message: e.user_message(),
    }
}

/// Tempo curves sample BPM in scaled integer units: `10^exp` units per BPM, so
/// the smaller anchor keeps six significant digits.
fn bpm_exponent(a: f64, b: f64) -> i32 {
    let m = a.min(b).max(1e-6);
    (5 - m.log10().floor() as i32).clamp(-9, 12)
}

fn bpm_of(units: i64, exp: i32) -> Result<Bpm, CurveError> {
    Bpm::new(units.max(1) as u64, -exp).map_err(|e| CurveError::from(e.to_string()))
}

impl<'a> CurveContext<'a> {
    fn ppqn(&self) -> Option<u64> {
        self.meter.ppqn()
    }

    fn spec<'e>(
        &self,
        shape: Shape,
        from: (u64, f64),
        to: (u64, f64),
        limits: (i64, i64),
        custom: Option<&'e Expr>,
    ) -> CurveSpec<'e> {
        CurveSpec {
            shape,
            bend: self.settings.bend,
            from,
            to,
            seed: self.settings.seed,
            random_range: (from.1.min(to.1), from.1.max(to.1)),
            custom,
            clamp: self.settings.clamp,
            normalize: self.settings.normalize,
            limits,
        }
    }

    fn custom_expr(&self, shape: Shape) -> Result<Option<&Expr>, CurveError> {
        if shape != Shape::Custom {
            return Ok(None);
        }
        match self.custom {
            Some(Ok(e)) => Ok(Some(e)),
            Some(Err(e)) => Err(format!("The custom function does not work: {e}").into()),
            None => {
                Err("Choose a custom function in the Curve menu (Custom Functions…) first.".into())
            }
        }
    }

    fn track(&self) -> Result<plumspirit_engine::editor::HostView<'a>, CurveError> {
        self.track
            .ok_or_else(|| "Select a regular track to draw on this graph.".into())
    }

    /// Notes in `[t0, t1]` the scope covers: (ranks, starts).
    fn notes(
        &self,
        t0: u64,
        t1: u64,
        channel: Option<u8>,
        budget: usize,
    ) -> Result<(RankSet, Vec<u64>), CurveError> {
        let t = self.track()?;
        let selected_only =
            self.settings.scope == NoteScope::SelectedOrAll && t.notes.selected_count() > 0;
        let (ranks, starts) =
            notes_in_scope(t.notes, t0, t1, selected_only, channel, budget).map_err(err)?;
        if starts.is_empty() {
            return Err(if selected_only {
                "No selected note starts inside the curve.".into()
            } else {
                "No note starts inside the curve.".into()
            });
        }
        Ok((ranks, starts))
    }

    /// The events a curve from `from` to `to` writes on `target`.
    pub fn generate(
        &self,
        target: GraphTarget,
        shape: Shape,
        from: (u64, f64),
        to: (u64, f64),
        budget: usize,
    ) -> Result<Generated, CurveError> {
        let (t0, t1) = (from.0.min(to.0), from.0.max(to.0));
        let custom = self.custom_expr(shape)?;
        let interval = self.settings.interval.resolve(self.ppqn(), self.snap);
        match target {
            GraphTarget::Velocity => {
                let (ranks, starts) = self.notes(t0, t1, None, budget)?;
                let spec = self.spec(shape, from, to, (1, 127), custom);
                let values = spec.sample(&starts).map_err(err)?;
                Ok(Generated::Velocities {
                    ranks,
                    starts,
                    values: values.into_iter().map(|(_, v)| v as u8).collect(),
                })
            }
            GraphTarget::MasterVolume => {
                let ticks = interval_ticks(t0, t1, interval, self.meter, budget).map_err(err)?;
                let spec = self.spec(shape, from, to, (0, 16_383), custom);
                let pts = spec.sample(&ticks).map_err(err)?;
                Ok(Generated::Master(
                    pts.into_iter().map(|(t, v)| (t, v as u16)).collect(),
                ))
            }
            GraphTarget::Bpm => {
                if from.1 <= 0.0 || to.1 <= 0.0 {
                    return Err("Tempo curves need positive BPM values.".into());
                }
                let exp = bpm_exponent(from.1, to.1);
                let k = 10f64.powi(exp);
                if shape == Shape::Line {
                    // One exact linear ramp between the anchors.
                    let a = bpm_of((from.1 * k).round() as i64, exp)?;
                    let b = bpm_of((to.1 * k).round() as i64, exp)?;
                    let (first, second) = if from.0 <= to.0 { (a, b) } else { (b, a) };
                    return Ok(Generated::Tempo(vec![
                        (t0, TempoValue::Bpm(first), Interp::Linear),
                        (t1, TempoValue::Bpm(second), Interp::Step),
                    ]));
                }
                let ticks = interval_ticks(t0, t1, interval, self.meter, budget).map_err(err)?;
                let spec = self.spec(
                    shape,
                    (from.0, from.1 * k),
                    (to.0, to.1 * k),
                    (1, i64::MAX / 2),
                    custom,
                );
                let pts = spec.sample(&ticks).map_err(err)?;
                let mut out = Vec::with_capacity(pts.len());
                for (t, v) in pts {
                    out.push((t, TempoValue::Bpm(bpm_of(v, exp)?), Interp::Step));
                }
                Ok(Generated::Tempo(out))
            }
            other => {
                let kind = graph_controller(other).ok_or("This graph has no events to draw.")?;
                if !kind.curve_drawable() {
                    return Err(format!(
                        "{} is a channel-mode command; add it with the Edit tool, one event at a time.",
                        kind.full_label()
                    )
                    .into());
                }
                let (lo, hi) = target_limits(other).unwrap_or((0, 127));
                let ticks = if self.settings.before_notes {
                    let ch = self.settings.same_channel.then_some(self.channel);
                    let (_, starts) = self.notes(t0, t1, ch, budget)?;
                    distinct_ticks(&starts)
                } else {
                    self.track()?;
                    interval_ticks(t0, t1, interval, self.meter, budget).map_err(err)?
                };
                let spec = self.spec(shape, from, to, (lo, hi), custom);
                let pts = spec.sample(&ticks).map_err(err)?;
                Ok(Generated::Controller {
                    kind,
                    channel: self.channel,
                    points: pts.into_iter().map(|(t, v)| (t, v as u16)).collect(),
                })
            }
        }
    }

    /// The events a free-hand stroke writes on `target`.
    pub fn generate_freehand(
        &self,
        target: GraphTarget,
        samples: &[(u64, f64)],
        budget: usize,
    ) -> Result<Generated, CurveError> {
        let step = match self.settings.interval.resolve(self.ppqn(), self.snap) {
            Interval::Ticks(t) => t,
            Interval::Beat => self.ppqn().unwrap_or(120),
            Interval::Measure => self.ppqn().map_or(480, |p| 4 * p),
        };
        let (lo, hi) = (
            samples.iter().map(|s| s.0).min().unwrap_or(0),
            samples.iter().map(|s| s.0).max().unwrap_or(0),
        );
        match target {
            GraphTarget::Velocity => {
                let (ranks, starts) = self.notes(lo, hi, None, budget)?;
                let values = starts
                    .iter()
                    .map(|&t| stroke_value_at(samples, t).round().clamp(1.0, 127.0) as u8)
                    .collect();
                Ok(Generated::Velocities {
                    ranks,
                    starts,
                    values,
                })
            }
            GraphTarget::MasterVolume => {
                let pts = curves::freehand(samples, step, (0, 16_383), budget).map_err(err)?;
                Ok(Generated::Master(
                    pts.into_iter().map(|(t, v)| (t, v as u16)).collect(),
                ))
            }
            GraphTarget::Bpm => {
                let min = samples.iter().map(|s| s.1).fold(f64::INFINITY, f64::min);
                let exp = bpm_exponent(min, min);
                let k = 10f64.powi(exp);
                let scaled: Vec<(u64, f64)> = samples.iter().map(|&(t, v)| (t, v * k)).collect();
                let pts =
                    curves::freehand(&scaled, step, (1, i64::MAX / 2), budget).map_err(err)?;
                let mut out = Vec::with_capacity(pts.len());
                for (t, v) in pts {
                    out.push((t, TempoValue::Bpm(bpm_of(v, exp)?), Interp::Step));
                }
                Ok(Generated::Tempo(out))
            }
            other => {
                let kind = graph_controller(other).ok_or("This graph has no events to draw.")?;
                if !kind.curve_drawable() {
                    return Err(format!(
                        "{} is a channel-mode command; add it with the Edit tool.",
                        kind.full_label()
                    )
                    .into());
                }
                self.track()?;
                let (vlo, vhi) = target_limits(other).unwrap_or((0, 127));
                let pts = if self.settings.before_notes {
                    let ch = self.settings.same_channel.then_some(self.channel);
                    let (_, starts) = self.notes(lo, hi, ch, budget)?;
                    distinct_ticks(&starts)
                        .into_iter()
                        .map(|t| {
                            (
                                t,
                                (stroke_value_at(samples, t).round() as i64).clamp(vlo, vhi),
                            )
                        })
                        .collect()
                } else {
                    curves::freehand(samples, step, (vlo, vhi), budget).map_err(err)?
                };
                Ok(Generated::Controller {
                    kind,
                    channel: self.channel,
                    points: pts.into_iter().map(|(t, v)| (t, v as u16)).collect(),
                })
            }
        }
    }

    /// The preview of a curve (the real events when few enough, else the shape).
    pub fn preview(
        &self,
        target: GraphTarget,
        shape: Shape,
        from: (u64, f64),
        to: (u64, f64),
    ) -> CurvePreview {
        match self.generate(target, shape, from, to, PREVIEW_BUDGET) {
            Ok(g) => CurvePreview {
                note: Some(format!("{} event(s)", group_digits(g.len() as u64))),
                points: g.preview_points(),
                ..CurvePreview::default()
            },
            Err(e) if e.too_many => {
                // Too many to list: draw the shape (the real generation checks its
                // own, larger budget).
                let custom = self.custom_expr(shape).ok().flatten();
                let limits = target_limits(target).unwrap_or((1, i64::MAX / 2));
                let spec = self.spec(shape, from, to, limits, custom);
                CurvePreview {
                    points: Vec::new(),
                    shape: spec.preview(160).unwrap_or_default(),
                    note: Some(
                        "Many events: the shape is shown; applying checks the limit.".into(),
                    ),
                    error: false,
                }
            }
            Err(e) => CurvePreview {
                note: Some(e.message),
                error: true,
                ..CurvePreview::default()
            },
        }
    }

    /// The operation that applies `g`, with its undo label.
    pub fn operation(
        &self,
        g: Generated,
        label: &'static str,
        budget: usize,
    ) -> Result<Operation, CurveError> {
        match g {
            Generated::Controller {
                kind,
                channel,
                points,
            } => {
                let t = self.track()?;
                let edit: EventsEdit = controllers::prepare_points(
                    t,
                    kind,
                    channel,
                    &points,
                    self.settings.policy,
                    budget,
                )
                .map_err(err)?;
                Ok(Operation::Events { edit, label })
            }
            Generated::Velocities { ranks, values, .. } => Ok(Operation::SetVelocities {
                track: self.track_index,
                ranks,
                values,
            }),
            Generated::Master(points) => {
                let edit: ConductorEdit = self
                    .doc
                    .conductor()
                    .prepare_master_points(&points, self.settings.policy == WritePolicy::Replace)
                    .map_err(|e| CurveError::from(e.to_string()))?;
                Ok(Operation::Conductor { edit, label })
            }
            Generated::Tempo(points) => {
                let edit = self
                    .doc
                    .conductor()
                    .prepare_tempo_points(&points, self.settings.policy == WritePolicy::Replace)
                    .map_err(|e| CurveError::from(e.to_string()))?;
                Ok(Operation::Conductor { edit, label })
            }
        }
    }
}

/// The stroke's value at `tick`: linear between the samples around it (sorted
/// by tick; the last drawn sample wins at equal ticks).
pub fn stroke_value_at(samples: &[(u64, f64)], tick: u64) -> f64 {
    let mut s: Vec<(u64, f64)> = samples.to_vec();
    s.sort_by_key(|p| p.0);
    let i = s.partition_point(|p| p.0 <= tick);
    match (i.checked_sub(1).map(|j| s[j]), s.get(i)) {
        (Some(a), Some(b)) if b.0 > a.0 => {
            let f = (tick - a.0) as f64 / (b.0 - a.0) as f64;
            a.1 + (b.1 - a.1) * f
        }
        (Some(a), _) => a.1,
        (None, Some(b)) => b.1,
        (None, None) => 0.0,
    }
}

/// Undo label of a curve.
pub fn curve_label(shape: Option<Shape>) -> &'static str {
    match shape {
        None => "Free-Hand",
        Some(Shape::Line) => "Line Curve",
        Some(Shape::Gentle) => "Gentle Curve",
        Some(Shape::Fast) => "Fast Curve",
        Some(Shape::SCurve) => "S Curve",
        Some(Shape::Random) => "Random Curve",
        Some(Shape::Custom) => "Custom Curve",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plumspirit_engine::model::{NoteAttrs, NoteOffKind, NoteRecord};

    fn doc() -> PlumDocument {
        let mut d = PlumDocument::new_project("t".into(), Bpm::DEFAULT, 480);
        let recs: Vec<NoteRecord> = (0..8u64)
            .map(|i| NoteRecord {
                start_tick: i * 480,
                duration_ticks: 240,
                attrs: NoteAttrs::new(60, 100, (i % 2) as u8, 0, NoteOffKind::NoteOff).unwrap(),
                sub: Default::default(),
                group: 0,
            })
            .collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        d
    }

    #[test]
    fn intervals_resolve_against_the_project() {
        assert_eq!(
            IntervalSetting::Note(1, 4).resolve(Some(480), None),
            Interval::Ticks(120)
        );
        assert_eq!(
            IntervalSetting::Note(1, 6).resolve(Some(1000), None),
            Interval::Ticks(167)
        );
        assert_eq!(
            IntervalSetting::Snap.resolve(Some(480), Some(60)),
            Interval::Ticks(60)
        );
        assert_eq!(
            IntervalSetting::Snap.resolve(Some(480), None),
            Interval::Ticks(120)
        );
        for (s, _) in IntervalSetting::PRESETS {
            assert_eq!(IntervalSetting::from_key(&s.key()), Some(s));
        }
        assert_eq!(IntervalSetting::from_key("ticks:0"), None);
    }

    #[test]
    fn curves_write_the_right_kind_of_events() {
        let d = doc();
        let meter = MeterMap::new(d.division(), &d.meter_events(0));
        let settings = CurveSettings::default();
        let ctx = CurveContext {
            doc: &d,
            track: d.track(0).map(Into::into),
            track_index: 0.into(),
            channel: 0,
            meter: &meter,
            settings: &settings,
            custom: None,
            snap: None,
        };
        // CC 7 line over one beat at 1/16 = 5 events incl. both ends, exact ends.
        let g = ctx
            .generate(
                GraphTarget::Cc(7),
                Shape::Line,
                (0, 0.0),
                (480, 127.0),
                1000,
            )
            .unwrap();
        let Generated::Controller { points, .. } = &g else {
            panic!("{g:?}")
        };
        assert_eq!(points.first(), Some(&(0, 0)));
        assert_eq!(points.last(), Some(&(480, 127)));
        assert_eq!(points.len(), 5);
        // Velocity: one value per note start (no selection → all notes).
        let g = ctx
            .generate(
                GraphTarget::Velocity,
                Shape::Line,
                (0, 1.0),
                (3360, 127.0),
                1000,
            )
            .unwrap();
        let Generated::Velocities { values, .. } = &g else {
            panic!()
        };
        assert_eq!(values.len(), 8);
        assert_eq!((values[0], values[7]), (1, 127));
        // BPM line: one exact ramp.
        let g = ctx
            .generate(
                GraphTarget::Bpm,
                Shape::Line,
                (960, 120.0),
                (1920, 60.0),
                1000,
            )
            .unwrap();
        let Generated::Tempo(p) = &g else { panic!() };
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].2, Interp::Linear);
        // Only Before the Note: CC at the note starts on the graph's channel.
        let obn = CurveSettings {
            before_notes: true,
            ..CurveSettings::default()
        };
        let ctx2 = CurveContext {
            settings: &obn,
            ..ctx
        };
        let g = ctx2
            .generate(
                GraphTarget::Cc(1),
                Shape::Line,
                (0, 0.0),
                (3360, 127.0),
                1000,
            )
            .unwrap();
        let Generated::Controller { points, .. } = &g else {
            panic!()
        };
        assert_eq!(
            points.iter().map(|p| p.0).collect::<Vec<_>>(),
            vec![0, 960, 1920, 2880],
            "channel-1 notes only"
        );
        // Too many events is refused, not truncated.
        let e = ctx
            .generate(
                GraphTarget::Cc(7),
                Shape::Line,
                (0, 0.0),
                (1_000_000, 1.0),
                100,
            )
            .unwrap_err();
        assert!(e.too_many && e.message.contains("100"), "{e}");
        // The operation applies as one undo step and Undo restores everything.
        let g = ctx
            .generate(
                GraphTarget::Cc(7),
                Shape::SCurve,
                (0, 0.0),
                (480, 127.0),
                1000,
            )
            .unwrap();
        let op = ctx.operation(g, "S Curve", 1000).unwrap();
        let mut d2 = d.clone();
        let fp = d2.fingerprint();
        let mut h = plumspirit_engine::history::History::default();
        h.apply(&mut d2, op).unwrap();
        assert_ne!(d2.fingerprint(), fp);
        h.undo(&mut d2).unwrap();
        assert_eq!(d2.fingerprint(), fp);
        h.redo(&mut d2).unwrap();
        assert_ne!(d2.fingerprint(), fp);
    }

    #[test]
    fn free_hand_strokes_fill_gaps_and_follow_the_interval() {
        let d = doc();
        let meter = MeterMap::new(d.division(), &d.meter_events(0));
        let settings = CurveSettings::default();
        let ctx = CurveContext {
            doc: &d,
            track: d.track(0).map(Into::into),
            track_index: 0.into(),
            channel: 0,
            meter: &meter,
            settings: &settings,
            custom: None,
            snap: None,
        };
        let g = ctx
            .generate_freehand(GraphTarget::Cc(11), &[(0, 0.0), (960, 100.0)], 1000)
            .unwrap();
        let Generated::Controller { points, .. } = g else {
            panic!()
        };
        assert_eq!(points.len(), 9); // every 120 ticks from 0 to 960
        assert_eq!(points[4], (480, 50));
        assert!((stroke_value_at(&[(0, 0.0), (100, 10.0)], 50) - 5.0).abs() < 1e-9);
    }

    #[test]
    fn random_curves_are_reproducible_from_their_seed() {
        let d = doc();
        let meter = MeterMap::new(d.division(), &d.meter_events(0));
        let settings = CurveSettings::default();
        let ctx = CurveContext {
            doc: &d,
            track: d.track(0).map(Into::into),
            track_index: 0.into(),
            channel: 0,
            meter: &meter,
            settings: &settings,
            custom: None,
            snap: None,
        };
        let a = ctx
            .generate(
                GraphTarget::Cc(1),
                Shape::Random,
                (0, 10.0),
                (1920, 90.0),
                1000,
            )
            .unwrap();
        let b = ctx
            .generate(
                GraphTarget::Cc(1),
                Shape::Random,
                (0, 10.0),
                (1920, 90.0),
                1000,
            )
            .unwrap();
        assert_eq!(a, b);
        let Generated::Controller { points, .. } = a else {
            panic!()
        };
        assert!(points.iter().all(|p| (10..=90).contains(&p.1)));
    }
}
