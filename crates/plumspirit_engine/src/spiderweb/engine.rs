//! Overlaps and channels (port of the second half of Spiderweb
//! `scripts/notes/engine.py`, MIT © Kanade Tachibana): Multi-channel slot
//! assignment, overlap resolution and the final render of every shape's notes.

/// `(start, end, pitch, velocity)`.
pub type Note4 = [i64; 4];
/// `(start, end, pitch, velocity, slot, owner)`.
pub type Note6 = [i64; 6];

/// Every MIDI channel except 10 (drums), one per slot in turn.
pub const CHANNELS: [u8; 15] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 12, 13, 14, 15];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelMode {
    /// "As drawn": one channel, overlaps kept.
    Raw,
    /// "Single channel": one channel, overlaps fixed.
    Single,
    /// "Multi channel": overlapping shapes on their own channels.
    Auto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Split {
    /// Only notes on the same key at the same time clash.
    Key,
    /// Any notes sounding at the same time clash.
    Time,
}

pub fn slot_channel(slot: usize) -> u8 {
    CHANNELS[slot % CHANNELS.len()]
}

/// Rows of `a` without repeats of an earlier row (order kept).
pub fn unique_rows<const N: usize>(a: Vec<[i64; N]>) -> Vec<[i64; N]> {
    if a.len() < 2 {
        return a;
    }
    let mut seen = std::collections::HashSet::with_capacity(a.len());
    let mut out = Vec::with_capacity(a.len());
    for r in a {
        if seen.insert(r) {
            out.push(r);
        }
    }
    out
}

/// Multi channel slots: lists whose notes clash get different slots; earlier
/// lists (by first start) get the lower ones. `apart`: groups of list numbers
/// that always clash.
pub fn assign_slots(lists: &[Vec<Note4>], split: Split, apart: &[Vec<usize>]) -> Vec<usize> {
    let n = lists.len();
    let mut by: std::collections::HashMap<i64, Vec<(i64, i64, usize)>> = Default::default();
    for (owner, notes) in lists.iter().enumerate() {
        let bucket = |p: i64| if split == Split::Key { p } else { 0 };
        let mut a: Vec<(i64, i64, i64)> = Vec::new(); // (bucket, start, end)
        for r in notes {
            if r[1] <= r[0] {
                by.entry(bucket(r[2]))
                    .or_default()
                    .push((r[0], r[1], owner));
            } else {
                a.push((bucket(r[2]), r[0], r[1]));
            }
        }
        if a.is_empty() {
            continue;
        }
        a.sort_by_key(|x| (x.0, x.1)); // stable: lexsort((start, key))
        let mut i = 0;
        while i < a.len() {
            let k = a[i].0;
            let s0 = a[i].1;
            let mut e0 = a[i].2;
            let mut run = a[i].2;
            let mut j = i + 1;
            while j < a.len() && a[j].0 == k && a[j].1 <= run {
                run = run.max(a[j].2);
                e0 = e0.max(a[j].2);
                j += 1;
            }
            by.entry(k).or_default().push((s0, e0, owner));
            i = j;
        }
    }
    let mut clashes: Vec<std::collections::HashSet<usize>> = vec![Default::default(); n];
    for items in by.values_mut() {
        items.sort();
        let mut active: Vec<(i64, usize)> = Vec::new();
        for &(s, e, o) in items.iter() {
            active.retain(|a| a.0 > s);
            for &(_, ao) in &active {
                if ao != o {
                    clashes[o].insert(ao);
                    clashes[ao].insert(o);
                }
            }
            active.push((e, o));
        }
    }
    for g in apart {
        for &a in g {
            for &b in g {
                if a != b && a < n && b < n {
                    clashes[a].insert(b);
                }
            }
        }
    }
    let first: Vec<(i64, usize)> = lists
        .iter()
        .enumerate()
        .map(|(i, l)| (l.iter().map(|r| r[0]).min().unwrap_or(i64::MAX), i))
        .collect();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| first[i]);
    let mut slots = vec![0usize; n];
    let mut done = vec![false; n];
    for i in order {
        let used: std::collections::HashSet<usize> = clashes[i]
            .iter()
            .filter(|&&j| done[j])
            .map(|&j| slots[j])
            .collect();
        let mut k = 0;
        while used.contains(&k) {
            k += 1;
        }
        slots[i] = k;
        done[i] = true;
    }
    slots
}

/// Notes on one (slot, key) that overlap: the earlier is cut where the later
/// starts, the later stretched to where the earlier would have ended; same
/// start → one note (loudest, longest). Groups in first-seen order.
pub fn resolve_overlaps(notes: Vec<Note6>) -> Vec<Note6> {
    if notes.is_empty() {
        return notes;
    }
    // first_seen(slot * 256 + pitch)
    let mut ids: std::collections::HashMap<i64, usize> = Default::default();
    let group: Vec<usize> = notes
        .iter()
        .map(|r| {
            let key = r[4] * 256 + r[2];
            let next = ids.len();
            *ids.entry(key).or_insert(next)
        })
        .collect();
    let mut order: Vec<usize> = (0..notes.len()).collect();
    // lexsort((-end, velocity, start, group)), stable.
    order.sort_by_key(|&i| (group[i], notes[i][0], notes[i][3], -notes[i][1]));
    let a: Vec<Note6> = order.iter().map(|&i| notes[i]).collect();
    let g: Vec<usize> = order.iter().map(|&i| group[i]).collect();
    let m = a.len();
    let mut run = vec![0i64; m];
    for i in 0..m {
        run[i] = if i > 0 && g[i] == g[i - 1] {
            run[i - 1].max(a[i][1])
        } else {
            a[i][1]
        };
    }
    let mut end = vec![0i64; m];
    let mut over = vec![false; m];
    for i in 0..m {
        let same = i > 0 && g[i] == g[i - 1];
        let before = if i > 0 { run[i - 1] } else { 0 };
        over[i] = same && a[i][0] < before;
        end[i] = if over[i] {
            a[i][1].max(before)
        } else {
            a[i][1]
        };
    }
    for i in 0..m.saturating_sub(1) {
        if over[i + 1] {
            end[i] = a[i + 1][0];
        }
    }
    let mut out = Vec::with_capacity(m);
    for i in 0..m {
        let last = i + 1 == m || g[i + 1] != g[i];
        if last || end[i] > a[i][0] {
            let mut r = a[i];
            r[1] = end[i];
            out.push(r);
        }
    }
    out
}

/// Every shape's notes → the final notes (`(start, end, pitch, velocity, slot,
/// owner)`) and the number of slots used.
pub fn render(
    lists: &[Vec<Note4>],
    mode: ChannelMode,
    split: Split,
    tracks: &[Option<Vec<i64>>],
    apart: &[bool],
) -> (Vec<Note6>, usize) {
    let n = lists.len();
    // Per shape: one slot, or one per note (shapes split by track).
    let mut slot_of: Vec<Result<usize, Vec<usize>>> = Vec::with_capacity(n);
    let count;
    if mode == ChannelMode::Auto {
        let mut units: Vec<Vec<Note4>> = Vec::new();
        let mut unit_of: Vec<Result<usize, Vec<usize>>> = Vec::new();
        let mut forced: Vec<Vec<usize>> = Vec::new();
        for (i, lst) in lists.iter().enumerate() {
            match tracks.get(i).and_then(|t| t.as_ref()) {
                Some(tr) if !lst.is_empty() => {
                    let mut ids: Vec<i64> = tr.clone();
                    ids.sort_unstable();
                    ids.dedup();
                    let base = units.len();
                    let which: Vec<usize> = tr
                        .iter()
                        .map(|t| base + ids.binary_search(t).expect("present"))
                        .collect();
                    if apart.get(i).copied().unwrap_or(false) {
                        forced.push((base..base + ids.len()).collect());
                    }
                    for k in 0..ids.len() {
                        units.push(
                            lst.iter()
                                .zip(&which)
                                .filter(|(_, w)| **w == base + k)
                                .map(|(r, _)| *r)
                                .collect(),
                        );
                    }
                    unit_of.push(Err(which));
                }
                _ => {
                    unit_of.push(Ok(units.len()));
                    units.push(lst.clone());
                }
            }
        }
        let unit_slots = assign_slots(&units, split, &forced);
        count = unit_slots.iter().max().map_or(0, |m| m + 1);
        for u in unit_of {
            slot_of.push(match u {
                Ok(x) => Ok(unit_slots[x]),
                Err(v) => Err(v.into_iter().map(|x| unit_slots[x]).collect()),
            });
        }
    } else {
        count = usize::from(n > 0);
        slot_of = vec![Ok(0); n];
    }
    let mut notes: Vec<Note6> = Vec::with_capacity(lists.iter().map(Vec::len).sum());
    for (o, lst) in lists.iter().enumerate() {
        for (k, r) in lst.iter().enumerate() {
            let slot = match &slot_of[o] {
                Ok(s) => *s,
                Err(v) => v[k],
            };
            notes.push([r[0], r[1], r[2], r[3], slot as i64, o as i64]);
        }
    }
    if mode != ChannelMode::Raw {
        notes = resolve_overlaps(notes);
    }
    (notes, count)
}
