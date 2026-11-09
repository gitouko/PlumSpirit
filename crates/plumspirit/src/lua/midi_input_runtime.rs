//! Bounded native I/O tasks. A slow WinMM call never occupies Lua/UI/audio threads.
//! Detached tasks retain their task/stack accounting until native work actually ends.
use super::{native, windows as backend};
use crate::lua::bridge::{BridgeError, JobLease, NativeBudget, Reservation};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

pub const MAX_TASKS: usize = 8;
const TASK_BYTES: usize = 2 * 1024 * 1024;
const STACK_BYTES: usize = 1024 * 1024;
const INVENTORY_BYTES: usize = 128 * 1024;
const CHECK: Duration = Duration::from_millis(10);
const CONNECTION_CHECK: Duration = Duration::from_millis(250);

#[derive(Debug)]
pub enum Error {
    Bridge(BridgeError),
    Native(backend::Error),
    Open(backend::Error, Option<backend::CloseReport>),
    Queue(native::Error),
    Busy,
    Spawn,
    Unavailable,
    Wait,
}
impl From<BridgeError> for Error {
    fn from(e: BridgeError) -> Self {
        Self::Bridge(e)
    }
}
struct Permit {
    slots: Arc<AtomicUsize>,
    _reservation: Reservation,
}
fn task_pool() -> &'static Arc<AtomicUsize> {
    static POOL: OnceLock<Arc<AtomicUsize>> = OnceLock::new();
    POOL.get_or_init(|| Arc::new(AtomicUsize::new(0)))
}
impl Permit {
    fn take(budget: &NativeBudget, slots: &Arc<AtomicUsize>) -> Result<Self, Error> {
        let reservation = budget.reserve(TASK_BYTES)?;
        slots
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                n.checked_add(1).filter(|n| *n <= MAX_TASKS)
            })
            .map_err(|_| Error::Busy)?;
        Ok(Self {
            slots: Arc::clone(slots),
            _reservation: reservation,
        })
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.slots.fetch_sub(1, Ordering::AcqRel);
    }
}
fn launch<T: Send + 'static>(
    permit: Permit,
    work: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<Receiver<Result<T, Error>>, Error> {
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("plumspirit-midi-input-io".into())
        .stack_size(STACK_BYTES)
        .spawn(move || {
            let _permit = permit;
            let _ = tx.try_send(work());
        })
        .map_err(|_| Error::Spawn)?;
    Ok(rx)
}
fn wait<T>(lease: &JobLease, rx: Receiver<Result<T, Error>>) -> Result<T, Error> {
    loop {
        lease.check()?;
        match rx.recv_timeout(CHECK) {
            Ok(result) => {
                lease.check()?;
                return result;
            }
            Err(RecvTimeoutError::Timeout) => (),
            Err(RecvTimeoutError::Disconnected) => return Err(Error::Unavailable),
        }
    }
}

pub struct Enumerated {
    pub inventory: backend::Inventory,
    _reservation: Reservation,
}
#[cfg(test)]
impl Enumerated {
    pub(crate) fn fixture(
        devices: Vec<backend::Device>,
        budget: &NativeBudget,
    ) -> Result<Self, Error> {
        let reported = devices.len() as u32;
        Ok(Self {
            inventory: backend::Inventory {
                devices,
                reported,
                unavailable: 0,
                truncated: false,
            },
            _reservation: budget.reserve(INVENTORY_BYTES)?,
        })
    }
}
pub fn enumerate(lease: &Arc<JobLease>, budget: &NativeBudget) -> Result<Enumerated, Error> {
    lease.check()?;
    let permit = Permit::take(budget, task_pool())?;
    let reservation = budget.reserve(INVENTORY_BYTES)?;
    let rx = launch(permit, move || {
        Ok(Enumerated {
            inventory: backend::inventory().map_err(Error::Native)?,
            _reservation: reservation,
        })
    })?;
    wait(lease, rx)
}

pub struct Input {
    pub context: Arc<backend::Context>,
    lease: Arc<JobLease>,
    stop: Arc<AtomicBool>,
}
impl Input {
    #[cfg(test)]
    pub(crate) fn fixture(
        lease: Arc<JobLease>,
        budget: &NativeBudget,
        capacity: usize,
    ) -> Result<Self, Error> {
        Ok(Self {
            context: backend::Context::fixture(capacity, budget).map_err(Error::Native)?,
            lease,
            stop: Arc::new(AtomicBool::new(false)),
        })
    }
    pub fn statistics(&self) -> Result<native::Stats, Error> {
        self.lease.check()?;
        let s = self.context.capture.statistics().map_err(Error::Queue)?;
        self.lease.check()?;
        Ok(s)
    }
    pub fn read(&self, maximum: usize, wait_ms: u64) -> Result<native::Batch, Error> {
        if !(1..=native::MAX_BATCH).contains(&maximum) || wait_ms > 1000 {
            return Err(Error::Wait);
        }
        let until = Instant::now() + Duration::from_millis(wait_ms);
        loop {
            let stats = self.statistics()?;
            if stats.queued != 0 || stats.status != native::Status::Open || Instant::now() >= until
            {
                let b = self.context.capture.drain(maximum).map_err(Error::Queue)?;
                self.lease.check()?;
                return Ok(b);
            }
            let remaining = until.saturating_duration_since(Instant::now());
            std::thread::sleep(CHECK.min(remaining));
        }
    }
    pub fn close(&self) {
        self.context.capture.close(native::Status::Closed);
        self.stop.store(true, Ordering::Release);
    }
}
impl Drop for Input {
    fn drop(&mut self) {
        self.close();
    }
}

pub fn open(
    approved: backend::Approved,
    capacity: usize,
    lease: Arc<JobLease>,
    budget: &NativeBudget,
) -> Result<Input, Error> {
    let native_budget = budget.clone();
    open_with(lease, budget, move || {
        backend::Owner::open(approved, capacity, &native_budget)
    })
}
fn open_with<D: backend::Driver + 'static>(
    lease: Arc<JobLease>,
    budget: &NativeBudget,
    factory: impl FnOnce() -> Result<backend::Owner<D>, backend::OpenFailure> + Send + 'static,
) -> Result<Input, Error> {
    lease.check()?;
    let permit = Permit::take(budget, task_pool())?;
    let stop = Arc::new(AtomicBool::new(false));
    let child_stop = Arc::clone(&stop);
    let child_lease = Arc::clone(&lease);
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::Builder::new().name("plumspirit-midi-input-owner".into()).stack_size(STACK_BYTES)
        .spawn(move || {
            let _permit = permit;
            if let Err(e) = child_lease.check() { let _ = tx.try_send(Err(Error::Bridge(e))); return; }
            let mut owner = match factory() {
                Ok(owner) => owner,
                Err(e) => { let _ = tx.try_send(Err(Error::Open(e.reason, e.cleanup))); return; }
            };
            let context = owner.context();
            if child_lease.check().is_ok() && tx.try_send(Ok(Arc::clone(&context))).is_ok() {
                let mut probe = Instant::now();
                while !child_stop.load(Ordering::Acquire) && child_lease.check().is_ok()
                    && context.capture.status() == native::Status::Open {
                    if probe.elapsed() >= CONNECTION_CHECK {
                        if !owner.check_connection() { break; }
                        probe = Instant::now();
                    }
                    std::thread::sleep(CHECK);
                }
            }
            context.capture.close(native::Status::Closed);
            if let Some(report) = owner.shutdown()
                && (report.stop != 0 || report.reset != 0 || report.close != 0) {
                log::warn!("Plugin MIDI input cleanup codes: stop={}, reset={}, close={}; failed-close context remains reserved", report.stop, report.reset, report.close);
            }
        }).map_err(|_| Error::Spawn)?;
    let context = wait(&lease, rx)?;
    lease.check()?;
    Ok(Input {
        context,
        lease,
        stop,
    })
}

#[cfg(test)]
#[path = "midi_input_runtime_tests.rs"]
mod tests;
