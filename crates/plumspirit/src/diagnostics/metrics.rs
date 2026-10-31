//! CPU / RAM of the PlumSpirit process for the status bar.
//!
//! * **CPU**: the process's CPU time (all threads, user + kernel) over the last
//!   interval divided by the wall time and the number of logical processors, so
//!   **100 % means the whole machine** (every logical CPU fully busy) and one
//!   busy core on a 4-thread CPU shows 25 %.
//! * **RAM**: the process's working set (Windows) / resident set (Linux) — the
//!   physical memory it occupies now, including shared pages such as loaded
//!   DLLs; not the private committed memory.
//!
//! Sampled once per second from lightweight OS counters (`GetProcessTimes` /
//! `K32GetProcessMemoryInfo`, `/proc/self`) on a small `plumspirit-metrics`
//! thread, never in the audio path. A counter that is unavailable shows "N/A".
//! Nothing leaves the computer.
//!
//! **No idle repaint loop.** Showing a new number costs a whole frame, which on
//! a PC without GPU acceleration (software OpenGL) is tens of milliseconds. The
//! sampler therefore wakes the UI only when the text changes noticeably (CPU by
//! 0.5 points or more, RAM by 1 MiB or more), and never for an interval in which
//! the only frame drawn was its own refresh — otherwise the cost of showing the
//! number would itself change the number, forever. Frames drawn for any other
//! reason (input, playback) always show the latest sample.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sample {
    /// Percent of the whole machine's CPU capacity.
    pub cpu: Option<f64>,
    /// Bytes resident.
    pub ram: Option<u64>,
    /// Physical memory of the computer.
    pub total: Option<u64>,
}

pub struct Metrics {
    last: Option<(Instant, u64)>,
    pub current: Sample,
    logical: u32,
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics {
            last: None,
            current: Sample::default(),
            logical: std::thread::available_parallelism()
                .map(|n| n.get() as u32)
                .unwrap_or(1),
        }
    }
}

/// CPU percent of the whole machine from two samples.
pub fn cpu_percent(cpu_ns: u64, wall: Duration, logical: u32) -> Option<f64> {
    let wall_ns = wall.as_nanos() as f64;
    if wall_ns <= 0.0 || logical == 0 {
        return None;
    }
    Some((cpu_ns as f64 / wall_ns / f64::from(logical) * 100.0).clamp(0.0, 100.0))
}

impl Metrics {
    /// Takes a sample (the sampler thread calls this once per [`INTERVAL`]).
    pub fn sample(&mut self, now: Instant) {
        let cpu = crate::platform::process_cpu_nanos();
        self.current.cpu = match (cpu, self.last) {
            (Some(c), Some((t, c0))) if c >= c0 => {
                cpu_percent(c - c0, now.duration_since(t), self.logical)
            }
            _ => self.current.cpu,
        };
        self.last = cpu.map(|c| (now, c));
        self.current.ram = super::process_memory_bytes();
        if self.current.total.is_none() {
            self.current.total = crate::platform::total_memory_bytes();
        }
    }
}

pub fn format_sample(s: &Sample) -> String {
    let cpu = s
        .cpu
        .map_or_else(|| "N/A".to_owned(), |c| format!("{c:.1}%"));
    let mib = |b: u64| b as f64 / (1024.0 * 1024.0);
    let ram = match s.ram {
        Some(b) => format!("{:.0} MiB", mib(b)),
        None => "N/A".into(),
    };
    match s.total {
        Some(t) if s.ram.is_some() => format!(
            "CPU: {cpu}  |  RAM: {ram} / {:.1} GiB",
            t as f64 / (1024.0 * 1024.0 * 1024.0)
        ),
        _ => format!("CPU: {cpu}  |  RAM: {ram}"),
    }
}

/// Whether the text for `new` differs noticeably from what is shown.
pub fn noticeable(shown: &Sample, new: &Sample) -> bool {
    let cpu = match (shown.cpu, new.cpu) {
        (Some(a), Some(b)) => (a - b).abs() >= 0.5,
        (a, b) => a.is_some() != b.is_some(),
    };
    let ram = match (shown.ram, new.ram) {
        (Some(a), Some(b)) => a.abs_diff(b) >= 1024 * 1024,
        (a, b) => a.is_some() != b.is_some(),
    };
    cpu || ram
}

/// Decides whether a new sample should wake the UI. `frames_since_trigger` is
/// the number of frames drawn since the previous wake-up this sampler caused
/// (`None` if the previous sample caused none).
pub fn should_wake(shown: &Sample, new: &Sample, frames_since_trigger: Option<u64>) -> bool {
    // An interval whose only frames were our own refresh measured that
    // refresh. One wake-up draws two frames (egui paints one more frame after a
    // repaint request; measured).
    let self_caused = frames_since_trigger.is_some_and(|f| f <= 2);
    !self_caused && noticeable(shown, new)
}

struct Shared {
    /// (latest sample, sample last shown by the UI).
    samples: Mutex<(Sample, Sample)>,
    frames: AtomicU64,
}

/// The background sampler. Dropping it stops and joins the thread (at most one
/// wake-up: the thread waits on a channel, not a sleep).
pub struct Sampler {
    shared: Arc<Shared>,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Sampler {
    /// Starts sampling; `wake` asks the UI for one repaint.
    pub fn start(wake: impl Fn() + Send + 'static) -> Sampler {
        let mut m = Metrics::default();
        m.sample(Instant::now());
        let shared = Arc::new(Shared {
            samples: Mutex::new((m.current, Sample::default())),
            frames: AtomicU64::new(0),
        });
        let (tx, rx) = mpsc::channel::<()>();
        let sh = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("plumspirit-metrics".into())
            .spawn(move || {
                let mut trigger: Option<u64> = None;
                while let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(INTERVAL) {
                    m.sample(Instant::now());
                    let frames = sh.frames.load(Ordering::Relaxed);
                    let since = trigger.map(|f| frames.saturating_sub(f));
                    let wake_now = {
                        let Ok(mut g) = sh.samples.lock() else { break };
                        g.0 = m.current;
                        should_wake(&g.1, &g.0, since)
                    };
                    trigger = wake_now.then_some(frames);
                    if wake_now {
                        wake();
                    }
                }
            })
            .ok();
        Sampler {
            shared,
            stop: Some(tx),
            thread,
        }
    }

    /// Called when the UI draws frame `frame_nr` (egui's cumulative frame
    /// number, so extra layout passes of one frame count once): the latest
    /// sample, recorded as shown.
    pub fn frame(&self, frame_nr: u64) -> Sample {
        self.shared.frames.fetch_max(frame_nr, Ordering::Relaxed);
        match self.shared.samples.lock() {
            Ok(mut g) => {
                g.1 = g.0;
                g.0
            }
            Err(_) => Sample::default(),
        }
    }

    pub fn text(&self, frame_nr: u64) -> String {
        format_sample(&self.frame(frame_nr))
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_is_normalised_to_the_whole_machine() {
        // One core busy for the whole second on 4 logical CPUs = 25 %.
        assert_eq!(
            cpu_percent(1_000_000_000, Duration::from_secs(1), 4),
            Some(25.0)
        );
        assert_eq!(
            cpu_percent(8_000_000_000, Duration::from_secs(1), 4),
            Some(100.0)
        );
        assert_eq!(cpu_percent(5, Duration::ZERO, 4), None);
    }

    #[test]
    fn unavailable_metrics_show_na_not_zero() {
        let s = Sample::default();
        assert_eq!(format_sample(&s), "CPU: N/A  |  RAM: N/A");
        let s = Sample {
            cpu: Some(3.24),
            ram: Some(184 * 1024 * 1024),
            total: Some(2 * 1024 * 1024 * 1024),
        };
        assert_eq!(format_sample(&s), "CPU: 3.2%  |  RAM: 184 MiB / 2.0 GiB");
    }

    #[test]
    fn an_idle_window_is_not_repainted_just_to_show_its_own_repaint_cost() {
        let idle = Sample {
            cpu: Some(0.0),
            ram: Some(100 << 20),
            total: None,
        };
        let busy = Sample {
            cpu: Some(4.5),
            ..idle
        };
        // Activity ended: the drop to idle is shown (one wake-up) ...
        assert!(should_wake(&busy, &idle, None));
        // ... the next interval only contains that refresh: no wake-up, even
        // though it measured 4.5 %.
        assert!(!should_wake(&idle, &busy, Some(2)));
        // Back to idle: nothing noticeable changed since what is shown.
        assert!(!should_wake(&idle, &idle, None));
        // Real activity (other frames were drawn, or none were but CPU rose).
        assert!(should_wake(&idle, &busy, Some(5)));
        assert!(should_wake(&idle, &busy, None));
        // Small changes wait for the next natural frame.
        let tiny = Sample {
            cpu: Some(0.3),
            ram: Some((100 << 20) + 4096),
            total: None,
        };
        assert!(!should_wake(&idle, &tiny, None));
        assert!(should_wake(&idle, &Sample::default(), None), "N/A appears");
    }

    #[test]
    fn the_sampler_thread_stops_promptly_when_dropped() {
        let woken = Arc::new(AtomicU64::new(0));
        let w = Arc::clone(&woken);
        let s = Sampler::start(move || {
            w.fetch_add(1, Ordering::Relaxed);
        });
        assert!(!s.text(1).is_empty());
        let t = Instant::now();
        drop(s);
        assert!(
            t.elapsed() < Duration::from_millis(500),
            "{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn sampling_reads_this_process_cheaply() {
        let mut m = Metrics::default();
        let t0 = Instant::now();
        m.sample(t0);
        // Busy work so CPU time advances.
        let mut x = 0u64;
        for i in 0..20_000_000u64 {
            x = x.wrapping_add(i * i);
        }
        std::hint::black_box(x);
        m.sample(t0 + Duration::from_secs(1));
        #[cfg(any(windows, target_os = "linux"))]
        {
            assert!(m.current.ram.is_some_and(|b| b > 0));
            assert!(m.current.cpu.is_some());
        }
        let started = Instant::now();
        for k in 0..100 {
            m.sample(t0 + Duration::from_secs(2 + k));
        }
        // 100 samples cost well under a millisecond each.
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "{:?}",
            started.elapsed()
        );
    }
}
