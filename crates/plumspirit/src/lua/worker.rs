//! Script workers (v1.0-beta1 B1-0025 / B1-0036, `docs/LUA_API.md`
//! "Threads").
//!
//! A [`ScriptWorker`] is a thread that owns one [`Host`] (a Lua state never
//! leaves the thread that made it). The UI thread hands it a job — the
//! script and the [`World`] (an immutable snapshot: `Arc` pointers, no copy)
//! — and polls for the result without blocking; a progress bar reads
//! [`ScriptWorker::progress`] while it runs. The worker never touches the
//! document: what a script commits comes back as [`Commits`] for the UI
//! thread to apply. Cancellation reaches it through its flag (also in the
//! app's [`Scripts`] registry, so closing PlumSpirit stops it without
//! waiting). Nothing here runs in the audio callback.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::selection::UiSelection;
use super::tx::Commits;
use super::{Capability, Category, Host, Limits, Plain, Progress, ScriptError, Scripts, World};

/// One script to run.
pub struct ScriptJob {
    /// The script file's name (shown in errors).
    pub file: String,
    pub source: String,
    /// The project the script reads (`None`: no project is open).
    pub world: Option<World>,
    /// This run's time budget instead of the host's (`None`: the host's).
    pub time_limit: Option<Duration>,
}

/// What a job gave.
pub struct ScriptResult {
    pub result: Result<Plain, ScriptError>,
    /// What the script committed, for the UI thread to apply.
    pub commits: Option<Commits>,
    /// The selection it asked for.
    pub ui: Option<UiSelection>,
    /// What it printed.
    pub output: Vec<String>,
    /// What a plugin registered, when the job changed it.
    pub decls: Option<super::plugin_api::Decls>,
    /// A plugin's project settings change, settings notes, notifications.
    pub plugin: super::PluginOut,
}

/// A thread running scripts in its own Lua host.
pub struct ScriptWorker {
    jobs: Option<Sender<ScriptJob>>,
    results: Receiver<ScriptResult>,
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<Progress>>,
    busy: bool,
    dead: bool,
}

impl ScriptWorker {
    /// Starts a worker whose host has `limits` and `granted`; its cancel
    /// flag joins `scripts`.
    pub fn spawn(
        limits: Limits,
        granted: Vec<Capability>,
        scripts: &Scripts,
    ) -> Result<ScriptWorker, ScriptError> {
        ScriptWorker::spawn_plugin(limits, granted, scripts, None)
    }

    /// [`ScriptWorker::spawn`] for a plugin: the host gets `plum.plugin`
    /// for `meta` (B1-0038).
    pub fn spawn_plugin(
        limits: Limits,
        granted: Vec<Capability>,
        scripts: &Scripts,
        meta: Option<super::plugin_api::PluginMeta>,
    ) -> Result<ScriptWorker, ScriptError> {
        let (job_tx, job_rx) = mpsc::channel::<ScriptJob>();
        let (res_tx, res_rx) = mpsc::channel::<ScriptResult>();
        let (ready_tx, ready_rx) =
            mpsc::channel::<Result<(Arc<AtomicBool>, Arc<Mutex<Progress>>), ScriptError>>();
        std::thread::Builder::new()
            .name("plumspirit-lua".into())
            .spawn(move || {
                let host = match Host::new(limits, granted) {
                    Ok(h) => h,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                if let Some(meta) = meta
                    && let Err(e) = host.set_plugin(meta)
                {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
                let _ = ready_tx.send(Ok((host.cancel_handle(), host.progress_handle())));
                while let Ok(job) = job_rx.recv() {
                    host.set_world(job.world);
                    host.set_time_limit(job.time_limit);
                    let result = host.run(&job.file, &job.source);
                    let commits = host.take_commits();
                    let ui = host.take_ui_selection();
                    let output = host.take_output();
                    let decls = host.take_decls();
                    let plugin = host.take_plugin_out();
                    // The document snapshot is not kept between jobs.
                    host.set_world(None);
                    if res_tx
                        .send(ScriptResult {
                            result,
                            commits,
                            ui,
                            output,
                            decls,
                            plugin,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|e| {
                ScriptError::new(
                    Category::Internal,
                    format!("the script worker could not start ({e})"),
                )
            })?;
        let (cancel, progress) = ready_rx.recv().map_err(|_| {
            ScriptError::new(
                Category::Internal,
                "the script worker stopped while starting",
            )
        })??;
        scripts.add_flag(&cancel);
        Ok(ScriptWorker {
            jobs: Some(job_tx),
            results: res_rx,
            cancel,
            progress,
            busy: false,
            dead: false,
        })
    }

    /// Starts `job`; refused while another job runs or after the worker
    /// stopped.
    pub fn submit(&mut self, job: ScriptJob) -> Result<(), ScriptError> {
        if self.busy {
            return Err(ScriptError::new(
                Category::Runtime,
                "A script is already running in this worker.",
            ));
        }
        let sent = self.jobs.as_ref().is_some_and(|j| j.send(job).is_ok());
        if !sent {
            self.dead = true;
            return Err(ScriptError::new(
                Category::Internal,
                "The script worker has stopped; start a new one.",
            ));
        }
        self.busy = true;
        Ok(())
    }

    /// The result, when the job has finished (never blocks: call it from
    /// the UI thread every frame while [`ScriptWorker::is_busy`]).
    pub fn poll(&mut self) -> Option<ScriptResult> {
        match self.results.try_recv() {
            Ok(r) => {
                self.busy = false;
                Some(r)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => self.lost(),
        }
    }

    /// Waits at most `timeout` for the result (tests, and closing a dialog
    /// that may wait).
    pub fn wait(&mut self, timeout: Duration) -> Option<ScriptResult> {
        match self.results.recv_timeout(timeout) {
            Ok(r) => {
                self.busy = false;
                Some(r)
            }
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => self.lost(),
        }
    }

    /// The worker thread ended while a job ran (a defect): report it once.
    fn lost(&mut self) -> Option<ScriptResult> {
        if self.dead || !self.busy {
            self.dead = true;
            return None;
        }
        self.dead = true;
        self.busy = false;
        Some(ScriptResult {
            result: Err(ScriptError::new(
                Category::Internal,
                "The script worker stopped unexpectedly; nothing was changed.",
            )),
            commits: None,
            ui: None,
            output: Vec::new(),
            decls: None,
            plugin: super::PluginOut::default(),
        })
    }

    /// Stops the running script at its next check (it ends `canceled`).
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn is_busy(&self) -> bool {
        self.busy
    }

    /// False after the worker thread ended.
    pub fn is_alive(&self) -> bool {
        !self.dead
    }

    /// What the running script last reported with `plum.progress`.
    pub fn progress(&self) -> Progress {
        self.progress.lock().map(|p| p.clone()).unwrap_or_default()
    }
}

impl Drop for ScriptWorker {
    /// Cancels a running script and lets the thread end on its own (never
    /// joined: dropping a worker never waits for a script).
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.jobs = None;
    }
}
