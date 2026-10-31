//! `scale_lattice`: steps on a set of pitch classes — chromatic, a key and
//! scale, its pentatonic, a chord's tones or a single pitch class (octaves)
//! — for passing tones, glides, glissandos, scale-degree scatter and
//! chord-tone generation. A lattice is a 12-bit pitch-class mask; every step
//! is O(1) (at most 11 semitones are inspected). Pitches are `i64`: callers
//! clamp to the project's keys.

use super::super::theory::Scale;

/// Pitch classes to step on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lattice {
    /// Bit `pc` set = pitch class `pc` (0 = C) belongs to the lattice; never 0.
    mask: u16,
}

impl Lattice {
    /// Every semitone.
    pub fn chromatic() -> Lattice {
        Lattice { mask: 0xFFF }
    }

    /// From a pitch-class mask (an empty mask is chromatic).
    pub fn from_mask(mask: u16) -> Lattice {
        let mask = mask & 0xFFF;
        Lattice {
            mask: if mask == 0 { 0xFFF } else { mask },
        }
    }

    /// The scale's notes.
    pub fn of_scale(scale: &Scale) -> Lattice {
        Lattice::from_mask(scale.mask())
    }

    /// The scale's pentatonic: of a seven-note scale with a major third
    /// degrees 1 2 3 5 6, with a minor third degrees 1 3 4 5 7 (C major →
    /// C D E G A, C minor → C E♭ F G B♭); any other scale as it is.
    pub fn pentatonic_of(scale: &Scale) -> Lattice {
        if scale.steps.len() != 7 {
            return Lattice::of_scale(scale);
        }
        let pick: [usize; 5] = if scale.steps.contains(&4) {
            [0, 1, 2, 4, 5]
        } else {
            [0, 2, 3, 4, 6]
        };
        let mask = pick
            .iter()
            .fold(0u16, |m, &d| m | 1 << ((scale.root + scale.steps[d]) % 12));
        Lattice::from_mask(mask)
    }

    /// The pitch classes of `pitches` (a chord's tones).
    pub fn of_pitches(pitches: impl IntoIterator<Item = i64>) -> Lattice {
        Lattice::from_mask(
            pitches
                .into_iter()
                .fold(0u16, |m, p| m | 1 << p.rem_euclid(12)),
        )
    }

    /// Octaves of `pitch` only.
    pub fn octaves_of(pitch: i64) -> Lattice {
        Lattice::from_mask(1 << pitch.rem_euclid(12))
    }

    pub fn mask(self) -> u16 {
        self.mask
    }

    #[inline]
    pub fn contains(self, pitch: i64) -> bool {
        self.mask & (1 << pitch.rem_euclid(12)) != 0
    }

    /// The nearest lattice pitch strictly above `pitch`.
    pub fn above(self, pitch: i64) -> i64 {
        (1..=12)
            .map(|d| pitch + d)
            .find(|&p| self.contains(p))
            .unwrap_or(pitch + 12)
    }

    /// The nearest lattice pitch strictly below `pitch`.
    pub fn below(self, pitch: i64) -> i64 {
        (1..=12)
            .map(|d| pitch - d)
            .find(|&p| self.contains(p))
            .unwrap_or(pitch - 12)
    }

    /// `pitch` itself when on the lattice, else the nearer neighbour (the
    /// lower one on a tie).
    pub fn nearest(self, pitch: i64) -> i64 {
        if self.contains(pitch) {
            return pitch;
        }
        let (up, down) = (self.above(pitch), self.below(pitch));
        if up - pitch < pitch - down { up } else { down }
    }

    /// `k` lattice steps from `pitch` (up for `k > 0`): from a pitch off the
    /// lattice the first step lands on the neighbour in that direction; `k
    /// = 0` gives [`Lattice::nearest`]. O(|k| mod n + 1).
    pub fn step(self, pitch: i64, k: i64) -> i64 {
        if k == 0 {
            return self.nearest(pitch);
        }
        let n = i64::from(self.mask.count_ones());
        // Whole octaves at once, then the remainder one step at a time.
        let (octaves, rest) = (k / n, k % n);
        let mut p = pitch + 12 * octaves;
        if rest == 0 && !self.contains(p) {
            // A whole number of octaves from an off-lattice pitch: one
            // octave less, then the remaining n steps land on the lattice.
            p -= 12 * k.signum();
            for _ in 0..n {
                p = if k > 0 { self.above(p) } else { self.below(p) };
            }
            return p;
        }
        for _ in 0..rest.abs() {
            p = if rest > 0 {
                self.above(p)
            } else {
                self.below(p)
            };
        }
        p
    }

    /// The lattice pitches strictly between `a` and `b`, from `a` toward
    /// `b` (empty when they are neighbours or equal).
    pub fn between(self, a: i64, b: i64) -> impl Iterator<Item = i64> {
        let up = b > a;
        let mut p = a;
        std::iter::from_fn(move || {
            p = if up { self.above(p) } else { self.below(p) };
            let inside = if up { p < b } else { p > b };
            inside.then_some(p)
        })
    }

    /// The number of lattice steps from `a` up to `b` (negative when `b` is
    /// lower): pitches of the lattice in `(a, b]`, counted. O(octaves).
    pub fn steps_between(self, a: i64, b: i64) -> i64 {
        let count = |lo: i64, hi: i64| -> i64 {
            // Lattice pitches in (lo, hi].
            let full = (hi - lo).div_euclid(12);
            let n = i64::from(self.mask.count_ones());
            let rem = (lo + 12 * full + 1..=hi)
                .filter(|&p| self.contains(p))
                .count() as i64;
            full * n + rem
        };
        if b >= a { count(a, b) } else { -count(b, a) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scale(root: usize, name: &str) -> Scale {
        let i = super::super::super::theory::SCALES
            .iter()
            .position(|s| s.0 == name)
            .unwrap_or_else(|| panic!("no scale {name}"));
        Scale::new(root, i)
    }

    /// Brute force: walk semitone by semitone.
    fn brute_step(l: Lattice, pitch: i64, k: i64) -> i64 {
        if k == 0 {
            return l.nearest(pitch);
        }
        let mut p = pitch;
        let mut left = k.abs();
        while left > 0 {
            p += k.signum();
            if l.contains(p) {
                left -= 1;
            }
        }
        p
    }

    #[test]
    fn steps_match_a_semitone_walk_on_every_lattice() {
        let lattices = [
            Lattice::chromatic(),
            Lattice::of_scale(&Scale::new(2, 0)),
            Lattice::of_pitches([60, 64, 67]),
            Lattice::octaves_of(61),
            Lattice::from_mask(0b1001_0010_0101),
        ];
        for l in lattices {
            for pitch in 40..80 {
                for k in -30..=30 {
                    assert_eq!(
                        l.step(pitch, k),
                        brute_step(l, pitch, k),
                        "{l:?} {pitch} {k}"
                    );
                }
                for b in 40..80 {
                    let between: Vec<i64> = l.between(pitch, b).collect();
                    let lo = pitch.min(b) + 1;
                    let mut want: Vec<i64> =
                        (lo..pitch.max(b)).filter(|&p| l.contains(p)).collect();
                    if b < pitch {
                        want.reverse();
                    }
                    assert_eq!(between, want, "{l:?} {pitch} → {b}");
                    let brute = (pitch.min(b) + 1..=pitch.max(b))
                        .filter(|&p| l.contains(p))
                        .count() as i64;
                    let want = if b >= pitch { brute } else { -brute };
                    assert_eq!(l.steps_between(pitch, b), want, "{l:?} {pitch} {b}");
                }
            }
        }
    }

    #[test]
    fn named_lattices_hold_the_expected_pitch_classes() {
        let pcs = |l: Lattice| -> Vec<i64> { (0..12).filter(|&p| l.contains(p)).collect() };
        assert_eq!(pcs(Lattice::chromatic()), (0..12).collect::<Vec<_>>());
        assert_eq!(pcs(Lattice::from_mask(0)), (0..12).collect::<Vec<_>>());
        assert_eq!(pcs(Lattice::octaves_of(-1)), vec![11]);
        assert_eq!(pcs(Lattice::of_pitches([62, 66, 69])), vec![2, 6, 9]);
        let c_major = scale(0, "Major (Ionian)");
        assert_eq!(pcs(Lattice::of_scale(&c_major)), vec![0, 2, 4, 5, 7, 9, 11]);
        assert_eq!(pcs(Lattice::pentatonic_of(&c_major)), vec![0, 2, 4, 7, 9]);
        let a_minor = scale(9, "Natural minor (Aeolian)");
        assert_eq!(pcs(Lattice::pentatonic_of(&a_minor)), vec![0, 2, 4, 7, 9]); // A C D E G
        let d_dorian = scale(2, "Dorian");
        assert_eq!(pcs(Lattice::pentatonic_of(&d_dorian)), vec![0, 2, 5, 7, 9]); // D F G A C
        assert_eq!(Lattice::chromatic().nearest(61), 61);
        assert_eq!(Lattice::of_scale(&c_major).nearest(61), 60); // tie → lower
        assert_eq!(Lattice::of_scale(&c_major).nearest(66), 65);
    }
}
