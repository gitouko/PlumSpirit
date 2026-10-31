//! Task duration reporting (v0.5): how long significant user operations took,
//! shown at the bottom of the window ("Saved project — 1.31 s").
//!
//! One small, generic mechanism: every meaningful operation — opening, saving,
//! importing, exporting, a batch edit, a structural change — is timed with a
//! monotonic clock ([`Instant`]) from start to end and recorded once, with its
//! outcome. Micro-actions (pointer moves, single frames, each note of a batch)
//! are never recorded. Background jobs report their own start time; their result
//! is recorded when the UI thread receives it (workers never touch UI state).
//! Timing costs two `Instant::now()` calls per operation.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How an operation ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskOutcome {
    Done,
    Failed,
    Cancelled,
}

/// One finished operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskRecord {
    /// Past-tense description for success ("Saved project"), or the operation's
    /// name for failures ("Export").
    pub name: String,
    pub elapsed: Duration,
    pub outcome: TaskOutcome,
}

impl TaskRecord {
    /// "Saved project — 1.31 s", "Export failed after 2.17 s",
    /// "Import cancelled after 5.93 s".
    pub fn text(&self) -> String {
        let t = format_task_duration(self.elapsed);
        match self.outcome {
            TaskOutcome::Done => format!("{} — {t}", self.name),
            TaskOutcome::Failed => format!("{} failed after {t}", self.name),
            TaskOutcome::Cancelled => format!("{} canceled after {t}", self.name),
        }
    }
}

/// Readable duration: "0.4 ms", "842 ms", "1.31 s", "12.4 s", "3 min 05 s".
pub fn format_task_duration(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms < 10.0 {
        format!("{ms:.1} ms")
    } else if ms < 1000.0 {
        format!("{ms:.0} ms")
    } else if ms < 10_000.0 {
        format!("{:.2} s", ms / 1000.0)
    } else if ms < 60_000.0 {
        format!("{:.1} s", ms / 1000.0)
    } else {
        let s = d.as_secs();
        format!("{} min {:02} s", s / 60, s % 60)
    }
}

/// Bounded history of recent operations (the last one is shown).
#[derive(Debug, Default)]
pub struct TaskLog {
    recent: VecDeque<TaskRecord>,
}

/// Operations kept for diagnostics.
pub const TASK_HISTORY: usize = 20;

impl TaskLog {
    pub fn record(&mut self, name: impl Into<String>, elapsed: Duration, outcome: TaskOutcome) {
        if self.recent.len() == TASK_HISTORY {
            self.recent.pop_front();
        }
        self.recent.push_back(TaskRecord {
            name: name.into(),
            elapsed,
            outcome,
        });
    }

    /// Records an operation that started at `started` and ends now.
    pub fn finish(&mut self, name: impl Into<String>, started: Instant, outcome: TaskOutcome) {
        self.record(name, started.elapsed(), outcome);
    }

    pub fn last(&self) -> Option<&TaskRecord> {
        self.recent.back()
    }

    pub fn recent(&self) -> impl Iterator<Item = &TaskRecord> {
        self.recent.iter()
    }

    /// "Last: Saved project — 1.31 s".
    pub fn last_text(&self) -> Option<String> {
        self.last().map(|r| format!("Last: {}", r.text()))
    }
}

/// Text shown while a background task runs: "Importing huge.mid… 4.2 s".
pub fn running_text(label: &str, started: Instant) -> String {
    let s = started.elapsed().as_secs_f64();
    let label = label.trim_end_matches('…').trim_end_matches("...");
    format!("{label}… {s:.1} s")
}

/// Whether a history operation deserves a task record: batch and structural
/// edits always, others only when they took noticeable time (so drawing single
/// notes never floods the status bar).
pub fn significant(label: &str, elapsed: Duration) -> bool {
    const ALWAYS: [&str; 14] = [
        "Paste",
        "Delete Notes",
        "Delete Track",
        "Add Track",
        "Add Subtrack",
        "Add Subtracks",
        "Add Subchannel",
        "Add Subchannels",
        "Delete Subchannel",
        "Move Notes to Subchannel",
        "Import MIDI Into Track",
        "Quantize",
        "Flip X",
        "Flip Y",
    ];
    ALWAYS.contains(&label) || label.starts_with("Scale") || elapsed >= Duration::from_millis(5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_show_success_failure_and_cancellation_with_their_time() {
        let mut log = TaskLog::default();
        assert!(log.last_text().is_none());
        log.record(
            "Saved project",
            Duration::from_millis(1310),
            TaskOutcome::Done,
        );
        assert_eq!(log.last_text().unwrap(), "Last: Saved project — 1.31 s");
        log.record("Export", Duration::from_millis(2170), TaskOutcome::Failed);
        assert_eq!(log.last().unwrap().text(), "Export failed after 2.17 s");
        log.record(
            "Import",
            Duration::from_millis(5930),
            TaskOutcome::Cancelled,
        );
        assert_eq!(log.last().unwrap().text(), "Import canceled after 5.93 s");
        assert_eq!(format_task_duration(Duration::from_micros(420)), "0.4 ms");
        assert_eq!(format_task_duration(Duration::from_millis(842)), "842 ms");
        assert_eq!(
            format_task_duration(Duration::from_millis(12_400)),
            "12.4 s"
        );
        assert_eq!(format_task_duration(Duration::from_secs(185)), "3 min 05 s");
    }

    #[test]
    fn history_is_bounded_and_keeps_the_newest() {
        let mut log = TaskLog::default();
        for i in 0..100 {
            log.record(format!("op {i}"), Duration::ZERO, TaskOutcome::Done);
        }
        assert_eq!(log.recent().count(), TASK_HISTORY);
        assert_eq!(log.last().unwrap().name, "op 99");
    }

    #[test]
    fn overlapping_timers_measure_their_own_spans() {
        let mut log = TaskLog::default();
        let a = Instant::now();
        std::thread::sleep(Duration::from_millis(20));
        let b = Instant::now();
        std::thread::sleep(Duration::from_millis(20));
        log.finish("B", b, TaskOutcome::Done);
        log.finish("A", a, TaskOutcome::Done);
        let v: Vec<&TaskRecord> = log.recent().collect();
        assert!(v[0].elapsed >= Duration::from_millis(20));
        assert!(v[1].elapsed >= Duration::from_millis(40));
        assert!(v[1].elapsed > v[0].elapsed);
    }

    #[test]
    fn timing_overhead_is_negligible() {
        let mut log = TaskLog::default();
        let t = Instant::now();
        for _ in 0..100_000 {
            let s = Instant::now();
            log.finish("x", s, TaskOutcome::Done);
        }
        // 100,000 timed operations (with string allocation) well under a second.
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
        assert!(significant("Paste", Duration::ZERO));
        assert!(!significant("Draw Note", Duration::from_micros(80)));
        assert!(significant("Draw Note", Duration::from_millis(9)));
        assert!(
            running_text("Importing huge.mid…", Instant::now())
                .starts_with("Importing huge.mid… 0.")
        );
    }
}
