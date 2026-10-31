//! Wall-clock time of ticks, for Playing Time and time ↔ position conversion.
//!
//! [`TimeMap`] is the playback [`TempoMap`] evaluated at one "frame" per
//! nanosecond, so it follows the Conductor exactly — step changes, linear BPM
//! ramps, SMPTE divisions and Format 2 sequences — and shares the tested
//! forward and inverse functions with playback. Building it is O(tempo points);
//! each query is a binary search over the segments, never a scan of notes.
//!
//! Rounding policy: a tick's time is the floor of its exact start time in
//! nanoseconds. A time converts to **the last tick that has started by that
//! time** (floor), so conversion is monotonic, and converting a tick's time back
//! gives the same tick whenever ticks are at least a nanosecond apart (any tempo
//! below 60 × 10⁹ / PPQN BPM). Displayed times are rounded to the millisecond.

use crate::model::PlumDocument;
use crate::model::tempo::{Speed, TempoMap};

/// Nanoseconds per second (the map's "sample rate").
pub const NANOS_PER_SECOND: u32 = 1_000_000_000;

/// Whether a duration is musical time (the tempo map) or audition time (the
/// tempo map at the current playback speed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeBasis {
    Musical,
    Audition(Speed),
}

impl TimeBasis {
    pub fn speed(self) -> Speed {
        match self {
            TimeBasis::Musical => Speed::NORMAL,
            TimeBasis::Audition(s) => s,
        }
    }

    pub fn label(self) -> String {
        match self {
            TimeBasis::Musical => "Musical time (tempo map)".into(),
            TimeBasis::Audition(s) if s.is_normal() => "Playback speed 1x (same as musical)".into(),
            TimeBasis::Audition(s) => format!("At playback speed {s}"),
        }
    }
}

/// Tick ↔ time for one document at one basis.
#[derive(Clone, Debug)]
pub struct TimeMap {
    map: TempoMap,
    basis: TimeBasis,
}

impl TimeMap {
    pub fn for_document(doc: &PlumDocument, basis: TimeBasis) -> Self {
        TimeMap {
            map: TempoMap::for_document_at(doc, NANOS_PER_SECOND, basis.speed()),
            basis,
        }
    }

    pub fn basis(&self) -> TimeBasis {
        self.basis
    }

    /// Nanoseconds from the start to `tick` (floor, saturating).
    pub fn nanos_at(&self, tick: u64) -> u64 {
        self.map.frame_at_tick(tick)
    }

    /// Duration of `[t0, t1)` in nanoseconds (0 when `t1 <= t0`).
    pub fn span_nanos(&self, t0: u64, t1: u64) -> u64 {
        if t1 <= t0 {
            return 0;
        }
        self.nanos_at(t1).saturating_sub(self.nanos_at(t0))
    }

    /// The last tick that has started by `nanos` (see the module's rounding
    /// policy). Monotonic in `nanos`.
    pub fn tick_at_nanos(&self, nanos: u64) -> u64 {
        self.map.tick_at_frame(nanos)
    }
}

/// `m:ss.mmm`, or `h:mm:ss.mmm` from one hour (rounded half up to the
/// millisecond).
pub fn format_duration(nanos: u64) -> String {
    let ms = (u128::from(nanos) + 500_000) / 1_000_000;
    let (s, ms) = (ms / 1000, ms % 1000);
    let (m, s) = (s / 60, s % 60);
    if m >= 60 {
        let (h, m) = (m / 60, m % 60);
        format!("{h}:{m:02}:{s:02}.{ms:03}")
    } else {
        format!("{m}:{s:02}.{ms:03}")
    }
}

/// Parses a decimal number of seconds ("90", "12.5", up to 9 decimals) into
/// nanoseconds.
fn parse_seconds(s: &str) -> Result<u128, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("Enter a number of seconds.".into());
    }
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, f),
        None => (s, ""),
    };
    let digits = |x: &str| x.bytes().all(|b| b.is_ascii_digit());
    if !digits(int) || !digits(frac) || (int.is_empty() && frac.is_empty()) {
        return Err(format!(
            "“{s}” is not a number of seconds (use digits, e.g. 12.5)."
        ));
    }
    if frac.len() > 9 {
        return Err("Seconds can have at most 9 decimals (nanoseconds).".into());
    }
    if int.len() > 18 {
        return Err("That time is too large.".into());
    }
    let whole: u128 = if int.is_empty() {
        0
    } else {
        int.parse().map_err(|_| "That time is too large.")?
    };
    let mut f: u128 = if frac.is_empty() {
        0
    } else {
        frac.parse().map_err(|_| "Invalid decimals.")?
    };
    for _ in frac.len()..9 {
        f *= 10;
    }
    Ok(whole * 1_000_000_000 + f)
}

fn to_nanos(v: u128) -> Result<u64, String> {
    u64::try_from(v).map_err(|_| "That time is too large (the limit is about 584 years).".into())
}

/// Minutes and seconds fields (either may be empty = 0; seconds may be decimal
/// and may exceed 59) into nanoseconds.
pub fn parse_minutes_seconds(minutes: &str, seconds: &str) -> Result<u64, String> {
    let m = minutes.trim();
    let mins: u128 = if m.is_empty() {
        0
    } else if m.bytes().all(|b| b.is_ascii_digit()) && m.len() <= 16 {
        m.parse().map_err(|_| "The minutes are too large.")?
    } else {
        return Err(format!("“{m}” is not a whole number of minutes."));
    };
    let secs = if seconds.trim().is_empty() {
        if m.is_empty() {
            return Err("Enter minutes and/or seconds.".into());
        }
        0
    } else {
        parse_seconds(seconds)?
    };
    to_nanos(mins * 60_000_000_000 + secs)
}

/// A time written as `s`, `m:ss` or `h:mm:ss` (seconds may be decimal; fields
/// after the first must be below 60) into nanoseconds.
pub fn parse_time(input: &str) -> Result<u64, String> {
    let s = input.trim();
    let parts: Vec<&str> = s.split(':').collect();
    let hint = "Use seconds, m:ss or h:mm:ss (e.g. 1:30.5).";
    let whole = |p: &str| -> Result<u128, String> {
        if !p.is_empty() && p.len() <= 16 && p.bytes().all(|b| b.is_ascii_digit()) {
            p.parse().map_err(|_| "That time is too large.".to_string())
        } else {
            Err(format!("“{s}” is not a time. {hint}"))
        }
    };
    let below_60 = |ns: u128| -> Result<u128, String> {
        if ns < 60_000_000_000 {
            Ok(ns)
        } else {
            Err(format!(
                "“{s}”: seconds and minutes after a colon must be below 60. {hint}"
            ))
        }
    };
    let v = match parts.as_slice() {
        [sec] => parse_seconds(sec).map_err(|e| format!("{e} {hint}"))?,
        [m, sec] => {
            whole(m)? * 60_000_000_000
                + below_60(parse_seconds(sec).map_err(|_| format!("“{s}” is not a time. {hint}"))?)?
        }
        [h, m, sec] => {
            let mm = whole(m)?;
            if mm >= 60 {
                return Err(format!(
                    "“{s}”: minutes after a colon must be below 60. {hint}"
                ));
            }
            whole(h)? * 3_600_000_000_000
                + mm * 60_000_000_000
                + below_60(parse_seconds(sec).map_err(|_| format!("“{s}” is not a time. {hint}"))?)?
        }
        _ => return Err(format!("“{s}” is not a time. {hint}")),
    };
    to_nanos(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{History, Operation};
    use crate::model::Bpm;
    use crate::model::conductor::Interp;

    fn doc(bpm: &str) -> PlumDocument {
        PlumDocument::new_project("t".into(), Bpm::parse(bpm).unwrap(), 480)
    }

    #[test]
    fn constant_tempo_times_are_exact_and_invert() {
        let d = doc("120");
        let m = TimeMap::for_document(&d, TimeBasis::Musical);
        // 120 BPM, 480 PPQN: one minute = 57,600 ticks.
        assert_eq!(m.nanos_at(57_600), 60_000_000_000);
        assert_eq!(m.tick_at_nanos(60_000_000_000), 57_600);
        assert_eq!(m.tick_at_nanos(60_000_000_000 - 1), 57_599);
        assert_eq!(format_duration(m.nanos_at(57_600)), "1:00.000");
        for t in [0u64, 1, 479, 480, 1_000_000, 123_456_789_012] {
            assert_eq!(m.tick_at_nanos(m.nanos_at(t)), t);
        }
    }

    #[test]
    fn playback_speed_changes_audition_time_only() {
        let d = doc("120");
        let musical = TimeMap::for_document(&d, TimeBasis::Musical);
        let fast = TimeMap::for_document(&d, TimeBasis::Audition(Speed::parse("2").unwrap()));
        let slow = TimeMap::for_document(&d, TimeBasis::Audition(Speed::parse("0.5").unwrap()));
        assert_eq!(musical.nanos_at(57_600), 60_000_000_000);
        assert_eq!(fast.nanos_at(57_600), 30_000_000_000);
        assert_eq!(slow.nanos_at(57_600), 120_000_000_000);
        assert_eq!(fast.tick_at_nanos(30_000_000_000), 57_600);
    }

    #[test]
    fn tempo_changes_and_ramps_follow_the_conductor() {
        let mut d = doc("120");
        // 60 BPM from tick 960, ramp 60 → 120 from 1920 to 2880.
        let bpm = |s: &str| crate::model::tempo::TempoValue::Bpm(Bpm::parse(s).unwrap());
        let edit = d
            .conductor()
            .prepare_tempo_points(
                &[
                    (960, bpm("60"), Interp::Step),
                    (1920, bpm("60"), Interp::Linear),
                    (2880, bpm("120"), Interp::Step),
                ],
                false,
            )
            .unwrap();
        let mut h = History::default();
        h.apply(
            &mut d,
            Operation::Conductor {
                edit,
                label: "Tempo",
            },
        )
        .unwrap();
        let m = TimeMap::for_document(&d, TimeBasis::Musical);
        assert_eq!(m.nanos_at(960), 1_000_000_000); // 2 beats at 120
        assert_eq!(m.nanos_at(1920), 3_000_000_000); // + 2 beats at 60
        // Ramp 60→120 over 2 beats: 2 · ln(2) seconds ≈ 1.386 s.
        let ramp = m.span_nanos(1920, 2880) as f64 / 1e9;
        assert!((ramp - 2.0 * 2f64.ln()).abs() < 1e-6, "{ramp}");
        // Monotonic and inverse inside the ramp.
        let mut last = 0;
        for t in (1920..=2880).step_by(7) {
            let ns = m.nanos_at(t);
            assert!(ns >= last);
            last = ns;
            assert_eq!(m.tick_at_nanos(ns), t);
        }
        for ns in (0..6_000_000_000u64).step_by(9_999_991) {
            let t = m.tick_at_nanos(ns);
            assert!(m.nanos_at(t) <= ns && m.nanos_at(t + 1) > ns);
        }
    }

    #[test]
    fn huge_positions_and_extreme_tempos_do_not_overflow() {
        // 0.001 BPM at 480 PPQN: one tick is 125,000 s, so ~147,000 ticks fill
        // the 584-year range; 1,000,000 BPM: a tick is 125 ns.
        for (bpm, t) in [("0.001", 100_000u64), ("1000000", 1u64 << 40)] {
            let d = doc(bpm);
            let m = TimeMap::for_document(&d, TimeBasis::Musical);
            let ns = m.nanos_at(t);
            assert!(ns > 0 && ns < u64::MAX);
            assert_eq!(m.tick_at_nanos(ns), t);
            assert_eq!(m.tick_at_nanos(0), 0);
        }
        // Beyond the range the time saturates instead of wrapping.
        let slow = TimeMap::for_document(&doc("0.001"), TimeBasis::Musical);
        assert_eq!(slow.nanos_at(u64::MAX / 2), u64::MAX);
        assert_eq!(format_duration(u64::MAX), "5124095:34:33.710");
    }

    #[test]
    fn durations_format_and_parse() {
        assert_eq!(format_duration(0), "0:00.000");
        assert_eq!(format_duration(1_499_999), "0:00.001");
        assert_eq!(format_duration(61_234_500_000), "1:01.235");
        assert_eq!(format_duration(3_600_000_000_000), "1:00:00.000");
        assert_eq!(format_duration(90_061_000_000_000), "25:01:01.000");
        assert_eq!(parse_time("90"), Ok(90_000_000_000));
        assert_eq!(parse_time("1:30.5"), Ok(90_500_000_000));
        assert_eq!(parse_time("1:00:00"), Ok(3_600_000_000_000));
        assert_eq!(parse_time(".25"), Ok(250_000_000));
        for bad in [
            "",
            "1:60",
            "1:2:3:4",
            "-1",
            "1:-5",
            "abc",
            "1:30:",
            "0.1234567890",
            "1:60:00",
        ] {
            assert!(parse_time(bad).is_err(), "{bad}");
        }
        assert_eq!(parse_minutes_seconds("2", "5.5"), Ok(125_500_000_000));
        assert_eq!(parse_minutes_seconds("", "90"), Ok(90_000_000_000));
        assert_eq!(parse_minutes_seconds("3", ""), Ok(180_000_000_000));
        assert!(parse_minutes_seconds("", "").is_err());
        assert!(parse_minutes_seconds("1.5", "0").is_err());
        assert!(parse_minutes_seconds("9999999999999999", "0").is_err());
    }
}
