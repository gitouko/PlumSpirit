//! Project Properties → Change PPQN (Experimental) — v1.0-beta1 B1-0103.
//!
//! The dialog's Preview runs [`prepare_change_ppqn`] on a worker (one pass
//! over every note, about 30 ms per million notes); Apply swaps the prepared
//! timebase in as one undo step ([`Operation::SetTimebase`]), reusing the
//! Preview when the inputs and the project are unchanged. In Preserve beats
//! mode the views, playheads and the time selection follow the rescaled
//! ticks — after Apply and after Undo / Redo of the step.

use super::PlumSpiritApp;
use super::dialogs::{ChangePpqnDialog, Dialog};
use super::jobs;
use crate::ui::status_bar::StatusKind;
use plumspirit_engine::editor::ppqn::{PpqnMode, PpqnReport, Timebase, current_ppqn};
use plumspirit_engine::history::Operation;
use plumspirit_engine::midi::group_digits;
use plumspirit_engine::model::PlumDocument;

/// A computed change waiting for Apply.
pub(super) struct PreparedPpqn {
    pub to: u32,
    pub mode: PpqnMode,
    pub key: u64,
    pub timebase: Box<Timebase>,
    pub report: PpqnReport,
}

/// Everything a prepared change read, cheaply: content, events, clips,
/// tracks, track properties, the Conductor, the division and the project's
/// tick settings.
pub(super) fn state_key(doc: &PlumDocument) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    doc.analysis_revision().hash(&mut h);
    doc.conductor().revision().hash(&mut h);
    doc.division().encoded().hash(&mut h);
    for t in doc.tracks() {
        t.props_revision().hash(&mut h);
    }
    let p = doc.project();
    p.looping.range.hash(&mut h);
    p.zoom_regions.hash(&mut h);
    p.track_list.hash(&mut h);
    h.finish()
}

/// `round_half_up(t × to / from)`, saturating (view state only).
fn rescale(t: u64, from: u32, to: u32) -> u64 {
    let from = u128::from(from.max(1));
    let v = (u128::from(t) * u128::from(to) * 2 + from) / (2 * from);
    u64::try_from(v).unwrap_or(u64::MAX)
}

impl PlumSpiritApp {
    pub(super) fn open_change_ppqn(&mut self) {
        let Some(d) = self.doc() else {
            return;
        };
        match current_ppqn(d) {
            Ok(from) => {
                self.ppqn_prepared = None;
                self.dialog = Some(Dialog::ChangePpqn(ChangePpqnDialog::new(from)));
            }
            Err(why) => {
                if let Some(Dialog::ProjectProperties { error, .. }) = self.dialog.as_mut() {
                    *error = Some(why);
                } else {
                    self.set_status(why, StatusKind::Warning);
                }
            }
        }
    }

    /// Preview (`apply` false) or Apply (`apply` true) from the dialog.
    pub(super) fn ppqn_request(&mut self, apply: bool, ctx: &egui::Context) {
        let Some(Dialog::ChangePpqn(d)) = self.dialog.as_mut() else {
            return;
        };
        let (to, mode) = match d.inputs() {
            Ok(i) => i,
            Err(e) => {
                d.error = Some(e);
                return;
            }
        };
        d.error = None;
        let Some(o) = self.open.as_ref() else {
            return;
        };
        let key = state_key(&o.doc);
        // A Preview of other inputs (or of another project state) is dropped.
        if apply
            && let Some(p) = self.ppqn_prepared.take()
            && (p.to, p.mode, p.key) == (to, mode, key)
        {
            self.apply_ppqn(p);
            return;
        }
        if self.job.is_some() {
            return;
        }
        let doc = o.doc.clone();
        let id = self.next_id();
        match jobs::spawn_ppqn(id, doc, to, mode, apply, key, ctx) {
            Ok(job) => {
                self.job = Some(job);
                if let Some(Dialog::ChangePpqn(d)) = self.dialog.as_mut() {
                    d.busy = true;
                }
            }
            Err(e) => self.show_error("Could not start Change PPQN.", e.to_string()),
        }
    }

    /// The worker's result: the Preview, or Apply when it was asked for.
    pub(super) fn finish_ppqn_job(
        &mut self,
        to: u32,
        mode: PpqnMode,
        apply: bool,
        key: u64,
        r: Result<(Box<Timebase>, PpqnReport), String>,
    ) {
        let open = matches!(self.dialog, Some(Dialog::ChangePpqn(_)));
        if let Some(Dialog::ChangePpqn(d)) = self.dialog.as_mut() {
            d.busy = false;
        }
        let (timebase, report) = match r {
            Ok(x) => x,
            Err(why) => {
                match self.dialog.as_mut() {
                    Some(Dialog::ChangePpqn(d)) => d.error = Some(why),
                    _ => self.set_status(why, StatusKind::Error),
                }
                return;
            }
        };
        let p = PreparedPpqn {
            to,
            mode,
            key,
            timebase,
            report,
        };
        if apply {
            self.apply_ppqn(p);
        } else if open {
            if let Some(Dialog::ChangePpqn(d)) = self.dialog.as_mut() {
                d.preview = Some(((to, mode), p.report.lines()));
            }
            self.ppqn_prepared = Some(p);
        }
    }

    fn apply_ppqn(&mut self, p: PreparedPpqn) {
        if self.doc().is_none_or(|d| state_key(d) != p.key) {
            if let Some(Dialog::ChangePpqn(d)) = self.dialog.as_mut() {
                d.preview = None;
                d.error =
                    Some("The project changed since the Preview; press Preview again.".into());
            }
            return;
        }
        let from = p.report.from;
        self.halt_for_retime();
        self.roll.cancel_gesture();
        self.graphs.cancel();
        let label = "Change PPQN";
        let op = Operation::SetTimebase {
            timebase: p.timebase,
            label,
        };
        if self.apply_op(op).is_some() {
            if p.mode == PpqnMode::Beats {
                self.retime_views(from, p.to);
            }
            self.dialog = None;
            let exact = if p.report.is_exact() {
                "exact".to_string()
            } else {
                format!(
                    "{} positions rounded, {} notes share a tick and key",
                    group_digits(p.report.rounded_ticks),
                    group_digits(p.report.collisions)
                )
            };
            self.set_status(
                format!(
                    "The project now uses {} PPQN ({}, {exact}). Undo restores {} PPQN.",
                    group_digits(u64::from(p.to)),
                    match p.mode {
                        PpqnMode::Beats => "beats kept",
                        PpqnMode::Seconds => "seconds kept",
                    },
                    group_digits(u64::from(from))
                ),
                StatusKind::Info,
            );
        } else if matches!(self.dialog, Some(Dialog::ChangePpqn(_))) {
            // Refused (the reason is on the status bar) — the dialog stays.
            if let Some(Dialog::ChangePpqn(d)) = self.dialog.as_mut() {
                d.preview = None;
            }
        }
    }

    /// Playback stops before the ticks change under it (without revealing
    /// the playhead: the view keeps its place).
    pub(super) fn halt_for_retime(&mut self) {
        if self.transport.state().is_running() {
            self.transport.stop();
        }
        self.preroll_watch = None;
    }

    /// The app's own tick state follows a rescale from `from` to `to` PPQN:
    /// the Piano Roll and Track List views (same place, same zoom on screen),
    /// the edit cursor, the playheads and the time selection.
    pub(super) fn retime_views(&mut self, from: u32, to: u32) {
        let r = |t: u64| rescale(t, from, to);
        let zoom =
            |ppt: f64| plumspirit_engine::view::clamp_zoom(ppt * f64::from(from) / f64::from(to));
        for view in [&mut self.roll.view, &mut self.track_list.view] {
            view.time.origin_tick = r(view.time.origin_tick);
            view.time.pixels_per_tick = zoom(view.time.pixels_per_tick);
        }
        self.roll.cursor_tick = r(self.roll.cursor_tick);
        self.roll.anchor = self.roll.anchor.map(r);
        self.time_selection = self.time_selection.map(|(a, b)| {
            let (a, b) = (r(a), r(b));
            (a, b.max(a.saturating_add(1)))
        });
        self.transport.rescale_ticks(r);
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::Harness;
    use super::*;
    use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord};

    fn song() -> PlumDocument {
        let mut d = PlumDocument::new_project("ppqn".into(), Bpm::DEFAULT, 960);
        let recs: Vec<NoteRecord> = (0..64u64)
            .map(|k| {
                NoteRecord::new(
                    k * 241,
                    240,
                    NoteAttrs::new(60 + (k % 12) as u8, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                )
            })
            .collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        d
    }

    fn wait_job(h: &mut Harness) {
        for _ in 0..500 {
            if h.app.job.is_none() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            let (ctx, frame) = (h.ctx.clone(), eframe::Frame::_new_kittest());
            h.app.poll_job(&ctx, &frame);
        }
        panic!("the PPQN job did not finish");
    }

    #[test]
    fn change_ppqn_previews_applies_as_one_step_and_views_follow_undo() {
        let mut h = Harness::new(song());
        h.app.open_properties();
        h.app.open_change_ppqn();
        let Some(Dialog::ChangePpqn(d)) = h.app.dialog.as_mut() else {
            panic!("dialog");
        };
        assert_eq!(d.from, 960);
        d.to = "480".into();
        h.app.roll.view.time.origin_tick = 9_600;
        h.app.roll.view.time.pixels_per_tick = 0.05;
        h.app.time_selection = Some((961, 1_921));
        let ctx = h.ctx.clone();
        h.app.ppqn_request(false, &ctx);
        wait_job(&mut h);
        let Some(Dialog::ChangePpqn(d)) = h.app.dialog.as_ref() else {
            panic!("dialog");
        };
        let lines = &d.preview.as_ref().expect("preview").1;
        assert!(lines.iter().any(|l| l.contains("rounded")), "{lines:?}");
        assert!(h.app.ppqn_prepared.is_some());
        let steps = h.app.open.as_ref().unwrap().history.undo_len();
        h.app.ppqn_request(true, &ctx);
        assert!(h.app.job.is_none(), "Apply reuses the Preview");
        assert!(h.app.dialog.is_none());
        assert_eq!(h.doc().division().musical_ppqn(), Some(480));
        assert_eq!(h.app.open.as_ref().unwrap().history.undo_len(), steps + 1);
        assert_eq!(h.app.roll.view.time.origin_tick, 4_800);
        assert!((h.app.roll.view.time.pixels_per_tick - 0.1).abs() < 1e-12);
        assert_eq!(h.app.time_selection, Some((481, 961)));
        assert_eq!(
            h.doc().tracks()[0].notes.iter().nth(1).unwrap().start_tick,
            121
        );
        h.app.undo();
        assert_eq!(h.doc().division().musical_ppqn(), Some(960));
        assert_eq!(h.app.roll.view.time.origin_tick, 9_600);
        assert_eq!(h.app.time_selection, Some((962, 1_922)));
        h.app.redo();
        assert_eq!(h.doc().division().musical_ppqn(), Some(480));
        assert_eq!(h.app.roll.view.time.origin_tick, 4_800);
    }

    /// The whole application frame shows the Project Properties button and
    /// the dialog's fields, both modes and the Preview lines.
    #[test]
    fn the_dialog_draws_its_fields_modes_and_preview() {
        let mut h = Harness::new(song());
        let size = egui::vec2(1600.0, 1000.0);
        let shown = |h: &mut Harness| -> Vec<String> {
            crate::ui::testkit::texts(&h.app_frame(size, 1.0))
                .into_iter()
                .map(|t| t.0)
                .collect()
        };
        h.app.open_properties();
        h.app_frame(size, 1.0);
        let t = shown(&mut h);
        assert!(
            t.iter().any(|x| x == "Change PPQN (Experimental)…"),
            "{t:?}"
        );
        h.app.open_change_ppqn();
        if let Some(Dialog::ChangePpqn(d)) = h.app.dialog.as_mut() {
            d.to = "480".into();
        }
        h.app_frame(size, 1.0);
        let t = shown(&mut h);
        for want in [
            "Change PPQN (Experimental)",
            "Current PPQN",
            "960",
            "New PPQN",
            PpqnMode::Beats.label(),
            PpqnMode::Seconds.label(),
            "Preview",
            "Apply",
        ] {
            assert!(t.iter().any(|x| x == want), "{want} in {t:?}");
        }
        let ctx = h.ctx.clone();
        h.app.ppqn_request(false, &ctx);
        wait_job(&mut h);
        h.app_frame(size, 1.0);
        let t = shown(&mut h);
        assert!(
            t.iter()
                .any(|x| x.starts_with("64 notes and 0 events are rescaled")),
            "{t:?}"
        );
    }

    #[test]
    fn preserve_seconds_keeps_the_views_and_a_stale_preview_is_refused() {
        let mut h = Harness::new(song());
        h.app.open_change_ppqn();
        let Some(Dialog::ChangePpqn(d)) = h.app.dialog.as_mut() else {
            panic!("dialog");
        };
        d.to = "1920".into();
        d.mode = PpqnMode::Seconds;
        h.app.roll.view.time.origin_tick = 9_600;
        let ctx = h.ctx.clone();
        h.app.ppqn_request(false, &ctx);
        wait_job(&mut h);
        // The inputs changed after the Preview: Apply computes again.
        if let Some(Dialog::ChangePpqn(d)) = h.app.dialog.as_mut() {
            d.to = "240".into();
        }
        h.app.ppqn_request(true, &ctx);
        assert!(h.app.job.is_some(), "a new computation");
        wait_job(&mut h);
        assert!(h.app.dialog.is_none());
        assert_eq!(h.doc().division().musical_ppqn(), Some(240));
        assert_eq!(h.app.roll.view.time.origin_tick, 9_600, "ticks kept");
        assert_eq!(
            h.doc().tracks()[0].notes.iter().nth(1).unwrap().start_tick,
            241
        );
        h.app.undo();
        assert_eq!(h.doc().division().musical_ppqn(), Some(960));
        assert_eq!(h.app.roll.view.time.origin_tick, 9_600);
    }
}
