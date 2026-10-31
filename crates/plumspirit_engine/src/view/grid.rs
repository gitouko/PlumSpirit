//! Musical grid (bars / beats / subdivisions) and non-musical tick grids.
//!
//! All grid positions are computed with integer arithmetic from the start of each
//! meter segment (`start + floor(k · bar_length)` with the bar length held as an exact
//! fraction), so no floating-point error accumulates across thousands of bars.
//! Only lines that are visible and at least `min_spacing_px` apart are produced,
//! so the line count per frame is bounded by the viewport width.
//!
//! The grid is purely visual: it never writes time signatures into the document.
//! Without a time signature the grid assumes 4/4.
//!
//! # Time signature changes (v0.3 policy)
//!
//! * A change always starts a new bar at its own tick. When it falls inside a
//!   bar, that bar is *partial*: it ends at the change.
//! * Bars are numbered continuously from the start of the song; a partial bar
//!   keeps its number and the change starts the next number. Numbering never
//!   restarts at 1.
//! * Bar `k` of a segment starting at `s` begins at `s + floor(k · N·4·PPQN / 2^d)`
//!   (exact integer arithmetic), so fractional bar lengths never drift and the
//!   boundaries are strictly increasing. Grid lines, bar numbers, Previous/Next
//!   Measure and the marker/signature labels all use these boundaries.

use crate::model::timing::{Division, TimingKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// Bar line (musical) / whole second (SMPTE) / major step (ticks).
    Major,
    /// Beat (musical) / frame step (SMPTE) / minor step (ticks).
    Minor,
    /// Subdivision below the beat.
    Sub,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridLine {
    pub tick: u64,
    pub kind: LineKind,
    /// Value to print on the ruler (bar number, seconds, or tick), if labelled.
    pub label: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Segment {
    start: u64,
    numerator: u64,
    denom_pow: u32,
    /// Zero-based index of the bar that begins at `start`.
    first_bar: u64,
    /// v0.9 (V09-S13): zero-based index of the beat that begins at `start`
    /// (a partial beat before a change counts, as a partial bar does).
    first_beat: u64,
}

impl Segment {
    /// Bar length = bar_num / 2^denom_pow ticks.
    fn bar_num(&self, ppqn: u64) -> u128 {
        u128::from(self.numerator) * u128::from(ppqn) * 4
    }
    fn den(&self) -> u128 {
        1u128 << self.denom_pow
    }
    /// Tick of bar `k` of this segment (k counted from the segment start).
    fn bar_tick(&self, ppqn: u64, k: u64) -> u128 {
        u128::from(self.start) + u128::from(k) * self.bar_num(ppqn) / self.den()
    }
    /// Index (from the segment start) of the bar containing relative tick `rel`:
    /// the largest `k` with `floor(k · bar_len) <= rel`. Exact for fractional bar
    /// lengths (e.g. 7/64 at 3 PPQN).
    fn bar_index_at(&self, ppqn: u64, rel: u128) -> u128 {
        ((rel + 1) * self.den() - 1) / self.bar_num(ppqn)
    }
    /// Tick of beat `j` of this segment.
    fn beat_tick(&self, ppqn: u64, j: u64) -> u128 {
        u128::from(self.start) + u128::from(j) * u128::from(ppqn) * 4 / self.den()
    }
    /// Index (from the segment start) of the beat containing relative tick
    /// `rel`: the largest `j` with `floor(j · beat_len) <= rel`.
    fn beat_index_at(&self, ppqn: u64, rel: u128) -> u128 {
        ((rel + 1) * self.den() - 1) / (u128::from(ppqn) * 4)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Timing {
    Musical {
        ppqn: u64,
        segments: Vec<Segment>,
    },
    Smpte {
        fps_nominal: u64,
        ticks_per_frame: u64,
    },
    Ticks,
}

/// Grid description derived from the document's division and time signatures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeterMap {
    timing: Timing,
}

impl MeterMap {
    /// `meter_events` are `(tick, numerator, denominator_power)` sorted by tick
    /// (see `PlumDocument::meter_events`).
    pub fn new(division: Division, meter_events: &[(u64, u8, u8)]) -> Self {
        let timing = match division.kind() {
            TimingKind::Ppqn(0) => Timing::Ticks,
            TimingKind::Ppqn(p) => {
                let ppqn = u64::from(p);
                let mut segments: Vec<Segment> = vec![Segment {
                    start: 0,
                    numerator: 4,
                    denom_pow: 2,
                    first_bar: 0,
                    first_beat: 0,
                }];
                for &(tick, num, dpow) in meter_events {
                    if num == 0 || dpow > crate::model::conductor::MAX_DENOM_POW {
                        continue;
                    }
                    let last = *segments.last().expect("non-empty");
                    if tick == last.start {
                        // Several signatures at one tick: the last one wins.
                        let l = segments.last_mut().expect("non-empty");
                        l.numerator = u64::from(num);
                        l.denom_pow = u32::from(dpow);
                        continue;
                    }
                    // Bars begun in the previous segment (a partial bar counts).
                    let span = u128::from(tick - last.start) * last.den();
                    let bars = span.div_ceil(last.bar_num(ppqn));
                    // Beats likewise (V09-S13).
                    let beats = span.div_ceil(u128::from(ppqn) * 4);
                    segments.push(Segment {
                        start: tick,
                        numerator: u64::from(num),
                        denom_pow: u32::from(dpow),
                        first_bar: last
                            .first_bar
                            .saturating_add(u64::try_from(bars).unwrap_or(u64::MAX)),
                        first_beat: last
                            .first_beat
                            .saturating_add(u64::try_from(beats).unwrap_or(u64::MAX)),
                    });
                }
                Timing::Musical { ppqn, segments }
            }
            TimingKind::Smpte {
                frames_per_second,
                ticks_per_frame,
            } => {
                // 29 means 29.97 drop-frame; the nominal frame count per second is 30.
                let fps = match frames_per_second {
                    29 => 30,
                    0 => 25,
                    f => u64::from(f),
                };
                if ticks_per_frame == 0 {
                    Timing::Ticks
                } else {
                    Timing::Smpte {
                        fps_nominal: fps,
                        ticks_per_frame: u64::from(ticks_per_frame),
                    }
                }
            }
        };
        MeterMap { timing }
    }

    /// True when bars and beats can be shown (PPQN timing with a non-zero PPQN).
    pub fn is_musical(&self) -> bool {
        matches!(self.timing, Timing::Musical { .. })
    }

    /// Short note for the UI when the grid is not musical.
    pub fn non_musical_note(&self) -> Option<&'static str> {
        match self.timing {
            Timing::Musical { .. } => None,
            Timing::Smpte { .. } => Some("SMPTE timing: grid shows seconds and frames"),
            Timing::Ticks => Some("No musical timing: grid shows ticks"),
        }
    }

    fn segment_at(segments: &[Segment], tick: u64) -> (usize, &Segment) {
        let i = segments
            .partition_point(|s| s.start <= tick)
            .saturating_sub(1);
        (i, &segments[i])
    }

    /// 1-based (bar, beat) and the tick offset within the beat, for musical timing.
    pub fn bar_beat(&self, tick: u64) -> Option<(u64, u64, u64)> {
        let Timing::Musical { ppqn, segments } = &self.timing else {
            return None;
        };
        let (_, s) = Self::segment_at(segments, tick);
        let rel = u128::from(tick - s.start);
        let bar_in_seg = s.bar_index_at(*ppqn, rel);
        let bar_start = s.bar_tick(*ppqn, u64::try_from(bar_in_seg).ok()?);
        let beat_len_num = u128::from(*ppqn) * 4;
        let within = (u128::from(tick) - bar_start) * s.den();
        let beat = within / beat_len_num;
        let beat_start = bar_start + beat * beat_len_num / s.den();
        Some((
            s.first_bar.checked_add(u64::try_from(bar_in_seg).ok()?)? + 1,
            u64::try_from(beat).ok()? + 1,
            u64::try_from(u128::from(tick) - beat_start).ok()?,
        ))
    }

    /// Spacing of navigation boundaries when there are no bars: one second for
    /// SMPTE timing, 1,000 ticks without any timing information.
    fn plain_step(&self) -> u64 {
        match self.timing {
            Timing::Smpte {
                fps_nominal,
                ticks_per_frame,
            } => (fps_nominal * ticks_per_frame).max(1),
            _ => 1000,
        }
    }

    /// The last bar boundary at or before `tick` (start of the bar containing it).
    /// Uses exact integer arithmetic, the PPQN and every time signature change.
    pub fn bar_start_at_or_before(&self, tick: u64) -> u64 {
        match &self.timing {
            Timing::Musical { ppqn, segments } => {
                let (_, s) = Self::segment_at(segments, tick);
                let k = s.bar_index_at(*ppqn, u128::from(tick - s.start));
                // bar_tick(k) <= tick by construction.
                s.bar_tick(*ppqn, u64::try_from(k).unwrap_or(u64::MAX)) as u64
            }
            _ => {
                let step = self.plain_step();
                tick - tick % step
            }
        }
    }

    /// Previous Measure: the start of the current bar when `tick` is inside it,
    /// otherwise (exactly on a bar line) the start of the previous bar. Stays at 0.
    pub fn previous_bar(&self, tick: u64) -> u64 {
        if tick == 0 {
            0
        } else {
            self.bar_start_at_or_before(tick - 1)
        }
    }

    /// Next Measure: the first bar boundary strictly after `tick`, or `None` when
    /// it would lie beyond the largest representable tick.
    pub fn next_bar(&self, tick: u64) -> Option<u64> {
        match &self.timing {
            Timing::Musical { ppqn, segments } => {
                let (i, s) = Self::segment_at(segments, tick);
                let rel = u128::from(tick - s.start);
                let k = ((rel + 1) * s.den()).div_ceil(s.bar_num(*ppqn));
                let candidate = u128::from(s.start) + k * s.bar_num(*ppqn) / s.den();
                let bound = segments
                    .get(i + 1)
                    .map_or(candidate, |n| candidate.min(u128::from(n.start)));
                u64::try_from(bound).ok()
            }
            _ => {
                let step = self.plain_step();
                (tick - tick % step).checked_add(step)
            }
        }
    }

    /// Human-readable position, e.g. "Bar 12 · Beat 3", "0:12.05" or "Tick 3,840".
    pub fn describe(&self, tick: u64) -> String {
        match &self.timing {
            Timing::Musical { .. } => match self.bar_beat(tick) {
                Some((bar, beat, _)) => format!("Bar {bar} · Beat {beat}"),
                None => format!("Tick {}", crate::midi::group_digits(tick)),
            },
            Timing::Smpte {
                fps_nominal,
                ticks_per_frame,
            } => {
                let frames = tick / ticks_per_frame;
                let secs = frames / fps_nominal;
                format!("{}:{:02}.{:02}", secs / 60, secs % 60, frames % fps_nominal)
            }
            Timing::Ticks => format!("Tick {}", crate::midi::group_digits(tick)),
        }
    }

    /// PPQN of musical timing.
    pub fn ppqn(&self) -> Option<u64> {
        match &self.timing {
            Timing::Musical { ppqn, .. } => Some(*ppqn),
            _ => None,
        }
    }

    /// Tick at which measure `measure` (1-based) starts, following every time
    /// signature change (a partial bar before a change counts as a measure).
    /// `None` for measure 0, non-musical timing, or a position beyond the largest
    /// tick.
    pub fn measure_start(&self, measure: u64) -> Option<u64> {
        let Timing::Musical { ppqn, segments } = &self.timing else {
            return None;
        };
        let b = measure.checked_sub(1)?;
        let i = segments
            .partition_point(|s| s.first_bar <= b)
            .checked_sub(1)?;
        let s = &segments[i];
        u64::try_from(s.bar_tick(*ppqn, b - s.first_bar)).ok()
    }

    /// The measure (1-based) containing `tick` and the tick's offset from the
    /// measure start.
    pub fn measure_at(&self, tick: u64) -> Option<(u64, u64)> {
        let (bar, _, _) = self.bar_beat(tick)?;
        Some((bar, tick - self.bar_start_at_or_before(tick)))
    }

    /// Length in ticks of the measure containing `tick` (up to a signature
    /// change that cuts it short).
    pub fn measure_length_at(&self, tick: u64) -> Option<u64> {
        if !self.is_musical() {
            return None;
        }
        let start = self.bar_start_at_or_before(tick);
        Some(self.next_bar(start)?.saturating_sub(start))
    }

    /// v0.9 (V09-S13, Paste Repeat): the position `n` bars after `tick` — the
    /// same offset into the bar, `n` measures later, following every time
    /// signature change (a partial bar counts as a measure, as in
    /// [`MeterMap::measure_start`]). The offset is kept in ticks, so it may
    /// reach past a shorter bar. `None` for non-musical timing or beyond the
    /// largest tick. O(log segments).
    pub fn advance_bars(&self, tick: u64, n: u64) -> Option<u64> {
        let (m, off) = self.measure_at(tick)?;
        self.measure_start(m.checked_add(n)?)?.checked_add(off)
    }

    /// The zero-based index of the beat containing `tick` (counted from the
    /// song start across signature changes; a beat is a 1/denominator note,
    /// a partial beat before a change counts) and the tick's offset into it.
    pub fn beat_at(&self, tick: u64) -> Option<(u64, u64)> {
        let Timing::Musical { ppqn, segments } = &self.timing else {
            return None;
        };
        let (_, s) = Self::segment_at(segments, tick);
        let j = s.beat_index_at(*ppqn, u128::from(tick - s.start));
        let start = s.beat_tick(*ppqn, u64::try_from(j).ok()?);
        let index = s.first_beat.checked_add(u64::try_from(j).ok()?)?;
        Some((index, u64::try_from(u128::from(tick) - start).ok()?))
    }

    /// The tick at which beat `index` (zero-based, as [`MeterMap::beat_at`])
    /// starts. `None` for non-musical timing or beyond the largest tick.
    pub fn beat_start(&self, index: u64) -> Option<u64> {
        let Timing::Musical { ppqn, segments } = &self.timing else {
            return None;
        };
        let i = segments
            .partition_point(|s| s.first_beat <= index)
            .checked_sub(1)?;
        let s = &segments[i];
        u64::try_from(s.beat_tick(*ppqn, index - s.first_beat)).ok()
    }

    /// The position `n` beats after `tick` (the same offset into the beat),
    /// following every time signature change. O(log segments).
    pub fn advance_beats(&self, tick: u64, n: u64) -> Option<u64> {
        let (b, off) = self.beat_at(tick)?;
        self.beat_start(b.checked_add(n)?)?.checked_add(off)
    }

    /// Ticks per SMPTE frame (`None` for musical or plain-tick timing).
    pub fn ticks_per_frame(&self) -> Option<u64> {
        match self.timing {
            Timing::Smpte {
                ticks_per_frame, ..
            } => Some(ticks_per_frame),
            _ => None,
        }
    }

    /// The time signature `(numerator, denominator power)` in effect at `tick`.
    pub fn signature_at(&self, tick: u64) -> Option<(u64, u32)> {
        let Timing::Musical { segments, .. } = &self.timing else {
            return None;
        };
        let (_, s) = Self::segment_at(segments, tick);
        Some((s.numerator, s.denom_pow))
    }

    /// `Measure:Beat:Tick` (1-based measure and beat, 0-based tick within the
    /// beat), or the plain tick for non-musical timing.
    pub fn format_position(&self, tick: u64) -> String {
        match self.bar_beat(tick) {
            Some((bar, beat, t)) => format!("{bar}:{beat}:{t}"),
            None => crate::midi::group_digits(tick),
        }
    }

    /// `Measure:Tick` (1-based measure, 0-based tick offset within it).
    pub fn format_measure_tick(&self, tick: u64) -> String {
        match self.measure_at(tick) {
            Some((bar, t)) => format!("{bar}:{t}"),
            None => crate::midi::group_digits(tick),
        }
    }

    /// Parses a musical position: `M` (start of measure M), `M:T` (T ticks into
    /// measure M) or `M:B:T` (T ticks into beat B of measure M). Measures and
    /// beats count from 1, ticks from 0; offsets must lie inside their measure
    /// or beat (nothing is carried into the next one). Uses the time signatures
    /// at that measure, never an assumed 4/4.
    pub fn parse_position(&self, input: &str) -> Result<u64, String> {
        let text = input.trim();
        if !self.is_musical() {
            return Err(
                "This project has no bars (SMPTE or no timing); enter a tick instead.".into(),
            );
        }
        let parts: Vec<&str> = text.split(':').map(str::trim).collect();
        let num = |s: &str, what: &str| -> Result<u64, String> {
            if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!(
                    "\"{}\" is not a valid position. Use Measure, Measure:Tick or Measure:Beat:Tick (for example 3, 3:240 or 3:2:0).",
                    text.chars().take(40).collect::<String>()
                ));
            }
            s.parse::<u64>()
                .map_err(|_| format!("The {what} number is too large."))
        };
        if parts.len() > 3 || text.is_empty() {
            return Err(format!(
                "\"{}\" is not a valid position. Use Measure, Measure:Tick or Measure:Beat:Tick (for example 3, 3:240 or 3:2:0).",
                text.chars().take(40).collect::<String>()
            ));
        }
        let measure = num(parts[0], "measure")?;
        if measure == 0 {
            return Err("Measures are numbered from 1.".into());
        }
        let start = self
            .measure_start(measure)
            .ok_or_else(|| format!("Measure {measure} is beyond the largest possible tick."))?;
        let len = self.measure_length_at(start);
        match parts.len() {
            1 => Ok(start),
            2 => {
                let t = num(parts[1], "tick")?;
                if let Some(len) = len
                    && t >= len
                {
                    return Err(format!(
                        "Measure {measure} is {} ticks long, so its tick offset must be 0–{}. (Use {}:0 for the next measure.)",
                        crate::midi::group_digits(len),
                        crate::midi::group_digits(len - 1),
                        measure + 1
                    ));
                }
                start
                    .checked_add(t)
                    .ok_or_else(|| "That position is beyond the largest possible tick.".into())
            }
            _ => {
                let beat = num(parts[1], "beat")?;
                let t = num(parts[2], "tick")?;
                let Timing::Musical { ppqn, segments } = &self.timing else {
                    unreachable!("checked above")
                };
                let (_, s) = Self::segment_at(segments, start);
                if beat == 0 || beat > s.numerator {
                    return Err(format!(
                        "Measure {measure} has {} beat(s); beats are numbered from 1.",
                        s.numerator
                    ));
                }
                let beat_tick = |j: u64| -> u128 {
                    u128::from(start) + u128::from(j) * u128::from(*ppqn) * 4 / s.den()
                };
                let b0 = beat_tick(beat - 1);
                let mut b1 = beat_tick(beat);
                if let Some(len) = len {
                    b1 = b1.min(u128::from(start) + u128::from(len));
                }
                let pos = b0 + u128::from(t);
                if pos >= b1 {
                    return Err(format!(
                        "Beat {beat} of measure {measure} is {} ticks long, so its tick offset must be below that.",
                        crate::midi::group_digits(u64::try_from(b1 - b0).unwrap_or(u64::MAX))
                    ));
                }
                u64::try_from(pos)
                    .map_err(|_| "That position is beyond the largest possible tick.".into())
            }
        }
    }

    /// A span in whole measures, beats and ticks when it starts on a bar line:
    /// `(measures, beats, ticks)`, with the beats and ticks counted in the last
    /// (partial) measure's signature. `None` when `t0` is not on a bar line or
    /// the timing is not musical.
    pub fn span_in_measures(&self, t0: u64, t1: u64) -> Option<(u64, u64, u64)> {
        if t1 < t0 || !self.is_musical() || self.bar_start_at_or_before(t0) != t0 {
            return None;
        }
        let (m0, _) = self.measure_at(t0)?;
        let (m1, _) = self.measure_at(t1)?;
        let (_, beat, tick) = self.bar_beat(t1)?;
        Some((m1 - m0, beat - 1, tick))
    }

    /// Formats a ruler label produced by [`MeterMap::lines`].
    pub fn format_label(&self, value: u64) -> String {
        match self.timing {
            Timing::Musical { .. } => value.to_string(),
            Timing::Smpte { .. } => format!("{}:{:02}", value / 60, value % 60),
            Timing::Ticks => crate::midi::group_digits(value),
        }
    }

    /// Appends the grid lines visible in `[t0, t1]` at zoom `pixels_per_tick`.
    /// Lines closer than `min_spacing_px` are thinned. `sub_ticks` is the
    /// subdivision to show below the beat (typically the snap unit).
    pub fn lines(
        &self,
        t0: u64,
        t1: u64,
        pixels_per_tick: f64,
        min_spacing_px: f64,
        sub_ticks: Option<u64>,
        out: &mut Vec<GridLine>,
    ) {
        out.clear();
        if t0 > t1 || pixels_per_tick <= 0.0 || !pixels_per_tick.is_finite() {
            return;
        }
        match &self.timing {
            Timing::Musical { ppqn, segments } => {
                musical_lines(
                    *ppqn,
                    segments,
                    t0,
                    t1,
                    pixels_per_tick,
                    min_spacing_px,
                    sub_ticks,
                    out,
                );
            }
            Timing::Smpte {
                fps_nominal,
                ticks_per_frame,
            } => {
                let frame = *ticks_per_frame;
                let second = frame * fps_nominal;
                // Minor step in frames, major step in seconds.
                let minor_steps = [1u64, 2, 5, 10].map(|f| f * frame);
                let major_steps = [
                    1u64, 2, 5, 10, 15, 30, 60, 120, 300, 600, 1800, 3600, 7200, 36000,
                ];
                let major = major_steps
                    .iter()
                    .map(|s| s * second)
                    .find(|s| *s as f64 * pixels_per_tick >= min_spacing_px * 3.0)
                    .unwrap_or_else(|| {
                        nice_step((min_spacing_px * 3.0 / pixels_per_tick) as u64, second)
                    });
                // Minor lines: the finest frame or second step that is readable and
                // divides the major step.
                let minor = minor_steps
                    .into_iter()
                    .chain(major_steps.iter().map(|s| s * second))
                    .find(|s| {
                        *s as f64 * pixels_per_tick >= min_spacing_px
                            && *s < major
                            && major % *s == 0
                    });
                push_steps(t0, t1, minor, major, out, |t| Some(t / second));
            }
            Timing::Ticks => {
                let major = nice_step((min_spacing_px * 5.0 / pixels_per_tick).ceil() as u64, 1);
                let minor_candidate = major / 5;
                let minor = (minor_candidate > 0
                    && minor_candidate as f64 * pixels_per_tick >= min_spacing_px)
                    .then_some(minor_candidate);
                push_steps(t0, t1, minor, major, out, Some);
            }
        }
    }
}

/// Smallest value of the form {1, 2, 5} × 10^k × `unit` that is >= `min`.
fn nice_step(min: u64, unit: u64) -> u64 {
    let mut base = unit.max(1);
    loop {
        for m in [1u64, 2, 5] {
            let s = base.saturating_mul(m);
            if s >= min || s == u64::MAX {
                return s.max(1);
            }
        }
        base = base.saturating_mul(10);
    }
}

fn push_steps(
    t0: u64,
    t1: u64,
    minor: Option<u64>,
    major: u64,
    out: &mut Vec<GridLine>,
    label: impl Fn(u64) -> Option<u64>,
) {
    let major = major.max(1);
    if let Some(minor) = minor.filter(|m| *m > 0) {
        let mut t = t0.div_ceil(minor).saturating_mul(minor);
        while t <= t1 {
            if t % major != 0 {
                out.push(GridLine {
                    tick: t,
                    kind: LineKind::Minor,
                    label: None,
                });
            }
            match t.checked_add(minor) {
                Some(n) => t = n,
                None => break,
            }
        }
    }
    let mut t = t0.div_ceil(major).saturating_mul(major);
    while t <= t1 {
        out.push(GridLine {
            tick: t,
            kind: LineKind::Major,
            label: label(t),
        });
        match t.checked_add(major) {
            Some(n) => t = n,
            None => break,
        }
    }
}

#[allow(clippy::too_many_arguments)] // plain numeric inputs of one routine
fn musical_lines(
    ppqn: u64,
    segments: &[Segment],
    t0: u64,
    t1: u64,
    ppt: f64,
    min_px: f64,
    sub_ticks: Option<u64>,
    out: &mut Vec<GridLine>,
) {
    let first = segments
        .partition_point(|s| s.start <= t0)
        .saturating_sub(1);
    for (i, s) in segments.iter().enumerate().skip(first) {
        if s.start > t1 {
            break;
        }
        let seg_end_excl = segments
            .get(i + 1)
            .map_or(u128::MAX, |n| u128::from(n.start));
        let lo = t0.max(s.start);
        let hi = u128::from(t1).min(seg_end_excl.saturating_sub(1));
        let bar_len = s.bar_num(ppqn) as f64 / s.den() as f64;
        let bar_px = bar_len * ppt;
        let mut step: u64 = 1;
        while (step as f64) * bar_px < min_px && step < (1 << 62) {
            step *= 2;
        }
        let beat_px = (ppqn * 4) as f64 / s.den() as f64 * ppt;
        let show_beats = step == 1 && beat_px >= min_px;
        let sub = sub_ticks.filter(|u| *u > 0 && show_beats && (*u as f64) * ppt >= min_px);

        // Bars: iterate only multiples of `step` in global bar numbering.
        let k0 = (u128::from(lo - s.start) * s.den() / s.bar_num(ppqn)) as u64;
        let global0 = s.first_bar.saturating_add(k0);
        let aligned = global0.div_ceil(step).saturating_mul(step);
        let mut k = aligned.saturating_sub(s.first_bar);
        // Beats start from the bar containing `lo`, so a partially visible bar still
        // shows its beats.
        let mut kb = k0;
        loop {
            let tick = s.bar_tick(ppqn, k);
            if tick > hi {
                break;
            }
            if tick >= u128::from(lo) {
                out.push(GridLine {
                    tick: tick as u64,
                    kind: LineKind::Major,
                    label: Some(s.first_bar.saturating_add(k).saturating_add(1)),
                });
            }
            match k.checked_add(step) {
                Some(n) => k = n,
                None => break,
            }
        }
        if show_beats {
            let beats_per_bar = s.numerator;
            loop {
                let bar_tick = s.bar_tick(ppqn, kb);
                if bar_tick > hi {
                    break;
                }
                for j in 1..beats_per_bar {
                    let bt = s.beat_tick(ppqn, kb * beats_per_bar + j);
                    if bt > hi {
                        break;
                    }
                    if bt >= u128::from(lo) {
                        out.push(GridLine {
                            tick: bt as u64,
                            kind: LineKind::Minor,
                            label: None,
                        });
                    }
                }
                kb += 1;
            }
        }
        if let Some(u) = sub {
            let rel0 = (lo - s.start).div_ceil(u);
            let mut m = rel0;
            loop {
                let t = u128::from(s.start) + u128::from(m) * u128::from(u);
                if t > hi {
                    break;
                }
                out.push(GridLine {
                    tick: t as u64,
                    kind: LineKind::Sub,
                    label: None,
                });
                m += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_four_four_bars() {
        let m = MeterMap::new(Division::ppqn(96), &[]);
        let mut out = Vec::new();
        // 1 px per tick: bars (384 ticks) and beats (96) visible, 1/16 (24) visible.
        m.lines(0, 800, 1.0, 8.0, Some(24), &mut out);
        let bars: Vec<(u64, Option<u64>)> = out
            .iter()
            .filter(|l| l.kind == LineKind::Major)
            .map(|l| (l.tick, l.label))
            .collect();
        assert_eq!(bars, vec![(0, Some(1)), (384, Some(2)), (768, Some(3))]);
        let beats: Vec<u64> = out
            .iter()
            .filter(|l| l.kind == LineKind::Minor)
            .map(|l| l.tick)
            .collect();
        assert_eq!(beats, vec![96, 192, 288, 480, 576, 672]);
        assert!(out.iter().any(|l| l.kind == LineKind::Sub && l.tick == 24));
        assert_eq!(m.bar_beat(0), Some((1, 1, 0)));
        assert_eq!(m.bar_beat(390), Some((2, 1, 6)));
        assert_eq!(m.bar_beat(96 * 6), Some((2, 3, 0)));
    }

    #[test]
    fn meter_change_and_numbering() {
        // 4/4 for one bar, then 3/4 from tick 384, then 7/8 from tick 384 + 288*2.
        let m = MeterMap::new(Division::ppqn(96), &[(384, 3, 2), (960, 7, 3)]);
        let mut out = Vec::new();
        m.lines(0, 2000, 1.0, 8.0, None, &mut out);
        let bars: Vec<(u64, u64)> = out
            .iter()
            .filter(|l| l.kind == LineKind::Major)
            .map(|l| (l.tick, l.label.unwrap()))
            .collect();
        // 7/8 bar = 7 * 48 = 336 ticks.
        assert_eq!(bars[..5], [(0, 1), (384, 2), (672, 3), (960, 4), (1296, 5)]);
        assert_eq!(m.bar_beat(960 + 48), Some((4, 2, 0)));
    }

    #[test]
    fn far_zoom_is_bounded_and_integer_exact() {
        // 3 PPQN with 7/64 signature: bar length 21/16 ticks (fractional).
        let m = MeterMap::new(Division::ppqn(3), &[(0, 7, 6)]);
        let mut out = Vec::new();
        let t1 = 10_000_000_000u64;
        m.lines(0, t1, 1e-6, 10.0, None, &mut out);
        assert!(out.len() < 2000, "{}", out.len());
        // Bar positions follow floor(k * 21 / 16) exactly.
        for l in &out {
            let k = l.label.unwrap() - 1;
            assert_eq!(u128::from(l.tick), u128::from(k) * 21 / 16);
        }
        // Huge absolute ticks do not lose precision or loop forever.
        let m4 = MeterMap::new(Division::ppqn(960), &[]);
        m4.lines(u64::MAX - 100_000, u64::MAX, 0.01, 8.0, Some(240), &mut out);
        assert!(!out.is_empty());
    }

    #[test]
    fn measure_navigation() {
        // 4/4 at 96 PPQN (bar 384), 3/4 from 384 (bar 288), 7/8 from 960 (bar 336).
        let m = MeterMap::new(Division::ppqn(96), &[(384, 3, 2), (960, 7, 3)]);
        assert_eq!(m.previous_bar(0), 0);
        assert_eq!(m.previous_bar(1), 0);
        assert_eq!(m.previous_bar(384), 0); // on a boundary → preceding boundary
        assert_eq!(m.previous_bar(385), 384); // inside → start of this bar
        assert_eq!(m.previous_bar(672), 384);
        assert_eq!(m.previous_bar(960), 672);
        assert_eq!(m.previous_bar(1000), 960);
        assert_eq!(m.next_bar(0), Some(384));
        assert_eq!(m.next_bar(383), Some(384));
        assert_eq!(m.next_bar(384), Some(672));
        assert_eq!(m.next_bar(700), Some(960));
        assert_eq!(m.next_bar(960), Some(1296));
        assert_eq!(m.bar_start_at_or_before(1295), 960);
        // A meter change in the middle of a bar ends that (partial) bar early.
        let m = MeterMap::new(Division::ppqn(96), &[(500, 3, 2)]);
        assert_eq!(m.next_bar(400), Some(500));
        assert_eq!(m.previous_bar(501), 500);
        // Fractional bars (7/64 at 3 PPQN: 21/16 ticks) use floor(k·21/16) exactly.
        let f = MeterMap::new(Division::ppqn(3), &[(0, 7, 6)]);
        let boundaries: Vec<u64> = (0..40u64).map(|k| k * 21 / 16).collect();
        let mut t = 0;
        for &b in &boundaries[1..] {
            let n = f.next_bar(t).unwrap();
            assert_eq!(n, b, "after {t}");
            assert_eq!(f.previous_bar(n), f.bar_start_at_or_before(n - 1));
            t = n;
        }
        // Bars are not hard-coded as 4 × PPQN, and huge ticks stay checked.
        let big = MeterMap::new(Division::ppqn(960), &[]);
        assert_eq!(big.next_bar(u64::MAX - 1), None);
        assert_eq!(big.previous_bar(u64::MAX), u64::MAX - u64::MAX % 3840);
        // SMPTE: one-second steps.
        let s = MeterMap::new(Division::from_raw(0xE728), &[]);
        assert_eq!(s.next_bar(0), Some(1000));
        assert_eq!(s.previous_bar(1000), 0);
        assert_eq!(s.previous_bar(1500), 1000);
    }

    #[test]
    fn common_and_unusual_signatures_have_exact_bars() {
        // 3/4, 7/8, 5/16 and 3/128 at 960 PPQN: bar = N · 3840 / 2^d ticks.
        for (num, pow, bar) in [(3u8, 2u8, 2880u64), (7, 3, 3360), (5, 4, 1200), (3, 7, 90)] {
            let m = MeterMap::new(Division::ppqn(960), &[(0, num, pow)]);
            assert_eq!(m.next_bar(0), Some(bar), "{num}/{}", 1 << pow);
            assert_eq!(m.next_bar(bar), Some(2 * bar));
            assert_eq!(m.bar_beat(bar * 10), Some((11, 1, 0)));
            assert_eq!(m.previous_bar(bar * 10), bar * 9);
        }
        // Unusual PPQN: 5/16 at 7 PPQN is 35/4 ticks per bar → floor(k·35/4).
        let m = MeterMap::new(Division::ppqn(7), &[(0, 5, 4)]);
        let mut t = 0;
        for k in 1..50u64 {
            let n = m.next_bar(t).unwrap();
            assert_eq!(n, k * 35 / 4);
            assert!(n > t);
            t = n;
        }
        // A change at tick 0 replaces the default 4/4.
        let m = MeterMap::new(Division::ppqn(96), &[(0, 7, 3)]);
        assert_eq!(m.next_bar(0), Some(336));
        // A change near the end of the song (one tick before a bar line).
        let m = MeterMap::new(Division::ppqn(96), &[(383, 5, 3)]);
        assert_eq!(m.bar_beat(382), Some((1, 4, 94)));
        assert_eq!(m.bar_beat(383), Some((2, 1, 0)));
        assert_eq!(m.next_bar(383), Some(383 + 240));
    }

    #[test]
    fn random_signature_maps_give_increasing_boundaries() {
        let mut seed = 0x1234_5678_9ABC_DEF0u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..200 {
            let ppqn = 1 + next() % 2000;
            let mut events = Vec::new();
            let mut tick = 0;
            for _ in 0..(next() % 8) {
                tick += next() % 20_000;
                events.push((tick, (1 + next() % 32) as u8, (next() % 8) as u8));
            }
            events.dedup_by_key(|e| e.0);
            let m = MeterMap::new(Division::ppqn(ppqn as u32), &events);
            let mut t = 0u64;
            let mut bar = 1;
            for _ in 0..300 {
                let n = m.next_bar(t).unwrap();
                assert!(n > t);
                assert_eq!(m.previous_bar(n), m.bar_start_at_or_before(n - 1));
                let (b, beat, off) = m.bar_beat(n).unwrap();
                assert_eq!((beat, off), (1, 0), "a boundary starts a bar");
                assert_eq!(b, bar + 1, "continuous numbering");
                bar = b;
                t = n;
            }
            // Every change tick is a bar boundary.
            for &(tick, ..) in &events {
                assert_eq!(m.bar_start_at_or_before(tick), tick);
            }
        }
    }

    #[test]
    fn smpte_and_tick_grids() {
        let m = MeterMap::new(Division::from_raw(0xE728), &[]); // 25 fps x 40
        assert!(!m.is_musical());
        let mut out = Vec::new();
        // One second = 1000 ticks = 10 px; majors need >= 24 px, so every 5 s.
        m.lines(0, 1000 * 25 * 40, 0.01, 8.0, None, &mut out);
        let majors: Vec<Option<u64>> = out
            .iter()
            .filter(|l| l.kind == LineKind::Major)
            .map(|l| l.label)
            .take(3)
            .collect();
        assert_eq!(majors, vec![Some(0), Some(5), Some(10)]);
        assert!(out.iter().any(|l| l.kind == LineKind::Minor));
        assert_eq!(m.describe(25 * 40 * 61 + 40 * 3), "1:01.03");
        let t = MeterMap::new(Division::from_raw(0), &[]);
        t.lines(0, 1000, 1.0, 10.0, None, &mut out);
        assert!(out.iter().all(|l| l.tick % 10 == 0));
        assert!(!out.is_empty());
    }

    #[test]
    fn musical_positions_follow_time_signature_changes() {
        // 480 PPQN: 4/4 for 2 bars, then 3/4 from tick 3840, then 7/8 from a
        // point inside a 3/4 bar (a partial measure).
        let m = MeterMap::new(
            Division::ppqn(480),
            &[(0, 4, 2), (3840, 3, 2), (3840 + 1440 + 720, 7, 3)],
        );
        assert_eq!(m.measure_start(1), Some(0));
        assert_eq!(m.measure_start(2), Some(1920));
        assert_eq!(m.measure_start(3), Some(3840));
        assert_eq!(m.measure_start(4), Some(3840 + 1440));
        // The partial 3/4 bar (720 ticks) is measure 4; 7/8 starts measure 5.
        assert_eq!(m.measure_start(5), Some(6000));
        assert_eq!(m.measure_start(6), Some(6000 + 1680));
        assert_eq!(m.measure_length_at(3840 + 1440 + 10), Some(720));
        assert_eq!(m.measure_length_at(6000 + 5), Some(1680));
        assert_eq!(m.measure_start(0), None);
        for (text, tick) in [
            ("1", 0u64),
            ("1:0", 0),
            ("2", 1920),
            ("2:960", 2880),
            ("3:2:0", 3840 + 480),
            ("3:3:479", 3840 + 960 + 479),
            ("6:7:0", 7680 + 1440),
            (" 4 : 100 ", 3840 + 1440 + 100),
        ] {
            assert_eq!(m.parse_position(text), Ok(tick), "{text}");
            // Formatting gives a position that parses back to the same tick.
            assert_eq!(m.parse_position(&m.format_position(tick)), Ok(tick));
            assert_eq!(m.parse_position(&m.format_measure_tick(tick)), Ok(tick));
        }
        for bad in [
            "",
            "0",
            "0:5",
            "1:1920",
            "3:4:0",
            "3:0:0",
            "3:1:480",
            "a",
            "1:2:3:4",
            "-1",
            "1.5",
            "99999999999999999999999",
        ] {
            assert!(m.parse_position(bad).is_err(), "{bad}");
        }
        assert_eq!(m.span_in_measures(1920, 3840 + 480 + 5), Some((1, 1, 5)));
        assert_eq!(m.span_in_measures(1921, 4000), None);
        // Huge measures keep exact integers.
        let big = m.measure_start(1_000_000_000_000).unwrap();
        assert_eq!(m.parse_position("1000000000000"), Ok(big));
        assert_eq!(m.measure_at(big).map(|x| x.0), Some(1_000_000_000_000));
        // Non-musical timing refuses positions.
        let smpte = MeterMap::new(Division::from_raw(0xE728), &[]);
        assert!(smpte.parse_position("1").is_err());
    }

    /// Beat and bar boundaries by a plain walk: within each signature
    /// segment, a boundary every `len_num / den` ticks (floor) up to the next
    /// change, which starts a new one.
    fn walk(ppqn: u64, sigs: &[(u64, u8, u8)], limit: u64, bars: bool) -> Vec<u64> {
        let mut segs: Vec<(u64, u64, u32)> = vec![(0, 4, 2)];
        for &(t, n, d) in sigs {
            if segs.last().unwrap().0 == t {
                *segs.last_mut().unwrap() = (t, u64::from(n), u32::from(d));
            } else {
                segs.push((t, u64::from(n), u32::from(d)));
            }
        }
        let mut out = Vec::new();
        for (i, &(start, num, dp)) in segs.iter().enumerate() {
            let end = segs.get(i + 1).map_or(limit, |s| s.0);
            let len_num = u128::from(ppqn) * 4 * if bars { u128::from(num) } else { 1 };
            let mut j = 0u128;
            loop {
                let t = u128::from(start) + j * len_num / (1u128 << dp);
                if t >= u128::from(end) {
                    break;
                }
                out.push(t as u64);
                j += 1;
            }
        }
        out
    }

    /// V09-S13: beat indices, beat starts and the bar / beat advance equal a
    /// plain walk of the boundaries across 4/4 → 3/4 → 7/8 → 5/16 changes,
    /// a change inside a bar and inside a beat, and odd PPQNs.
    #[test]
    fn bar_and_beat_advance_equal_a_walk_across_signature_changes() {
        for ppqn in [96u64, 480, 7] {
            let p = ppqn;
            let sigs = [
                (4 * 4 * p, 3, 2),
                (4 * 4 * p + 3 * 3 * p, 7, 3),
                // Inside a 7/8 bar and inside a beat.
                (4 * 4 * p + 3 * 3 * p + 7 * p / 2 + 3 * p + p / 3, 5, 4),
                (40 * 4 * p, 4, 2),
            ];
            let m = MeterMap::new(Division::ppqn(p as u32), &sigs);
            let limit = 60 * 4 * p;
            let beats = walk(p, &sigs, limit, false);
            let bars = walk(p, &sigs, limit, true);
            for (i, &b) in beats.iter().enumerate() {
                assert_eq!(m.beat_start(i as u64), Some(b), "ppqn {p} beat {i}");
            }
            for &b in &bars {
                assert_eq!(m.bar_start_at_or_before(b), b, "ppqn {p} bar at {b}");
            }
            let step = (p / 5).max(1);
            let mut t = 0;
            while t < 50 * 4 * p {
                let bi = beats.partition_point(|&x| x <= t) - 1;
                assert_eq!(
                    m.beat_at(t),
                    Some((bi as u64, t - beats[bi])),
                    "ppqn {p} t {t}"
                );
                for n in [1u64, 2, 7] {
                    let want = beats.get(bi + n as usize).map(|&x| x + (t - beats[bi]));
                    if let Some(w) = want {
                        assert_eq!(m.advance_beats(t, n), Some(w), "ppqn {p} t {t} +{n} beats");
                    }
                    let ri = bars.partition_point(|&x| x <= t) - 1;
                    if let Some(&x) = bars.get(ri + n as usize) {
                        assert_eq!(
                            m.advance_bars(t, n),
                            Some(x + (t - bars[ri])),
                            "ppqn {p} t {t} +{n} bars"
                        );
                    }
                }
                t += step;
            }
        }
        // Non-musical timing has no bars / beats; SMPTE has frames.
        let smpte = MeterMap::new(Division::from_raw(0xE728), &[]);
        assert_eq!(smpte.advance_beats(0, 1), None);
        assert_eq!(smpte.advance_bars(0, 1), None);
        assert_eq!(smpte.ticks_per_frame(), Some(40));
        assert_eq!(
            MeterMap::new(Division::ppqn(96), &[]).ticks_per_frame(),
            None
        );
    }
}
