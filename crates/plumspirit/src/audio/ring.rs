//! Lock-free single-producer / single-consumer sample ring (no `unsafe`).
//!
//! The synthesis worker (producer) writes interleaved stereo `f32` samples; the
//! audio callback (consumer) reads them. Samples are stored as `AtomicU32` bit
//! patterns, and the two positions are monotonic sample counters, so neither side
//! ever blocks, allocates or takes a lock.
//!
//! **Flush.** When playback is paused, seeks or switches data, buffered audio
//! becomes stale. The producer publishes `flush_to` (a write position); the
//! consumer skips everything before it. This replaces a "clear" operation, which
//! would race with the consumer.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub struct Ring {
    buf: Box<[AtomicU32]>,
    /// Samples written so far (producer only).
    write: AtomicU64,
    /// Samples read so far (consumer only).
    read: AtomicU64,
    /// Samples before this position are stale and must be skipped.
    flush_to: AtomicU64,
}

impl Ring {
    /// A ring holding `capacity` samples (rounded up to a power of two).
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.next_power_of_two().max(64);
        Ring {
            buf: (0..cap).map(|_| AtomicU32::new(0)).collect(),
            write: AtomicU64::new(0),
            read: AtomicU64::new(0),
            flush_to: AtomicU64::new(0),
        }
    }

    #[cfg(test)]
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    #[inline]
    fn mask(&self) -> u64 {
        self.buf.len() as u64 - 1
    }

    // ------------------------------------------------------------- producer

    pub fn write_pos(&self) -> u64 {
        self.write.load(Ordering::Relaxed)
    }

    /// Samples the producer may write now.
    pub fn free(&self) -> usize {
        let w = self.write.load(Ordering::Relaxed);
        let r = self
            .read
            .load(Ordering::Acquire)
            .max(self.flush_to.load(Ordering::Relaxed));
        self.buf.len() - (w - r.min(w)) as usize
    }

    /// Appends samples; returns how many fit.
    pub fn push(&self, samples: &[f32]) -> usize {
        let n = samples.len().min(self.free());
        let w = self.write.load(Ordering::Relaxed);
        let mask = self.mask();
        for (i, s) in samples[..n].iter().enumerate() {
            self.buf[((w + i as u64) & mask) as usize].store(s.to_bits(), Ordering::Relaxed);
        }
        self.write.store(w + n as u64, Ordering::Release);
        n
    }

    /// Marks everything written so far as stale. Returns the new start position.
    pub fn flush(&self) -> u64 {
        let w = self.write.load(Ordering::Relaxed);
        self.flush_to.store(w, Ordering::Release);
        w
    }

    // ------------------------------------------------------------- consumer

    /// Skips stale samples. Returns true when something was skipped.
    #[inline]
    pub fn skip_stale(&self) -> bool {
        let f = self.flush_to.load(Ordering::Acquire);
        let r = self.read.load(Ordering::Relaxed);
        if r < f {
            self.read.store(f, Ordering::Release);
            true
        } else {
            false
        }
    }

    /// Samples available to the consumer.
    #[inline]
    pub fn available(&self) -> usize {
        let w = self.write.load(Ordering::Acquire);
        let r = self.read.load(Ordering::Relaxed);
        (w - r.min(w)) as usize
    }

    /// Reads one sample (call only when `available() > 0`).
    #[inline]
    pub fn pop(&self) -> f32 {
        let r = self.read.load(Ordering::Relaxed);
        let v = f32::from_bits(self.buf[(r & self.mask()) as usize].load(Ordering::Relaxed));
        self.read.store(r + 1, Ordering::Release);
        v
    }

    pub fn read_pos(&self) -> u64 {
        self.read.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn push_pop_flush() {
        let r = Ring::new(8);
        assert_eq!(r.capacity(), 64);
        assert_eq!(r.push(&[1.0, 2.0, 3.0]), 3);
        assert_eq!(r.available(), 3);
        assert_eq!(r.pop(), 1.0);
        r.flush();
        assert!(r.skip_stale());
        assert_eq!(r.available(), 0);
        let big = vec![0.5f32; 100];
        assert_eq!(r.push(&big), 64);
        assert_eq!(r.free(), 0);
    }

    #[test]
    fn threads_preserve_order() {
        let r = Arc::new(Ring::new(1024));
        let p = Arc::clone(&r);
        let producer = std::thread::spawn(move || {
            let mut next = 0u32;
            while next < 200_000 {
                let chunk: Vec<f32> = (next..(next + 97).min(200_000)).map(|v| v as f32).collect();
                let n = p.push(&chunk);
                next += n as u32;
                if n == 0 {
                    std::thread::yield_now();
                }
            }
        });
        let mut expect = 0u32;
        while expect < 200_000 {
            if r.available() > 0 {
                assert_eq!(r.pop(), expect as f32);
                expect += 1;
            } else {
                std::thread::yield_now();
            }
        }
        producer.join().unwrap();
    }
}
