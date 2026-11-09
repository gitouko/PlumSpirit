//! Host-owned UI requests. The worker may wait; UI polling never waits.
//! No Lua values cross this channel. A job lease expires even if a script
//! retained its userdata or a native dialog outlives cancellation.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use plumspirit_engine::history::ProjectSerial;

use super::{Category, stop};

const WAIT_CHECK: Duration = Duration::from_millis(10);
pub const MAX_DIALOGS_PER_JOB: usize = 8;
pub const NATIVE_PLUGIN_BYTES: usize = 256 * 1024 * 1024;
pub const NATIVE_GLOBAL_BYTES: usize = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectStamp {
    pub serial: ProjectSerial,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BridgeError {
    Canceled,
    Expired,
    Deadline,
    Unavailable,
    Busy,
    NotUserInvoked,
    Budget,
}

impl BridgeError {
    pub fn lua(&self) -> mlua::Error {
        let (category, message) = match self {
            Self::Canceled => (Category::Canceled, "The plugin job was canceled."),
            Self::Expired => (
                Category::Expired,
                "This UI request belongs to a finished job; invoke the command again.",
            ),
            Self::Deadline => (
                Category::TimeLimit,
                "The plugin's time limit elapsed while waiting for the dialog.",
            ),
            Self::Unavailable => (
                Category::Runtime,
                "The application UI is no longer available.",
            ),
            Self::Busy => (
                Category::Budget,
                "A plugin UI request is already waiting; finish or cancel it first.",
            ),
            Self::NotUserInvoked => (
                Category::Capability,
                "File dialogs require an explicitly invoked plugin command; loading, lifecycle hooks and automatic previews cannot prompt.",
            ),
            Self::Budget => (
                Category::Budget,
                "This job exceeded its dialog or native resource budget.",
            ),
        };
        stop(category, message)
    }
}

/// Constructed by the host for each run, never by Lua.
pub struct JobLease {
    pub plugin: Arc<str>,
    pub job: u64,
    pub project: Option<ProjectStamp>,
    deadline: Instant,
    cancel: Arc<AtomicBool>,
    active: AtomicBool,
    interactive: bool,
    requests: AtomicUsize,
}

impl JobLease {
    #[cfg(test)]
    pub(crate) fn fixture(
        plugin: &str,
        project: Option<ProjectStamp>,
        time: Duration,
        interactive: bool,
    ) -> Arc<Self> {
        Arc::new(Self::new(
            Arc::from(plugin),
            1,
            project,
            time,
            Arc::new(AtomicBool::new(false)),
            interactive,
        ))
    }
    pub fn user_invoked(&self) -> bool {
        self.interactive
    }
    /// Shared bounded allowance for host-owned file/device consent surfaces.
    pub(crate) fn reserve_prompt(&self) -> Result<usize, BridgeError> {
        self.check()?;
        if !self.interactive {
            return Err(BridgeError::NotUserInvoked);
        }
        self.requests
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                n.checked_add(1).filter(|n| *n <= MAX_DIALOGS_PER_JOB)
            })
            .map_err(|_| BridgeError::Budget)
    }
    /// Only a live host-owned surface may cancel its owning job.
    pub fn cancel_from_surface(&self) {
        if self.active.load(Ordering::Acquire) {
            self.cancel.store(true, Ordering::Release);
        }
    }
    pub fn cancellation_requested(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    fn new(
        plugin: Arc<str>,
        job: u64,
        project: Option<ProjectStamp>,
        time: Duration,
        cancel: Arc<AtomicBool>,
        interactive: bool,
    ) -> Self {
        Self {
            plugin,
            job,
            project,
            deadline: Instant::now()
                .checked_add(time)
                .unwrap_or_else(Instant::now),
            cancel,
            active: AtomicBool::new(true),
            interactive,
            requests: AtomicUsize::new(0),
        }
    }

    pub fn check(&self) -> Result<(), BridgeError> {
        if !self.active.load(Ordering::Acquire) {
            return Err(BridgeError::Expired);
        }
        if self.cancel.load(Ordering::Acquire) {
            return Err(BridgeError::Canceled);
        }
        if Instant::now() >= self.deadline {
            return Err(BridgeError::Deadline);
        }
        Ok(())
    }

    pub fn matches_project(&self, current: Option<ProjectStamp>) -> bool {
        self.project == current && self.check().is_ok()
    }

    pub fn finish(&self) {
        self.active.store(false, Ordering::Release);
    }
}

/// Validated by the file API before queueing. The host supplies the title.
#[derive(Clone, Debug)]
pub struct FileDialogSpec {
    pub save: bool,
    pub filters: Vec<(String, Vec<String>)>,
    pub suggested_name: Option<String>,
}

pub struct UiRequest {
    pub lease: Arc<JobLease>,
    pub id: usize,
    pub spec: FileDialogSpec,
    reply: SyncSender<Result<Option<PathBuf>, BridgeError>>,
}

impl UiRequest {
    pub fn reject(self, error: BridgeError) {
        let _ = self.reply.try_send(Err(error));
    }

    pub fn respond(
        self,
        current: Option<ProjectStamp>,
        path: Option<PathBuf>,
    ) -> Result<(), BridgeError> {
        let response = self.lease.check().and_then(|()| {
            if self.lease.project == current {
                Ok(path)
            } else {
                Err(BridgeError::Expired)
            }
        });
        self.reply
            .try_send(response)
            .map_err(|_| BridgeError::Expired)
    }
}

#[derive(Clone)]
pub struct UiClient {
    plugin: Arc<str>,
    requests: SyncSender<UiRequest>,
}

pub fn channel(plugin: &str) -> (UiClient, Receiver<UiRequest>) {
    let (requests, receiver) = mpsc::sync_channel(1);
    (
        UiClient {
            plugin: Arc::from(plugin),
            requests,
        },
        receiver,
    )
}

impl UiClient {
    pub fn request(
        &self,
        lease: &Arc<JobLease>,
        spec: FileDialogSpec,
    ) -> Result<Option<PathBuf>, BridgeError> {
        lease.check()?;
        if !lease.interactive {
            return Err(BridgeError::NotUserInvoked);
        }
        if self.plugin != lease.plugin {
            return Err(BridgeError::Expired);
        }
        let id = lease.reserve_prompt()?;
        let (reply, receiver) = mpsc::sync_channel(1);
        self.requests
            .try_send(UiRequest {
                lease: Arc::clone(lease),
                id,
                spec,
                reply,
            })
            .map_err(|e| match e {
                TrySendError::Full(_) => BridgeError::Busy,
                TrySendError::Disconnected(_) => BridgeError::Unavailable,
            })?;
        loop {
            lease.check()?;
            match receiver.recv_timeout(WAIT_CHECK) {
                Ok(response) => {
                    lease.check()?;
                    return response;
                }
                Err(RecvTimeoutError::Timeout) => (),
                Err(RecvTimeoutError::Disconnected) => {
                    lease.check()?;
                    return Err(BridgeError::Unavailable);
                }
            }
        }
    }
}

/// App data belongs to one Lua state. A new scope is created for every run.
#[derive(Default)]
pub(super) struct Session {
    pub client: Option<UiClient>,
    pub interactive: bool,
    pub lease: Option<Arc<JobLease>>,
}

pub(super) fn begin_run(lua: &mlua::Lua, time: Duration, cancel: Arc<AtomicBool>) {
    static NEXT_JOB: AtomicU64 = AtomicU64::new(1);
    let project = super::handles::current_world(lua)
        .map(|w| ProjectStamp {
            serial: w.project_serial(),
            revision: w.info.revision,
        })
        .or_else(|| super::reactive::view_project(lua));
    if let Some(mut session) = lua.app_data_mut::<Session>() {
        if let Some(old) = session.lease.take() {
            old.finish();
        }
        let plugin = session
            .client
            .as_ref()
            .map_or_else(|| Arc::from("script"), |c| Arc::clone(&c.plugin));
        session.lease = Some(Arc::new(JobLease::new(
            plugin,
            NEXT_JOB.fetch_add(1, Ordering::Relaxed),
            project,
            time,
            cancel,
            session.interactive,
        )));
    }
}

pub(super) fn end_run(lua: &mlua::Lua) {
    if let Some(mut session) = lua.app_data_mut::<Session>() {
        if let Some(lease) = session.lease.take() {
            lease.finish();
        }
        session.interactive = false;
    }
}

/// Native allocations have finite global and per-state caps independent of
/// the Lua VM. Reservations must live as long as the buffers they account for.
#[derive(Clone)]
pub struct NativeBudget {
    local: Arc<AtomicUsize>,
    global: Arc<AtomicUsize>,
    local_limit: usize,
    global_limit: usize,
}

pub struct Reservation {
    budget: NativeBudget,
    bytes: usize,
}

fn acquire(counter: &AtomicUsize, limit: usize, bytes: usize) -> Result<(), BridgeError> {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            n.checked_add(bytes).filter(|n| *n <= limit)
        })
        .map(|_| ())
        .map_err(|_| BridgeError::Budget)
}

impl Default for NativeBudget {
    fn default() -> Self {
        static GLOBAL: OnceLock<Arc<AtomicUsize>> = OnceLock::new();
        Self {
            local: Arc::new(AtomicUsize::new(0)),
            global: Arc::clone(GLOBAL.get_or_init(|| Arc::new(AtomicUsize::new(0)))),
            local_limit: NATIVE_PLUGIN_BYTES,
            global_limit: NATIVE_GLOBAL_BYTES,
        }
    }
}

impl NativeBudget {
    /// Quota fixtures must not exhaust the process-wide pool of parallel tests.
    #[cfg(test)]
    pub(crate) fn isolated(limit: usize) -> Self {
        Self {
            local: Arc::new(AtomicUsize::new(0)),
            global: Arc::new(AtomicUsize::new(0)),
            local_limit: limit,
            global_limit: limit,
        }
    }

    /// Reload candidates and retiring workers share the owner's limit.
    /// Weak entries do not retain resource allocations after plugin removal.
    pub fn for_plugin(plugin: &str) -> Self {
        type Owners = std::collections::BTreeMap<String, Weak<AtomicUsize>>;
        static OWNERS: OnceLock<Mutex<Owners>> = OnceLock::new();
        let mut budget = Self::default();
        if let Ok(mut owners) = OWNERS.get_or_init(Mutex::default).lock() {
            owners.retain(|_, counter| counter.strong_count() > 0);
            if let Some(local) = owners.get(plugin).and_then(Weak::upgrade) {
                budget.local = local;
            } else {
                owners.insert(plugin.to_owned(), Arc::downgrade(&budget.local));
            }
        } else {
            // A poisoned accounting registry must fail closed.
            budget.local_limit = 0;
        }
        budget
    }

    pub fn reserve(&self, bytes: usize) -> Result<Reservation, BridgeError> {
        acquire(&self.local, self.local_limit, bytes)?;
        if let Err(e) = acquire(&self.global, self.global_limit, bytes) {
            self.local.fetch_sub(bytes, Ordering::AcqRel);
            return Err(e);
        }
        Ok(Reservation {
            budget: self.clone(),
            bytes,
        })
    }

    pub fn used(&self) -> usize {
        self.local.load(Ordering::Acquire)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.local.fetch_sub(self.bytes, Ordering::AcqRel);
        self.budget.global.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

impl Reservation {
    /// Release temporary decode workspace while retaining the final buffer.
    pub fn shrink_to(&mut self, bytes: usize) {
        assert!(
            bytes <= self.bytes,
            "a reservation cannot grow without preflight"
        );
        let released = self.bytes - bytes;
        self.budget.local.fetch_sub(released, Ordering::AcqRel);
        self.budget.global.fetch_sub(released, Ordering::AcqRel);
        self.bytes = bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease(interactive: bool, time: Duration) -> Arc<JobLease> {
        Arc::new(JobLease::new(
            Arc::from("example"),
            7,
            None,
            time,
            Arc::new(AtomicBool::new(false)),
            interactive,
        ))
    }

    fn spec() -> FileDialogSpec {
        FileDialogSpec {
            save: false,
            filters: Vec::new(),
            suggested_name: None,
        }
    }

    #[test]
    fn disconnected_reply_preserves_expired_scope_but_live_backend_loss_is_unavailable() {
        for expired in [false, true] {
            let (client, ui) = channel("example");
            let scope = lease(true, Duration::from_secs(2));
            let worker_scope = Arc::clone(&scope);
            let worker = std::thread::spawn(move || client.request(&worker_scope, spec()));
            let request = ui.recv_timeout(Duration::from_secs(1)).unwrap();
            if expired {
                scope.finish();
            }
            drop(request);
            assert_eq!(
                worker.join().unwrap(),
                Err(if expired {
                    BridgeError::Expired
                } else {
                    BridgeError::Unavailable
                })
            );
        }
    }

    #[test]
    fn ui_can_respond_or_cancel_without_running_lua() {
        for path in [None, Some(PathBuf::from("chosen.txt"))] {
            let (client, ui) = channel("example");
            let scope = lease(true, Duration::from_secs(2));
            let expected = path.clone();
            let worker = std::thread::spawn(move || client.request(&scope, spec()));
            let request = ui.recv_timeout(Duration::from_secs(1)).unwrap();
            assert_eq!(request.lease.plugin.as_ref(), "example");
            assert_eq!(request.lease.job, 7);
            assert_eq!(request.id, 0);
            request.respond(None, path).unwrap();
            assert_eq!(worker.join().unwrap().unwrap(), expected);
        }
    }

    #[test]
    fn lifecycle_cannot_prompt_and_identity_cannot_be_reused() {
        let (client, ui) = channel("example");
        assert_eq!(
            client.request(&lease(false, Duration::from_secs(1)), spec()),
            Err(BridgeError::NotUserInvoked)
        );
        assert!(ui.try_recv().is_err());
        let (other, _) = channel("other");
        assert_eq!(
            other.request(&lease(true, Duration::from_secs(1)), spec()),
            Err(BridgeError::Expired)
        );
    }

    #[test]
    fn cancel_finish_deadline_and_disconnect_release_waiting_worker() {
        for reason in 0..3 {
            let (client, ui) = channel("example");
            let scope = lease(true, Duration::from_secs(2));
            let w = Arc::clone(&scope);
            let worker = std::thread::spawn(move || client.request(&w, spec()));
            let request = ui.recv_timeout(Duration::from_secs(1)).unwrap();
            let expected = match reason {
                0 => {
                    scope.cancel.store(true, Ordering::Release);
                    BridgeError::Canceled
                }
                1 => {
                    scope.finish();
                    BridgeError::Expired
                }
                2 => {
                    drop(request);
                    BridgeError::Unavailable
                }
                _ => unreachable!(),
            };
            assert_eq!(worker.join().unwrap(), Err(expected));
        }
        let (client, _ui) = channel("example");
        assert_eq!(
            client.request(&lease(true, Duration::ZERO), spec()),
            Err(BridgeError::Deadline)
        );
    }

    #[test]
    fn a_reply_for_another_project_is_rejected() {
        let (client, ui) = channel("example");
        let scope = lease(true, Duration::from_secs(2));
        let worker = std::thread::spawn(move || client.request(&scope, spec()));
        let request = ui.recv_timeout(Duration::from_secs(1)).unwrap();
        request
            .respond(
                Some(ProjectStamp {
                    serial: 1,
                    revision: 4,
                }),
                Some(PathBuf::from("private.txt")),
            )
            .unwrap();
        assert_eq!(worker.join().unwrap(), Err(BridgeError::Expired));
    }

    #[test]
    fn queue_and_per_job_prompt_count_are_bounded() {
        let (client, ui) = channel("example");
        let scope = lease(true, Duration::from_secs(2));
        for expected in 0..MAX_DIALOGS_PER_JOB {
            std::thread::scope(|threads| {
                let worker = threads.spawn(|| client.request(&scope, spec()));
                let request = ui.recv_timeout(Duration::from_secs(1)).unwrap();
                assert_eq!(request.id, expected);
                request.respond(None, None).unwrap();
                assert_eq!(worker.join().unwrap(), Ok(None));
            });
        }
        assert_eq!(client.request(&scope, spec()), Err(BridgeError::Budget));
        assert!(ui.try_recv().is_err());

        // A full queue is refused, never waited on from the producer.
        let scope = lease(true, Duration::from_secs(2));
        let (reply, _receiver) = mpsc::sync_channel(1);
        client
            .requests
            .try_send(UiRequest {
                lease: Arc::clone(&scope),
                id: 0,
                spec: spec(),
                reply,
            })
            .ok()
            .unwrap();
        assert_eq!(client.request(&scope, spec()), Err(BridgeError::Busy));
        drop(ui);
        assert_eq!(
            client.request(&scope, spec()),
            Err(BridgeError::Unavailable)
        );
    }

    #[test]
    fn a_queued_request_obeys_its_deadline_without_a_ui_reply() {
        let (client, ui) = channel("example");
        let scope = lease(true, Duration::from_millis(80));
        let worker = std::thread::spawn(move || client.request(&scope, spec()));
        let _request = ui.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(worker.join().unwrap(), Err(BridgeError::Deadline));
    }

    #[test]
    fn native_accounting_is_cumulative_and_rollback_safe() {
        let budget = NativeBudget {
            local: Arc::new(AtomicUsize::new(0)),
            global: Arc::new(AtomicUsize::new(0)),
            local_limit: 20,
            global_limit: 15,
        };
        let a = budget.reserve(10).unwrap();
        assert_eq!(budget.used(), 10);
        assert!(budget.reserve(6).is_err()); // global fails, local rolled back
        assert_eq!(budget.used(), 10);
        assert!(budget.reserve(usize::MAX).is_err());
        let b = budget.reserve(5).unwrap();
        assert_eq!(budget.used(), 15);
        drop(a);
        assert_eq!(budget.used(), 5);
        drop(b);
        assert_eq!(budget.used(), 0);
        assert_eq!(budget.global.load(Ordering::Acquire), 0);
    }

    #[test]
    fn plugin_reload_candidates_share_the_same_native_quota() {
        let old = NativeBudget::for_plugin("bridge.test.reload");
        let candidate = NativeBudget::for_plugin("bridge.test.reload");
        let other = NativeBudget::for_plugin("bridge.test.other");
        let reservation = old.reserve(123).unwrap();
        assert_eq!(candidate.used(), 123);
        assert_eq!(other.used(), 0);
        drop(old);
        assert_eq!(candidate.used(), 123);
        drop(reservation);
        assert_eq!(candidate.used(), 0);
    }

    #[test]
    fn host_run_scopes_expire_even_after_a_lua_error() {
        use super::super::{Host, Limits};
        use std::cell::RefCell;
        use std::rc::Rc;
        let host = Host::new(Limits::default(), []).unwrap();
        let held = Rc::new(RefCell::new(Vec::<Arc<JobLease>>::new()));
        let captured = Rc::clone(&held);
        host.register("capture_scope_for_test", move |lua, _| {
            let scope = lua
                .app_data_ref::<Session>()
                .unwrap()
                .lease
                .clone()
                .unwrap();
            scope.check().map_err(|e| e.lua())?;
            captured.borrow_mut().push(scope);
            Ok(mlua::MultiValue::new())
        })
        .unwrap();
        host.run("ok.lua", "plum.capture_scope_for_test()").unwrap();
        host.run("bad.lua", "plum.capture_scope_for_test(); error('failed')")
            .unwrap_err();
        let scopes = held.borrow();
        assert_eq!(scopes.len(), 2);
        assert_ne!(scopes[0].job, scopes[1].job);
        assert_eq!(scopes[0].check(), Err(BridgeError::Expired));
        assert_eq!(scopes[1].check(), Err(BridgeError::Expired));
    }
}
