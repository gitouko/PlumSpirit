//! Transient previews outside tool windows (v0.7): the Piano Roll's scale
//! handles and Scale Y with Duplicate / Fill. Both evaluate an exact
//! [`BoxScale`] kernel over the viewport while the user drags or types, and
//! commit that same kernel as one undo step. v0.9 (§084, §085): Ctrl /
//! Ctrl+Shift corner drags preview and commit the exact [`CornerKernel`]
//! (Skew / Perspective on the shared geometry engine) the same way.

use super::dialogs::{Dialog, ScaleAxis};
use super::{PlumSpiritApp, StatusKind};
use crate::piano_roll::ToolPreview;
use plumspirit_engine::editor::ExactFactor;
use plumspirit_engine::midi::group_digits;
use plumspirit_engine::model::project::KeyRange;
use plumspirit_engine::transform::geom::{
    BoxScale, CornerKernel, CornerWarp, Fill, GeomBox, WarpKind,
};
use plumspirit_engine::transform::run::build_operation;
use plumspirit_engine::transform::{
    Boundary, Kernel, Limits, PointKernel, Source, TrackScope, TransformRun, run_kernel,
};

/// Selected notes up to which a cleaned transient preview is computed on the
/// UI thread; above it a worker computes it (B2-F28).
const SYNC_PREVIEW_NOTES: usize = 50_000;

/// The exact kernel a transient preview evaluates (and its commit runs).
pub enum TransientKernel {
    Scale(BoxScale),
    ScaleY(plumspirit_engine::editor::ScaleYKernel),
    Corner(CornerKernel),
}

impl TransientKernel {
    fn point(&self) -> Option<&dyn PointKernel> {
        match self {
            TransientKernel::Scale(k) => Some(k),
            TransientKernel::Corner(k) => Some(k),
            TransientKernel::ScaleY(_) => None,
        }
    }
}

impl TransientKernel {
    fn kernel(&self) -> &dyn Kernel {
        match self {
            Self::Scale(k) => k,
            Self::ScaleY(k) => k,
            Self::Corner(k) => k,
        }
    }
    /// The preview needs the whole kernel run (the cleanup or the shrink's
    /// collapse look at every result), not a per-note mapping.
    fn needs_run(&self) -> bool {
        match self {
            Self::Scale(k) => k.shrinks() || (k.prevent && k.fill.is_some()),
            Self::ScaleY(_) => true,
            Self::Corner(_) => false,
        }
    }
}

struct RunWorker {
    rx: std::sync::mpsc::Receiver<Result<TransformRun, String>>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}
impl Drop for RunWorker {
    fn drop(&mut self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

pub struct Transient {
    pub kernel: TransientKernel,
    /// The notes it previews on (the roll's target).
    pub track: plumspirit_engine::editor::NoteTarget,
    /// Notes outside the project's keys are dropped, as on commit.
    pub boundary: Boundary,
    key: String,
    /// The cleaned result and its longest note (the renderer's range query
    /// needs it; computed once, never per frame).
    run: Option<(TransformRun, u64)>,
    /// A handle drag's last cleaned result, shown while the next one computes.
    stale: Option<(TransformRun, u64)>,
    worker: Option<RunWorker>,
    error: Option<String>,
}

impl Transient {
    pub(super) fn stop_worker(&mut self) {
        self.worker = None;
    }
    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(w) = &self.worker {
            match w.rx.try_recv() {
                Ok(Ok(run)) => {
                    self.run = Some(with_max(run));
                    self.stale = None;
                    self.worker = None;
                }
                Ok(Err(e)) => {
                    self.error = Some(e);
                    self.worker = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(30))
                }
                Err(_) => {
                    self.error = Some("Scale preview worker stopped.".into());
                    self.worker = None;
                }
            }
        }
    }
}

fn with_max(run: TransformRun) -> (TransformRun, u64) {
    let max = run.notes.iter().map(|n| n.dur).max().unwrap_or(0);
    (run, max)
}

/// The transient preview for the roll's notes `ti`.
pub fn transient_preview(
    t: &Option<Transient>,
    ti: plumspirit_engine::editor::NoteTarget,
) -> Option<ToolPreview<'_>> {
    let t = t.as_ref()?;
    if t.track != ti {
        return None;
    }
    if let Some((r, max_dur)) = t.run.as_ref().or(t.stale.as_ref()) {
        return Some(ToolPreview::Run {
            notes: &r.notes,
            extra: &[],
            max_dur: *max_dur,
            all: false,
            rev: 0,
        });
    }
    if t.kernel.needs_run() {
        // While computing, hide selected source notes instead of displaying
        // an uncleaned mapping that Apply would never commit.
        return Some(ToolPreview::Run {
            notes: &[],
            extra: &[],
            max_dur: 0,
            all: false,
            rev: 0,
        });
    }
    Some(ToolPreview::Lazy {
        kernel: t.kernel.point()?,
        boundary: t.boundary,
        slot: 0,
        all: false,
        rev: 0,
    })
}

/// The undo label of a corner gesture.
pub fn warp_label(w: &CornerWarp) -> &'static str {
    match w.kind {
        WarpKind::SkewX | WarpKind::SkewY => "Skew Selection",
        WarpKind::Perspective => "Perspective Selection",
        WarpKind::RotateCenter | WarpKind::RotateCorner => "Rotate Selection",
    }
}

/// B1-0091: Duplicate / Fill of a corner gesture — Alt with Ctrl (Skew /
/// Perspective); a rotation (Alt alone) never fills.
fn corner_fill(w: &CornerWarp, alt: bool) -> Option<Fill> {
    (!w.kind.is_rotation() && alt).then(Fill::default)
}

impl PlumSpiritApp {
    /// The exact kernel of a scale-handle drag from `from` to `to`.
    /// Duplicate Notes when Transform → Scale Handles: Duplicate Notes is on
    /// (B1-0115), reversed for a drag that started with Alt held — with the
    /// last Scale Y Duplicate Notes options; ordinary scaling stays N → N
    /// (v0.8 §190). The Scale Y overlap prevention (B1-0116) applies when the
    /// drag changes the number of rows.
    pub(super) fn handle_kernel(&self, from: GeomBox, to: GeomBox, alt: bool) -> BoxScale {
        let p = &self.settings.scale_y;
        let mut k = BoxScale::plain(from, to);
        k.fill = (p.handles_duplicate != alt).then_some(p.fill);
        k.prevent = p.prevent && k.scales_y();
        k.cleanup = p.cleanup;
        k
    }

    /// Keeps the transient preview in step with the handle drag and the Scale Y
    /// Fill dialog (called once per frame after the Piano Roll).
    pub(super) fn update_transient(&mut self, ctx: &egui::Context) {
        let ti = self.edit_target();
        let keys = self.project_keys();
        let boundary = Boundary::default().within(keys);
        let spec = if let Some((from, to, alt)) = self.roll.scale_drag() {
            Some((
                true,
                format!("{from:?}|{to:?}|{alt}|{:?}", self.settings.scale_y),
                Ok(TransientKernel::Scale(self.handle_kernel(from, to, alt))),
            ))
        } else if let Some((from, warp, alt)) = self.roll.corner_drag() {
            Some((
                true,
                format!("{from:?}|{warp:?}|{alt}"),
                Ok(TransientKernel::Corner(warp.kernel(
                    from,
                    corner_fill(&warp, alt),
                    keys,
                ))),
            ))
        } else if let Some(Dialog::Scale {
            axis: ScaleAxis::Y,
            text,
            fill_on,
            fill,
            preview: true,
            prevent,
            cleanup,
            ..
        }) = &self.dialog
        {
            let opts = plumspirit_engine::editor::ScaleY {
                fill: fill_on.then_some(*fill),
                prevent_overlap: *prevent,
                cleanup: *cleanup,
            };
            let key = format!("{}|{opts:?}", text.trim());
            let kernel = self
                .doc()
                .ok_or_else(|| "Open a file first.".to_owned())
                .and_then(|d| {
                    let t = plumspirit_engine::editor::note_view(d, ti)
                        .map_err(|e| e.user_message())?;
                    let f = ExactFactor::parse(text).map_err(|e| e.to_string())?;
                    plumspirit_engine::editor::scale_y_run_kernel(t, &f, opts, keys)
                        .map(TransientKernel::ScaleY)
                        .map_err(|e| e.user_message())
                });
            Some((false, key, kernel))
        } else {
            None
        };
        // `drag`: a handle / corner drag keeps its last cleaned result on
        // screen while the next position computes (no flicker).
        let Some((drag, params, kernel)) = spec else {
            self.transient = None;
            return;
        };
        let revision = self.edit_notes().map_or(0, |n| n.revision());
        let key = format!("{params}|{ti:?}|{revision}|{keys:?}");
        if self.transient.as_ref().is_none_or(|t| t.key != key) {
            // Cancel / join the previous generation.
            let stale = self
                .transient
                .take()
                .filter(|_| drag)
                .and_then(|t| t.run.or(t.stale));
            match kernel {
                Err(e) => {
                    if let Some(Dialog::Scale { summary, .. }) = &mut self.dialog {
                        *summary = e;
                    }
                    return;
                }
                Ok(kernel) => {
                    let mut t = Transient {
                        kernel,
                        track: ti,
                        boundary,
                        key,
                        run: None,
                        stale,
                        worker: None,
                        error: None,
                    };
                    if t.kernel.needs_run() {
                        let doc = self.open.as_ref().map(|o| o.doc.clone());
                        if let Some(doc) = doc {
                            let limits =
                                Limits::for_system_memory(crate::platform::total_memory_bytes());
                            let selected = self.edit_notes().map_or(0, |n| n.selected_count());
                            if selected <= SYNC_PREVIEW_NOTES {
                                match Source::collect(
                                    &doc,
                                    &[TrackScope::of(ti, false)],
                                    limits.cap(),
                                )
                                .and_then(|src| {
                                    run_kernel(t.kernel.kernel(), &src, boundary, &limits, None)
                                }) {
                                    Ok(run) => t.run = Some(with_max(run)),
                                    Err(e) => t.error = Some(e.user_message()),
                                }
                            } else {
                                let k = match &t.kernel {
                                    TransientKernel::Scale(k) => TransientKernel::Scale(*k),
                                    TransientKernel::ScaleY(k) => {
                                        TransientKernel::ScaleY(k.clone())
                                    }
                                    _ => unreachable!(),
                                };
                                let (tx, rx) = std::sync::mpsc::channel();
                                let cancel =
                                    std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                                let c = cancel.clone();
                                let wake = ctx.clone();
                                let handle = std::thread::spawn(move || {
                                    let r = Source::collect(
                                        &doc,
                                        &[TrackScope::of(ti, false)],
                                        limits.cap(),
                                    )
                                    .and_then(|src| {
                                        run_kernel(k.kernel(), &src, boundary, &limits, Some(c))
                                    })
                                    .map_err(|e| e.user_message());
                                    let _ = tx.send(r);
                                    wake.request_repaint();
                                });
                                t.worker = Some(RunWorker {
                                    rx,
                                    cancel,
                                    handle: Some(handle),
                                });
                            }
                        }
                    }
                    self.transient = Some(t);
                }
            }
            ctx.request_repaint();
        }
        if let Some(t) = &mut self.transient {
            t.poll(ctx);
            if let Some(Dialog::Scale { summary, .. }) = &mut self.dialog {
                *summary = t.error.clone().unwrap_or_else(|| {
                    t.run.as_ref().map_or_else(
                        || "Preview: computing cleaned notes…".into(),
                        |(r, _)| r.summary(),
                    )
                });
            }
        }
    }

    fn project_keys(&self) -> KeyRange {
        self.open
            .as_ref()
            .map_or_else(KeyRange::default, |o| o.doc.project().keys)
    }

    /// Commits a scale-handle drag: the selection's box `from` onto `to`.
    pub(super) fn commit_scale_box(&mut self, from: GeomBox, to: GeomBox, alt: bool) {
        let kernel = self.handle_kernel(from, to, alt);
        let duplicated = kernel.fill.is_some();
        let size = format!(
            "{} ticks × {} rows",
            group_digits(to.t1 - to.t0),
            u32::from(to.p1 - to.p0) + 1
        );
        let label = if duplicated {
            "Scale Selection (Duplicate Notes)"
        } else {
            "Scale Selection"
        };
        self.commit_handle_kernel(&kernel, label, "Scale was not applied.", |r| {
            let mut s = format!(
                "Scaled {} note(s) to {size}.",
                group_digits(r.source_count as u64)
            );
            if duplicated {
                s.push_str(&format!(
                    " {} note(s) after duplicating.",
                    group_digits(r.notes.len() as u64)
                ));
            }
            if r.stats.overlaps_removed > 0 || r.stats.overlaps_trimmed > 0 {
                s.push_str(&format!(
                    " Cleanup: {} removed, {} shortened.",
                    group_digits(r.stats.overlaps_removed),
                    group_digits(r.stats.overlaps_trimmed)
                ));
            }
            if r.stats.collapsed > 0 {
                s.push_str(&format!(
                    " {} duplicate(s) the shrink created removed.",
                    group_digits(r.stats.collapsed)
                ));
            }
            s
        });
    }

    /// Commits a Ctrl / Ctrl+Shift corner drag of the selection's box `from`.
    pub(super) fn commit_warp_box(&mut self, from: GeomBox, warp: CornerWarp, alt: bool) {
        let kernel = warp.kernel(from, corner_fill(&warp, alt), self.project_keys());
        if warp.kind.is_rotation() {
            let deg = warp.degrees();
            let deg = if deg > 180.0 { deg - 360.0 } else { deg };
            self.commit_handle_kernel(
                &kernel,
                warp_label(&warp),
                "Rotation was not applied.",
                |r| {
                    format!(
                        "Rotated {} note(s) by {deg:.1}° ({} note(s) after rasterizing).",
                        group_digits(r.source_count as u64),
                        group_digits(r.notes.len() as u64)
                    )
                },
            );
            return;
        }
        let (failed, done) = match warp.kind {
            WarpKind::SkewX | WarpKind::SkewY => ("Skew was not applied.", "Skewed"),
            _ => ("Perspective was not applied.", "Applied Perspective to"),
        };
        self.commit_handle_kernel(&kernel, warp_label(&warp), failed, |r| {
            format!("{done} {} note(s).", group_digits(r.source_count as u64))
        });
    }

    /// Runs a handle gesture's kernel over the active track's selection and
    /// commits it as one undo step `label`.
    fn commit_handle_kernel(
        &mut self,
        kernel: &dyn Kernel,
        label: &'static str,
        failed: &str,
        status: impl FnOnce(&TransformRun) -> String,
    ) {
        self.release_preview_worker();
        self.transient = None;
        let ti = self.edit_target();
        let Some(doc) = self.doc() else {
            return;
        };
        let limits = Limits::for_system_memory(crate::platform::total_memory_bytes());
        let scope = [TrackScope::of(ti, false)];
        // The project's keys, as the preview (copies a pitch step apart can
        // reach past them).
        let boundary = Boundary::default().within(doc.project().keys);
        // B1-0117: the General Note Overlap Prevention, when on.
        let general = self.general_overlap();
        let mut prevented = None;
        let run = Source::collect(doc, &scope, limits.cap()).and_then(|src| {
            let mut run = run_kernel(kernel, &src, boundary, &limits, None)?;
            if let Some(g) = general {
                prevented = Some(plumspirit_engine::transform::overlap::prevent_in_run(
                    doc, &src, &mut run, g,
                ));
            }
            Ok(run)
        });
        let run = match run {
            Ok(r) => r,
            Err(e) => {
                self.show_error(failed, e.user_message());
                return;
            }
        };
        let op = match self.doc_mut() {
            Some(d) => build_operation(d, &run, label),
            None => return,
        };
        match op {
            Ok(op) => {
                if self.apply_op(op).is_some() {
                    let s = status(&run) + &super::editing::overlap_feedback(prevented);
                    self.set_status(s, StatusKind::Info);
                }
            }
            Err(e) => self.show_error(failed, e.user_message()),
        }
    }
}

#[cfg(test)]
mod tests {
    //! B1-S38: the scale handles and Scale Y with Duplicate Notes and the
    //! Scale Y overlap prevention, through the app's own entry points.
    use super::super::commands::Command;
    use super::super::test_support::Harness;
    use super::*;
    use plumspirit_engine::model::{
        Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, SubchannelId,
    };
    use plumspirit_engine::transform::geom::FillMode;

    fn rec(start: u64, pitch: u8) -> NoteRecord {
        NoteRecord {
            start_tick: start,
            duration_ticks: 480,
            attrs: NoteAttrs::new(pitch, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
            sub: SubchannelId(0),
            group: 0,
        }
    }

    /// A selected 4-row column of art (60 – 63).
    fn harness() -> Harness {
        let mut d = PlumDocument::new_project("b1s38".into(), Bpm::DEFAULT, 960);
        let v: Vec<NoteRecord> = (60..=63).map(|p| rec(0, p)).collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&v, true)
            .unwrap();
        let mut h = Harness::new(d);
        h.app.main_view = super::super::track_list::MainView::PianoRoll;
        h
    }

    fn count(h: &Harness) -> usize {
        h.doc().track(0).unwrap().notes.len()
    }

    const FROM: GeomBox = GeomBox {
        t0: 0,
        t1: 480,
        p0: 60,
        p1: 63,
    };
    /// 4 → 10 rows (×2.5).
    const TALL: GeomBox = GeomBox { p1: 69, ..FROM };

    #[test]
    fn the_handles_duplicate_only_when_asked_and_alt_reverses_it() {
        let mut h = harness();
        assert!(!h.app.settings.scale_y.handles_duplicate, "default off");
        assert!(h.app.settings.scale_y.prevent, "default on");
        // Off: N → N (v0.8 §190); Alt duplicates for one drag.
        assert!(h.app.handle_kernel(FROM, TALL, false).fill.is_none());
        assert!(h.app.handle_kernel(FROM, TALL, true).fill.is_some());
        h.app.commit_scale_box(FROM, TALL, false);
        assert_eq!(count(&h), 4);
        h.app.undo();
        // On (Transform → Scale Handles: Duplicate Notes): the stretch fills
        // the added rows; Alt keeps a drag N → N.
        assert!(h.command(Command::ToggleScaleYDuplicate));
        assert!(h.app.settings.scale_y.handles_duplicate);
        assert!(h.app.handle_kernel(FROM, TALL, true).fill.is_none());
        h.app.commit_scale_box(FROM, TALL, false);
        assert_eq!(count(&h), 10, "every added row filled once");
        // One undo step.
        h.app.undo();
        assert_eq!(count(&h), 4);
        // Copy by pitch step (Auto) with prevention: one thickness, the
        // doubled rows go — the same solid art.
        h.app.settings.scale_y.fill.mode = FillMode::Step { step: 1, copies: 0 };
        let k = h.app.handle_kernel(FROM, TALL, false);
        assert!(k.prevent, "the drag changes the rows");
        h.app.commit_scale_box(FROM, TALL, false);
        assert_eq!(count(&h), 10);
        h.app.undo();
        // Prevention off: the step copies double rows 62 and 67.
        assert!(h.command(Command::ToggleScaleYOverlap));
        h.app.commit_scale_box(FROM, TALL, false);
        assert_eq!(count(&h), 12);
        h.app.undo();
        // A drag that changes only the time never turns prevention on.
        h.app.settings.scale_y.prevent = true;
        let wide = GeomBox { t1: 960, ..FROM };
        assert!(!h.app.handle_kernel(FROM, wide, false).prevent);
    }

    #[test]
    fn scale_y_modes_apply_remember_their_options_and_repeat() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        let step = Fill {
            mode: FillMode::Step { step: 1, copies: 0 },
            ..Fill::default()
        };
        let opts = plumspirit_engine::editor::ScaleY {
            fill: Some(step),
            prevent_overlap: true,
            cleanup: Default::default(),
        };
        h.app.apply_scale(ScaleAxis::Y, "2.5", opts, &ctx).unwrap();
        // Pivot-centered ×2.5 of 60 – 63 (pivot 61.5): rows 58 – 65, every
        // row once.
        assert_eq!(count(&h), 8);
        let mut rows: Vec<u8> = h
            .doc()
            .track(0)
            .unwrap()
            .notes
            .iter()
            .map(|r| r.attrs.pitch())
            .collect();
        rows.sort_unstable();
        assert_eq!(rows, (58..=65).collect::<Vec<u8>>());
        // The options the dialog starts from next time; Duplicate Notes
        // itself is not remembered (§191).
        assert_eq!(h.app.settings.scale_y.fill.mode, step.mode);
        assert!(h.command(Command::ScaleY) || h.app.dialog.is_some());
        match &h.app.dialog {
            Some(Dialog::Scale {
                fill_on,
                fill,
                prevent,
                ..
            }) => {
                assert!(!*fill_on);
                assert_eq!(fill.mode, step.mode);
                assert!(*prevent);
            }
            other => panic!("{:?}", other.is_some()),
        }
        h.app.dialog = None;
        // Transform existing notes: N → N.
        h.app.undo();
        h.app
            .apply_scale(ScaleAxis::Y, "2", Default::default(), &ctx)
            .unwrap();
        assert_eq!(count(&h), 4);
        // Repeat Last Tool repeats the recipe with its options.
        assert!(matches!(
            h.app.last_recipe,
            Some(super::super::tools::Recipe::Scale { opts, .. }) if opts.fill.is_none()
        ));
    }

    /// B2-F28: the Scale Y dialog's live Preview is the cleaned result Apply
    /// commits (same kernel), in every cleanup mode, with the cleanup counts
    /// in the summary; Apply is one undo step.
    #[test]
    fn the_scale_y_preview_shows_what_apply_commits_in_every_cleanup_mode() {
        use plumspirit_engine::transform::geom::ScaleYCleanup;
        for cleanup in ScaleYCleanup::ALL {
            // 60 / 61 meet on one key under ×0.5; 61 / 62 at staggered times.
            let mut d = PlumDocument::new_project("b2s37".into(), Bpm::DEFAULT, 960);
            let v = vec![
                rec(0, 60),
                rec(0, 61),
                rec(100, 62),
                rec(150, 63),
                rec(240, 64),
            ];
            d.track_mut(0)
                .unwrap()
                .notes
                .insert_sorted_batch(&v, true)
                .unwrap();
            let mut h = Harness::new(d);
            h.app.main_view = super::super::track_list::MainView::PianoRoll;
            let ctx = h.ctx.clone();
            let before = h.doc().fingerprint();
            h.app.dialog = Some(Dialog::Scale {
                axis: ScaleAxis::Y,
                text: "0.5".into(),
                error: None,
                focused: false,
                preview: true,
                summary: String::new(),
                fill_on: false,
                fill: Fill::default(),
                prevent: true,
                cleanup,
            });
            h.app.update_transient(&ctx);
            let ti = h.app.edit_target();
            let Some(ToolPreview::Run { notes, .. }) = transient_preview(&h.app.transient, ti)
            else {
                panic!("{cleanup:?}: a run preview");
            };
            let mut shown: Vec<(u64, u64, u8)> =
                notes.iter().map(|n| (n.start, n.dur, n.pitch)).collect();
            shown.sort_unstable();
            let Some(Dialog::Scale { summary, .. }) = &h.app.dialog else {
                panic!()
            };
            if cleanup == ScaleYCleanup::ExactDuplicates {
                assert!(summary.contains("Overlaps: 1 removed"), "{summary}");
            }
            let opts = plumspirit_engine::editor::ScaleY {
                fill: None,
                prevent_overlap: true,
                cleanup,
            };
            h.app.dialog = None;
            h.app.apply_scale(ScaleAxis::Y, "0.5", opts, &ctx).unwrap();
            let mut got: Vec<(u64, u64, u8)> = h
                .doc()
                .track(0)
                .unwrap()
                .notes
                .iter()
                .map(|r| (r.start_tick, r.duration_ticks, r.attrs.pitch()))
                .collect();
            got.sort_unstable();
            assert_eq!(got, shown, "{cleanup:?}");
            h.app.undo();
            assert_eq!(h.doc().fingerprint(), before, "{cleanup:?}: one undo step");
        }
    }

    #[test]
    fn an_alt_corner_rotation_commits_one_undo_step_and_never_fills() {
        use plumspirit_engine::transform::geom::{Corner, CornerWarp};
        let mut h = harness();
        let before = h.doc().fingerprint();
        // A quarter turn of the 4-row column about its center (the box looks
        // square when one row is 120 ticks tall on screen).
        let w = CornerWarp::rotation(Corner::TopRight, true, 90.0, 120.0);
        h.app.commit_warp_box(FROM, w, true);
        let n = count(&h);
        assert!(n >= 4, "{n}");
        assert_ne!(h.doc().fingerprint(), before);
        let status = h
            .app
            .status
            .as_ref()
            .map(|s| s.text.clone())
            .unwrap_or_default();
        assert!(status.starts_with("Rotated 4 note(s) by 90.0°"), "{status}");
        h.app.undo();
        assert_eq!(h.doc().fingerprint(), before);
        assert_eq!(super::warp_label(&w), "Rotate Selection");
        assert!(
            super::corner_fill(&w, true).is_none(),
            "Alt is the rotate key"
        );
    }
}
