//! `metric_position`: where a tick lies in the meter — its bar, the bar's
//! start and length, its beat, and its slot on a grid inside the bar — for
//! the functions that give every position in the bar its own value
//! (metric gates, unit maps, strum directions, metric profiles, per-bar
//! seeds). Bars follow every time-signature change ([`MeterMap`]); without
//! musical timing (SMPTE, plain ticks) the meter map's navigation step
//! stands in for the bar and it has one beat.

use crate::view::grid::MeterMap;

/// The metric place of one tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metric {
    /// Bar index from the song start (0 = the first bar).
    pub bar: u64,
    pub bar_start: u64,
    /// Ticks of this bar (a bar cut short by a signature change is shorter).
    pub bar_len: u64,
    /// Beat in the bar (0 = the downbeat); a beat is one 1 / denominator note.
    pub beat: u64,
    pub beat_start: u64,
    pub beat_len: u64,
}

impl Metric {
    /// The offset of `tick` (a tick of this bar) from the bar start.
    pub fn offset(&self, tick: u64) -> u64 {
        tick.saturating_sub(self.bar_start)
    }

    /// The slot of `tick` on a grid of `step` ticks restarting at the bar
    /// line, and the bar's number of slots (the last may be partial):
    /// `(slot, slots)`, `slot < slots`.
    pub fn slot(&self, tick: u64, step: u64) -> (u64, u64) {
        let step = step.max(1);
        let slots = self.bar_len.div_ceil(step).max(1);
        ((self.offset(tick) / step).min(slots - 1), slots)
    }

    /// The bar's position along the bar, 0 at the downbeat … < 1 (for lists
    /// and curves drawn over the bar).
    pub fn phase(&self, tick: u64) -> f64 {
        self.offset(tick) as f64 / self.bar_len.max(1) as f64
    }
}

/// The bar holding `tick`: its start and length — [`metric_position`]'s
/// `bar_start` / `bar_len` without the bar number and beat lookups, for
/// per-note rules that only need the bar (V09-S36: Humanize's rubato, swing,
/// grid amounts and accents). O(log signature changes).
pub fn bar_bounds(meter: &MeterMap, tick: u64) -> (u64, u64) {
    let bar_start = meter.bar_start_at_or_before(tick);
    let bar_end = meter
        .next_bar(bar_start)
        .unwrap_or(u64::MAX)
        .max(bar_start.saturating_add(1));
    (bar_start, bar_end - bar_start)
}

/// The bar holding `tick`: its index from the song start (as
/// [`Metric::bar`]), start and length — [`metric_position`]'s bar fields
/// without the beat lookups, for per-note rules that count bars (V09-S36:
/// Transpose's root list per bar and Shepard band). O(log signature
/// changes).
pub fn bar_place(meter: &MeterMap, tick: u64) -> (u64, u64, u64) {
    let (bar_start, bar_len) = bar_bounds(meter, tick);
    let bar = match meter.bar_beat(tick) {
        Some((m, _, _)) => m - 1,
        None => bar_start / bar_len,
    };
    (bar, bar_start, bar_len)
}

/// The metric place of `tick`. O(log signature changes).
pub fn metric_position(meter: &MeterMap, tick: u64) -> Metric {
    let (bar_start, bar_len) = bar_bounds(meter, tick);
    let bar_end = bar_start + bar_len;
    let bar = match meter.measure_at(tick) {
        Some((m, _)) => m - 1,
        None => bar_start / bar_len,
    };
    let (beat, beat_start, beat_len) = match meter.beat_at(tick) {
        Some((index, off)) => {
            let start = tick - off;
            let next = meter
                .beat_start(index + 1)
                .unwrap_or(bar_end)
                .clamp(start + 1, bar_end);
            let first = meter.beat_at(bar_start).map_or(index, |(first, _)| first);
            (index - first, start, next - start)
        }
        None => (0, bar_start, bar_len),
    };
    Metric {
        bar,
        bar_start,
        bar_len,
        beat,
        beat_start,
        beat_len,
    }
}

/// Region boundaries for per-region values (seed lattices, cells): the
/// index of the region holding `tick` when regions are `n` bars long
/// (bar-aligned from the song start), and the region's start.
pub fn bars_region(meter: &MeterMap, tick: u64, n: u64) -> (u64, u64) {
    let m = metric_position(meter, tick);
    let region = m.bar / n.max(1);
    let start = match meter.measure_start(region * n.max(1) + 1) {
        Some(s) => s,
        None => region * n.max(1) * m.bar_len,
    };
    (region, start.min(tick))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::timing::Division;

    /// 4/4 for two bars, 7/8 for three, then 3/4 (960 PPQN).
    fn meter() -> MeterMap {
        MeterMap::new(
            Division::ppqn(960),
            &[(0, 4, 2), (7680, 7, 3), (7680 + 3 * 3360, 3, 2)],
        )
    }

    /// Brute force: walk the bars from tick 0 with their signatures.
    fn brute(tick: u64) -> (u64, u64, u64, u64, u64) {
        let sigs = [
            (0u64, 3840u64, 960u64),
            (7680, 3360, 480),
            (17_760, 2880, 960),
        ];
        let mut bar = 0;
        let mut start = 0;
        loop {
            let (_, len, beat) = *sigs.iter().rev().find(|s| s.0 <= start).unwrap();
            if tick < start + len {
                let b = (tick - start) / beat;
                return (bar, start, len, b, start + b * beat);
            }
            start += len;
            bar += 1;
        }
    }

    #[test]
    fn bars_beats_and_slots_follow_signature_changes() {
        let m = meter();
        for tick in (0..30_000)
            .step_by(37)
            .chain([0, 3839, 3840, 7679, 7680, 17_759, 17_760])
        {
            let p = metric_position(&m, tick);
            assert_eq!(bar_bounds(&m, tick), (p.bar_start, p.bar_len));
            assert_eq!(bar_place(&m, tick), (p.bar, p.bar_start, p.bar_len));
            let (bar, start, len, beat, beat_start) = brute(tick);
            assert_eq!(
                (p.bar, p.bar_start, p.bar_len, p.beat, p.beat_start),
                (bar, start, len, beat, beat_start),
                "tick {tick}"
            );
            assert!(p.beat_start <= tick && tick < p.beat_start + p.beat_len);
            let (slot, slots) = p.slot(tick, 240);
            assert_eq!(slot, (tick - start) / 240);
            assert_eq!(slots, len.div_ceil(240));
            assert!((0.0..1.0).contains(&p.phase(tick)));
        }
        // 7/8: 3,360-tick bars of seven 480-tick beats.
        let p = metric_position(&m, 7680 + 3360 + 2000);
        assert_eq!((p.bar, p.bar_len, p.beat, p.beat_len), (3, 3360, 4, 480));
        // Regions of two bars: bars 0-1, 2-3, 4-5 …
        assert_eq!(bars_region(&m, 100, 2), (0, 0));
        assert_eq!(bars_region(&m, 7680 + 3360 + 5, 2), (1, 7680));
        assert_eq!(
            bars_region(&m, 7680 + 2 * 3360 + 5, 2),
            (2, 7680 + 2 * 3360)
        );
    }

    #[test]
    fn plain_tick_timing_has_one_beat_bars() {
        let m = MeterMap::new(Division::ppqn(0), &[]);
        let p = metric_position(&m, 2500);
        assert_eq!(p.beat, 0);
        assert!(p.bar_start <= 2500 && 2500 < p.bar_start + p.bar_len);
        assert_eq!(p.bar, p.bar_start / p.bar_len);
        assert_eq!(bar_place(&m, 2500), (p.bar, p.bar_start, p.bar_len));
        assert_eq!(p.slot(2500, 1), (p.offset(2500), p.bar_len));
    }
}
