//! `chord_groups`: notes grouped into chords — onsets within a tolerance of
//! the group's first note, in a time-ordered index list (the FL tools'
//! chord rule) — and each voice's rank in its chord. One flat index list
//! with group bounds: no allocation per chord or per note.

use super::super::TNote;

/// Chords of a time-ordered index list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChordGroups {
    /// The note indices, grouped (the input order).
    idx: Vec<usize>,
    /// Start of each group in `idx`, then `idx.len()`.
    bounds: Vec<usize>,
}

impl ChordGroups {
    /// Groups `idx` (indices into `notes`, sorted by start): a note joins
    /// the current chord when it starts within `tol` ticks of the chord's
    /// first note, else it begins the next chord.
    pub fn of(notes: &[TNote], idx: &[usize], tol: u64) -> ChordGroups {
        let mut bounds = Vec::new();
        let mut first = None;
        for (k, &i) in idx.iter().enumerate() {
            let s = notes[i].start;
            match first {
                Some(f) if s - f <= tol => {}
                _ => {
                    bounds.push(k);
                    first = Some(s);
                }
            }
        }
        bounds.push(idx.len());
        ChordGroups {
            idx: idx.to_vec(),
            bounds,
        }
    }

    /// The number of chords.
    pub fn len(&self) -> usize {
        self.bounds.len().saturating_sub(1)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Chord `k`'s note indices.
    pub fn get(&self, k: usize) -> &[usize] {
        &self.idx[self.bounds[k]..self.bounds[k + 1]]
    }

    /// The chords in time order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &[usize]> + '_ {
        (0..self.len()).map(move |k| self.get(k))
    }

    /// Appends the chords of `other` (several tracks' chords in one list).
    pub fn append(&mut self, other: &ChordGroups) {
        if other.is_empty() {
            return;
        }
        let base = self.idx.len();
        // Drop the closing bound (`base`; absent in a default list).
        self.bounds.pop();
        self.idx.extend_from_slice(&other.idx);
        self.bounds.extend(other.bounds.iter().map(|b| b + base));
    }

    /// Every note's voice rank in its chord, 0 = the lowest: by pitch, then
    /// subchannel, then start, then index — `ranks[i]` for note index `i`
    /// (a `notes.len()` table; notes outside the groups get `u32::MAX`).
    /// The chord sizes come with it: `sizes[i]` = notes in `i`'s chord.
    pub fn voice_ranks(&self, notes: &[TNote]) -> (Vec<u32>, Vec<u32>) {
        let mut ranks = vec![u32::MAX; notes.len()];
        let mut sizes = vec![0; notes.len()];
        let mut order: Vec<usize> = Vec::new();
        for g in self.iter() {
            order.clear();
            order.extend_from_slice(g);
            order.sort_by_key(|&i| (notes[i].pitch, notes[i].sub, notes[i].start, i));
            for (r, &i) in order.iter().enumerate() {
                ranks[i] = r as u32;
                sizes[i] = g.len() as u32;
            }
        }
        (ranks, sizes)
    }

    /// The chord holding each note: `chord[i]` for note index `i`
    /// (`usize::MAX` outside the groups).
    pub fn chord_of(&self, n: usize) -> Vec<usize> {
        let mut out = vec![usize::MAX; n];
        for (k, g) in self.iter().enumerate() {
            for &i in g {
                out[i] = k;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(start: u64, pitch: u8) -> TNote {
        TNote {
            start,
            dur: 100,
            pitch,
            vel: 100,
            ..Default::default()
        }
    }

    /// The literal rule, written the old way (one Vec per chord).
    fn brute(notes: &[TNote], idx: &[usize], tol: u64) -> Vec<Vec<usize>> {
        let mut out: Vec<Vec<usize>> = Vec::new();
        for &i in idx {
            let s = notes[i].start;
            match out.last_mut() {
                Some(g) if s - notes[g[0]].start <= tol => g.push(i),
                _ => out.push(vec![i]),
            }
        }
        out
    }

    #[test]
    fn chords_follow_the_tolerance_rule_and_rank_voices_by_pitch() {
        let notes: Vec<TNote> = [
            (0, 60),
            (0, 64),
            (5, 55),
            (12, 67),
            (480, 50),
            (480, 50),
            (490, 72),
            (960, 40),
        ]
        .iter()
        .map(|&(s, p)| note(s, p))
        .collect();
        let idx: Vec<usize> = (0..notes.len()).collect();
        for tol in [0, 5, 10, 12, 20, 600] {
            let g = ChordGroups::of(&notes, &idx, tol);
            let b = brute(&notes, &idx, tol);
            assert_eq!(g.len(), b.len(), "tol {tol}");
            assert_eq!(g.iter().map(<[usize]>::to_vec).collect::<Vec<_>>(), b);
        }
        let g = ChordGroups::of(&notes, &idx, 12);
        assert_eq!(g.get(0), &[0, 1, 2, 3]);
        let (ranks, sizes) = g.voice_ranks(&notes);
        assert_eq!(&ranks[..4], &[1, 2, 0, 3]); // 55 < 60 < 64 < 67
        assert_eq!(&ranks[4..7], &[0, 1, 2]); // equal pitches by index
        assert_eq!((sizes[0], sizes[7]), (4, 1));
        assert_eq!(g.chord_of(notes.len()), vec![0, 0, 0, 0, 1, 1, 1, 2]);
        // Appending another track's chords keeps both.
        let mut all = ChordGroups::of(&notes, &idx[..4], 12);
        all.append(&ChordGroups::of(&notes, &idx[4..], 12));
        assert_eq!(all, g);
        let empty = ChordGroups::of(&notes, &[], 0);
        assert!(empty.is_empty() && empty.iter().next().is_none());
        // A default list collects other lists; empty ones add nothing.
        let mut acc = ChordGroups::default();
        assert!(acc.is_empty() && acc.iter().next().is_none());
        acc.append(&empty);
        acc.append(&ChordGroups::of(&notes, &idx[..4], 12));
        acc.append(&ChordGroups::default());
        acc.append(&ChordGroups::of(&notes, &idx[4..], 12));
        assert_eq!(
            acc.iter().map(<[usize]>::to_vec).collect::<Vec<_>>(),
            g.iter().map(<[usize]>::to_vec).collect::<Vec<_>>()
        );
    }
}
