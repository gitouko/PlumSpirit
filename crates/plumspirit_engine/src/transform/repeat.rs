//! Paste Repeat's timing core (v0.9 V09-S13, contract §088 – §091;
//! `docs/PASTE_REPEAT.md`).
//!
//! One plan for both domains: the selected notes (the `paste_repeat_notes`
//! tool) and the selected clips (`editor::clips::repeat_clips`) ask [`plan`]
//! where copy *k* (k = 1 … N) of their source goes; nothing here knows notes
//! or clips.
//!
//! * **End to end** — copy *k* at `k × period`, the period being the
//!   source's span rounded up to the Snap unit (the Duplicate tool's and
//!   Ctrl+D's rule; exact ticks with Snap off).
//! * **Even spacing** — copy *k* at *k × every* after the anchor, in the
//!   chosen unit: ticks, note values and SMPTE frames are fixed tick
//!   lengths; bars and beats advance through the meter map (the offset into
//!   the bar / beat is kept); seconds, milliseconds and microseconds are
//!   physical time through the tempo map (§091: "every 1 second" is one real
//!   second, so the tick distance differs under tempo changes — each copy is
//!   the last tick that has started by its exact time).
//!
//! Every conversion is exact or refused with a sentence (§090): the value is
//! an exact decimal, never a float; nothing is clamped silently. The cost is
//! O(copies × log segments), never per note.

pub mod series;

use super::Ctx;
use crate::model::TempoMap;
use crate::model::tempo::FX_SHIFT;
use crate::view::grid::MeterMap;

/// Most copies one Paste Repeat makes (the preflight limits notes / clips
/// further).
pub const MAX_COPIES: u32 = 100_000;

/// An exact decimal number: `mantissa / 10^scale`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Decimal {
    pub mantissa: u64,
    pub scale: u32,
}

impl Decimal {
    /// Most decimals a value may have.
    pub const MAX_SCALE: u32 = 9;

    pub const fn whole(n: u64) -> Decimal {
        Decimal {
            mantissa: n,
            scale: 0,
        }
    }

    /// Parses `"3"`, `"1.5"`, `"0.250"`, `".5"` (no sign, no exponent; up
    /// to [`Decimal::MAX_SCALE`] decimals).
    pub fn parse(text: &str) -> Result<Decimal, String> {
        let t = text.trim();
        let bad = || format!("“{t}” is not a number (write it like 1, 2.5 or 0.125).");
        if t.is_empty() {
            return Err("Enter a number.".into());
        }
        let (int, frac) = match t.split_once('.') {
            Some((a, b)) => (a, b),
            None => (t, ""),
        };
        if (int.is_empty() && frac.is_empty())
            || !int.bytes().all(|b| b.is_ascii_digit())
            || !frac.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(bad());
        }
        let frac = frac.trim_end_matches('0');
        let scale = u32::try_from(frac.len()).map_err(|_| bad())?;
        if scale > Self::MAX_SCALE {
            return Err(format!("“{t}” has more than {} decimals.", Self::MAX_SCALE));
        }
        let digits = format!("{int}{frac}");
        let digits = digits.trim_start_matches('0');
        let mantissa = if digits.is_empty() {
            0
        } else {
            digits
                .parse::<u64>()
                .map_err(|_| format!("“{t}” is too large."))?
        };
        Ok(Decimal { mantissa, scale })
    }

    pub fn is_zero(self) -> bool {
        self.mantissa == 0
    }

    fn pow10(self) -> u128 {
        10u128.pow(self.scale)
    }

    /// The value as a whole number, if it is one.
    pub fn whole_value(self) -> Option<u64> {
        let p = self.pow10();
        (u128::from(self.mantissa) % p == 0).then(|| (u128::from(self.mantissa) / p) as u64)
    }

    /// `value × num / den` when it is a whole number.
    fn exact_times(self, num: u128, den: u128) -> Option<u128> {
        let n = u128::from(self.mantissa).checked_mul(num)?;
        let d = den.checked_mul(self.pow10())?;
        (d != 0 && n % d == 0).then(|| n / d)
    }
}

impl std::fmt::Display for Decimal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let p = 10u64.pow(self.scale);
        let (i, r) = (self.mantissa / p, self.mantissa % p);
        if r == 0 {
            write!(f, "{}", crate::midi::group_digits(i))
        } else {
            let s = format!("{r:0width$}", width = self.scale as usize);
            write!(
                f,
                "{}.{}",
                crate::midi::group_digits(i),
                s.trim_end_matches('0')
            )
        }
    }
}

/// The unit of *Paste every* (§090).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RepeatUnit {
    Bars,
    Beats,
    /// A note value 1/n — the Snap values PlumSpirit offers (4, 8, 16, 32).
    Note(u8),
    /// SMPTE frames.
    Frames,
    Ticks,
    Seconds,
    Milliseconds,
    Microseconds,
}

impl RepeatUnit {
    /// Every unit, in the dialog's order.
    pub const ALL: [RepeatUnit; 11] = [
        RepeatUnit::Bars,
        RepeatUnit::Beats,
        RepeatUnit::Note(4),
        RepeatUnit::Note(8),
        RepeatUnit::Note(16),
        RepeatUnit::Note(32),
        RepeatUnit::Frames,
        RepeatUnit::Ticks,
        RepeatUnit::Seconds,
        RepeatUnit::Milliseconds,
        RepeatUnit::Microseconds,
    ];

    /// The dialog's label.
    pub fn label(self) -> &'static str {
        match self {
            RepeatUnit::Bars => "Bars",
            RepeatUnit::Beats => "Beats",
            RepeatUnit::Note(4) => "1/4 notes",
            RepeatUnit::Note(8) => "1/8 notes",
            RepeatUnit::Note(16) => "1/16 notes",
            RepeatUnit::Note(32) => "1/32 notes",
            RepeatUnit::Note(_) => "notes",
            RepeatUnit::Frames => "Frames",
            RepeatUnit::Ticks => "Ticks",
            RepeatUnit::Seconds => "Seconds",
            RepeatUnit::Milliseconds => "Milliseconds",
            RepeatUnit::Microseconds => "Microseconds",
        }
    }

    /// Physical time (through the tempo map), not musical.
    pub fn is_physical(self) -> bool {
        matches!(
            self,
            RepeatUnit::Seconds | RepeatUnit::Milliseconds | RepeatUnit::Microseconds
        )
    }

    fn lower(self) -> String {
        self.label().to_lowercase()
    }
}

/// Paste spacing (§089).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Spacing {
    /// Each copy starts where the previous one ends (the span, rounded up
    /// to the Snap unit). *Paste every* is not used.
    EndToEnd,
    /// A copy every *Paste every* value.
    Even,
}

/// What the dialog asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepeatSpec {
    /// Number of times to paste (copies; the source stays).
    pub count: u32,
    pub spacing: Spacing,
    /// *Paste every* (Even spacing).
    pub every: Decimal,
    pub unit: RepeatUnit,
}

/// The source of a repeat, in the caller's ticks (a clip session's content
/// ticks, or song ticks).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepeatSource {
    /// The anchor: the first note start / first clip start.
    pub start: u64,
    /// The last end.
    pub end: u64,
    /// The Snap unit in ticks (`None` or 1: off).
    pub snap: Option<u64>,
}

/// Song time for the musical and physical units.
#[derive(Clone, Copy, Debug)]
pub struct RepeatClock<'a> {
    pub meter: &'a MeterMap,
    /// The Conductor's tempo map at [`super::TEMPO_RATE`] (1 frame = 1 µs);
    /// `None`: 120 BPM at `ppq`.
    pub tempo: Option<&'a TempoMap>,
    /// Ticks per quarter note (for the 120 BPM fallback).
    pub ppq: u64,
    /// Song tick = caller tick + `shift` (a clip session's `start − offset`).
    pub shift: i128,
}

impl<'a> RepeatClock<'a> {
    /// The clock of a transformation context (its meter, tempo map and clip
    /// session shift).
    pub fn of(ctx: &'a Ctx) -> RepeatClock<'a> {
        RepeatClock {
            meter: &ctx.meter,
            tempo: ctx.tempo.as_deref(),
            ppq: ctx.ppq.max(1),
            shift: ctx.shift,
        }
    }

    /// Start time of song tick `t` in µs × 2^32.
    fn fx(&self, t: u64) -> u128 {
        match self.tempo {
            Some(m) => m.fx_at_tick(t),
            // 120 BPM: 500,000 µs per quarter note.
            None => (u128::from(t) * (500_000u128 << FX_SHIFT)) / u128::from(self.ppq),
        }
    }

    /// The last song tick whose start time is at or before `target`.
    fn last_tick_by(&self, target: u128) -> u64 {
        match self.tempo {
            Some(m) => m
                .first_tick_at_or_after_fx(target.saturating_add(1))
                .saturating_sub(1),
            None => {
                let per = 500_000u128 << FX_SHIFT;
                let t = (target.saturating_add(1) * u128::from(self.ppq)).div_ceil(per);
                u64::try_from(t.saturating_sub(1)).unwrap_or(u64::MAX)
            }
        }
    }
}

/// Where the copies go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepeatPlan {
    /// Copy *k*'s start relative to the anchor (index k − 1), strictly
    /// increasing, ≥ 1.
    pub offsets: Vec<u64>,
    /// The distance between copies when it is the same for all.
    pub period: Option<u64>,
    /// The source's span (anchor → last end).
    pub span: u64,
    /// The end of the last copy (caller ticks).
    pub end: u64,
}

impl RepeatPlan {
    pub fn count(&self) -> usize {
        self.offsets.len()
    }
}

/// Plans `spec` for `src` (pure; per copy, never per note). `Err` holds the
/// sentence the dialog shows.
pub fn plan(
    spec: &RepeatSpec,
    src: RepeatSource,
    clock: &RepeatClock,
) -> Result<RepeatPlan, String> {
    if spec.count == 0 || spec.count > MAX_COPIES {
        return Err(format!(
            "Number of times to paste must be from 1 to {}.",
            crate::midi::group_digits(u64::from(MAX_COPIES))
        ));
    }
    let span = src.end.saturating_sub(src.start);
    if span == 0 {
        return Err("Nothing to repeat: the selection has no length.".into());
    }
    let n = spec.count as usize;
    let offsets = match spec.spacing {
        Spacing::EndToEnd => {
            let period = match src.snap.filter(|&s| s > 1) {
                Some(s) => span.div_ceil(s).checked_mul(s).ok_or_else(beyond)?,
                None => span,
            };
            fixed(period, n)?
        }
        Spacing::Even => even(spec, src.start, clock, n)?,
    };
    let last = *offsets.last().expect("count ≥ 1");
    let end = src
        .start
        .checked_add(last)
        .and_then(|t| t.checked_add(span))
        .ok_or_else(beyond)?;
    let period = offsets
        .windows(2)
        .all(|w| w[1] - w[0] == offsets[0])
        .then_some(offsets[0]);
    Ok(RepeatPlan {
        offsets,
        period,
        span,
        end,
    })
}

fn beyond() -> String {
    "The copies would reach past the last tick PlumSpirit can hold: paste fewer times or closer together.".into()
}

/// `k × step` for k = 1 … n.
fn fixed(step: u64, n: usize) -> Result<Vec<u64>, String> {
    if step == 0 {
        return Err("The spacing is shorter than one tick: use a longer spacing.".into());
    }
    (1..=n as u64)
        .map(|k| k.checked_mul(step).ok_or_else(beyond))
        .collect()
}

fn even(spec: &RepeatSpec, anchor: u64, clock: &RepeatClock, n: usize) -> Result<Vec<u64>, String> {
    let v = spec.every;
    let unit = spec.unit;
    if v.is_zero() {
        return Err("Paste every must be more than 0.".into());
    }
    let not_whole_ticks = |what: String| {
        format!("{v} {what} is not a whole number of ticks here: change the value or use ticks.")
    };
    match unit {
        RepeatUnit::Ticks => {
            let t = v
                .whole_value()
                .ok_or_else(|| "Ticks take whole numbers.".to_string())?;
            fixed(t, n)
        }
        RepeatUnit::Note(d) => {
            let ppq = clock.meter.ppqn().ok_or_else(|| {
                "Note values need musical timing (bars and beats); this song uses SMPTE timing — use frames, ticks or seconds.".to_string()
            })?;
            let t = v
                .exact_times(u128::from(ppq) * 4, u128::from(d.max(1)))
                .ok_or_else(|| not_whole_ticks(unit.lower()))?;
            fixed(u64::try_from(t).map_err(|_| beyond())?, n)
        }
        RepeatUnit::Frames => {
            let tpf = clock.meter.ticks_per_frame().ok_or_else(|| {
                "Frames need SMPTE timing; this song counts in bars and beats — use a note value, ticks or seconds.".to_string()
            })?;
            let t = v
                .exact_times(u128::from(tpf), 1)
                .ok_or_else(|| not_whole_ticks("frames".into()))?;
            fixed(u64::try_from(t).map_err(|_| beyond())?, n)
        }
        RepeatUnit::Bars | RepeatUnit::Beats => {
            let bars = unit == RepeatUnit::Bars;
            let step = v.whole_value().ok_or_else(|| {
                if bars {
                    "Bars take whole numbers: for part of a bar use beats or a note value."
                        .to_string()
                } else {
                    "Beats take whole numbers: for part of a beat use a note value or ticks."
                        .to_string()
                }
            })?;
            if !clock.meter.is_musical() {
                return Err(
                    "Bars and beats need musical timing; this song uses SMPTE timing — use frames, ticks or seconds."
                        .into(),
                );
            }
            let a = song_tick(anchor, clock)?;
            let mut out = Vec::with_capacity(n);
            for k in 1..=n as u64 {
                let by = k.checked_mul(step).ok_or_else(beyond)?;
                let t = if bars {
                    clock.meter.advance_bars(a, by)
                } else {
                    clock.meter.advance_beats(a, by)
                }
                .ok_or_else(beyond)?;
                push_increasing(&mut out, t - a, k, spec)?;
            }
            Ok(out)
        }
        RepeatUnit::Seconds | RepeatUnit::Milliseconds | RepeatUnit::Microseconds => {
            let per = match unit {
                RepeatUnit::Seconds => 1_000_000,
                RepeatUnit::Milliseconds => 1_000,
                _ => 1,
            };
            let us = v.exact_times(per, 1).ok_or_else(|| {
                format!(
                    "{v} {} is not a whole number of microseconds: use at most {} decimals.",
                    unit.lower(),
                    match unit {
                        RepeatUnit::Seconds => 6,
                        RepeatUnit::Milliseconds => 3,
                        _ => 0,
                    }
                )
            })?;
            let a = song_tick(anchor, clock)?;
            let a_fx = clock.fx(a);
            let mut out = Vec::with_capacity(n);
            for k in 1..=n as u128 {
                let target = k
                    .checked_mul(us)
                    .and_then(|x| x.checked_mul(1u128 << FX_SHIFT))
                    .and_then(|x| x.checked_add(a_fx))
                    .ok_or_else(beyond)?;
                let t = clock.last_tick_by(target);
                if t == u64::MAX {
                    return Err(beyond());
                }
                push_increasing(&mut out, t.saturating_sub(a), k as u64, spec)?;
            }
            Ok(out)
        }
    }
}

fn song_tick(t: u64, clock: &RepeatClock) -> Result<u64, String> {
    u64::try_from(i128::from(t) + clock.shift)
        .map_err(|_| "The selection starts before the song's start (in this clip).".to_string())
}

/// Appends copy `k`'s offset, refusing one on or before the previous copy.
fn push_increasing(out: &mut Vec<u64>, off: u64, k: u64, spec: &RepeatSpec) -> Result<(), String> {
    let prev = out.last().copied().unwrap_or(0);
    if off <= prev {
        return Err(format!(
            "Copy {} would start on the same tick as {}: {} {} is shorter than one tick at this tempo — use a longer spacing.",
            k,
            if k == 1 {
                "the original".to_string()
            } else {
                format!("copy {}", k - 1)
            },
            spec.every,
            spec.unit.lower()
        ));
    }
    out.push(off);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{History, Operation};
    use crate::model::Bpm;
    use crate::model::PlumDocument;
    use crate::model::conductor::Interp;
    use crate::model::tempo::TempoValue;
    use crate::model::timing::Division;

    fn spec(count: u32, spacing: Spacing, every: &str, unit: RepeatUnit) -> RepeatSpec {
        RepeatSpec {
            count,
            spacing,
            every: Decimal::parse(every).unwrap(),
            unit,
        }
    }

    fn src(start: u64, end: u64, snap: Option<u64>) -> RepeatSource {
        RepeatSource { start, end, snap }
    }

    #[test]
    fn decimals_parse_exactly_and_print_back() {
        for (t, m, s, shown) in [
            ("3", 3, 0, "3"),
            ("1.5", 15, 1, "1.5"),
            ("0.250", 25, 2, "0.25"),
            (".5", 5, 1, "0.5"),
            ("12000", 12000, 0, "12,000"),
            ("0.000001", 1, 6, "0.000001"),
            ("  2.  ", 2, 0, "2"),
        ] {
            let d = Decimal::parse(t).unwrap();
            assert_eq!((d.mantissa, d.scale), (m, s), "{t}");
            assert_eq!(d.to_string(), shown);
        }
        for bad in ["", "-1", "1e3", "1.2.3", "abc", ".", "0.0000000001"] {
            assert!(Decimal::parse(bad).is_err(), "{bad:?}");
        }
        assert_eq!(Decimal::parse("2.000").unwrap().whole_value(), Some(2));
        assert_eq!(Decimal::parse("2.5").unwrap().whole_value(), None);
    }

    #[test]
    fn end_to_end_rounds_the_span_up_to_the_snap() {
        let m = MeterMap::new(Division::ppqn(480), &[]);
        let c = RepeatClock {
            meter: &m,
            tempo: None,
            ppq: 480,
            shift: 0,
        };
        // A 4-bar loop whose last note ends 20 ticks early: copies on bars 5, 9, 13.
        let s = spec(3, Spacing::EndToEnd, "1", RepeatUnit::Seconds);
        let p = plan(&s, src(0, 7660, Some(120)), &c).unwrap();
        assert_eq!(p.offsets, vec![7680, 15360, 23040]);
        assert_eq!((p.period, p.span, p.end), (Some(7680), 7660, 23040 + 7660));
        // Snap off: exact end to end.
        let p = plan(&s, src(100, 7760, None), &c).unwrap();
        assert_eq!(p.offsets, vec![7660, 15320, 22980]);
        // The anchor need not be on the grid; the period is what is rounded.
        let p = plan(&s, src(30, 270, Some(120)), &c).unwrap();
        assert_eq!(p.offsets, vec![240, 480, 720]);
    }

    #[test]
    fn fixed_units_are_exact_multiples_or_refused() {
        let m = MeterMap::new(Division::ppqn(480), &[]);
        let c = RepeatClock {
            meter: &m,
            tempo: None,
            ppq: 480,
            shift: 0,
        };
        let even = |v: &str, u| plan(&spec(4, Spacing::Even, v, u), src(1000, 1100, None), &c);
        assert_eq!(
            even("1", RepeatUnit::Note(4)).unwrap().offsets,
            vec![480, 960, 1440, 1920]
        );
        assert_eq!(even("3", RepeatUnit::Note(16)).unwrap().period, Some(360));
        assert_eq!(even("1.5", RepeatUnit::Note(8)).unwrap().period, Some(360));
        assert_eq!(even("0.5", RepeatUnit::Note(32)).unwrap().period, Some(30));
        assert_eq!(even("250", RepeatUnit::Ticks).unwrap().period, Some(250));
        // A 1/32 note at 480 PPQN is 60 ticks: 0.7 of it is 42, 0.33 of it is not whole.
        assert_eq!(even("0.7", RepeatUnit::Note(32)).unwrap().period, Some(42));
        assert!(
            even("0.33", RepeatUnit::Note(32))
                .unwrap_err()
                .contains("not a whole number of ticks")
        );
        assert_eq!(
            even("2.5", RepeatUnit::Ticks).unwrap_err(),
            "Ticks take whole numbers."
        );
        assert!(
            even("0", RepeatUnit::Ticks)
                .unwrap_err()
                .contains("more than 0")
        );
        assert!(even("1", RepeatUnit::Frames).unwrap_err().contains("SMPTE"));
        // SMPTE: frames work, note values and bars do not.
        let sm = MeterMap::new(Division::from_raw(0xE728), &[]);
        let cs = RepeatClock { meter: &sm, ..c };
        let ev = |v: &str, u| plan(&spec(2, Spacing::Even, v, u), src(0, 10, None), &cs);
        assert_eq!(ev("2", RepeatUnit::Frames).unwrap().offsets, vec![80, 160]);
        assert_eq!(ev("0.25", RepeatUnit::Frames).unwrap().period, Some(10));
        assert!(ev("1", RepeatUnit::Note(4)).unwrap_err().contains("SMPTE"));
        assert!(ev("1", RepeatUnit::Bars).unwrap_err().contains("SMPTE"));
        // Counts and lengths.
        assert!(
            plan(
                &spec(0, Spacing::EndToEnd, "1", RepeatUnit::Ticks),
                src(0, 5, None),
                &c
            )
            .is_err()
        );
        assert!(
            plan(
                &spec(MAX_COPIES + 1, Spacing::EndToEnd, "1", RepeatUnit::Ticks),
                src(0, 5, None),
                &c
            )
            .unwrap_err()
            .contains("from 1 to 100,000")
        );
        assert!(
            plan(
                &spec(1, Spacing::EndToEnd, "1", RepeatUnit::Ticks),
                src(5, 5, None),
                &c
            )
            .unwrap_err()
            .contains("no length")
        );
        assert!(
            plan(
                &spec(3, Spacing::Even, "18446744073709551615", RepeatUnit::Ticks),
                src(0, 5, None),
                &c
            )
            .unwrap_err()
            .contains("past the last tick")
        );
    }

    #[test]
    fn bars_and_beats_follow_the_meter_and_take_whole_numbers() {
        // 4/4 for 2 bars, then 3/4, then 7/8 from bar 6.
        let m = MeterMap::new(
            Division::ppqn(480),
            &[(3840, 3, 2), (3840 + 3 * 1440, 7, 3)],
        );
        let c = RepeatClock {
            meter: &m,
            tempo: None,
            ppq: 480,
            shift: 0,
        };
        // From beat 2 of bar 1 (tick 480): one bar later each time.
        let p = plan(
            &spec(5, Spacing::Even, "1", RepeatUnit::Bars),
            src(480, 600, None),
            &c,
        )
        .unwrap();
        let want: Vec<u64> = (1..=5)
            .map(|k| m.advance_bars(480, k).unwrap() - 480)
            .collect();
        assert_eq!(p.offsets, want);
        assert_eq!(p.offsets, vec![1920, 3840, 5280, 6720, 8160]);
        assert_eq!(p.period, None, "bars of different lengths");
        let p = plan(
            &spec(3, Spacing::Even, "2", RepeatUnit::Beats),
            src(3840 - 480, 3840, None),
            &c,
        )
        .unwrap();
        assert_eq!(p.offsets, vec![960, 1920, 2880]);
        assert!(
            plan(
                &spec(1, Spacing::Even, "1.5", RepeatUnit::Bars),
                src(0, 9, None),
                &c
            )
            .unwrap_err()
            .starts_with("Bars take whole numbers")
        );
        assert!(
            plan(
                &spec(1, Spacing::Even, "0.5", RepeatUnit::Beats),
                src(0, 9, None),
                &c
            )
            .unwrap_err()
            .starts_with("Beats take whole numbers")
        );
        // A clip session: musical positions are song positions.
        let shifted = RepeatClock { shift: 3840, ..c };
        let p = plan(
            &spec(1, Spacing::Even, "1", RepeatUnit::Bars),
            src(0, 9, None),
            &shifted,
        )
        .unwrap();
        assert_eq!(p.offsets, vec![1440], "the first 3/4 bar of the song");
    }

    /// A document with a step, a linear ramp and (optionally) many points.
    fn tempo_doc(points: &[(u64, &str, Interp)]) -> PlumDocument {
        let mut d = PlumDocument::new_project("t".into(), Bpm::parse("120").unwrap(), 480);
        if points.is_empty() {
            return d;
        }
        let pts: Vec<(u64, TempoValue, Interp)> = points
            .iter()
            .map(|&(t, b, i)| (t, TempoValue::Bpm(Bpm::parse(b).unwrap()), i))
            .collect();
        let edit = d.conductor().prepare_tempo_points(&pts, false).unwrap();
        History::default()
            .apply(
                &mut d,
                Operation::Conductor {
                    edit,
                    label: "Tempo",
                },
            )
            .unwrap();
        d
    }

    /// Each copy is the last tick that has started by its exact time
    /// (anchor time + k × every), checked against `fx_at_tick` brute force
    /// (the copy's time ≤ target < the next tick's time), strictly increasing.
    fn check_physical(d: &PlumDocument, anchor: u64, every: &str, unit: RepeatUnit, n: u32) {
        let map = TempoMap::for_document(d, super::super::TEMPO_RATE);
        let m = MeterMap::new(Division::ppqn(480), &[]);
        let c = RepeatClock {
            meter: &m,
            tempo: Some(&map),
            ppq: 480,
            shift: 0,
        };
        let s = spec(n, Spacing::Even, every, unit);
        let p = plan(&s, src(anchor, anchor + 10, None), &c).unwrap();
        let us = s
            .every
            .exact_times(
                match unit {
                    RepeatUnit::Seconds => 1_000_000,
                    RepeatUnit::Milliseconds => 1_000,
                    _ => 1,
                },
                1,
            )
            .unwrap();
        let a = map.fx_at_tick(anchor);
        let mut prev = 0;
        for (i, &off) in p.offsets.iter().enumerate() {
            let target = a + (i as u128 + 1) * us * (1u128 << FX_SHIFT);
            let t = anchor + off;
            assert!(map.fx_at_tick(t) <= target, "copy {} at {t}", i + 1);
            assert!(
                map.fx_at_tick(t + 1) > target,
                "copy {} at {t}: not the last tick",
                i + 1
            );
            assert!(off > prev);
            prev = off;
        }
    }

    #[test]
    fn physical_units_are_real_time_under_steps_ramps_and_many_points() {
        // Constant 120 BPM: one second = 960 ticks exactly.
        let d = tempo_doc(&[]);
        check_physical(&d, 0, "1", RepeatUnit::Seconds, 8);
        let map = TempoMap::for_document(&d, super::super::TEMPO_RATE);
        let m = MeterMap::new(Division::ppqn(480), &[]);
        let c = RepeatClock {
            meter: &m,
            tempo: Some(&map),
            ppq: 480,
            shift: 0,
        };
        let p = plan(
            &spec(3, Spacing::Even, "1", RepeatUnit::Seconds),
            src(0, 5, None),
            &c,
        )
        .unwrap();
        assert_eq!(
            (p.offsets.clone(), p.period),
            (vec![960, 1920, 2880], Some(960))
        );
        // The 120 BPM fallback gives the same.
        let f = RepeatClock { tempo: None, ..c };
        assert_eq!(
            plan(
                &spec(3, Spacing::Even, "1", RepeatUnit::Seconds),
                src(0, 5, None),
                &f
            )
            .unwrap()
            .offsets,
            vec![960, 1920, 2880]
        );
        // A step to 60 BPM at tick 1920 and a ramp 60 → 180 from 3840 to 9600.
        let d = tempo_doc(&[
            (1920, "60", Interp::Step),
            (3840, "60", Interp::Linear),
            (9600, "180", Interp::Step),
        ]);
        let map = TempoMap::for_document(&d, super::super::TEMPO_RATE);
        let c = RepeatClock {
            tempo: Some(&map),
            ..c
        };
        let p = plan(
            &spec(6, Spacing::Even, "1", RepeatUnit::Seconds),
            src(0, 5, None),
            &c,
        )
        .unwrap();
        // 2 s at 120 BPM (960 / s), then 60 BPM (480 / s), then faster.
        assert_eq!(&p.offsets[..4], &[960, 1920, 2400, 2880]);
        assert_eq!(p.period, None, "different tick distances per copy");
        for (anchor, every, unit) in [
            (0, "1", RepeatUnit::Seconds),
            (1000, "0.25", RepeatUnit::Seconds),
            (3000, "333", RepeatUnit::Milliseconds),
            (3841, "12345", RepeatUnit::Microseconds),
            (5000, "0.00777", RepeatUnit::Seconds),
        ] {
            check_physical(&d, anchor, every, unit, 40);
        }
        // 10,000 tempo points (a stepped curve between 60 and 300 BPM).
        let pts: Vec<(u64, String, Interp)> = (0..10_000u64)
            .map(|i| {
                let bpm = 60 + (i * 7919 % 241);
                (
                    i * 37 + 1,
                    bpm.to_string(),
                    if i % 3 == 0 {
                        Interp::Linear
                    } else {
                        Interp::Step
                    },
                )
            })
            .collect();
        let refs: Vec<(u64, &str, Interp)> =
            pts.iter().map(|(t, b, i)| (*t, b.as_str(), *i)).collect();
        let d = tempo_doc(&refs);
        check_physical(&d, 17, "0.05", RepeatUnit::Seconds, 2000);
        check_physical(&d, 200_000, "7", RepeatUnit::Milliseconds, 2000);
    }

    #[test]
    fn physical_spacing_shorter_than_a_tick_is_refused() {
        let d = tempo_doc(&[]);
        let map = TempoMap::for_document(&d, super::super::TEMPO_RATE);
        let m = MeterMap::new(Division::ppqn(480), &[]);
        let c = RepeatClock {
            meter: &m,
            tempo: Some(&map),
            ppq: 480,
            shift: 0,
        };
        // One tick at 120 BPM / 480 PPQN is 1,041.67 µs.
        let e = plan(
            &spec(3, Spacing::Even, "500", RepeatUnit::Microseconds),
            src(0, 5, None),
            &c,
        )
        .unwrap_err();
        assert!(
            e.starts_with("Copy 1 would start on the same tick as the original"),
            "{e}"
        );
        assert!(
            plan(
                &spec(3, Spacing::Even, "1.0000001", RepeatUnit::Seconds),
                src(0, 5, None),
                &c
            )
            .unwrap_err()
            .contains("whole number of microseconds")
        );
        assert!(
            plan(
                &spec(3, Spacing::Even, "1.5", RepeatUnit::Microseconds),
                src(0, 5, None),
                &c
            )
            .unwrap_err()
            .contains("whole number of microseconds")
        );
        // A selection before the song start in a clip session.
        let s = RepeatClock { shift: -100, ..c };
        assert!(
            plan(
                &spec(1, Spacing::Even, "1", RepeatUnit::Seconds),
                src(50, 60, None),
                &s
            )
            .unwrap_err()
            .contains("before the song's start")
        );
    }
}
