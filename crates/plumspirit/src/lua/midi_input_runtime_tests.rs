use super::*;
use std::sync::mpsc;

fn lease() -> Arc<JobLease> {
    JobLease::fixture("test.midi", None, Duration::from_secs(5), true)
}
fn settled(budget: &NativeBudget, bytes: usize) {
    let until = Instant::now() + Duration::from_secs(2);
    while budget.used() != bytes && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(budget.used(), bytes);
}

#[test]
fn native_tasks_have_eight_slots_and_stack_accounting_and_failed_admission_releases_budget() {
    let budget = NativeBudget::isolated(64 * 1024 * 1024);
    let pool = Arc::new(AtomicUsize::new(0));
    let mut permits = Vec::new();
    for _ in 0..MAX_TASKS {
        permits.push(Permit::take(&budget, &pool).expect("permit"));
    }
    assert_eq!(pool.load(Ordering::Acquire), MAX_TASKS);
    assert_eq!(budget.used(), MAX_TASKS * TASK_BYTES);
    assert!(matches!(Permit::take(&budget, &pool), Err(Error::Busy)));
    assert_eq!(budget.used(), MAX_TASKS * TASK_BYTES);
    drop(permits);
    assert_eq!((pool.load(Ordering::Acquire), budget.used()), (0, 0));
    assert!(matches!(
        Permit::take(&NativeBudget::isolated(1), &pool),
        Err(Error::Bridge(BridgeError::Budget))
    ));
    assert_eq!(pool.load(Ordering::Acquire), 0);
}

#[test]
fn canceled_wait_never_joins_a_blocked_native_task_or_releases_its_held_reservation_early() {
    let budget = NativeBudget::isolated(8 * 1024 * 1024);
    let pool = Arc::new(AtomicUsize::new(0));
    let permit = Permit::take(&budget, &pool).ok().unwrap();
    let (release, blocked) = mpsc::sync_channel(1);
    let (started, ready) = mpsc::sync_channel(1);
    let root = std::thread::current().id();
    let rx = launch(permit, move || {
        assert_ne!(std::thread::current().id(), root);
        started.send(()).unwrap();
        blocked.recv().unwrap();
        Ok(17)
    })
    .unwrap();
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    let lease = lease();
    lease.cancel_from_surface();
    let t = Instant::now();
    assert!(matches!(
        wait(&lease, rx),
        Err(Error::Bridge(BridgeError::Canceled))
    ));
    assert!(t.elapsed() < Duration::from_secs(1));
    assert_eq!(
        (pool.load(Ordering::Acquire), budget.used()),
        (1, TASK_BYTES)
    );
    release.send(()).unwrap();
    settled(&budget, 0);
    assert_eq!(pool.load(Ordering::Acquire), 0);
}

#[test]
fn native_result_cannot_cross_expired_lease_and_deadline_does_not_wait_for_native_completion() {
    let budget = NativeBudget::isolated(8 * 1024 * 1024);
    let pool = Arc::new(AtomicUsize::new(0));
    let rx = launch(Permit::take(&budget, &pool).ok().unwrap(), || Ok(42)).unwrap();
    let expired = lease();
    expired.finish();
    assert!(matches!(
        wait(&expired, rx),
        Err(Error::Bridge(BridgeError::Expired))
    ));
    settled(&budget, 0);
    let (release, blocked) = mpsc::sync_channel(1);
    let rx = launch(Permit::take(&budget, &pool).ok().unwrap(), move || {
        blocked.recv().unwrap();
        Ok(9)
    })
    .unwrap();
    let expired = JobLease::fixture("test.midi", None, Duration::from_millis(5), true);
    assert!(matches!(
        wait(&expired, rx),
        Err(Error::Bridge(BridgeError::Deadline))
    ));
    assert_eq!(budget.used(), TASK_BYTES);
    release.send(()).unwrap();
    settled(&budget, 0);
}

#[test]
fn bounded_wait_reads_only_available_data_and_cancel_rejects_delivery_without_native_callbacks() {
    let budget = NativeBudget::isolated(8 * 1024 * 1024);
    let lease = lease();
    let input = Input::fixture(Arc::clone(&lease), &budget, 16).unwrap();
    assert!(input.read(8, 0).unwrap().events.is_empty());
    assert!(matches!(input.read(0, 0), Err(Error::Wait)));
    assert!(matches!(input.read(257, 0), Err(Error::Wait)));
    assert!(matches!(input.read(1, 1001), Err(Error::Wait)));
    let c = Arc::clone(&input.context);
    let producer = std::thread::spawn(move || {
        std::thread::sleep(CHECK);
        c.capture.ingest(0x7f4090, 7, 1, false);
    });
    let b = input.read(8, 1000).unwrap();
    producer.join().unwrap();
    assert_eq!(b.events.len(), 1);
    assert_eq!((b.events[0].message.data1, b.events[0].driver_ms), (64, 7));
    let l = Arc::clone(&lease);
    let cancel = std::thread::spawn(move || {
        std::thread::sleep(CHECK);
        l.finish();
    });
    assert!(matches!(
        input.read(1, 1000),
        Err(Error::Bridge(BridgeError::Expired))
    ));
    cancel.join().unwrap();
    drop(input);
    assert_eq!(budget.used(), 0);
}

#[test]
fn dropped_input_revokes_immediately_and_disconnect_returns_empty_typed_terminal_status() {
    let budget = NativeBudget::isolated(8 * 1024 * 1024);
    let input = Input::fixture(lease(), &budget, 16).unwrap();
    let context = Arc::clone(&input.context);
    let stop = Arc::clone(&input.stop);
    input.context.capture.ingest(0x7f4090, 0, 0, false);
    drop(input);
    assert!(stop.load(Ordering::Acquire));
    assert_eq!(context.capture.status(), native::Status::Closed);
    assert_eq!(context.capture.statistics().unwrap().queued, 0);
    drop(context);
    assert_eq!(budget.used(), 0);
    let input = Input::fixture(lease(), &budget, 16).unwrap();
    input.context.capture.ingest(0x7f4090, 0, 0, false);
    input.context.capture.close(native::Status::Disconnected);
    let b = input.read(1, 1000).unwrap();
    assert!(b.events.is_empty());
    assert_eq!(b.stats.status.name(), "disconnected");
    drop(input);
    assert_eq!(budget.used(), 0);
}

#[test]
fn owner_startup_errors_are_delivered_off_thread_without_opening_a_real_device() {
    let budget = NativeBudget::isolated(8 * 1024 * 1024);
    let root = std::thread::current().id();
    let result = open_with::<backend::WinMm>(lease(), &budget, move || {
        assert_ne!(std::thread::current().id(), root);
        Err(backend::OpenFailure {
            reason: backend::Error::DeviceChanged,
            cleanup: None,
        })
    });
    assert!(matches!(
        result,
        Err(Error::Open(backend::Error::DeviceChanged, None))
    ));
    settled(&budget, 0);
}

#[test]
fn actual_windows_inventory_runs_on_bounded_native_io_and_releases_result_accounting() {
    let budget = NativeBudget::isolated(8 * 1024 * 1024);
    let inventory = enumerate(&lease(), &budget).unwrap();
    assert!(inventory.inventory.devices.len() <= backend::MAX_DEVICES);
    settled(&budget, INVENTORY_BYTES);
    drop(inventory);
    settled(&budget, 0);
}

#[cfg(windows)]
#[test]
fn native_owner_thread_closes_on_job_expiry_or_reader_drop_and_releases_its_task_without_join() {
    for expire in [true, false] {
        let budget = NativeBudget::isolated(8 * 1024 * 1024);
        let lease = lease();
        let native_budget = budget.clone();
        let input = open_with(Arc::clone(&lease), &budget, move || {
            backend::fixture_owner(16, &native_budget)
        })
        .unwrap();
        let context = Arc::clone(&input.context);
        assert_eq!(context.capture.status(), native::Status::Open);
        if expire {
            lease.finish();
        } else {
            input.close();
        }
        let until = Instant::now() + Duration::from_secs(2);
        while context.capture.status() == native::Status::Open && Instant::now() < until {
            std::thread::sleep(CHECK);
        }
        assert_eq!(context.capture.status(), native::Status::Closed);
        assert!(context.capture.drain(16).unwrap().events.is_empty());
        drop(input);
        drop(context);
        settled(&budget, 0);
    }
}
