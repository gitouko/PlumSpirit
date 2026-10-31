//! Shared source grouping and span helpers for geometry tools.

use super::super::{Source, TNote};

/// Notes of each track slot (indices into `src.notes`, in start order).
pub(super) fn by_slot(src: &Source) -> Vec<(u16, Vec<usize>)> {
    let mut v: Vec<(u16, Vec<usize>)> = Vec::new();
    for (i, n) in src.notes.iter().enumerate() {
        match v.iter_mut().find(|e| e.0 == n.track) {
            Some(e) => e.1.push(i),
            None => v.push((n.track, vec![i])),
        }
    }
    v
}

/// Span of a set of notes: (min start, max end).
pub(super) fn span_of(ns: &[TNote], idx: &[usize]) -> (u64, u64) {
    let t0 = idx.iter().map(|&i| ns[i].start).min().unwrap_or(0);
    let t1 = idx.iter().map(|&i| ns[i].end()).max().unwrap_or(1);
    (t0, t1.max(t0 + 1))
}
