//! Background work: opening projects and MIDI files, validated saving and export,
//! and preparing large edits.
//!
//! Ownership rules:
//! * The worker owns its inputs (a path, or an `Arc` of the document for read-only
//!   access) and never touches UI objects.
//! * Results come back over a typed channel tagged with the job id; the UI thread
//!   applies them. A result from a job that is no longer current is dropped.
//! * Workers drop their document `Arc` *before* sending the result, so when the UI
//!   receives it the document is exclusively owned again and can be mutated.
//! * While a job runs, document mutations are disabled in the UI (and would be
//!   refused anyway: `Arc::get_mut` fails while the worker holds a reference).

use plumspirit_engine::editor::{EditControl, EditError, PreparedEdit};
use plumspirit_engine::history::StateId;
use plumspirit_engine::midi::{
    self, ExportControl, ExportError, ExportReport, ExportSnapshot, ImportControl, ImportStage,
    ImportSummary, MidiError,
};
use plumspirit_engine::model::PlumDocument;
use plumspirit_engine::model::project::KeyRange;
use plumspirit_engine::native::{self, LupControl, LupError, LupLoadReport, SaveReport};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::JoinHandle;
use std::time::Instant;

/// What to do after a save (or a decision about unsaved changes) completes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Then {
    Nothing,
    OpenDialog,
    NewProject,
    /// v0.7: close the project and return to the start screen.
    Close,
    Exit,
    /// v0.8: open this file (Recent Files).
    OpenPath(std::path::PathBuf),
}

/// Where a MIDI file opened from an archive came from (L-D02): the archive
/// on disk and the entry's logical path inside it. The spooled copy is only
/// read; everything shown and remembered names the archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportOrigin {
    pub archive: PathBuf,
    /// `a.zip › b.tar.gz › b.tar › c.mid`.
    pub logical: String,
    /// The entry's file name (`c.mid`).
    pub file_name: String,
}

impl ImportOrigin {
    /// The path the document is shown under: the entry's name next to the
    /// archive (never the spool; it may not exist on disk).
    pub fn shown_path(&self) -> PathBuf {
        self.archive.with_file_name(&self.file_name)
    }
}

#[derive(Clone, Debug)]
pub enum JobKind {
    Import {
        path: PathBuf,
        name: String,
        /// L-D02: opened from an archive (`path` is the spooled copy).
        origin: Option<ImportOrigin>,
        /// How the file is read (L-D05: `compat` = Compatibility Mode).
        keys: KeyRange,
        compat: bool,
        /// L-H02: the import's stage (`midi::ImportStage as u8`).
        stage: Arc<std::sync::atomic::AtomicU8>,
    },
    /// L-D02: unpacking an archive to list the MIDI files in it.
    OpenArchive {
        path: PathBuf,
        name: String,
    },
    /// Reading a MIDI file to import it into one track (v0.5).
    ImportInto {
        path: PathBuf,
        name: String,
        dest: plumspirit_engine::model::TrackId,
    },
    LoadProject {
        path: PathBuf,
        name: String,
    },
    Save {
        path: PathBuf,
        then: Then,
        state: StateId,
    },
    Export {
        path: PathBuf,
    },
    Edit {
        label: &'static str,
    },
    /// v0.8 §209: a transform tool computed on a worker (Repeat Last Tool,
    /// one-click commands, a window's Apply without a preview); the UI
    /// thread commits the result.
    Tool {
        label: &'static str,
        finish: crate::app::tools::ToolFinish,
    },
    /// B1-0103: Change PPQN prepared on a worker (the Preview); `apply`:
    /// applied as soon as it is ready. `key` is the document state it read.
    Ppqn {
        to: u32,
        mode: plumspirit_engine::editor::ppqn::PpqnMode,
        apply: bool,
        key: u64,
    },
}

pub enum JobOutput {
    Import(Result<(PlumDocument, ImportSummary), MidiError>),
    /// L-D02: the MIDI files of an archive (spooled) with their quick info.
    Archive(
        Result<
            (crate::archive::Opened, Vec<crate::archive::SmfInfo>),
            crate::archive::ArchiveError,
        >,
    ),
    /// The document, its load report and its history identity (v0.8).
    LoadProject(Result<(PlumDocument, LupLoadReport, u64), LupError>),
    /// The save report and the saved document's history identity (v0.8).
    Save(Result<(SaveReport, u64), LupError>),
    Export(Result<ExportReport, ExportError>),
    /// v1.0-beta1 (B1-0121): a strict no-edit export from the source file.
    ExportStrict(Result<midi::strict::StrictReport, ExportError>),
    Edit(Result<PreparedEdit, EditError>),
    Tool(
        Result<
            plumspirit_engine::transform::TransformRun,
            plumspirit_engine::transform::TransformError,
        >,
    ),
    Ppqn(
        Result<
            (
                Box<plumspirit_engine::editor::ppqn::Timebase>,
                plumspirit_engine::editor::ppqn::PpqnReport,
            ),
            String,
        >,
    ),
}

/// L-H02: a MIDI import's reading is at most this share of its progress.
pub const IMPORT_READ_SHARE: f64 = 0.85;
/// L-H02: a running job never shows more than this (100 % is "done").
pub const MAX_RUNNING_FRACTION: f64 = 0.99;

pub struct RunningJob {
    pub id: u64,
    pub kind: JobKind,
    pub started: Instant,
    /// Total units of progress (bytes for import, notes for export/edit).
    pub total: u64,
    cancel: Arc<AtomicBool>,
    progress: Arc<AtomicU64>,
    rx: Receiver<JobOutput>,
    handle: Option<JoinHandle<()>>,
    /// L-H04: the memory guard of an import / load ([`RunningJob::guard_memory`]).
    memory: Option<Arc<MemoryStop>>,
}

/// L-H04: what the memory guard saw when it stopped a job.
#[derive(Debug, Default)]
pub struct MemoryStop {
    /// The memory the system could still hand out when the job was stopped
    /// (bytes, at least 1); 0 while it was not stopped.
    at: AtomicU64,
    /// The headroom under which it stops a job.
    floor: u64,
}

/// L-H04: an import or a project load is stopped when the memory the system
/// can still hand out (`platform::commit_headroom_bytes`) falls under this:
/// 2 % of the limit, between 256 MiB and 2 GiB — before the next allocation
/// fails (Rust ends the process then) and while Windows and the other
/// programs can still work.
pub fn memory_floor(limit: u64) -> u64 {
    (limit / 50).clamp(256 << 20, 2 << 30)
}

/// How often the guard looks (`GlobalMemoryStatusEx` / `/proc/meminfo`).
const MEMORY_GUARD_PERIOD: std::time::Duration = std::time::Duration::from_millis(100);

impl RunningJob {
    /// L-H04: watches the system's memory while the job runs and cancels it
    /// when the headroom falls under [`memory_floor`]; the job then reports
    /// [`RunningJob::stopped_for_memory`] instead of the process running out
    /// (a failed allocation aborts it). `probe` gives (headroom, limit).
    pub fn guard_memory(
        mut self,
        probe: impl Fn() -> Option<(u64, u64)> + Send + 'static,
    ) -> RunningJob {
        let Some((_, limit)) = probe() else {
            return self;
        };
        let stop = Arc::new(MemoryStop {
            at: AtomicU64::new(0),
            floor: memory_floor(limit),
        });
        let cancel = Arc::downgrade(&self.cancel);
        let seen = Arc::clone(&stop);
        let spawned = std::thread::Builder::new()
            .name("plumspirit-memory-guard".into())
            .spawn(move || {
                loop {
                    // Ends with the job (its flags are dropped) or once it
                    // was canceled.
                    let Some(c) = cancel.upgrade() else {
                        return;
                    };
                    if c.load(Ordering::Relaxed) {
                        return;
                    }
                    if let Some((free, _)) = probe()
                        && free < seen.floor
                    {
                        log::warn!(
                            "memory guard: {} MB left to allocate (floor {} MB); stopping the job",
                            free >> 20,
                            seen.floor >> 20
                        );
                        seen.at.store(free.max(1), Ordering::Relaxed);
                        c.store(true, Ordering::Relaxed);
                        return;
                    }
                    drop(c);
                    std::thread::sleep(MEMORY_GUARD_PERIOD);
                }
            });
        if let Err(e) = spawned {
            log::warn!("the memory guard could not start ({e})");
        } else {
            self.memory = Some(stop);
        }
        self
    }

    /// L-H04: (headroom when stopped, floor) when the memory guard stopped
    /// the job.
    pub fn stopped_for_memory(&self) -> Option<(u64, u64)> {
        let m = self.memory.as_ref()?;
        let at = m.at.load(Ordering::Relaxed);
        (at > 0).then_some((at, m.floor))
    }

    /// The share done, never 100 % while the job runs (L-H02: the bytes of
    /// a file can all be read long before the document is usable). A MIDI
    /// import weighs its stages: reading up to 85 %, then decoding the last
    /// chunks, assembling and checking the document.
    pub fn fraction(&self) -> Option<f32> {
        let read = (self.total > 0)
            .then(|| (self.progress.load(Ordering::Relaxed) as f64 / self.total as f64).min(1.0));
        let f = match (&self.kind, self.import_stage()) {
            (_, Some(ImportStage::Reading)) => read.map(|r| r * IMPORT_READ_SHARE),
            (_, Some(ImportStage::Decoding)) => Some(0.88),
            (_, Some(ImportStage::Assembling)) => Some(0.92),
            (_, Some(ImportStage::Checking)) => Some(0.96),
            (_, Some(ImportStage::Done)) => Some(0.98),
            (_, None) => read,
        }?;
        Some(f.min(MAX_RUNNING_FRACTION) as f32)
    }

    /// The stage of a MIDI import (`None` for other jobs).
    pub fn import_stage(&self) -> Option<ImportStage> {
        match &self.kind {
            JobKind::Import { stage, .. } => {
                Some(ImportStage::from_u8(stage.load(Ordering::Relaxed)))
            }
            _ => None,
        }
    }

    pub fn progress_units(&self) -> u64 {
        self.progress.load(Ordering::Relaxed)
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Non-blocking poll. `Err(())` means the worker ended without a result
    /// (it panicked); the caller reports that as an error.
    pub fn poll(&mut self) -> Result<Option<JobOutput>, ()> {
        match self.rx.try_recv() {
            Ok(out) => {
                if let Some(h) = self.handle.take() {
                    let _ = h.join();
                }
                Ok(Some(out))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                if let Some(h) = self.handle.take() {
                    let _ = h.join();
                }
                Err(())
            }
        }
    }

    /// Cancels and returns the thread handle so the caller can join it later.
    pub fn retire(mut self) -> Option<JoinHandle<()>> {
        self.cancel();
        self.handle.take()
    }

    pub fn label(&self) -> String {
        match &self.kind {
            JobKind::Import { name, .. } => match self.import_stage() {
                Some(ImportStage::Decoding) => format!("Loading {name}: decoding the last tracks…"),
                Some(ImportStage::Assembling) => format!("Loading {name}: assembling the project…"),
                Some(ImportStage::Checking | ImportStage::Done) => {
                    format!("Loading {name}: checking the project…")
                }
                _ => format!("Loading {name}: reading…"),
            },
            JobKind::LoadProject { name, .. } => format!("Loading {name}…"),
            JobKind::OpenArchive { name, .. } => format!("Unpacking {name}…"),
            JobKind::ImportInto { name, .. } => format!("Reading {name}…"),
            JobKind::Save { .. } => {
                if self.total > 0 && self.progress_units() >= self.total {
                    "Verifying saved project…".into()
                } else {
                    "Saving…".into()
                }
            }
            JobKind::Export { .. } => {
                if self.total > 0 && self.progress_units() >= self.total {
                    "Validating exported MIDI…".into()
                } else {
                    "Exporting…".into()
                }
            }
            JobKind::Edit { label } | JobKind::Tool { label, .. } => {
                format!("Applying {label}…")
            }
            JobKind::Ppqn { apply: true, .. } => "Changing the PPQN…".into(),
            JobKind::Ppqn { .. } => "Computing the PPQN Preview…".into(),
        }
    }
}

fn spawn(
    id: u64,
    kind: JobKind,
    total: u64,
    ctx: &egui::Context,
    name: &str,
    work: impl FnOnce(&Arc<AtomicBool>, &AtomicU64) -> JobOutput + Send + 'static,
) -> std::io::Result<RunningJob> {
    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(AtomicU64::new(0));
    let (tx, rx) = mpsc::channel();
    let c2 = cancel.clone();
    let p2 = progress.clone();
    let ctx = ctx.clone();
    let handle = std::thread::Builder::new()
        .name(format!("plumspirit-{name}"))
        .spawn(move || {
            let out = work(&c2, &p2);
            let _ = tx.send(out);
            ctx.request_repaint();
        })?;
    Ok(RunningJob {
        id,
        kind,
        started: Instant::now(),
        total,
        cancel,
        progress,
        rx,
        handle: Some(handle),
        memory: None,
    })
}

/// Reads a MIDI file to open it; `keys` is the key reading
/// ([`KeyRange::Extended256`]: a non-standard 256-key file, v0.9 §062).
/// The app opens through [`spawn_import_from`].
#[cfg(test)]
pub fn spawn_import(
    id: u64,
    path: PathBuf,
    keys: KeyRange,
    ctx: &egui::Context,
) -> std::io::Result<RunningJob> {
    spawn_import_from(id, path, keys, false, None, ctx)
}

/// [`spawn_import`] in Compatibility Mode or not (`compat`, L-D05) and of a
/// spooled archive entry (`origin`: where it came from; it names the job).
pub fn spawn_import_from(
    id: u64,
    path: PathBuf,
    keys: KeyRange,
    compat: bool,
    origin: Option<ImportOrigin>,
    ctx: &egui::Context,
) -> std::io::Result<RunningJob> {
    import_job(id, path, keys, compat, origin, ctx)
        .map(|j| j.guard_memory(crate::platform::commit_headroom_bytes))
}

/// [`spawn_import_from`] without its memory guard.
fn import_job(
    id: u64,
    path: PathBuf,
    keys: KeyRange,
    compat: bool,
    origin: Option<ImportOrigin>,
    ctx: &egui::Context,
) -> std::io::Result<RunningJob> {
    let total = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let name = match &origin {
        Some(o) => o.file_name.clone(),
        None => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
    };
    let p = path.clone();
    let stage = Arc::new(std::sync::atomic::AtomicU8::new(ImportStage::Reading as u8));
    let s2 = stage.clone();
    spawn(
        id,
        JobKind::Import {
            path,
            name,
            origin,
            keys,
            compat,
            stage,
        },
        total,
        ctx,
        "import",
        move |cancel, progress| {
            JobOutput::Import(midi::import_file_with(
                &p,
                ImportControl {
                    cancel: Some(cancel),
                    progress_bytes: Some(progress),
                    compatibility: compat,
                    stage: Some(&s2),
                },
                keys,
            ))
        },
    )
}

/// L-D02: unpacks the archive at `path` into a spool in the temporary
/// folder (bounded, cancellable: `archive::open_archive`) and reads the
/// chooser's quick information of every MIDI file in it.
pub fn spawn_open_archive(
    id: u64,
    path: PathBuf,
    ctx: &egui::Context,
) -> std::io::Result<RunningJob> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let p = path.clone();
    spawn(
        id,
        JobKind::OpenArchive { path, name },
        0,
        ctx,
        "archive",
        move |cancel, _progress| {
            let opened = crate::archive::open_archive(
                &p,
                &crate::archive::Limits::default(),
                cancel,
                &std::env::temp_dir(),
            );
            JobOutput::Archive(opened.and_then(|o| {
                let mut infos = Vec::with_capacity(o.candidates.len());
                for c in &o.candidates {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(crate::archive::ArchiveError::Cancelled);
                    }
                    infos.push(crate::archive::quick_info(&c.file, cancel));
                }
                Ok((o, infos))
            }))
        },
    )
}

/// Reads a MIDI file for Import MIDI Into Track (the open project is untouched).
/// `keys` is the open project's key range: a 256-Key Mode project reads a
/// non-standard 256-key file's keys above 127, a standard project refuses it.
pub fn spawn_import_into(
    id: u64,
    path: PathBuf,
    dest: plumspirit_engine::model::TrackId,
    keys: KeyRange,
    ctx: &egui::Context,
) -> std::io::Result<RunningJob> {
    let total = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let p = path.clone();
    spawn(
        id,
        JobKind::ImportInto { path, name, dest },
        total,
        ctx,
        "import-into",
        move |cancel, progress| {
            JobOutput::Import(midi::import_file_with(
                &p,
                ImportControl {
                    cancel: Some(cancel),
                    progress_bytes: Some(progress),
                    compatibility: false,
                    stage: None,
                },
                keys,
            ))
        },
    )
    .map(|j| j.guard_memory(crate::platform::commit_headroom_bytes))
}

pub fn spawn_load_project(
    id: u64,
    path: PathBuf,
    ctx: &egui::Context,
) -> std::io::Result<RunningJob> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let p = path.clone();
    // Progress is counted in notes; the total is unknown until the file is read.
    spawn(
        id,
        JobKind::LoadProject { path, name },
        0,
        ctx,
        "load",
        move |cancel, progress| {
            JobOutput::LoadProject(
                native::load_project(
                    &p,
                    LupControl {
                        cancel: Some(cancel),
                        progress_notes: Some(progress),
                    },
                )
                .map(|(d, r)| {
                    let id = plumspirit_engine::history::journal::identity(&d);
                    (d, r, id)
                }),
            )
        },
    )
    .map(|j| j.guard_memory(crate::platform::commit_headroom_bytes))
}

pub fn spawn_save(
    id: u64,
    doc: Arc<PlumDocument>,
    path: PathBuf,
    then: Then,
    state: StateId,
    ctx: &egui::Context,
) -> std::io::Result<RunningJob> {
    let total = doc.note_count() as u64;
    let p = path.clone();
    spawn(
        id,
        JobKind::Save { path, then, state },
        total,
        ctx,
        "save",
        move |_cancel, progress| {
            // A save is never cancelled half-way: it either completes or fails
            // without touching the destination.
            let result = native::save_project(
                &doc,
                &p,
                LupControl {
                    cancel: None,
                    progress_notes: Some(progress),
                },
            );
            // The identity of what was written (the undo history's checkpoint).
            let result = result.map(|r| (r, plumspirit_engine::history::journal::identity(&doc)));
            drop(doc);
            JobOutput::Save(result)
        },
    )
}

/// Writes and validates `snapshot` (v0.9 `docs/EXPORT_PLAN.md` D3: the
/// worker holds the snapshot's own copy of the document, not the app's).
pub fn spawn_export(
    id: u64,
    snapshot: Arc<ExportSnapshot>,
    path: PathBuf,
    ctx: &egui::Context,
) -> std::io::Result<RunningJob> {
    let total = snapshot.document().note_count() as u64;
    let p = path.clone();
    spawn(
        id,
        JobKind::Export { path },
        total,
        ctx,
        "export",
        move |cancel, progress| {
            let result = midi::export_to_path(
                snapshot.document(),
                snapshot.plan(),
                &p,
                ExportControl {
                    cancel: Some(cancel),
                    progress_notes: Some(progress),
                },
            );
            drop(snapshot);
            JobOutput::Export(result)
        },
    )
}

/// Writes an unchanged imported project from its source file
/// (v1.0-beta1 B1-0121, `midi::strict`; it re-checks the project and the
/// file itself).
pub fn spawn_export_strict(
    id: u64,
    doc: Arc<PlumDocument>,
    path: PathBuf,
    status: midi::strict::StatusBytes,
    ctx: &egui::Context,
) -> std::io::Result<RunningJob> {
    let total = doc.note_count() as u64;
    let p = path.clone();
    spawn(
        id,
        JobKind::Export { path },
        total,
        ctx,
        "export",
        move |cancel, progress| {
            let result = midi::strict::export_strict(
                &doc,
                &p,
                status,
                ExportControl {
                    cancel: Some(cancel),
                    progress_notes: Some(progress),
                },
            );
            drop(doc);
            JobOutput::ExportStrict(result)
        },
    )
}

pub type EditWork =
    Box<dyn FnOnce(&PlumDocument, EditControl<'_>) -> Result<PreparedEdit, EditError> + Send>;

pub fn spawn_edit(
    id: u64,
    doc: Arc<PlumDocument>,
    label: &'static str,
    total: u64,
    work: EditWork,
    ctx: &egui::Context,
) -> std::io::Result<RunningJob> {
    spawn(
        id,
        JobKind::Edit { label },
        total,
        ctx,
        "edit",
        move |cancel, progress| {
            let result = work(
                &doc,
                EditControl {
                    cancel: Some(cancel),
                    progress: Some(progress),
                },
            );
            drop(doc);
            JobOutput::Edit(result)
        },
    )
}

/// Computes a transform tool on a worker (v0.8 §209). `work` gets the
/// document and the cancel flag; the document `Arc` is dropped before the
/// result is sent.
pub type ToolWork = Box<
    dyn FnOnce(
            &PlumDocument,
            Arc<AtomicBool>,
        ) -> Result<
            plumspirit_engine::transform::TransformRun,
            plumspirit_engine::transform::TransformError,
        > + Send,
>;

pub fn spawn_tool(
    id: u64,
    doc: Arc<PlumDocument>,
    label: &'static str,
    finish: crate::app::tools::ToolFinish,
    total: u64,
    work: ToolWork,
    ctx: &egui::Context,
) -> std::io::Result<RunningJob> {
    spawn(
        id,
        JobKind::Tool { label, finish },
        total,
        ctx,
        "tool",
        move |cancel, _progress| {
            let result = work(&doc, cancel.clone());
            drop(doc);
            JobOutput::Tool(result)
        },
    )
}

/// B1-0103: prepares Change PPQN on a worker (one pass over every note; the
/// document `Arc` is dropped before the result is sent).
pub fn spawn_ppqn(
    id: u64,
    doc: Arc<PlumDocument>,
    to: u32,
    mode: plumspirit_engine::editor::ppqn::PpqnMode,
    apply: bool,
    key: u64,
    ctx: &egui::Context,
) -> std::io::Result<RunningJob> {
    let total = doc.note_count() as u64;
    spawn(
        id,
        JobKind::Ppqn {
            to,
            mode,
            apply,
            key,
        },
        total,
        ctx,
        "ppqn",
        move |_, _| {
            let r = plumspirit_engine::editor::ppqn::prepare_change_ppqn(&doc, to, mode)
                .map(|(tb, report)| (Box::new(tb), report));
            drop(doc);
            JobOutput::Ppqn(r)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A MIDI file of `tracks` tracks of `notes` notes each (running status).
    fn write_midi(path: &std::path::Path, tracks: usize, notes: u32) {
        let mut out = b"MThd\0\0\0\x06\0\x01".to_vec();
        out.extend_from_slice(&(tracks as u16).to_be_bytes());
        out.extend_from_slice(&480u16.to_be_bytes());
        for t in 0..tracks {
            let mut d = vec![0u8, 0x90];
            for k in 0..notes {
                let key = (k % 100 + 10) as u8;
                if k > 0 {
                    d.push(2);
                }
                d.extend_from_slice(&[key, (t % 120 + 1) as u8, 1, key, 0]);
            }
            d.extend_from_slice(&[0, 0xff, 0x2f, 0]);
            out.extend_from_slice(b"MTrk");
            out.extend_from_slice(&(d.len() as u32).to_be_bytes());
            out.extend_from_slice(&d);
        }
        std::fs::write(path, out).unwrap();
    }

    /// L-H02 ("Loading … 1495.7 s · 100 %"): a running MIDI import never
    /// shows 100 %; its stages only move forward (reading, decoding the last
    /// chunks, assembling, checking) and its label names them; the result is
    /// the document.
    #[test]
    fn a_running_import_never_shows_100_percent_and_names_its_stage() {
        let dir = std::env::temp_dir().join(format!("plumspirit-l-h02-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stages.mid");
        write_midi(&path, 24, 120_000);
        let ctx = egui::Context::default();
        let mut job = spawn_import(1, path.clone(), KeyRange::Standard, &ctx).unwrap();
        let mut stages = vec![job.import_stage().unwrap()];
        let mut labels = vec![job.label()];
        let out = loop {
            let f = job.fraction().unwrap();
            assert!(f < 1.0, "{f}");
            let s = job.import_stage().unwrap();
            assert!(s >= *stages.last().unwrap(), "{stages:?} then {s:?}");
            if s != *stages.last().unwrap() {
                stages.push(s);
                labels.push(job.label());
            }
            if let Some(out) = job.poll().unwrap() {
                break out;
            }
            std::thread::sleep(std::time::Duration::from_micros(200));
        };
        assert_eq!(stages[0], ImportStage::Reading);
        assert!(labels[0].ends_with("reading…"), "{labels:?}");
        assert_eq!(job.import_stage(), Some(ImportStage::Done));
        assert!(job.fraction().unwrap() < 1.0);
        for l in &labels {
            assert!(l.starts_with("Loading stages.mid"), "{l}");
        }
        let JobOutput::Import(Ok((doc, _))) = out else {
            panic!("an imported document");
        };
        assert_eq!(doc.note_count(), 24 * 120_000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// L-H04: the memory guard stops a running import when the memory the
    /// system can still give falls under the floor, and says so; with
    /// enough memory it never interferes, and a user's cancel is not
    /// reported as a memory stop.
    #[test]
    fn the_memory_guard_stops_an_import_that_would_run_out_and_says_so() {
        assert_eq!(memory_floor(4 << 30), 256 << 20);
        assert_eq!(memory_floor(42_600 << 20), 852 << 20);
        assert_eq!(memory_floor(1 << 40), 2 << 30);
        let dir = std::env::temp_dir().join(format!("plumspirit-l-h04-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("guard.mid");
        write_midi(&path, 16, 200_000);
        let ctx = egui::Context::default();
        let run = |job: RunningJob| -> (RunningJob, JobOutput) {
            let mut job = job;
            loop {
                if let Some(out) = job.poll().unwrap() {
                    return (job, out);
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        };
        // 100 MiB left of 8 GiB: under the 256 MiB floor at the first look.
        let low = import_job(1, path.clone(), KeyRange::Standard, false, None, &ctx)
            .unwrap()
            .guard_memory(|| Some((100 << 20, 8 << 30)));
        let (job, out) = run(low);
        assert!(matches!(out, JobOutput::Import(Err(ref e)) if e.is_cancelled()));
        assert_eq!(job.stopped_for_memory(), Some((100 << 20, 256 << 20)));
        // Plenty left: the import completes, nothing is reported.
        let fine = import_job(2, path.clone(), KeyRange::Standard, false, None, &ctx)
            .unwrap()
            .guard_memory(|| Some((6 << 30, 8 << 30)));
        let (job, out) = run(fine);
        assert!(matches!(out, JobOutput::Import(Ok(_))));
        assert_eq!(job.stopped_for_memory(), None);
        // The user cancels: canceled, not a memory stop.
        let user = import_job(3, path.clone(), KeyRange::Standard, false, None, &ctx)
            .unwrap()
            .guard_memory(|| Some((6 << 30, 8 << 30)));
        user.cancel.store(true, Ordering::Relaxed);
        let (job, out) = run(user);
        assert!(matches!(out, JobOutput::Import(Err(ref e)) if e.is_cancelled()));
        assert_eq!(job.stopped_for_memory(), None);
        // No probe (unknown memory): no guard.
        let none = import_job(4, path.clone(), KeyRange::Standard, false, None, &ctx)
            .unwrap()
            .guard_memory(|| None);
        assert!(none.memory.is_none());
        let _ = run(none);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_stage_labels_follow_the_stage() {
        let job = |s: ImportStage| {
            let stage = Arc::new(std::sync::atomic::AtomicU8::new(s as u8));
            let (_tx, rx) = mpsc::channel();
            RunningJob {
                id: 1,
                kind: JobKind::Import {
                    path: PathBuf::from("a.mid"),
                    name: "a.mid".into(),
                    origin: None,
                    keys: KeyRange::Standard,
                    compat: false,
                    stage,
                },
                started: Instant::now(),
                total: 100,
                cancel: Arc::new(AtomicBool::new(false)),
                progress: Arc::new(AtomicU64::new(100)),
                rx,
                handle: None,
                memory: None,
            }
        };
        // Every byte read: 85 %, never 100 %.
        assert!((job(ImportStage::Reading).fraction().unwrap() - 0.85).abs() < 1e-6);
        let mut last = 0.0;
        for (s, words) in [
            (ImportStage::Decoding, "decoding the last tracks"),
            (ImportStage::Assembling, "assembling the project"),
            (ImportStage::Checking, "checking the project"),
        ] {
            let j = job(s);
            let f = j.fraction().unwrap();
            assert!(f > last && f < 1.0, "{s:?} {f}");
            last = f;
            assert!(j.label().contains(words), "{}", j.label());
        }
    }
}
