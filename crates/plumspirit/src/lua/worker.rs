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

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    pub response_guard: Option<super::response::SnapshotGuard>,
    pub service_reply: Option<super::structured::Payload>,
}

enum Work {
    FreezeExtensions,
    ReleaseUiImages,
    Service(super::services::Job),
    Format(super::formats::Request, Option<World>),
    Script(ScriptJob, bool, Option<Arc<super::drawing::Captured>>, u64),
    Observe(super::reactive::Delivery),
    Response {
        world: super::World,
        delivery: super::reactive::Delivery,
        request: super::response::Request,
    },
}

/// A thread running scripts in its own Lua host.
pub struct ScriptWorker {
    jobs: Option<Sender<Work>>,
    results: Receiver<ScriptResult>,
    ui_requests: Receiver<super::bridge::UiRequest>,
    midi_input_requests: Receiver<super::midi_input::ConsentRequest>,
    panel_requests: Receiver<Arc<super::ui::Shared>>,
    overlay_requests: super::overlays::Mailbox,
    overlay_pending: Arc<AtomicBool>,
    overlay_reads: [bool; 2],
    playback_requests: Receiver<super::playback::control::Request>,
    service_requests: Receiver<super::service_calls::Request>,
    cancel: Arc<AtomicBool>,
    control_epoch: Arc<AtomicU64>,
    midi_cancel_epoch: Arc<AtomicU64>,
    release_images_pending: Arc<AtomicBool>,
    image_release_epoch: Arc<AtomicU64>,
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
        let overlay_reads = [Capability::ReadProject, Capability::ReadPlayback]
            .map(|c| granted.contains(&Capability::UiOverlay) && granted.contains(&c));
        let control_epoch = Arc::new(AtomicU64::new(0));
        let host_control_epoch = Arc::clone(&control_epoch);
        let midi_cancel_epoch = Arc::new(AtomicU64::new(0));
        let host_midi_cancel_epoch = Arc::clone(&midi_cancel_epoch);
        let release_images_pending = Arc::new(AtomicBool::new(false));
        let host_release_images_pending = Arc::clone(&release_images_pending);
        let image_release_epoch = Arc::new(AtomicU64::new(0));
        let host_image_release_epoch = Arc::clone(&image_release_epoch);
        let (overlay_slot, overlay_requests) =
            super::overlays::channel(Arc::clone(&image_release_epoch));
        let overlay_pending = overlay_requests.pending_flag();
        let (job_tx, job_rx) = mpsc::channel::<Work>();
        let (ui_client, ui_requests) =
            super::bridge::channel(meta.as_ref().map_or("script", |m| m.id.as_str()));
        let (midi_input_client, midi_input_requests) =
            super::midi_input::channel(meta.as_ref().map_or("script", |m| m.id.as_str()));
        let (panel_client, panel_requests) =
            super::ui::channel(meta.as_ref().map_or("script", |m| m.id.as_str()));
        let (playback_client, playback_requests) =
            super::playback::control::channel(meta.as_ref().map_or("script", |m| m.id.as_str()));
        let (service_client, service_requests) =
            super::service_calls::channel(meta.as_ref().map_or("script", |m| m.id.as_str()));
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
                host.lua
                    .app_data_mut::<super::bridge::Session>()
                    .expect("host request session")
                    .client = Some(ui_client);
                host.lua.app_data_mut::<super::ui::Slot>().expect("UI scope").client=Some(panel_client);
                host.lua.app_data_mut::<super::midi_input::Slot>().expect("MIDI input scope").client=Some(midi_input_client);
                host.lua.set_app_data(super::service_calls::Calls {client:Some(service_client),..Default::default()});
                host.lua.app_data_mut::<super::playback::control::Slot>().expect("playback scope").client=Some(playback_client);
                host.lua.app_data_mut::<super::playback::loop_intent::Slot>().expect("loop scope").epoch=host_control_epoch;
                host.lua.app_data_mut::<super::playback::midi::Slot>().expect("MIDI scope").cancel_epoch=host_midi_cancel_epoch;
                super::ui::images::initialize_release_epoch(&host.lua,host_image_release_epoch);
                host.lua.set_app_data(overlay_slot);
                let _ = ready_tx.send(Ok((host.cancel_handle(), host.progress_handle())));
                while let Ok(work) = job_rx.recv() {
                    if matches!(work,Work::ReleaseUiImages) {
                        super::ui::images::release_retained(&host.lua);
                        host_release_images_pending.store(false,Ordering::Release);
                        continue;
                    }
                    if matches!(work,Work::FreezeExtensions) {
                        if let Some(mut slot)=host.lua.app_data_mut::<super::plugin_api::PluginSlot>() {slot.extensibility_ready=true;}
                        continue;
                    }
                    super::services::enter(&host.lua,None);
                    super::formats::enter(&host.lua,None);
                    let review_import=matches!(&work,Work::Format(request,_) if request.mode==super::formats::Mode::Import);
                    let work_started=std::time::Instant::now();
                    let mut response_guard=None;
                    let mut response_request=None;
                    let mut stroke=None;
                    let (job, interactive, observation) = match work {
                        Work::FreezeExtensions=>unreachable!("native declaration freeze handled above"),
                        Work::ReleaseUiImages=>unreachable!("native image release handled above"),
                        Work::Script(job, interactive, input,epoch) => {super::ui::images::set_run_epoch(&host.lua,epoch);stroke=input;(job, interactive, None)},
                        Work::Format(request,world)=> {
                            super::formats::enter(&host.lua,Some(request));
                            (ScriptJob {file:"=format".into(),source:"return plum.plugin.__run_format()".into(),world,time_limit:Some(super::USER_RUN_TIME)},true,None)
                        }
                        Work::Service(service)=> {
                            super::services::enter(&host.lua,Some(service));
                            (ScriptJob {file:"=service".into(),source:"return plum.plugin.__service()".into(),world:None,time_limit:Some(super::reactive::RUN_TIME)},false,None)
                        }
                        Work::Response {world,delivery,request} => {
                            let start=std::time::Instant::now();
                            let cancellation=host.cancel_handle();
                            let check=|| {
                                if !delivery.valid() {return Err(ScriptError::new(Category::Expired,"The transaction response expired."));}
                                if cancellation.load(Ordering::Relaxed) {return Err(ScriptError::new(Category::Canceled,"The transaction response was canceled."));}
                                if start.elapsed()>super::reactive::RUN_TIME {return Err(ScriptError::new(Category::TimeLimit,"The transaction response snapshot exceeded its time budget."));}
                                Ok(())
                            };
                            let budget=host.lua.app_data_ref::<super::bridge::NativeBudget>().expect("native budget").clone();
                            match super::response::detach(world,&delivery,&budget,check) {
                                Ok((world,guard))=> {
                                    response_guard=Some(guard);response_request=Some(request);
                                    (ScriptJob {file:format!("=response:{}",delivery.id),source:"return plum.plugin.__respond()".into(),world:Some(world),
                                        time_limit:Some(super::reactive::RUN_TIME.saturating_sub(start.elapsed()))},false,Some(delivery))
                                }
                                Err(error)=> {
                                    if res_tx.send(ScriptResult {result:Err(error),commits:None,ui:None,output:Vec::new(),decls:None,
                                        plugin:Default::default(),response_guard:None,service_reply:None}).is_err() {break;}
                                    continue;
                                }
                            }
                        }
                        Work::Observe(delivery) => (
                            ScriptJob {
                                file: format!("=observer:{}", delivery.id),
                                source: "return plum.plugin.__observe()".into(),
                                world: None,
                                time_limit: Some(super::reactive::RUN_TIME),
                            },
                            false,
                            Some(delivery),
                        ),
                    };
                    host.lua
                        .app_data_mut::<super::reactive::Scope>()
                        .expect("observer scope")
                        .delivery = observation;
                    host.lua.app_data_mut::<super::reactive::Scope>().expect("observer scope").request=response_request;
                    host.set_world(job.world);
                    host.set_time_limit(job.time_limit);
                    host.lua
                        .app_data_mut::<super::bridge::Session>()
                        .expect("host request session")
                        .interactive = interactive;
                    super::drawing::set_input(&host.lua,stroke);
                    let mut result = host.run(&job.file, &job.source);
                    let mut commits = host.take_commits();
                    if review_import&&result.is_ok()&&let Some(c)=commits.as_mut() {
                        let native=host.lua.app_data_ref::<super::bridge::NativeBudget>().expect("native budget").clone();
                        let time=job.time_limit.unwrap_or(host.limits.time);
                        let cancel=host.cancel_handle();
                        let check=|| {
                            if cancel.load(Ordering::Acquire){return Err(super::stop(Category::Canceled,"Import review preparation was canceled."));}
                            if work_started.elapsed()>=time{return Err(super::stop(Category::TimeLimit,"Import review preparation reached its time limit."));}
                            Ok(())
                        };
                        if let Err(e)=c.prepare_format_review(&native,check){result=Err(super::classify(&e));commits=None;}
                    }
                    let ui = host.take_ui_selection();
                    let output = host.take_output();
                    let decls = host.take_decls();
                    let plugin = host.take_plugin_out();
                    let service_reply=super::services::finish(&host.lua,result.is_ok());
                    host.lua
                        .app_data_mut::<super::reactive::Scope>()
                        .expect("observer scope")
                        .delivery = None;
                    host.lua.app_data_mut::<super::reactive::Scope>().expect("observer scope").request=None;
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
                            response_guard,
                            service_reply,
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
        scripts.add_midi_epoch(&midi_cancel_epoch);
        Ok(ScriptWorker {
            jobs: Some(job_tx),
            results: res_rx,
            ui_requests,
            midi_input_requests,
            panel_requests,
            overlay_requests,
            overlay_pending,
            overlay_reads,
            playback_requests,
            service_requests,
            cancel,
            control_epoch,
            midi_cancel_epoch,
            release_images_pending,
            image_release_epoch,
            progress,
            busy: false,
            dead: false,
        })
    }

    /// Starts `job`; refused while another job runs or after the worker
    /// stopped.
    pub fn submit(&mut self, job: ScriptJob) -> Result<(), ScriptError> {
        self.submit_with_interaction(job, false)
    }

    /// Only the plugin manager may authorize an actual user invocation.
    /// Time budgets or script-supplied data never confer permission to prompt.
    pub(super) fn submit_with_interaction(
        &mut self,
        job: ScriptJob,
        interactive: bool,
    ) -> Result<(), ScriptError> {
        self.submit_with_stroke(job, interactive, None)
    }

    pub(super) fn submit_with_stroke(
        &mut self,
        job: ScriptJob,
        interactive: bool,
        stroke: Option<Arc<super::drawing::Captured>>,
    ) -> Result<(), ScriptError> {
        if self.busy {
            return Err(ScriptError::new(
                Category::Runtime,
                "A script is already running in this worker.",
            ));
        }
        let sent = self.jobs.as_ref().is_some_and(|j| {
            j.send(Work::Script(
                job,
                interactive,
                stroke,
                self.image_release_epoch.load(Ordering::Acquire),
            ))
            .is_ok()
        });
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

    /// Native typed delivery only; Lua cannot construct or authorize this work.
    pub(super) fn submit_observation(
        &mut self,
        delivery: super::reactive::Delivery,
    ) -> Result<(), ScriptError> {
        if self.busy {
            return Err(ScriptError::new(Category::Runtime, "The worker is busy."));
        }
        let sent = self
            .jobs
            .as_ref()
            .is_some_and(|j| j.send(Work::Observe(delivery)).is_ok());
        if !sent {
            self.dead = true;
            return Err(ScriptError::new(
                Category::Internal,
                "The script worker has stopped.",
            ));
        }
        self.busy = true;
        Ok(())
    }

    pub(super) fn freeze_extensions(&self) {
        if let Some(sender) = &self.jobs {
            let _ = sender.send(Work::FreezeExtensions);
        }
    }

    /// Fixed native format job, never authorized by a script literal.
    pub(super) fn submit_format(
        &mut self,
        request: super::formats::Request,
        world: Option<World>,
    ) -> Result<(), ScriptError> {
        if self.busy {
            return Err(ScriptError::new(
                Category::Runtime,
                "The format worker is busy.",
            ));
        }
        if !self
            .jobs
            .as_ref()
            .is_some_and(|sender| sender.send(Work::Format(request, world)).is_ok())
        {
            self.dead = true;
            return Err(ScriptError::new(
                Category::Internal,
                "The format worker stopped.",
            ));
        }
        self.busy = true;
        Ok(())
    }

    /// Native provider dispatch after broker admission.
    pub(super) fn submit_service(&mut self, job: super::services::Job) -> Result<(), ScriptError> {
        if self.busy {
            return Err(ScriptError::new(
                Category::Runtime,
                "The service worker is busy.",
            ));
        }
        if !self
            .jobs
            .as_ref()
            .is_some_and(|sender| sender.send(Work::Service(job)).is_ok())
        {
            self.dead = true;
            return Err(ScriptError::new(
                Category::Internal,
                "The provider worker stopped.",
            ));
        }
        self.busy = true;
        Ok(())
    }

    pub(super) fn submit_response(
        &mut self,
        world: super::World,
        delivery: super::reactive::Delivery,
        request: super::response::Request,
    ) -> Result<(), ScriptError> {
        if self.busy {
            return Err(ScriptError::new(Category::Runtime, "The worker is busy."));
        }
        self.cancel.store(false, Ordering::SeqCst);
        if !self.jobs.as_ref().is_some_and(|j| {
            j.send(Work::Response {
                world,
                delivery,
                request,
            })
            .is_ok()
        }) {
            self.dead = true;
            return Err(ScriptError::new(
                Category::Internal,
                "The response worker has stopped.",
            ));
        }
        self.busy = true;
        Ok(())
    }

    pub(super) fn poll_service_request(&self) -> Option<super::service_calls::Request> {
        self.service_requests.try_recv().ok()
    }

    /// A host request awaiting the application UI (never blocks). Stale
    /// queued requests are discarded after cancel, completion or timeout.
    pub fn poll_panel_request(&self) -> Option<Arc<super::ui::Shared>> {
        while let Ok(request) = self.panel_requests.try_recv() {
            if request.active() {
                return Some(request);
            }
        }
        None
    }
    pub fn poll_overlay_request(&self) -> Option<super::overlays::Batch> {
        self.overlay_requests.try_recv().ok()
    }
    pub(super) fn overlay_reads(&self) -> [bool; 2] {
        self.overlay_reads
    }
    pub(super) fn has_pending_overlay(&self) -> bool {
        self.overlay_pending.load(Ordering::Acquire)
    }
    pub fn poll_playback_request(&self) -> Option<super::playback::control::Request> {
        while let Ok(request) = self.playback_requests.try_recv() {
            if request.lease.check().is_ok() {
                return Some(request);
            }
        }
        None
    }

    pub fn poll_ui_request(&self) -> Option<super::bridge::UiRequest> {
        while let Ok(request) = self.ui_requests.try_recv() {
            if request.lease.check().is_ok() {
                return Some(request);
            }
        }
        None
    }
    pub fn poll_midi_input_request(&self) -> Option<super::midi_input::ConsentRequest> {
        while let Ok(request) = self.midi_input_requests.try_recv() {
            if request.lease.check().is_ok() {
                return Some(request);
            }
        }
        None
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
            response_guard: None,
            service_reply: None,
        })
    }

    /// Stops the running script at its next check (it ends `canceled`).
    pub fn cancel(&self) {
        self.cancel_midi_output();
        self.cancel_job();
    }
    /// Coalesced native cleanup; no Lua execution/result or UI-thread resource drop.
    pub fn release_ui_images(&self) {
        let _ =
            self.image_release_epoch
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                    Some(epoch.saturating_add(1))
                });
        if !self.release_images_pending.swap(true, Ordering::AcqRel)
            && self
                .jobs
                .as_ref()
                .is_none_or(|sender| sender.send(Work::ReleaseUiImages).is_err())
        {
            self.release_images_pending.store(false, Ordering::Release);
        }
    }
    pub(crate) fn cancel_midi_output(&self) {
        self.midi_cancel_epoch.fetch_add(1, Ordering::SeqCst);
    }
    /// Internal preemption of a read-only automatic job preserves accepted MIDI.
    pub(crate) fn cancel_job(&self) {
        self.control_epoch.fetch_add(1, Ordering::SeqCst);
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
        self.cancel();
        self.jobs = None;
    }
}
