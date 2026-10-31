//! Ordered worker pipelines for MIDI files (v1.0-beta1 B1-0161 / B1-0162).
//!
//! Standard MIDI Files are sequences of independent track chunks. Import,
//! export and validation hand chunks (or the work of producing one) to worker
//! threads and take the results back **in chunk order**, so the outcome —
//! bytes, warnings, the first error — is the one a single thread gives. At
//! most `window` bytes of chunk data are in flight (submitted and not yet taken
//! back); a chunk larger than the window is processed in place by the caller.
//! The engine stays std-only: scoped threads and channels.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

/// Chunk data allowed in flight at once per worker (bytes).
pub(crate) const WINDOW: u64 = 128 << 20;

/// The window never grows past this (raw chunk bytes held at once).
pub(crate) const MAX_WINDOW: u64 = 1 << 30;

/// L-S29 (L-H05): chunk data in flight for `threads` workers — [`WINDOW`]
/// each, at most [`MAX_WINDOW`]. One fixed 128 MB window let only one or
/// two big track chunks (the 302M-note file: 63 MB a track) be decoded at
/// once whatever the number of workers: the "weirdly low" CPU during load
/// (64M notes in 96 MB tracks, 2 workers: slower than one thread).
/// `PLUMSPIRIT_MIDI_WINDOW_MB` overrides it (A/B measurements, diagnosis).
pub(crate) fn window_for(threads: usize) -> u64 {
    if let Some(mb) = std::env::var("PLUMSPIRIT_MIDI_WINDOW_MB")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        return mb.max(1) << 20;
    }
    (threads.max(1) as u64)
        .saturating_mul(WINDOW)
        .min(MAX_WINDOW)
}

/// Inputs smaller than this run on the calling thread.
pub(crate) const PARALLEL_MIN_BYTES: u64 = 4 << 20;

/// Worker threads for an input of about `bytes` bytes: the hardware threads
/// (at most 8), or 0 (in place) for a small input or a single-threaded
/// machine. `PLUMSPIRIT_MIDI_THREADS` overrides it (0 = in place; for A/B
/// measurements and diagnosis).
pub(crate) fn threads_for(bytes: Option<u64>) -> usize {
    if let Some(n) = std::env::var("PLUMSPIRIT_MIDI_THREADS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
    {
        return n.min(64);
    }
    match bytes {
        Some(n) if n >= PARALLEL_MIN_BYTES => {
            let hw = std::thread::available_parallelism().map_or(1, |n| n.get());
            if hw >= 2 { hw.min(8) } else { 0 }
        }
        _ => 0,
    }
}

/// Jobs out, results back by index. Created by [`run`].
pub(crate) struct Pipeline<J, R> {
    jobs: Option<mpsc::Sender<(usize, J)>>,
    done: mpsc::Receiver<(usize, R)>,
    costs: BTreeMap<usize, u64>,
    finished: BTreeMap<usize, R>,
    in_flight: u64,
    window: u64,
}

impl<J, R> Pipeline<J, R> {
    /// Bytes submitted and not yet taken back.
    #[cfg(test)]
    pub(crate) fn in_flight(&self) -> u64 {
        self.in_flight
    }

    pub(crate) fn window(&self) -> u64 {
        self.window
    }

    /// Whether a job of `cost` bytes may be submitted now without waiting.
    pub(crate) fn has_room(&self, cost: u64) -> bool {
        self.in_flight == 0 || self.in_flight + cost <= self.window
    }

    /// The smallest index submitted and not taken back.
    pub(crate) fn oldest(&self) -> Option<usize> {
        self.costs.keys().next().copied()
    }

    /// Queues job `index` (indices increase). Never blocks.
    pub(crate) fn submit(&mut self, index: usize, job: J, cost: u64) {
        self.costs.insert(index, cost);
        self.in_flight += cost;
        if let Some(tx) = &self.jobs {
            // Fails only when every worker is gone; `take` then reports it.
            let _ = tx.send((index, job));
        }
    }

    /// Waits for the result of job `index` and takes it back (its bytes leave
    /// the window). `None`: the job was never submitted, the workers are gone,
    /// or `cancel` was set while waiting.
    pub(crate) fn take(&mut self, index: usize, cancel: Option<&AtomicBool>) -> Option<R> {
        let cost = *self.costs.get(&index)?;
        loop {
            if let Some(r) = self.finished.remove(&index) {
                self.costs.remove(&index);
                self.in_flight -= cost;
                return Some(r);
            }
            match self.done.recv_timeout(Duration::from_millis(50)) {
                Ok((i, r)) => {
                    self.finished.insert(i, r);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                        return None;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        }
    }
}

/// Runs `body` with a pipeline whose `threads` workers apply `work` to every
/// submitted job. When `body` returns, the queue is closed and the workers
/// finish what is queued (results nobody takes are dropped).
pub(crate) fn run<J, R, T>(
    threads: usize,
    window: u64,
    work: impl Fn(J) -> R + Sync,
    body: impl FnOnce(&mut Pipeline<J, R>) -> T,
) -> T
where
    J: Send,
    R: Send,
{
    let (job_tx, job_rx) = mpsc::channel::<(usize, J)>();
    let job_rx = Mutex::new(job_rx);
    let (done_tx, done_rx) = mpsc::channel::<(usize, R)>();
    let work = &work;
    let job_rx = &job_rx;
    std::thread::scope(|scope| {
        for _ in 0..threads.max(1) {
            let done_tx = done_tx.clone();
            scope.spawn(move || {
                loop {
                    let next = match job_rx.lock() {
                        Ok(rx) => rx.recv(),
                        Err(_) => return,
                    };
                    let Ok((i, job)) = next else { return };
                    if done_tx.send((i, work(job))).is_err() {
                        return;
                    }
                }
            });
        }
        drop(done_tx);
        let mut p = Pipeline {
            jobs: Some(job_tx),
            done: done_rx,
            costs: BTreeMap::new(),
            finished: BTreeMap::new(),
            in_flight: 0,
            window,
        };
        let out = body(&mut p);
        p.jobs = None;
        drop(p);
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// L-S29: the window grows with the workers (128 MB each), never past
    /// 1 GiB — a 63 MB track per worker fits for up to eight workers.
    #[test]
    fn the_window_grows_with_the_workers_up_to_its_cap() {
        if std::env::var("PLUMSPIRIT_MIDI_WINDOW_MB").is_ok() {
            return;
        }
        assert_eq!(window_for(0), WINDOW);
        assert_eq!(window_for(1), WINDOW);
        assert_eq!(window_for(2), 2 * WINDOW);
        assert_eq!(window_for(8), MAX_WINDOW);
        assert_eq!(window_for(64), MAX_WINDOW);
        assert!(window_for(8) >= 8 * (63 << 20));
    }

    #[test]
    fn results_come_back_by_index_and_the_window_is_kept() {
        let out = run(
            3,
            10,
            |x: u64| x * 2,
            |p| {
                let mut got = Vec::new();
                let mut next = 0;
                for i in 0..20usize {
                    while !p.has_room(4) {
                        got.push(p.take(next, None).unwrap());
                        next += 1;
                    }
                    p.submit(i, i as u64, 4);
                    assert!(p.in_flight() <= p.window().max(4));
                }
                while let Some(i) = p.oldest() {
                    got.push(p.take(i, None).unwrap());
                }
                got
            },
        );
        assert_eq!(out, (0..20).map(|i| i * 2).collect::<Vec<u64>>());
    }

    #[test]
    fn a_job_never_submitted_has_no_result() {
        let r = run(2, 10, |x: u8| x, |p| p.take(5, None));
        assert_eq!(r, None);
    }
}
