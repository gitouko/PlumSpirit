//! `phrase_detect`: phrases of a time-ordered note list — split where a
//! rest longer than a threshold separates two onsets (silence measured from
//! the latest end so far, so overlapping notes never make a rest), and
//! optionally at every bar line — plus a phrase's contour slope. Flat
//! bounds into the index list, no allocation per phrase.

use super::super::TNote;
use crate::view::grid::MeterMap;

/// Phrases of a time-ordered index list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Phrases {
    /// Start of each phrase in the index list, then its length.
    bounds: Vec<usize>,
}

impl Phrases {
    /// Splits `idx` (indices into `notes`, sorted by start) before a note
    /// that starts more than `rest` ticks after every earlier note of the
    /// phrase has ended, and — with `bars` — before the first note of each
    /// new bar.
    pub fn of(notes: &[TNote], idx: &[usize], rest: u64, bars: Option<&MeterMap>) -> Phrases {
        let mut bounds = Vec::new();
        let mut sounding_until = 0u64;
        let mut prev_start = None;
        for (k, &i) in idx.iter().enumerate() {
            let n = &notes[i];
            let split = match prev_start {
                None => true,
                Some(p) => {
                    n.start.saturating_sub(sounding_until) > rest
                        || bars.is_some_and(|m| m.bar_start_at_or_before(n.start) > p)
                }
            };
            if split {
                bounds.push(k);
                sounding_until = 0;
            }
            sounding_until = sounding_until.max(n.end());
            prev_start = Some(n.start);
        }
        bounds.push(idx.len());
        Phrases { bounds }
    }

    /// [`Phrases::of`] with each note's own threshold: the rest before note
    /// `idx[k]` splits when it is longer than `rest(idx[k])` (a Control
    /// Sweep of the rest, E0422).
    pub fn of_by(
        notes: &[TNote],
        idx: &[usize],
        rest: &dyn Fn(usize) -> u64,
        bars: Option<&MeterMap>,
    ) -> Phrases {
        let mut bounds = Vec::new();
        let mut sounding_until = 0u64;
        let mut prev_start = None;
        for (k, &i) in idx.iter().enumerate() {
            let n = &notes[i];
            let split = match prev_start {
                None => true,
                Some(p) => {
                    n.start.saturating_sub(sounding_until) > rest(i)
                        || bars.is_some_and(|m| m.bar_start_at_or_before(n.start) > p)
                }
            };
            if split {
                bounds.push(k);
                sounding_until = 0;
            }
            sounding_until = sounding_until.max(n.end());
            prev_start = Some(n.start);
        }
        bounds.push(idx.len());
        Phrases { bounds }
    }

    /// [`Phrases::of`] without bar lines, with each note's own threshold:
    /// the rest before note `idx[k]` splits when it is longer than
    /// `rest(idx[k])` (V09-S38: Paint V2's swept phrase rest).
    pub fn of_with(notes: &[TNote], idx: &[usize], rest: &dyn Fn(usize) -> u64) -> Phrases {
        let mut bounds = Vec::new();
        let mut sounding_until = 0u64;
        for (k, &i) in idx.iter().enumerate() {
            let n = &notes[i];
            if k == 0 || n.start.saturating_sub(sounding_until) > rest(i) {
                bounds.push(k);
                sounding_until = 0;
            }
            sounding_until = sounding_until.max(n.end());
        }
        bounds.push(idx.len());
        Phrases { bounds }
    }

    pub fn len(&self) -> usize {
        self.bounds.len() - 1
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Phrase `k` as a slice of `idx` (the list it was built from).
    pub fn get<'i>(&self, idx: &'i [usize], k: usize) -> &'i [usize] {
        &idx[self.bounds[k]..self.bounds[k + 1]]
    }

    /// The phrases as slices of `idx`, in time order.
    pub fn iter<'i>(&'i self, idx: &'i [usize]) -> impl Iterator<Item = &'i [usize]> + 'i {
        (0..self.len()).map(move |k| self.get(idx, k))
    }
}

/// The least-squares slope of pitch over start time in semitones per tick
/// (0 for fewer than two notes or a phrase without a time span).
pub fn contour_slope(notes: &[TNote], phrase: &[usize]) -> f64 {
    let n = phrase.len() as f64;
    if phrase.len() < 2 {
        return 0.0;
    }
    let t0 = notes[phrase[0]].start as f64;
    let (mut sx, mut sy) = (0.0, 0.0);
    for &i in phrase {
        sx += notes[i].start as f64 - t0;
        sy += f64::from(notes[i].pitch);
    }
    let (mx, my) = (sx / n, sy / n);
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for &i in phrase {
        let dx = notes[i].start as f64 - t0 - mx;
        sxy += dx * (f64::from(notes[i].pitch) - my);
        sxx += dx * dx;
    }
    if sxx <= 0.0 { 0.0 } else { sxy / sxx }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::timing::Division;

    fn note(start: u64, dur: u64, pitch: u8) -> TNote {
        TNote {
            start,
            dur,
            pitch,
            vel: 100,
            ..Default::default()
        }
    }

    #[test]
    fn phrases_split_at_rests_measured_from_the_latest_end_and_at_bars() {
        // A long note covers the gap between 100 and 1000: no rest there.
        let notes = vec![
            note(0, 2000, 60),
            note(100, 50, 62),
            note(1000, 100, 64),
            note(2500, 100, 65), // 500 after the long note ends
            note(2700, 100, 67), // 100 after
            note(3900, 100, 69), // bar 2 (3,840) starts before it
        ];
        let idx: Vec<usize> = (0..notes.len()).collect();
        let p = Phrases::of(&notes, &idx, 300, None);
        let got: Vec<Vec<usize>> = p.iter(&idx).map(<[usize]>::to_vec).collect();
        assert_eq!(got, vec![vec![0, 1, 2], vec![3, 4], vec![5]]);
        let p = Phrases::of(&notes, &idx, 600, None);
        assert_eq!(p.len(), 2); // 3,900 − 2,800 = 1,100 > 600 still splits
        let meter = MeterMap::new(Division::ppqn(960), &[(0, 4, 2)]);
        let p = Phrases::of(&notes, &idx, 5000, Some(&meter));
        let got: Vec<Vec<usize>> = p.iter(&idx).map(<[usize]>::to_vec).collect();
        assert_eq!(got, vec![vec![0, 1, 2, 3, 4], vec![5]]);
        assert!(Phrases::of(&notes, &[], 0, None).is_empty());
    }

    #[test]
    fn contour_slopes_are_least_squares_fits() {
        let rising: Vec<TNote> = (0..8)
            .map(|k| note(k * 240, 100, 60 + 2 * k as u8))
            .collect();
        let idx: Vec<usize> = (0..8).collect();
        assert!((contour_slope(&rising, &idx) - 2.0 / 240.0).abs() < 1e-12);
        let flat = vec![note(0, 10, 60), note(0, 10, 72)];
        assert_eq!(contour_slope(&flat, &[0, 1]), 0.0);
        assert_eq!(contour_slope(&rising, &[3]), 0.0);
        let falling: Vec<TNote> = (0..5).map(|k| note(k * 100, 10, 80 - k as u8)).collect();
        assert!((contour_slope(&falling, &[0, 1, 2, 3, 4]) + 0.01).abs() < 1e-12);
    }
}
