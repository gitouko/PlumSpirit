//! Native worker-owned WinMM input. No public Lua API or device consent is provided
//! here. Callers must obtain a fresh host approval before constructing `Approved`.
//! No SysEx buffers are registered. The callback only admits fixed short packets.
use super::native::{Capture, Status};
use crate::lua::bridge::{NativeBudget, Reservation};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;
#[cfg(windows)]
use windows_sys::Win32::Media::Audio::{
    CALLBACK_FUNCTION, HMIDIIN, MIDI_IO_STATUS, MIDIINCAPSW, midiInClose, midiInGetDevCapsW,
    midiInGetID, midiInGetNumDevs, midiInOpen, midiInReset, midiInStart, midiInStop,
};
#[cfg(windows)]
use windows_sys::Win32::Media::{
    MM_MIM_CLOSE, MM_MIM_DATA, MM_MIM_ERROR, MM_MIM_LONGDATA, MM_MIM_LONGERROR, MM_MIM_MOREDATA,
};

pub const MAX_DEVICES: usize = 256;
pub const MAX_OPEN: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    manufacturer: u16,
    product: u16,
    version: u32,
    name: [u16; 32],
    support: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub slot: u32,
    identity: Identity,
}
impl Device {
    #[cfg(test)]
    pub(crate) fn fixture(slot: u32, name: &str) -> Self {
        let mut raw = [0; 32];
        for (a, b) in raw.iter_mut().zip(name.encode_utf16()) {
            *a = b;
        }
        Self {
            slot,
            identity: Identity {
                manufacturer: 1,
                product: 2,
                version: 3,
                name: raw,
                support: 0,
            },
        }
    }
    /// Labels are display only, never persistent device identifiers.
    pub fn name(&self) -> String {
        let n = self
            .identity
            .name
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(32);
        let s: String = String::from_utf16_lossy(&self.identity.name[..n])
            .chars()
            .filter(|c| !c.is_control() && !matches!(c, '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
            .collect();
        let s = s.trim();
        if s.is_empty() {
            "Unnamed MIDI input".into()
        } else {
            s.into()
        }
    }
}
pub struct Inventory {
    pub devices: Vec<Device>,
    pub reported: u32,
    pub unavailable: usize,
    pub truncated: bool,
}

/// Fresh host-selected device only. Never serialized or manufactured from Lua.
pub struct Approved(Device);
impl Approved {
    pub fn device(&self) -> &Device {
        &self.0
    }
    pub(super) fn after_user_consent(device: Device) -> Self {
        Self(device)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Allocation,
    Budget,
    Slots,
    Queue(super::native::Error),
    DeviceChanged,
    Disconnected,
    Driver { operation: &'static str, code: u32 },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloseReport {
    pub stop: u32,
    pub reset: u32,
    pub close: u32,
}
pub struct OpenFailure {
    pub reason: Error,
    pub cleanup: Option<CloseReport>,
}
impl From<Error> for OpenFailure {
    fn from(reason: Error) -> Self {
        Self {
            reason,
            cleanup: None,
        }
    }
}

struct Slot(Arc<AtomicUsize>);
impl Slot {
    fn take(pool: &Arc<AtomicUsize>) -> Result<Self, Error> {
        pool.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            n.checked_add(1).filter(|n| *n <= MAX_OPEN)
        })
        .map_err(|_| Error::Slots)?;
        Ok(Self(Arc::clone(pool)))
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
fn global_pool() -> &'static Arc<AtomicUsize> {
    static POOL: OnceLock<Arc<AtomicUsize>> = OnceLock::new();
    POOL.get_or_init(|| Arc::new(AtomicUsize::new(0)))
}

pub struct Context {
    pub capture: Capture,
    epoch: Instant,
    // Reservations and slots outlive every reader and any driver callback.
    _reservation: Reservation,
    _slot: Slot,
}
impl Context {
    #[cfg(test)]
    pub(crate) fn fixture(capacity: usize, budget: &NativeBudget) -> Result<Arc<Self>, Error> {
        Self::new(capacity, budget, &Arc::new(AtomicUsize::new(0)))
    }
    fn new(
        capacity: usize,
        budget: &NativeBudget,
        pool: &Arc<AtomicUsize>,
    ) -> Result<Arc<Self>, Error> {
        let bytes = Capture::reservation_bytes(capacity)
            .ok_or(Error::Queue(super::native::Error::Capacity))?
            .checked_add(16 * 1024)
            .ok_or(Error::Budget)?;
        let slot = Slot::take(pool)?;
        let reservation = budget.reserve(bytes).map_err(|_| Error::Budget)?;
        let capture = Capture::new(capacity).map_err(Error::Queue)?;
        Ok(Arc::new(Self {
            capture,
            epoch: Instant::now(),
            _reservation: reservation,
            _slot: slot,
        }))
    }
    #[cfg(windows)]
    fn notification(&self, message: u32, packet: u32, driver_ms: u32) {
        match message {
            MM_MIM_DATA | MM_MIM_MOREDATA => {
                let ns = self.epoch.elapsed().as_nanos();
                let Ok(ns) = u64::try_from(ns) else {
                    self.capture.close(Status::Exhausted);
                    return;
                };
                self.capture
                    .ingest(packet, driver_ms, ns, message == MM_MIM_MOREDATA);
            }
            MM_MIM_ERROR | MM_MIM_LONGERROR | MM_MIM_LONGDATA => self.capture.driver_error(),
            MM_MIM_CLOSE => self.capture.close(Status::Disconnected),
            _ => (),
        }
    }
}

// SAFETY: instance points to Context retained by Owner until midiInClose succeeds.
// The function only uses bounded native admission; it never calls multimedia/Lua/UI.
#[cfg(windows)]
unsafe extern "system" fn callback(
    _: HMIDIIN,
    message: u32,
    instance: usize,
    p1: usize,
    p2: usize,
) {
    if instance == 0 {
        return;
    }
    let context = unsafe { &*(instance as *const Context) };
    context.notification(message, p1 as u32, p2 as u32);
}

/// Narrow driver seam, private to the native owner and manufactured lifecycle tests.
///
/// # Safety
/// Failed open must leave no live callback/handle. Successful close must return only
/// after all callbacks have returned, and prevent all later callbacks. A failed close
/// may retain the instance, so it must never be freed. Operations never panic.
pub(crate) unsafe trait Driver {
    fn count(&self) -> u32;
    fn caps(&self, id_or_handle: usize) -> Result<Identity, u32>;
    /// # Safety
    /// `instance` points to a Context retained until successful close (or forever).
    unsafe fn open(&self, slot: u32, instance: usize) -> Result<usize, u32>;
    fn id(&self, handle: usize) -> Result<u32, u32>;
    fn start(&self, handle: usize) -> u32;
    fn stop(&self, handle: usize) -> u32;
    fn reset(&self, handle: usize) -> u32;
    fn close(&self, handle: usize) -> u32;
}
pub struct WinMm;
// SAFETY: WinMM owns its callbacks; successful close waits for callbacks. No SysEx
// buffers exist, so there are no outstanding user buffers to free on failure.
#[cfg(windows)]
unsafe impl Driver for WinMm {
    fn count(&self) -> u32 {
        unsafe { midiInGetNumDevs() }
    }
    fn caps(&self, id_or_handle: usize) -> Result<Identity, u32> {
        let mut c = MIDIINCAPSW::default();
        // SAFETY: correctly sized local packed output; copied fields avoid unaligned references.
        let result = unsafe {
            midiInGetDevCapsW(
                id_or_handle,
                &mut c,
                std::mem::size_of::<MIDIINCAPSW>() as u32,
            )
        };
        if result != 0 {
            return Err(result);
        }
        Ok(Identity {
            manufacturer: c.wMid,
            product: c.wPid,
            version: c.vDriverVersion,
            name: c.szPname,
            support: c.dwSupport,
        })
    }
    unsafe fn open(&self, slot: u32, instance: usize) -> Result<usize, u32> {
        let mut h = std::ptr::null_mut();
        // SAFETY: stable Context instance retained before call; fixed native callback ABI.
        let r = unsafe {
            midiInOpen(
                &mut h,
                slot,
                callback as *const () as usize,
                instance,
                CALLBACK_FUNCTION | MIDI_IO_STATUS,
            )
        };
        if r == 0 { Ok(h as usize) } else { Err(r) }
    }
    fn id(&self, handle: usize) -> Result<u32, u32> {
        let mut id = 0;
        // SAFETY: only called with our successfully opened handle before close.
        let r = unsafe { midiInGetID(handle as HMIDIIN, &mut id) };
        if r == 0 { Ok(id) } else { Err(r) }
    }
    fn start(&self, handle: usize) -> u32 {
        unsafe { midiInStart(handle as HMIDIIN) }
    }
    fn stop(&self, handle: usize) -> u32 {
        unsafe { midiInStop(handle as HMIDIIN) }
    }
    fn reset(&self, handle: usize) -> u32 {
        unsafe { midiInReset(handle as HMIDIIN) }
    }
    fn close(&self, handle: usize) -> u32 {
        unsafe { midiInClose(handle as HMIDIIN) }
    }
}

// SAFETY: unsupported platform never opens a handle or starts a callback.
#[cfg(not(windows))]
unsafe impl Driver for WinMm {
    fn count(&self) -> u32 {
        0
    }
    fn caps(&self, _: usize) -> Result<Identity, u32> {
        Err(50)
    }
    unsafe fn open(&self, _: u32, _: usize) -> Result<usize, u32> {
        Err(50)
    }
    fn id(&self, _: usize) -> Result<u32, u32> {
        Err(50)
    }
    fn start(&self, _: usize) -> u32 {
        50
    }
    fn stop(&self, _: usize) -> u32 {
        50
    }
    fn reset(&self, _: usize) -> u32 {
        50
    }
    fn close(&self, _: usize) -> u32 {
        50
    }
}

fn inventory_with(driver: &impl Driver) -> Result<Inventory, Error> {
    let reported = driver.count();
    let n = (reported as usize).min(MAX_DEVICES);
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(n)
        .map_err(|_| Error::Allocation)?;
    let mut unavailable = 0;
    for slot in 0..n as u32 {
        match driver.caps(slot as usize) {
            Ok(identity) => devices.push(Device { slot, identity }),
            Err(_) => unavailable += 1,
        }
    }
    Ok(Inventory {
        devices,
        reported,
        unavailable,
        truncated: reported as usize > MAX_DEVICES,
    })
}
pub fn inventory() -> Result<Inventory, Error> {
    inventory_with(&WinMm)
}

/// Construct and destroy on a native worker, never on the rendering/audio thread.
/// Rc marker prevents accidentally sending a live handle to another thread.
pub struct Owner<D: Driver = WinMm> {
    driver: D,
    device: Device,
    handle: Option<usize>,
    context: Option<Arc<Context>>,
    _worker_only: PhantomData<Rc<()>>,
}
impl Owner {
    pub fn open(
        approved: Approved,
        capacity: usize,
        budget: &NativeBudget,
    ) -> Result<Self, OpenFailure> {
        Self::open_with(WinMm, approved, capacity, budget, global_pool())
    }
}
impl<D: Driver> Owner<D> {
    fn open_with(
        driver: D,
        approved: Approved,
        capacity: usize,
        budget: &NativeBudget,
        pool: &Arc<AtomicUsize>,
    ) -> Result<Self, OpenFailure> {
        let device = approved.0;
        if device.slot as usize >= MAX_DEVICES
            || driver.caps(device.slot as usize) != Ok(device.identity.clone())
        {
            return Err(Error::DeviceChanged.into());
        }
        let context = Context::new(capacity, budget, pool)?;
        // SAFETY: the native owner retains this stable Arc until successful close.
        let handle = unsafe { driver.open(device.slot, Arc::as_ptr(&context) as usize) }.map_err(
            |code| Error::Driver {
                operation: "open",
                code,
            },
        )?;
        let mut owner = Self {
            driver,
            device: device.clone(),
            handle: Some(handle),
            context: Some(context),
            _worker_only: PhantomData,
        };
        let reason = if owner.driver.caps(handle) != Ok(device.identity)
            || owner.driver.id(handle) != Ok(device.slot)
        {
            Some(Error::DeviceChanged)
        } else if owner.context.as_ref().unwrap().capture.status() != Status::Open {
            Some(Error::Disconnected)
        } else {
            let code = owner.driver.start(handle);
            if code != 0 {
                Some(Error::Driver {
                    operation: "start",
                    code,
                })
            } else {
                (owner.context.as_ref().unwrap().capture.status() != Status::Open)
                    .then_some(Error::Disconnected)
            }
        };
        if let Some(reason) = reason {
            return Err(OpenFailure {
                reason,
                cleanup: owner.shutdown(),
            });
        }
        Ok(owner)
    }
    pub fn context(&self) -> Arc<Context> {
        Arc::clone(self.context.as_ref().expect("live native input owner"))
    }
    /// Worker-only periodic check, not a callback. Legacy WinMM has no universal
    /// hotplug notification; disappearance/changed identity or invalid handles revoke.
    /// Equal legacy descriptors are not proof of a persistent physical identity.
    pub fn check_connection(&self) -> bool {
        let (Some(handle), Some(context)) = (self.handle, self.context.as_ref()) else {
            return false;
        };
        if context.capture.status() != Status::Open {
            return false;
        }
        let connected = self.driver.count() > self.device.slot
            && self.driver.caps(self.device.slot as usize) == Ok(self.device.identity.clone())
            && self.driver.id(handle) == Ok(self.device.slot)
            && self.driver.caps(handle) == Ok(self.device.identity.clone());
        if !connected {
            context.capture.close(Status::Disconnected);
        }
        connected
    }
    pub fn shutdown(&mut self) -> Option<CloseReport> {
        let handle = self.handle.take()?;
        let context = self.context.take().expect("live callback context");
        context.capture.close(Status::Closed);
        let report = CloseReport {
            stop: self.driver.stop(handle),
            reset: self.driver.reset(handle),
            close: self.driver.close(handle),
        };
        if report.close != 0 {
            // The driver may still call the raw instance. Preserve memory, native
            // reservation and slot for process lifetime. At most MAX_OPEN can exist.
            let _retained_for_driver = Arc::into_raw(context);
        }
        Some(report)
    }
}
impl<D: Driver> Drop for Owner<D> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(all(test, windows))]
#[path = "midi_input_windows_tests.rs"]
mod tests;
#[cfg(all(test, windows))]
pub(crate) use tests::fixture_owner;
