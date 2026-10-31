//! The live Preview of batch note operations (v0.5), for the Scale
//! dialogs — and, through the same engine kernels, every future batch tool.
//!
//! * The kernel (the operation's per-note rule) is rebuilt only when the dialog's
//!   parameters change; the piano roll draws its results for the notes near the
//!   viewport every frame (bounded work, no copy of the notes).
//! * The full-selection summary ("how many notes change, would anything be
//!   refused") runs on a worker thread for large selections. Each request gets a
//!   generation id; a result for older parameters is discarded, and a running
//!   worker is cancelled when the parameters change.
//! * Nothing here touches the document: no dirty state, no undo entry. Apply
//!   runs the operation's own `prepare_*` function (the same kernel) through the
//!   undo history; Cancel drops the state.

use super::PlumSpiritApp;
use super::dialogs::{Dialog, ScaleAxis};
use super::tracks::{DraftCorrections, parse_corrections};
use plumspirit_engine::editor::ExactFactor;
use plumspirit_engine::editor::preview::{
    NoteKernel, PreviewGeneration, PreviewScope, PreviewSummary, summarize,
};
use plumspirit_engine::editor::transforms::scale_y_kernel;
use plumspirit_engine::midi::group_digits;
use plumspirit_engine::model::correction::Correction;
use plumspirit_engine::model::{PlumDocument, TrackId};
use plumspirit_engine::view::grid::MeterMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::thread::JoinHandle;

/// Selections up to this size are summarised on the UI thread (a few ms);
/// larger ones on a worker.
const SYNC_SUMMARY_LIMIT: usize = 200_000;

struct Worker {
    generation: u64,
    rx: Receiver<(u64, PreviewSummary)>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Worker {
    fn stop(mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

pub struct PreviewState {
    /// Parameters the kernel was built for (dialog kind + fields + notes).
    key: String,
    pub kernel: Option<Arc<NoteKernel>>,
    pub scope: PreviewScope,
    pub summary: Option<PreviewSummary>,
    /// Why no kernel could be built (invalid factor, out of range …).
    pub error: Option<String>,
    generation: PreviewGeneration,
    worker: Option<Worker>,
}

impl PreviewState {
    fn new() -> Self {
        PreviewState {
            key: String::new(),
            kernel: None,
            scope: PreviewScope::Selected,
            summary: None,
            error: None,
            generation: PreviewGeneration::default(),
            worker: None,
        }
    }

    pub(super) fn stop_worker(&mut self) {
        if let Some(w) = self.worker.take() {
            w.stop();
        }
    }
}

/// The dialog parameters a preview depends on.
fn dialog_params(dialog: &Dialog) -> Option<(String, Params)> {
    match dialog {
        // Duplicate / Fill previews through the transient tool preview.
        Dialog::Scale {
            axis: ScaleAxis::Y, ..
        } => None,
        Dialog::Scale { axis, text, .. } => Some((
            format!("scale:{axis:?}:{}", text.trim()),
            Params::Scale(*axis, text.clone()),
        )),
        Dialog::TrackProperties(d, meter) => {
            let meter = meter.clone()?;
            let r = parse_corrections(d, &meter);
            Some((
                format!("corrections:{}:{r:?}", d.track.0),
                Params::Corrections(d.track, r, meter),
            ))
        }
        _ => None,
    }
}

enum Params {
    Scale(ScaleAxis, String),
    /// Track Properties corrections (preview only: how the notes will play).
    Corrections(TrackId, Result<DraftCorrections, String>, MeterMap),
    /// The toolbar's Gate field while typing, notes selected.
    Gate(u64),
    /// The toolbar's Velocity field while typing.
    Velocity(u8),
}

/// Builds the kernel for `params` on track `ti` (cheap: at most one pass over
/// the selected pitches for Scale Y).
fn build_kernel(
    doc: &PlumDocument,
    ti: plumspirit_engine::editor::NoteTarget,
    _shift: i128,
    params: &Params,
) -> Result<(NoteKernel, PreviewScope), String> {
    let v = plumspirit_engine::editor::note_view(doc, ti)
        .map_err(|_| "The track no longer exists.".to_owned())?;
    let t = v.track;
    match params {
        Params::Scale(axis, text) => {
            let f = ExactFactor::parse(text).map_err(|e| e.to_string())?;
            let b = v
                .notes
                .selection_bounds()
                .ok_or("Select one or more notes first.")?;
            let k = match axis {
                ScaleAxis::X => NoteKernel::scale_x(&b, &f),
                ScaleAxis::Y => scale_y_kernel(v, &f, doc.project().keys),
            }
            .map_err(|e| e.user_message())?;
            Ok((k, PreviewScope::Selected))
        }
        Params::Corrections(id, parsed, meter) => {
            if *id != t.id() {
                return Err(
                    "Preview shows the active track; make this track active to see its corrections."
                        .into(),
                );
            }
            let c = parsed.as_ref().map_err(Clone::clone)?;
            let props = c.applied_to(t.props());
            match Correction::resolve(&props, meter, doc.project().keys) {
                Some(c) => Ok((
                    NoteKernel::Correction {
                        c,
                        max_dur: v.notes.snapshot().max_duration(),
                    },
                    PreviewScope::All,
                )),
                None => Err("No corrections: the notes play as written.".into()),
            }
        }
        Params::Gate(g) => Ok((NoteKernel::SetDuration(*g), PreviewScope::Selected)),
        Params::Velocity(v) => Ok((NoteKernel::SetVelocity(*v), PreviewScope::Selected)),
    }
}

/// "12,345 of 20,000 note(s) change. First: start 960 → 1,440, length 480 → 960."
pub fn summary_text(s: &PreviewSummary) -> String {
    if let Some(e) = &s.error {
        return format!("{e} Apply would change nothing.");
    }
    if s.notes == 0 {
        return "No notes to change.".into();
    }
    let mut out = format!(
        "Preview: {} of {} note(s) change.",
        group_digits(s.changed as u64),
        group_digits(s.notes as u64)
    );
    if s.removed > 0 {
        out.push_str(&format!(
            " {} would not play (outside the track's key range, or beyond the project's keys).",
            group_digits(s.removed as u64)
        ));
    }
    if let Some((a, b)) = s.example {
        if a.start_tick != b.start_tick || a.duration_ticks != b.duration_ticks {
            out.push_str(&format!(
                " First: start {} → {}, length {} → {}.",
                group_digits(a.start_tick),
                group_digits(b.start_tick),
                group_digits(a.duration_ticks),
                group_digits(b.duration_ticks)
            ));
        } else if a.attrs.pitch() != b.attrs.pitch() {
            out.push_str(&format!(
                " First: pitch {} → {}.",
                plumspirit_engine::model::note::pitch_name(a.attrs.pitch()),
                plumspirit_engine::model::note::pitch_name(b.attrs.pitch())
            ));
        } else if a.attrs.velocity() != b.attrs.velocity() {
            out.push_str(&format!(
                " First: velocity {} → {}.",
                a.attrs.velocity(),
                b.attrs.velocity()
            ));
        }
    }
    out
}

impl PlumSpiritApp {
    /// The kernel to draw, when Preview is on and valid.
    pub(super) fn preview_kernel(&self) -> Option<(Arc<NoteKernel>, PreviewScope)> {
        if !self.settings.preview {
            return None;
        }
        let p = self.preview.as_ref()?;
        Some((p.kernel.clone()?, p.scope))
    }

    /// What to preview now: the open dialog's operation, or the toolbar's Gate /
    /// Velocity field while a new value is being typed for selected notes.
    fn preview_params(&self) -> Option<(String, Params)> {
        if let Some(d) = self.dialog.as_ref() {
            return dialog_params(d);
        }
        let selected = self.edit_notes().map_or(0, |n| n.selected_count());
        if selected == 0 || self.conductor_selected() {
            return None;
        }
        if self.gate_field.editing {
            let g = plumspirit_engine::editor::parse_gate(&self.gate_field.text).ok()?;
            return Some((format!("gate:{g}"), Params::Gate(g)));
        }
        if self.velocity_field.editing {
            let v = plumspirit_engine::editor::parse_velocity(&self.velocity_field.text).ok()?;
            return Some((format!("velocity:{v}"), Params::Velocity(v)));
        }
        None
    }

    /// Keeps the Preview in step with the open dialog (called once per frame).
    pub(super) fn update_preview(&mut self, ctx: &egui::Context) {
        let Some((key, params)) = self.preview_params() else {
            if let Some(mut p) = self.preview.take() {
                p.stop_worker();
                ctx.request_repaint();
            }
            return;
        };
        // What the roll edits (an opened clip's content; V09-S11).
        let ti = self.edit_target();
        let shift = self.edit_shift();
        let Some(o) = self.open.as_ref() else {
            return;
        };
        let doc = o.doc.clone();
        let Ok(t) = plumspirit_engine::editor::note_view(&doc, ti) else {
            return;
        };
        let full_key = format!(
            "{key}|{:?}|{}|{}|{shift}",
            t.host(),
            t.notes.revision(),
            self.settings.preview,
        );
        let state = self.preview.get_or_insert_with(PreviewState::new);
        // Poll a running summary.
        if let Some(w) = &state.worker {
            match w.rx.try_recv() {
                Ok((g, s)) => {
                    let done = state.worker.take().expect("checked");
                    done.stop();
                    // A late result for older parameters is discarded.
                    if state.generation.is_current(g) {
                        state.summary = Some(s);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(50));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    state.worker = None;
                }
            }
        }
        if state.key != full_key {
            // The roll was drawn before this update: show the new preview now.
            ctx.request_repaint();
            state.key = full_key;
            state.stop_worker();
            let generation = state.generation.next();
            state.summary = None;
            match build_kernel(&doc, ti, shift, &params) {
                Err(e) => {
                    state.kernel = None;
                    state.error = Some(e);
                }
                Ok((k, scope)) => {
                    state.error = None;
                    state.scope = scope;
                    let k = Arc::new(k);
                    state.kernel = Some(k.clone());
                    let count = match scope {
                        PreviewScope::Selected => t.notes.selected_count(),
                        PreviewScope::All => t.notes.len(),
                    };
                    if count <= SYNC_SUMMARY_LIMIT {
                        let never = AtomicBool::new(false);
                        state.summary = Some(summarize(t, &k, scope, &never));
                    } else {
                        let (tx, rx) = channel();
                        let cancel = Arc::new(AtomicBool::new(false));
                        let c2 = cancel.clone();
                        let doc2 = doc.clone();
                        let ctx2 = ctx.clone();
                        let handle = std::thread::Builder::new()
                            .name("preview".into())
                            .spawn(move || {
                                if let Ok(t) = plumspirit_engine::editor::note_view(&doc2, ti) {
                                    let s = summarize(t, &k, scope, &c2);
                                    if !s.cancelled {
                                        let _ = tx.send((generation, s));
                                    }
                                }
                                ctx2.request_repaint();
                            })
                            .ok();
                        state.worker = Some(Worker {
                            generation,
                            rx,
                            cancel,
                            handle,
                        });
                    }
                }
            }
        }
        // Show the result in the dialog.
        let text = match (&state.error, &state.summary, &state.worker) {
            (Some(e), _, _) => e.clone(),
            (None, Some(s), _) => summary_text(s),
            (None, None, Some(w)) => format!("Preview: checking every note… (#{})", w.generation),
            (None, None, None) => String::new(),
        };
        match self.dialog.as_mut() {
            Some(Dialog::Scale { summary, .. }) => *summary = text,
            Some(Dialog::TrackProperties(d, _)) => d.preview_text = text,
            _ => {}
        }
    }

    /// Stops a running summary worker but keeps the Preview (called before any
    /// edit: the worker shares the document, which must be unique to change).
    pub(super) fn release_preview_worker(&mut self) {
        if let Some(t) = self.transient.as_mut() {
            t.stop_worker();
        }
        if let Some(p) = self.preview.as_mut()
            && p.worker.is_some()
        {
            p.stop_worker();
            // Recompute the summary for the edited document next frame.
            p.key.clear();
        }
    }

    /// Stops the Preview worker (it shares the document, which must be unique
    /// for Apply to change it).
    pub(super) fn stop_preview(&mut self) {
        if let Some(mut p) = self.preview.take() {
            p.stop_worker();
        }
    }

    /// Id of the track the Preview is for (tests / diagnostics).
    #[allow(dead_code)]
    pub(super) fn preview_track(&self) -> Option<TrackId> {
        self.doc()
            .and_then(|d| d.track(self.edit_target().host_track()))
            .map(|t| t.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord};

    fn doc(n: u64) -> PlumDocument {
        let mut d = PlumDocument::new_project("p".into(), Bpm::DEFAULT, 960);
        let a = NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap();
        let recs: Vec<NoteRecord> = (0..n).map(|i| NoteRecord::new(i * 10 + 3, 7, a)).collect();
        let t = d.track_mut(0).unwrap();
        t.notes.insert_sorted_batch(&recs, true).unwrap();
        d
    }

    #[test]
    fn kernels_follow_the_dialog_parameters_and_report_errors() {
        let d = doc(100);
        let (k, scope) =
            build_kernel(&d, 0.into(), 0, &Params::Scale(ScaleAxis::X, "2".into())).unwrap();
        assert!(matches!(k, NoteKernel::ScaleX { .. }));
        assert_eq!(scope, PreviewScope::Selected);
        assert!(build_kernel(&d, 0.into(), 0, &Params::Scale(ScaleAxis::X, "abc".into())).is_err());
        let never = AtomicBool::new(false);
        let s = summarize(d.track(0).unwrap(), &k, scope, &never);
        assert_eq!(s.notes, 100);
        assert!(summary_text(&s).starts_with("Preview: "));
    }

    #[test]
    fn track_corrections_and_toolbar_fields_preview_through_kernels() {
        use plumspirit_engine::model::{Division, GatePercent};
        let d = doc(50);
        let id = d.track(0).unwrap().id();
        let meter = MeterMap::new(Division::ppqn(960), &[(0, 4, 2)]);
        let corr = |ticks: i64, keys: i16| DraftCorrections {
            ticks,
            from_measure: 1,
            keys,
            gate: GatePercent::from_hundredths(10_000).unwrap(),
            range: (0, 127),
        };
        let (k, scope) = build_kernel(
            &d,
            0.into(),
            0,
            &Params::Corrections(id, Ok(corr(120, 0)), meter.clone()),
        )
        .unwrap();
        assert!(matches!(k, NoteKernel::Correction { .. }));
        assert_eq!(scope, PreviewScope::All);
        let never = AtomicBool::new(false);
        let s = summarize(d.track(0).unwrap(), &k, scope, &never);
        assert_eq!((s.notes, s.changed, s.removed), (50, 50, 0));
        // Transposing beyond 127 silences the notes: shown as "would not play".
        let (k, _) = build_kernel(
            &d,
            0.into(),
            0,
            &Params::Corrections(id, Ok(corr(0, 100)), meter.clone()),
        )
        .unwrap();
        let s = summarize(d.track(0).unwrap(), &k, PreviewScope::All, &never);
        assert_eq!(s.removed, 50);
        assert!(summary_text(&s).contains("would not play"));
        // No correction, an invalid field, or another track: no kernel, a reason.
        let none = build_kernel(
            &d,
            0.into(),
            0,
            &Params::Corrections(id, Ok(corr(0, 0)), meter.clone()),
        );
        assert!(none.unwrap_err().contains("play as written"));
        let bad = build_kernel(
            &d,
            0.into(),
            0,
            &Params::Corrections(id, Err("Bad.".into()), meter.clone()),
        );
        assert_eq!(bad.unwrap_err(), "Bad.");
        let other = build_kernel(
            &d,
            0.into(),
            0,
            &Params::Corrections(TrackId(id.0 + 99), Ok(corr(5, 0)), meter),
        );
        assert!(other.unwrap_err().contains("active track"));
        // Toolbar fields.
        let (k, scope) = build_kernel(&d, 0.into(), 0, &Params::Gate(480)).unwrap();
        assert_eq!(
            (k, scope),
            (NoteKernel::SetDuration(480), PreviewScope::Selected)
        );
        let (k, _) = build_kernel(&d, 0.into(), 0, &Params::Velocity(5)).unwrap();
        let s = summarize(d.track(0).unwrap(), &k, PreviewScope::Selected, &never);
        assert!(
            summary_text(&s).contains("velocity 100 → 5"),
            "{}",
            summary_text(&s)
        );
    }

    #[test]
    fn a_late_worker_result_is_discarded() {
        // Two requests: the first worker finishes last; its generation is stale.
        let g = PreviewGeneration::default();
        let (tx, rx) = channel::<(u64, u64)>();
        let a = g.next();
        let b = g.next();
        let t1 = {
            let tx = tx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(30));
                tx.send((a, 20)).unwrap();
            })
        };
        tx.send((b, 25)).unwrap();
        t1.join().unwrap();
        let mut shown = None;
        while let Ok((gen_, v)) = rx.try_recv() {
            if g.is_current(gen_) {
                shown = Some(v);
            }
        }
        assert_eq!(shown, Some(25));
    }
}
