//! Resource preflight (contract §25, §141): before a kernel allocates anything,
//! its output size is bounded from the source statistics and the parameters,
//! and the operation is refused when it cannot fit.

use super::SourceStats;
use crate::midi::group_digits;

/// How a kernel's output size relates to its input.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Growth {
    /// One output per input note (or none).
    Map,
    /// At most one output per input note.
    Thin,
    /// At most `k` outputs per input note.
    Expand(u64),
    /// At most `extra` notes plus the source notes (generators that keep the
    /// selection, e.g. adding chords or generated material).
    Add(u64),
    /// At most `n` notes regardless of the input.
    Generate(u64),
}

/// Estimated cost of one run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Estimate {
    /// Upper bound of output notes.
    pub notes: u64,
    /// Temporary memory (source + output + records), bytes.
    pub temp_bytes: u64,
    /// Undo memory (removed + inserted compact records), bytes.
    pub undo_bytes: u64,
}

/// Bytes per note while transforming: TNote source + TNote output + NoteRecord.
pub const TEMP_BYTES_PER_NOTE: u64 = 32 + 32 + 24;
/// Typical compact undo cost per note (varint start delta, length, attributes).
pub const UNDO_BYTES_PER_NOTE: u64 = 9;
/// The typical whole-run cost of one output note (kernel, rasterisation,
/// the output's sort, the commit and its undo record) on the reference
/// 2-vCPU VM — V09-S43 / S44 measured 0.2 – 1.4 µs per note for geometry
/// tools. The runtime estimate (§135, V09-S45) is an order of magnitude, so
/// a huge request is understood before it runs.
pub const RUNTIME_NS_PER_NOTE: u64 = 500;

impl Estimate {
    /// The estimated runtime (ms) of making [`Estimate::notes`] notes.
    pub fn runtime_ms(&self) -> u64 {
        self.notes.saturating_mul(RUNTIME_NS_PER_NOTE) / 1_000_000
    }

    pub fn from_growth(g: Growth, s: &SourceStats) -> Estimate {
        let n = s.count as u64;
        let notes = match g {
            Growth::Map | Growth::Thin => n,
            Growth::Expand(k) => n.saturating_mul(k),
            Growth::Add(extra) => n.saturating_add(extra),
            Growth::Generate(m) => m,
        };
        Estimate::for_notes(n, notes)
    }

    pub fn for_notes(source: u64, out: u64) -> Estimate {
        Estimate {
            notes: out,
            temp_bytes: source
                .saturating_mul(32)
                .saturating_add(out.saturating_mul(TEMP_BYTES_PER_NOTE - 32)),
            undo_bytes: source
                .saturating_add(out)
                .saturating_mul(UNDO_BYTES_PER_NOTE),
        }
    }
}

/// Caps for one operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Hard cap on output notes.
    pub max_notes: u64,
    /// Above this the window warns (Apply still allowed).
    pub warn_notes: u64,
    /// Temporary memory available for the operation, bytes.
    pub memory: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_notes: 50_000_000,
            warn_notes: 2_000_000,
            memory: 1 << 30,
        }
    }
}

impl Limits {
    /// Limits for a machine with `total` bytes of RAM: the operation may use a
    /// quarter of it (min 256 MB, max 4 GB).
    pub fn for_system_memory(total: Option<u64>) -> Limits {
        let mem = total
            .map(|t| (t / 4).clamp(256 << 20, 4 << 30))
            .unwrap_or(1 << 30);
        Limits {
            max_notes: (mem / TEMP_BYTES_PER_NOTE).min(50_000_000),
            warn_notes: 2_000_000,
            memory: mem,
        }
    }

    /// The largest number of output notes the sink may accept.
    pub fn cap(&self) -> usize {
        self.max_notes.min(usize::MAX as u64) as usize
    }
}

/// Verdict of a preflight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    /// Allowed, with a warning sentence.
    Warn(String),
    /// Refused before allocation (full sentence).
    Refuse(String),
}

/// A runtime for a sentence ("about 3 s", "about 2 min").
pub fn runtime_words(ms: u64) -> String {
    match ms {
        0..=999 => "under a second".into(),
        1_000..=119_999 => format!("about {} s", group_digits(ms.div_ceil(1_000))),
        120_000..=7_199_999 => format!("about {} min", group_digits(ms.div_ceil(60_000))),
        _ => format!("about {} h", group_digits(ms.div_ceil(3_600_000))),
    }
}

pub fn check(e: &Estimate, l: &Limits) -> Verdict {
    if e.notes > l.max_notes {
        return Verdict::Refuse(format!(
            "This would create up to {} notes (the limit is {}; {} of work). Reduce the settings or select fewer notes.",
            group_digits(e.notes),
            group_digits(l.max_notes),
            runtime_words(e.runtime_ms())
        ));
    }
    if e.temp_bytes > l.memory {
        return Verdict::Refuse(format!(
            "This would need about {} MB of memory (the limit is {} MB). Reduce the settings or select fewer notes.",
            group_digits(e.temp_bytes >> 20),
            group_digits(l.memory >> 20)
        ));
    }
    if e.notes > l.warn_notes {
        return Verdict::Warn(format!(
            "Up to {} notes will be created (about {} MB while applying, {} MB of undo, {} of work).",
            group_digits(e.notes),
            group_digits((e.temp_bytes >> 20).max(1)),
            group_digits((e.undo_bytes >> 20).max(1)),
            runtime_words(e.runtime_ms())
        ));
    }
    Verdict::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explosive_growth_is_refused_before_allocation() {
        let s = SourceStats {
            count: 1_000_000,
            ..SourceStats::default()
        };
        let e = Estimate::from_growth(Growth::Expand(1_000), &s);
        assert!(matches!(check(&e, &Limits::default()), Verdict::Refuse(_)));
        let e = Estimate::from_growth(Growth::Expand(3), &s);
        assert!(matches!(check(&e, &Limits::default()), Verdict::Warn(_)));
        let e = Estimate::from_growth(Growth::Map, &s);
        assert_eq!(check(&e, &Limits::default()), Verdict::Ok);
        let e = Estimate::from_growth(Growth::Expand(u64::MAX), &s);
        assert_eq!(e.notes, u64::MAX);
        let l = Limits::for_system_memory(Some(2 << 30));
        assert_eq!(l.memory, 512 << 20);
    }

    #[test]
    fn sentences_carry_the_runtime_estimate() {
        let s = |n: usize| SourceStats {
            count: n,
            ..SourceStats::default()
        };
        // 5M notes × 100 trails: refused, with the work it would take.
        let e = Estimate::from_growth(Growth::Expand(100), &s(5_000_000));
        match check(&e, &Limits::default()) {
            Verdict::Refuse(m) => assert!(m.contains("about 5 min of work"), "{m}"),
            v => panic!("{v:?}"),
        }
        let e = Estimate::from_growth(Growth::Expand(4), &s(1_000_000));
        match check(&e, &Limits::default()) {
            Verdict::Warn(m) => assert!(m.ends_with("about 2 s of work)."), "{m}"),
            v => panic!("{v:?}"),
        }
        assert_eq!(runtime_words(400), "under a second");
        assert_eq!(runtime_words(150_000), "about 3 min");
        assert_eq!(runtime_words(9_000_000), "about 3 h");
    }
}
