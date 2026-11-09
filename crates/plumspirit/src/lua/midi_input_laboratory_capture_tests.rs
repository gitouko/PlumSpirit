//! Actual installed sample source on Host, manufactured MIDI, controlled consent.
use super::*;
use crate::lua::plugin_api::PluginMeta;
use plumspirit_engine::{
    history::{History, HistoryLimits},
    model::{Bpm, PlumDocument},
};
use std::path::PathBuf;
const ID: &str = "plumspirit.example.midi-input-laboratory";
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "plum-midi-capture-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let p = self.0.canonicalize().unwrap();
        let base = std::env::temp_dir().canonicalize().unwrap();
        assert!(
            p.parent() == Some(base.as_path())
                && p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("plum-midi-capture-test-")
        );
        std::fs::remove_dir_all(p).unwrap();
    }
}
fn prepared(
    sim: Arc<Simulation>,
    client: Client,
    data_dir: PathBuf,
) -> (Host, NativeBudget, Arc<PlumDocument>) {
    let (host, budget) = setup(
        sim,
        vec![
            Capability::ReadMidiInput,
            Capability::ReadProject,
            Capability::EditNotes,
        ],
        false,
        Some(client),
    );
    host.set_plugin(PluginMeta {
        id: ID.into(),
        name: "MIDI Input Laboratory".into(),
        version: "1.0.0".into(),
        root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples-lua/midi-input-laboratory"),
        data_dir: Some(data_dir),
        config_version: 1,
    })
    .unwrap();
    host.lua.set_app_data(budget.clone());
    let (file_client, _) = crate::lua::bridge::channel(ID);
    host.lua.app_data_mut::<Session>().unwrap().client = Some(file_client);
    host.run(
        "main.lua",
        &format!(
            "{}\nregister(plum.plugin)",
            crate::lua::examples::by_id(ID).unwrap().main_lua()
        ),
    )
    .unwrap();
    let mut doc = PlumDocument::new_project("Capture source".into(), Bpm::DEFAULT, 960);
    doc.project_mut().plugin_data=vec![(ID.into(),serde_json::json!({"version":1,"values":{"midi_input_capture":{"previous":true},"unrelated":42}}).to_string())];
    let doc = Arc::new(doc);
    let mut history = History::new(HistoryLimits::default());
    history.observe_ids(&doc);
    host.set_world(Some(crate::lua::World::new(
        Arc::clone(&doc),
        history.incarnations().snapshot(),
    )));
    host.lua.app_data_mut::<Session>().unwrap().interactive = true;
    (host, budget, doc)
}
#[test]
fn actual_laboratory_capture_reads_four_pages_and_persists_exact_columns_without_live_resources() {
    let temp = Temp::new();
    let (client, requests) = channel(ID);
    let packets = (0..256)
        .map(|i| {
            (
                if i == 0 { 0x7f4091 } else { 0xf8 },
                (u32::MAX - 127).wrapping_add(i),
                i as u64 * 1_000_000,
                false,
            )
        })
        .collect::<Vec<_>>();
    // Native driver timestamps wrap as unsigned DWORDs, not debug Rust additions.
    let sim = Simulation::new(packets);
    let child = Arc::clone(&sim);
    let dir = temp.0.join("data");
    let worker = std::thread::spawn(move || {
        let (host, budget, doc) = prepared(child.clone(), client, dir);
        let before = doc.export_fingerprint();
        host.run("capture.lua", "return plum.plugin.__run('capture')")
            .unwrap();
        assert_eq!(budget.used(), 0);
        assert!(child.empty());
        assert_eq!(doc.export_fingerprint(), before);
        assert!(host.take_commits().is_none());
        host.take_plugin_out().project_data.unwrap()
    });
    let request = requests.recv_timeout(Duration::from_secs(4)).unwrap();
    let stamp = request.lease.project;
    assert_eq!(sim.opened.load(Ordering::Acquire), 0);
    request.respond(stamp, true, Some(1)).unwrap();
    let data = worker.join().unwrap();
    let data: serde_json::Value = serde_json::from_str(&data).unwrap();
    assert_eq!(data["values"]["unrelated"], 42);
    let c = &data["values"]["midi_input_capture"];
    assert_eq!(c["count"], 256);
    assert_eq!(c["name"], "Second");
    assert_eq!(c["truncated"], true);
    for field in FIELDS {
        assert_eq!(c[field].as_array().unwrap().len(), 256);
    }
    assert_eq!(c["time"][0], u32::MAX - 127);
    assert_eq!(c["time"][255], u64::from(u32::MAX) + 128);
    assert_eq!(c["driver_ms"][255], 127);
    assert_eq!(c["sequence"][255], 256);
    assert_eq!(c["received_ns"][255], 255_000_000);
    assert_eq!(c["status"][0], 0x91);
    assert_eq!(c["status"][255], 0xf8);
    assert!(!data.to_string().contains("userdata"));
    assert!(sim.empty());
}
const FIELDS: [&str; 9] = [
    "status",
    "data1",
    "data2",
    "time",
    "driver_ms",
    "sequence",
    "received_ns",
    "out_of_order",
    "driver_overrun",
];

#[test]
fn rejected_discontinuous_and_canceled_capture_preserve_previous_project_settings_and_never_commit_notes()
 {
    for mode in 0..3 {
        let temp = Temp::new();
        let (client, requests) = channel(ID);
        let sim = Simulation::new(if mode == 0 {
            vec![(0x7f4090, 1, 1, true)]
        } else {
            vec![]
        });
        let child = sim.clone();
        let dir = temp.0.join("data");
        let worker = std::thread::spawn(move || {
            let (host, budget, doc) = prepared(child.clone(), client, dir);
            let before = doc.project().plugin_data.clone();
            let result = host.run("capture-failure.lua", "return plum.plugin.__run('capture')");
            assert_eq!(doc.project().plugin_data, before);
            assert!(host.take_plugin_out().project_data.is_none());
            assert!(host.take_commits().is_none());
            assert_eq!(budget.used(), 0);
            assert!(child.empty());
            result
        });
        let request = requests.recv_timeout(Duration::from_secs(4)).unwrap();
        let stamp = request.lease.project;
        match mode {
            0 => {
                request.respond(stamp, true, Some(0)).unwrap();
                assert!(
                    worker
                        .join()
                        .unwrap()
                        .unwrap_err()
                        .message
                        .contains("discontinuity")
                );
            }
            1 => {
                request.respond(stamp, true, None).unwrap();
                assert!(worker.join().unwrap().is_ok());
                assert_eq!(sim.opened.load(Ordering::Acquire), 0);
            }
            _ => {
                let lease = Arc::clone(&request.lease);
                request.respond(stamp, true, Some(0)).unwrap();
                std::thread::sleep(Duration::from_millis(20));
                lease.cancel_from_surface();
                assert_eq!(
                    worker.join().unwrap().unwrap_err().category,
                    Category::Canceled
                );
            }
        }
        assert!(sim.empty());
    }
}
