//! Render benchmark (v0.8 §105): `PlumSpirit --bench-render N file` opens
//! `file`, fits the song, then pans the Piano Roll a little every frame (back
//! and forth across a quarter of the view, so tiles are both reused and newly
//! built) for `N` frames with vsync off, logs the frame intervals of the
//! renderer in use and exits. Run it once with each renderer (View →
//! Renderer) on the same machine and file to compare them.

use std::time::Instant;

pub struct RenderBench {
    frames: usize,
    times_ms: Vec<f64>,
    last: Option<Instant>,
    origin: Option<(u64, u64)>,
}

impl RenderBench {
    pub fn new(frames: usize) -> RenderBench {
        RenderBench {
            frames: frames.max(10),
            times_ms: Vec::new(),
            last: None,
            origin: None,
        }
    }
}

/// Mean, median, 95th/99th percentiles and maximum of `v` (milliseconds).
pub fn summary(v: &[f64]) -> (f64, f64, f64, f64, f64) {
    if v.is_empty() {
        return (0.0, 0.0, 0.0, 0.0, 0.0);
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let at = |q: f64| s[((s.len() - 1) as f64 * q).round() as usize];
    (
        s.iter().sum::<f64>() / s.len() as f64,
        at(0.5),
        at(0.95),
        at(0.99),
        s[s.len() - 1],
    )
}

impl super::PlumSpiritApp {
    /// One benchmark step per frame (no-op without `--bench-render`).
    pub(super) fn bench_frame(&mut self, ctx: &egui::Context) {
        let Some(b) = self.bench.as_mut() else {
            return;
        };
        let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) else {
            ctx.request_repaint();
            return;
        };
        if self.job.is_some() {
            ctx.request_repaint();
            return;
        }
        let now = Instant::now();
        match b.origin {
            None => {
                self.roll.fit_song(&doc);
                let (t0, t1) = self
                    .roll
                    .view
                    .time
                    .visible_range(f64::from(self.roll_width));
                b.origin = Some((t0, t1.saturating_sub(t0).max(1)));
            }
            Some((t0, span)) => {
                if let Some(l) = b.last {
                    b.times_ms
                        .push(now.duration_since(l).as_secs_f64() * 1000.0);
                }
                let k = b.times_ms.len() as f64;
                // A slow sweep across ±¼ of the view.
                let off = (k * 0.05).sin() * span as f64 / 4.0;
                self.roll.view.time.origin_tick =
                    (t0 as f64 + span as f64 / 4.0 + off).max(0.0) as u64;
            }
        }
        b.last = Some(now);
        if b.times_ms.len() >= b.frames {
            let (mean, median, p95, p99, max) = summary(&b.times_ms);
            let line = format!(
                "render benchmark: {} · {} frames · {} notes · frame interval mean {mean:.2} ms, median {median:.2} ms, p95 {p95:.2} ms, p99 {p99:.2} ms, max {max:.2} ms ({:.0} fps)",
                self.renderer_info.line(),
                b.times_ms.len(),
                doc.note_count(),
                1000.0 / median.max(1e-6),
            );
            log::info!("{line}");
            println!("{line}");
            self.bench = None;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else {
            ctx.request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_summary_is_exact() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        let (mean, median, p95, p99, max) = summary(&v);
        assert_eq!(mean, 50.5);
        assert_eq!(median, 51.0);
        assert_eq!(p95, 95.0);
        assert_eq!(p99, 99.0);
        assert_eq!(max, 100.0);
        assert_eq!(summary(&[]), (0.0, 0.0, 0.0, 0.0, 0.0));
    }
}
