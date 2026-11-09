use super::*;
use std::sync::Mutex;
use windows_sys::Win32::Media::MM_MIM_OPEN;

const HANDLE: usize = 0x10000;
#[derive(Default)]
struct State {
    calls: Vec<String>,
    instance: usize,
    count: u32,
    missing: Vec<usize>,
    before: Option<Identity>,
    after: Option<Identity>,
    caps_error: Option<u32>,
    id_error: Option<u32>,
    wrong_id: bool,
    open: u32,
    start: u32,
    stop: u32,
    reset: u32,
    close: u32,
    close_on_open: bool,
    close_on_start: bool,
}
#[derive(Clone)]
pub(crate) struct Fake(Arc<Mutex<State>>);
fn identity(label: &str) -> Identity {
    let mut name = [0; 32];
    for (a, b) in name.iter_mut().zip(label.encode_utf16()) {
        *a = b;
    }
    Identity {
        manufacturer: 1,
        product: 2,
        version: 3,
        name,
        support: 0,
    }
}
fn selected() -> Approved {
    Approved::after_user_consent(Device {
        slot: 0,
        identity: identity("Fixture"),
    })
}
fn setup() -> (Fake, NativeBudget, Arc<AtomicUsize>) {
    (
        Fake(Arc::new(Mutex::new(State {
            count: 1,
            ..State::default()
        }))),
        NativeBudget::isolated(8 * 1024 * 1024),
        Arc::new(AtomicUsize::new(0)),
    )
}
pub(crate) fn fixture_owner(
    capacity: usize,
    budget: &NativeBudget,
) -> Result<Owner<Fake>, OpenFailure> {
    let (driver, _, pool) = setup();
    Owner::open_with(driver, selected(), capacity, budget, &pool)
}
fn send(instance: usize, message: u32, packet: usize, ms: usize) {
    // SAFETY: fixtures only pass a live Context pointer held by Owner or this test.
    unsafe { callback(std::ptr::null_mut(), message, instance, packet, ms) }
}
// SAFETY: manufactured driver calls back synchronously, never after successful
// close. Failed close fixtures retain their callback pointer until explicit cleanup.
unsafe impl Driver for Fake {
    fn count(&self) -> u32 {
        self.0.lock().unwrap().count
    }
    fn caps(&self, id: usize) -> Result<Identity, u32> {
        let mut s = self.0.lock().unwrap();
        s.calls.push(format!("caps:{id}"));
        if let Some(code) = s.caps_error {
            return Err(code);
        }
        if s.missing.contains(&id) {
            return Err(2);
        }
        Ok(if id == HANDLE {
            s.after.clone()
        } else {
            s.before.clone()
        }
        .unwrap_or_else(|| identity("Fixture")))
    }
    unsafe fn open(&self, id: u32, instance: usize) -> Result<usize, u32> {
        let (code, closed) = {
            let mut s = self.0.lock().unwrap();
            s.calls.push(format!("open:{id}"));
            if s.open == 0 {
                s.instance = instance;
            }
            (s.open, s.close_on_open)
        };
        if code != 0 {
            return Err(code);
        }
        send(instance, MM_MIM_OPEN, 0, 0);
        if closed {
            send(instance, MM_MIM_CLOSE, 0, 0);
        }
        Ok(HANDLE)
    }
    fn id(&self, _: usize) -> Result<u32, u32> {
        let mut s = self.0.lock().unwrap();
        s.calls.push("id".into());
        if let Some(code) = s.id_error {
            Err(code)
        } else {
            Ok(u32::from(s.wrong_id))
        }
    }
    fn start(&self, _: usize) -> u32 {
        let (code, instance, closed) = {
            let mut s = self.0.lock().unwrap();
            s.calls.push("start".into());
            (s.start, s.instance, s.close_on_start)
        };
        if closed {
            send(instance, MM_MIM_CLOSE, 0, 0);
        }
        code
    }
    fn stop(&self, _: usize) -> u32 {
        let mut s = self.0.lock().unwrap();
        s.calls.push("stop".into());
        // SAFETY: Owner retains its context throughout stop/reset/close.
        assert_ne!(
            unsafe { &*(s.instance as *const Context) }.capture.status(),
            Status::Open
        );
        s.stop
    }
    fn reset(&self, _: usize) -> u32 {
        let mut s = self.0.lock().unwrap();
        s.calls.push("reset".into());
        s.reset
    }
    fn close(&self, _: usize) -> u32 {
        let (code, instance) = {
            let mut s = self.0.lock().unwrap();
            s.calls.push("close".into());
            (s.close, s.instance)
        };
        if code == 0 {
            send(instance, MM_MIM_CLOSE, 0, 0);
        }
        code
    }
}

#[test]
fn bounded_inventory_preserves_real_slots_raw_identity_and_safe_display_labels() {
    let (driver, _, _) = setup();
    {
        let mut s = driver.0.lock().unwrap();
        s.count = 512;
        s.missing = vec![3, 7, 255];
    }
    let inv = inventory_with(&driver).unwrap();
    assert_eq!(
        (
            inv.reported,
            inv.unavailable,
            inv.truncated,
            inv.devices.len()
        ),
        (512, 3, true, 253)
    );
    assert_eq!(inv.devices[3].slot, 4);
    assert_eq!(driver.0.lock().unwrap().calls.len(), MAX_DEVICES);
    let device = Device {
        slot: 7,
        identity: identity("  A\nB\u{202e}C\u{2066}\u{61c}  "),
    };
    assert_eq!(device.name(), "ABC");
    assert_ne!(device.identity, identity("ABC"));
    assert_eq!(
        Device {
            slot: 0,
            identity: identity("\n\u{202e}")
        }
        .name(),
        "Unnamed MIDI input"
    );
    let mut malformed = identity("x");
    malformed.name[0] = 0xd800;
    assert_eq!(
        Device {
            slot: 0,
            identity: malformed
        }
        .name(),
        "\u{fffd}"
    );
}

#[test]
fn changed_or_unavailable_selection_is_refused_before_any_native_open_or_allocation() {
    for mode in 0..3 {
        let (driver, budget, pool) = setup();
        let mut approved = selected();
        match mode {
            0 => driver.0.lock().unwrap().before = Some(identity("Different")),
            1 => driver.0.lock().unwrap().caps_error = Some(2),
            _ => approved.0.slot = MAX_DEVICES as u32,
        }
        let failure = Owner::open_with(driver.clone(), approved, 16, &budget, &pool)
            .err()
            .expect("identity refusal");
        assert_eq!(failure.reason, Error::DeviceChanged);
        assert!(failure.cleanup.is_none());
        assert!(
            !driver
                .0
                .lock()
                .unwrap()
                .calls
                .iter()
                .any(|x| x.starts_with("open"))
        );
        assert_eq!((budget.used(), pool.load(Ordering::Acquire)), (0, 0));
    }
}

#[test]
fn opened_handle_identity_and_id_are_checked_before_start_and_failure_closes_once() {
    for mode in 0..3 {
        let (driver, budget, pool) = setup();
        {
            let mut s = driver.0.lock().unwrap();
            match mode {
                0 => s.after = Some(identity("Changed")),
                1 => s.wrong_id = true,
                _ => s.id_error = Some(5),
            }
        }
        let failure = Owner::open_with(driver.clone(), selected(), 16, &budget, &pool)
            .err()
            .expect("post-open refusal");
        assert_eq!(failure.reason, Error::DeviceChanged);
        assert_eq!(
            failure.cleanup,
            Some(CloseReport {
                stop: 0,
                reset: 0,
                close: 0
            })
        );
        let s = driver.0.lock().unwrap();
        assert!(!s.calls.contains(&"start".into()));
        assert_eq!(&s.calls[s.calls.len() - 3..], &["stop", "reset", "close"]);
        assert_eq!(s.calls.iter().filter(|x| *x == "close").count(), 1);
        assert_eq!((budget.used(), pool.load(Ordering::Acquire)), (0, 0));
    }
}

#[test]
fn failed_open_and_start_release_reservations_and_preserve_cleanup_error_codes() {
    let (driver, budget, pool) = setup();
    driver.0.lock().unwrap().open = 4;
    let failure = Owner::open_with(driver.clone(), selected(), 16, &budget, &pool)
        .err()
        .expect("open error");
    assert_eq!(
        failure.reason,
        Error::Driver {
            operation: "open",
            code: 4
        }
    );
    assert!(failure.cleanup.is_none());
    assert_eq!((budget.used(), pool.load(Ordering::Acquire)), (0, 0));
    assert!(!driver.0.lock().unwrap().calls.contains(&"close".into()));
    {
        let mut s = driver.0.lock().unwrap();
        s.open = 0;
        s.start = 5;
        s.stop = 12;
        s.reset = 13;
    }
    let failure = Owner::open_with(driver.clone(), selected(), 16, &budget, &pool)
        .err()
        .expect("start error");
    assert_eq!(
        failure.reason,
        Error::Driver {
            operation: "start",
            code: 5
        }
    );
    assert_eq!(
        failure.cleanup,
        Some(CloseReport {
            stop: 12,
            reset: 13,
            close: 0
        })
    );
    assert_eq!((budget.used(), pool.load(Ordering::Acquire)), (0, 0));
}

#[test]
fn unexpected_close_during_open_or_start_cannot_return_a_live_device() {
    for before_start in [true, false] {
        let (driver, budget, pool) = setup();
        {
            let mut s = driver.0.lock().unwrap();
            s.close_on_open = before_start;
            s.close_on_start = !before_start;
        }
        let failure = Owner::open_with(driver.clone(), selected(), 16, &budget, &pool)
            .err()
            .expect("closed during open");
        assert_eq!(failure.reason, Error::Disconnected);
        assert_eq!(
            driver.0.lock().unwrap().calls.contains(&"start".into()),
            !before_start
        );
        assert!(failure.cleanup.is_some());
        assert_eq!((budget.used(), pool.load(Ordering::Acquire)), (0, 0));
    }
}

#[test]
fn successful_close_revokes_before_driver_calls_and_keeps_accounting_until_last_reader() {
    let (driver, budget, pool) = setup();
    let mut owner = Owner::open_with(driver.clone(), selected(), 16, &budget, &pool)
        .ok()
        .expect("open");
    let context = owner.context();
    let weak = Arc::downgrade(&context);
    send(Arc::as_ptr(&context) as usize, MM_MIM_DATA, 0x016490, 20);
    assert_eq!(context.capture.drain(1).unwrap().events.len(), 1);
    send(Arc::as_ptr(&context) as usize, MM_MIM_DATA, 0x016590, 21);
    assert_eq!(
        owner.shutdown(),
        Some(CloseReport {
            stop: 0,
            reset: 0,
            close: 0
        })
    );
    assert_eq!(owner.shutdown(), None);
    drop(owner);
    assert_eq!(context.capture.status(), Status::Closed);
    assert!(context.capture.drain(256).unwrap().events.is_empty());
    assert!(budget.used() > 0);
    assert_eq!(pool.load(Ordering::Acquire), 1);
    assert_eq!(
        driver
            .0
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|x| *x == "close")
            .count(),
        1
    );
    drop(context);
    assert!(weak.upgrade().is_none());
    assert_eq!((budget.used(), pool.load(Ordering::Acquire)), (0, 0));
}

#[test]
fn drop_has_one_ordered_stop_reset_close_boundary_and_invalid_capacity_or_budget_never_opens() {
    let (driver, budget, pool) = setup();
    let failure = Owner::open_with(driver.clone(), selected(), 0, &budget, &pool)
        .err()
        .expect("capacity");
    assert_eq!(
        failure.reason,
        Error::Queue(super::super::native::Error::Capacity)
    );
    let failure = Owner::open_with(
        driver.clone(),
        selected(),
        16,
        &NativeBudget::isolated(1),
        &pool,
    )
    .err()
    .expect("budget");
    assert_eq!(failure.reason, Error::Budget);
    assert_eq!(pool.load(Ordering::Acquire), 0);
    assert!(
        !driver
            .0
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|x| x.starts_with("open"))
    );
    let owner = Owner::open_with(driver.clone(), selected(), 16, &budget, &pool)
        .ok()
        .expect("open");
    drop(owner);
    let s = driver.0.lock().unwrap();
    assert_eq!(&s.calls[s.calls.len() - 3..], &["stop", "reset", "close"]);
    assert_eq!((budget.used(), pool.load(Ordering::Acquire)), (0, 0));
}

#[test]
fn failed_close_keeps_callback_memory_and_budget_and_four_slots_bound_quarantine() {
    let (_, budget, pool) = setup();
    let mut instances = Vec::new();
    for _ in 0..MAX_OPEN {
        let (driver, _, _) = setup();
        driver.0.lock().unwrap().close = 5;
        let mut owner = Owner::open_with(driver.clone(), selected(), 16, &budget, &pool)
            .ok()
            .expect("open");
        let instance = driver.0.lock().unwrap().instance;
        assert_eq!(owner.shutdown().unwrap().close, 5);
        drop(owner);
        // A delayed driver callback must still have a valid, revoked context.
        send(instance, MM_MIM_DATA, 0x7f4090, 99);
        assert_eq!(
            unsafe { &*(instance as *const Context) }.capture.status(),
            Status::Closed
        );
        instances.push(instance);
    }
    let (driver, _, _) = setup();
    let failure = Owner::open_with(driver, selected(), 16, &budget, &pool)
        .err()
        .expect("slots exhausted");
    assert_eq!(failure.reason, Error::Slots);
    assert_eq!(pool.load(Ordering::Acquire), MAX_OPEN);
    assert!(budget.used() > 0);
    // Only this synchronous fake is now quiescent. Production failed-close contexts
    // are deliberately never reclaimed; there is no unsafe retry or cleanup API.
    for ptr in instances {
        unsafe {
            drop(Arc::from_raw(ptr as *const Context));
        }
    }
    assert_eq!((budget.used(), pool.load(Ordering::Acquire)), (0, 0));
}

#[test]
fn fixed_callback_preserves_packets_overrun_and_forced_invalid_without_touching_sysex_pointers() {
    let (_, budget, pool) = setup();
    let context = Context::new(8, &budget, &pool).unwrap();
    let ptr = Arc::as_ptr(&context) as usize;
    send(0, MM_MIM_DATA, 1, 1);
    send(ptr, MM_MIM_OPEN, 0, 0);
    send(ptr, MM_MIM_DATA, 0x7f4091, 10);
    send(ptr, MM_MIM_MOREDATA, 0x004091, 11);
    send(ptr, MM_MIM_ERROR, 0x7f4091, 12); // valid-looking packet remains invalid
    send(ptr, MM_MIM_LONGDATA, usize::MAX, 13); // never dereferenced
    send(ptr, MM_MIM_LONGERROR, usize::MAX, 14);
    let b = context.capture.drain(8).unwrap();
    assert_eq!(
        (
            b.events.len(),
            b.stats.accepted,
            b.stats.invalid,
            b.stats.driver_overruns
        ),
        (2, 2, 3, 1)
    );
    assert_eq!(b.events[0].message.kind, super::super::native::Kind::NoteOn);
    assert_eq!(b.events[0].message.channel(), Some(1));
    assert_eq!(
        b.events[1].message.kind,
        super::super::native::Kind::NoteOff
    );
    assert_eq!(b.events[1].driver_ms, 11);
    assert!(b.events[1].driver_overrun);
    send(ptr, MM_MIM_DATA, 0x7f4191, 15);
    send(ptr, MM_MIM_CLOSE, 0, 16);
    send(ptr, MM_MIM_DATA, 0x7f4291, 17);
    let b = context.capture.drain(8).unwrap();
    assert_eq!(b.stats.status, Status::Disconnected);
    assert!(b.events.is_empty());
}

#[test]
fn actual_windows_inventory_is_bounded_and_never_opens_any_device() {
    let inv = inventory().expect("native read-only inventory");
    assert!(inv.devices.len() + inv.unavailable <= MAX_DEVICES);
    assert!(
        inv.devices
            .iter()
            .all(|d| d.slot < MAX_DEVICES as u32 && !d.name().is_empty())
    );
    assert_eq!(inv.truncated, inv.reported as usize > MAX_DEVICES);
    eprintln!(
        "WinMM read-only inventory: {} reported, {} described, {} unavailable; no device opened",
        inv.reported,
        inv.devices.len(),
        inv.unavailable
    );
}

#[test]
fn worker_connection_probe_revokes_missing_changed_or_invalid_handles_without_callback_device_calls()
 {
    for mode in 0..5 {
        let (driver, budget, pool) = setup();
        let owner = Owner::open_with(driver.clone(), selected(), 16, &budget, &pool)
            .ok()
            .expect("open");
        let context = owner.context();
        assert!(owner.check_connection());
        {
            let mut s = driver.0.lock().unwrap();
            match mode {
                0 => s.count = 0,
                1 => s.before = Some(identity("Replacement")),
                2 => s.after = Some(identity("Stale handle")),
                3 => s.wrong_id = true,
                _ => s.id_error = Some(5),
            }
        }
        assert!(!owner.check_connection());
        assert_eq!(context.capture.status(), Status::Disconnected);
        let n = driver.0.lock().unwrap().calls.len();
        assert!(!owner.check_connection());
        assert_eq!(driver.0.lock().unwrap().calls.len(), n);
        drop(owner);
        drop(context);
        assert_eq!((budget.used(), pool.load(Ordering::Acquire)), (0, 0));
    }
}
