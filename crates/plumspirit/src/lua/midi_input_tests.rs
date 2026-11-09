//! Manufactured input and controlled native consent; never opens a MIDI device.
use super::*;
use crate::lua::{Limits, Plain, ScriptError};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

pub(super) struct Simulation {
    pub devices: Vec<windows::Device>,
    packets: Vec<(u32, u32, u64, bool)>,
    opened: AtomicUsize,
    contexts: Mutex<Vec<std::sync::Weak<windows::Context>>>,
}
impl Simulation {
    fn new(packets: Vec<(u32, u32, u64, bool)>) -> Arc<Self> {
        Arc::new(Self {
            devices: vec![
                windows::Device::fixture(0, "First"),
                windows::Device::fixture(3, "Second"),
            ],
            packets,
            opened: AtomicUsize::new(0),
            contexts: Mutex::new(Vec::new()),
        })
    }
    pub(super) fn open(
        &self,
        approved: windows::Approved,
        capacity: usize,
        lease: Arc<JobLease>,
        budget: &NativeBudget,
    ) -> Result<runtime::Input, runtime::Error> {
        if !self.devices.contains(approved.device()) {
            return Err(runtime::Error::Native(windows::Error::DeviceChanged));
        }
        let input = runtime::Input::fixture(lease, budget, capacity)?;
        self.opened.fetch_add(1, Ordering::AcqRel);
        self.contexts
            .lock()
            .unwrap()
            .push(Arc::downgrade(&input.context));
        for &(packet, time, ns, overrun) in &self.packets {
            input.context.capture.ingest(packet, time, ns, overrun);
        }
        Ok(input)
    }
    fn empty(&self) -> bool {
        self.contexts
            .lock()
            .unwrap()
            .iter()
            .all(|w| w.upgrade().is_none())
    }
}
fn packets() -> Vec<(u32, u32, u64, bool)> {
    vec![
        (0x7f4091, u32::MAX - 1, 0, false),
        (0x004091, u32::MAX, 1, true),
        (0x4000e2, 1, 2, false),
        (0xf8, 2, 3, false),
    ]
}
fn setup(
    sim: Arc<Simulation>,
    caps: Vec<Capability>,
    interactive: bool,
    client: Option<Client>,
) -> (Host, NativeBudget) {
    let host = Host::new(
        Limits {
            memory: 8 * 1024 * 1024,
            ..Limits::default()
        },
        caps,
    )
    .unwrap();
    let budget = NativeBudget::isolated(64 * 1024 * 1024);
    host.lua.set_app_data(budget.clone());
    let (file_client, _) = super::super::bridge::channel("test.midi");
    {
        let mut s = host.lua.app_data_mut::<Session>().unwrap();
        s.client = Some(file_client);
        s.interactive = interactive;
    }
    {
        let mut s = host.lua.app_data_mut::<Slot>().unwrap();
        s.client = client;
        s.simulation = Some(sim);
    }
    (host, budget)
}
fn execute(
    sim: Arc<Simulation>,
    source: &str,
    choices: Vec<Option<usize>>,
) -> Result<Plain, ScriptError> {
    let (client, requests) = channel("test.midi");
    let source = source.to_owned();
    let worker_sim = Arc::clone(&sim);
    let worker = std::thread::spawn(move || {
        let (host, budget) = setup(
            worker_sim.clone(),
            vec![Capability::ReadMidiInput],
            true,
            Some(client),
        );
        let result = host.run("midi-input.lua", &source);
        assert_eq!(
            budget.used(),
            0,
            "all scope resources must expire even from Lua globals"
        );
        assert!(worker_sim.empty());
        result
    });
    for index in choices {
        let request = requests.recv_timeout(Duration::from_secs(4)).unwrap();
        assert_eq!(sim.opened.load(Ordering::Acquire), 0);
        assert_eq!(request.respond(None, true, index), Ok(index.is_some()));
    }
    worker.join().unwrap()
}

#[test]
fn input_capability_is_explicit_and_discovery_or_open_cannot_run_from_loading_or_other_grants() {
    assert_eq!(
        Capability::parse("midi.input"),
        Some(Capability::ReadMidiInput)
    );
    assert_eq!(Capability::ALL.len(), 19);
    for (caps, interactive) in [
        (vec![], true),
        (vec![Capability::EmitMidi, Capability::ReadPlayback], true),
        (vec![Capability::ReadMidiInput], false),
    ] {
        for source in [
            "return plum.midi_input.devices()",
            "return plum.midi_input.open()",
        ] {
            let sim = Simulation::new(packets());
            let (host, budget) = setup(sim.clone(), caps.clone(), interactive, None);
            assert_eq!(
                host.run("denied.lua", source).unwrap_err().category,
                Category::Capability
            );
            assert_eq!(sim.opened.load(Ordering::Acquire), 0);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn device_discovery_is_bounded_plain_metadata_and_never_opens_or_confers_open_authority() {
    let sim = Simulation::new(packets());
    let (host, budget) = setup(sim.clone(), vec![Capability::ReadMidiInput], true, None);
    assert_eq!(
        host.run(
            "list.lua",
            r#"
        local r=plum.midi_input.devices()
        assert(r.reported==2 and not r.truncated and r.unavailable==0)
        assert(#r.devices==2 and r.devices[1].name=='First' and r.devices[2].index==4)
        r.devices[1].name='Forged'
        assert(plum.midi_input.devices().devices[1].name=='First')
        assert(not pcall(function() plum.midi_input.open{device=1} end))
        return true
    "#
        )
        .unwrap(),
        Plain::Boolean(true)
    );
    assert_eq!(sim.opened.load(Ordering::Acquire), 0);
    assert_eq!(budget.used(), 0);
}

#[test]
fn consent_cancel_returns_nil_and_selected_device_receives_exact_packed_typed_time_columns() {
    let sim = Simulation::new(packets());
    assert_eq!(
        execute(sim, "return plum.midi_input.open()==nil", vec![None]).unwrap(),
        Plain::Boolean(true)
    );
    let sim = Simulation::new(packets());
    assert_eq!(
        execute(
            sim,
            r#"
        local s=assert(plum.midi_input.open{capacity=16})
        assert(s:info().name=='Second')
        local b=s:read(256,0);local c=b.columns
        assert(b.count==4 and b.stats.capacity==16 and b.stats.status=='open')
        for _,v in pairs(c) do assert(#v==4) end
        assert(c.kind[1]=='note_on' and c.kind[2]=='note_off' and c.note_on_zero[2])
        assert(c.status[2]==0x91 and c.channel[1]==1 and c.channel[4]==-1)
        assert(c.kind[3]=='pitch_bend' and c.data1[3]==0 and c.data2[3]==64)
        assert(c.sequence[1]==1 and c.sequence[4]==4)
        assert(c.driver_ms[1]==4294967294 and c.timestamp_ms[3]==4294967297)
        assert(c.received_ns[4]==3 and c.driver_overrun[2] and b.stats.driver_overruns==1)
        assert(s:read().count==0)
        c.data1[1]=7; assert(s:info().accepted==4)
        assert(s:close() and not s:close())
        assert(not pcall(function() return s:info() end))
        return true
    "#,
            vec![Some(1)]
        )
        .unwrap(),
        Plain::Boolean(true)
    );
}

#[test]
fn strict_options_and_read_limits_fail_before_consuming_valid_events() {
    let sim = Simulation::new(packets());
    assert_eq!(
        execute(
            sim,
            r#"
        for _,v in ipairs({true,1,'x',{capacity=0},{capacity=4097},{capacity='16'},
            {capacity=1.5},{capacity=1/0},{other=1},setmetatable({capacity=16},{})}) do
            assert(not pcall(function() plum.midi_input.open(v) end))
        end
        local s=assert(plum.midi_input.open{capacity=16})
        for _,v in ipairs({0,257,'1',1.5,1/0}) do assert(not pcall(function() s:read(v) end)) end
        assert(not pcall(function() s:read(1,-1) end))
        assert(not pcall(function() s:read(1,1001) end))
        assert(s:read(1).columns.sequence[1]==1)
        assert(s:read(3).count==3)
        return true
    "#,
            vec![Some(0)]
        )
        .unwrap(),
        Plain::Boolean(true)
    );
}

#[test]
fn scope_end_eagerly_releases_native_input_even_when_userdata_is_retained_and_next_run_is_usable() {
    let (client, requests) = channel("test.midi");
    let sim = Simulation::new(packets());
    let child = sim.clone();
    let worker = std::thread::spawn(move || {
        let (host, budget) = setup(
            child.clone(),
            vec![Capability::ReadMidiInput],
            true,
            Some(client),
        );
        host.run(
            "retain.lua",
            "saved=assert(plum.midi_input.open()); return saved:read(1).count",
        )
        .unwrap();
        assert!(child.empty());
        assert_eq!(budget.used(), 0);
        assert_eq!(host.run("expired.lua","assert(not pcall(function() saved:read() end)); assert(not pcall(function() saved:info() end)); return not saved:close()").unwrap(),Plain::Boolean(true));
        host.lua.app_data_mut::<Session>().unwrap().interactive = true;
        host.run("again.lua", "return plum.midi_input.open():read(4).count")
            .unwrap()
    });
    for _ in 0..2 {
        requests
            .recv_timeout(Duration::from_secs(4))
            .unwrap()
            .respond(None, true, Some(0))
            .unwrap();
    }
    assert_eq!(worker.join().unwrap(), Plain::Integer(4));
    assert!(sim.empty());
}

#[test]
fn denied_stale_canceled_and_invalid_choices_never_create_a_stream() {
    for mode in 0..4 {
        let (client, requests) = channel("test.midi");
        let sim = Simulation::new(packets());
        let child = sim.clone();
        let worker = std::thread::spawn(move || {
            let (host, budget) = setup(
                child.clone(),
                vec![Capability::ReadMidiInput],
                true,
                Some(client),
            );
            let result = host.run("choice.lua", "return plum.midi_input.open()");
            assert_eq!(budget.used(), 0);
            result.unwrap_err()
        });
        let request = requests.recv_timeout(Duration::from_secs(4)).unwrap();
        let expected = match mode {
            0 => {
                assert_eq!(
                    request.respond(None, false, Some(0)),
                    Err(BridgeError::NotUserInvoked)
                );
                Category::Capability
            }
            1 => {
                assert_eq!(
                    request.respond(
                        Some(ProjectStamp {
                            serial: 1,
                            revision: 1
                        }),
                        true,
                        Some(0)
                    ),
                    Err(BridgeError::Expired)
                );
                Category::Expired
            }
            2 => {
                request.lease.cancel_from_surface();
                assert_eq!(
                    request.respond(None, true, Some(0)),
                    Err(BridgeError::Canceled)
                );
                Category::Canceled
            }
            _ => {
                assert_eq!(
                    request.respond(None, true, Some(99)),
                    Err(BridgeError::Unavailable)
                );
                Category::Runtime
            }
        };
        assert_eq!(worker.join().unwrap().category, expected);
        assert_eq!(sim.opened.load(Ordering::Acquire), 0);
        assert!(sim.empty());
    }
}

#[test]
fn lua_failure_and_canceled_wait_discard_resources_and_never_leave_a_live_global_stream() {
    let sim = Simulation::new(packets());
    let e = execute(
        sim,
        "saved=assert(plum.midi_input.open());error('intentional after open')",
        vec![Some(0)],
    )
    .unwrap_err();
    assert!(e.message.contains("intentional after open"));
    let sim = Simulation::new(vec![]);
    let (client, requests) = channel("test.midi");
    let child = sim.clone();
    let worker = std::thread::spawn(move || {
        let (host, budget) = setup(
            child.clone(),
            vec![Capability::ReadMidiInput],
            true,
            Some(client),
        );
        let e = host
            .run(
                "wait.lua",
                "saved=plum.midi_input.open();return saved:read(1,1000)",
            )
            .unwrap_err();
        assert_eq!(budget.used(), 0);
        assert!(child.empty());
        e
    });
    let request = requests.recv_timeout(Duration::from_secs(4)).unwrap();
    let lease = Arc::clone(&request.lease);
    request.respond(None, true, Some(0)).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    lease.cancel_from_surface();
    assert_eq!(worker.join().unwrap().category, Category::Canceled);
    assert!(sim.empty());
}

#[test]
fn four_discoveries_and_four_open_attempts_are_hard_cumulative_job_limits() {
    let sim = Simulation::new(vec![]);
    let (host, budget) = setup(sim, vec![Capability::ReadMidiInput], true, None);
    assert_eq!(host.run("enum-cap.lua","for i=1,4 do plum.midi_input.devices() end;local ok,e=pcall(plum.midi_input.devices);assert(not ok);return true").unwrap(),Plain::Boolean(true));
    assert_eq!(budget.used(), 0);
    let sim = Simulation::new(vec![]);
    let (client, requests) = channel("test.midi");
    let worker = std::thread::spawn(move || {
        let (host, budget) = setup(sim, vec![Capability::ReadMidiInput], true, Some(client));
        let r=host.run("open-cap.lua","for i=1,4 do assert(plum.midi_input.open()==nil) end;assert(not pcall(plum.midi_input.open));return true");
        assert_eq!(budget.used(), 0);
        r
    });
    for _ in 0..4 {
        requests
            .recv_timeout(Duration::from_secs(4))
            .unwrap()
            .respond(None, true, None)
            .unwrap();
    }
    assert_eq!(worker.join().unwrap().unwrap(), Plain::Boolean(true));
}

#[test]
fn delivery_budget_closes_stream_without_partial_lua_result_and_shared_prompt_budget_never_wraps() {
    let sim = Simulation::new(packets());
    let (client, requests) = channel("test.midi");
    let worker = std::thread::spawn(move || {
        let (host, budget) = setup(sim, vec![Capability::ReadMidiInput], true, Some(client));
        host.lua
            .globals()
            .set(
                "near_limit",
                host.lua
                    .create_function(|lua, ()| {
                        lua.app_data_ref::<Slot>()
                            .unwrap()
                            .job
                            .as_ref()
                            .unwrap()
                            .delivered
                            .set(MAX_DELIVERED - 1);
                        Ok(())
                    })
                    .unwrap(),
            )
            .unwrap();
        let r=host.run("delivery-cap.lua","local s=assert(plum.midi_input.open());near_limit();assert(not pcall(function()s:read(2)end));assert(s:info().status=='closed');return true");
        assert_eq!(budget.used(), 0);
        r
    });
    requests
        .recv_timeout(Duration::from_secs(4))
        .unwrap()
        .respond(None, true, Some(0))
        .unwrap();
    assert_eq!(worker.join().unwrap().unwrap(), Plain::Boolean(true));
    let lease = JobLease::fixture("test.midi", None, Duration::from_secs(1), true);
    for i in 0..super::super::bridge::MAX_DIALOGS_PER_JOB {
        assert_eq!(lease.reserve_prompt(), Ok(i));
    }
    for _ in 0..10 {
        assert_eq!(lease.reserve_prompt(), Err(BridgeError::Budget));
    }
}

#[test]
fn maximum_queue_streams_in_small_packed_pages_under_eight_mib_lua_without_event_loss() {
    let sim = Simulation::new(
        (0..4096)
            .map(|i| (0x7f4090, i, i as u64 * 1_000_000, false))
            .collect(),
    );
    assert_eq!(
        execute(
            sim,
            r#"
        local s=assert(plum.midi_input.open{capacity=4096});local last=0
        repeat
            local b=s:read(37);assert(b.stats.queue_dropped==0 and b.stats.rate_dropped==0)
            for i=1,b.count do assert(b.columns.sequence[i]==last+1);last=last+1 end
        until last==4096
        return last
    "#,
            vec![Some(0)]
        )
        .unwrap(),
        Plain::Integer(4096)
    );
}

#[test]
fn real_script_worker_exposes_only_native_consent_and_cancel_before_device_open() {
    use crate::lua::worker::{ScriptJob, ScriptWorker};
    let mut w = ScriptWorker::spawn(
        Limits::default(),
        vec![Capability::ReadMidiInput],
        &crate::lua::Scripts::default(),
    )
    .unwrap();
    w.submit_with_interaction(ScriptJob {file:"native-input-worker.lua".into(),source:"local d=plum.midi_input.devices();assert(type(d.devices)=='table');return plum.midi_input.open()==nil".into(),world:None,time_limit:None},true).unwrap();
    let until = Instant::now() + Duration::from_secs(4);
    let request = loop {
        if let Some(r) = w.poll_midi_input_request() {
            break r;
        }
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(1));
    };
    request.respond(None, true, None).unwrap();
    let result = loop {
        if let Some(r) = w.poll() {
            break r;
        }
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(result.result.unwrap(), Plain::Boolean(true));
    assert!(result.commits.is_none());
}

#[test]
fn published_midi_input_command_runs_with_real_host_packed_data_and_controlled_device_consent() {
    let doc = include_str!("../../../../docs/LUA_API.md");
    let section = doc
        .split("<!-- midi-input-example:start -->")
        .nth(1)
        .unwrap()
        .split("<!-- midi-input-example:end -->")
        .next()
        .unwrap();
    let source = section
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    assert_eq!(
        execute(Simulation::new(packets()), source, vec![Some(0)]).unwrap(),
        Plain::Integer(1)
    );
}

#[path = "midi_input_laboratory_capture_tests.rs"]
mod laboratory_capture;
