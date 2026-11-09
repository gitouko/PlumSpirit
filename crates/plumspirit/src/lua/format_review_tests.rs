use super::*;
use plumspirit_engine::history::{History, HistoryLimits, Operation};
use plumspirit_engine::model::events::{RawEvent, RawKind};
use plumspirit_engine::model::{
    Bpm, Division, NoteAttrs, NoteOffKind, NoteRecord, SmfFormat, Track,
};

fn doc() -> PlumDocument {
    PlumDocument::new_project("Review".into(), Bpm::DEFAULT, 960)
}
fn report(a: &PlumDocument, b: &PlumDocument) -> Report {
    Report::prepare(a, b, &NativeBudget::isolated(8 * 1024 * 1024), || Ok(())).unwrap()
}
fn apply(d: &mut PlumDocument, op: Operation) {
    History::new(HistoryLimits::default()).apply(d, op).unwrap();
}
fn note(tick: u64) -> NoteRecord {
    NoteRecord {
        start_tick: tick,
        duration_ticks: 17,
        attrs: NoteAttrs::new_wide(200, 93, 3, 0, NoteOffKind::NoteOff).unwrap(),
        sub: Default::default(),
        group: 0,
    }
}
#[test]
fn dense_shared_notes_are_counted_without_per_note_work_or_retained_source() {
    let mut a = doc();
    a.project_mut().keys = plumspirit_engine::model::project::KeyRange::Extended256;
    let values = (0..100_000)
        .map(|i| note(9_007_199_254_741_001 + i * 32))
        .collect::<Vec<_>>();
    a.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&values, false)
        .unwrap();
    let b = a.clone();
    let quota = NativeBudget::isolated(128 * 1024);
    let mut checks = 0;
    let r = Report::prepare(&a, &b, &quota, || {
        checks += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(r.before, r.after);
    assert_eq!(r.before.track_notes, 100_000);
    assert_eq!(r.changes, Changes::default());
    assert!(r.samples.is_empty());
    assert!(
        checks < 40,
        "comparison work depends on stores, not note count: {checks}"
    );
    assert_eq!(
        quota.used(),
        REPORT_BYTES,
        "scratch index is released before delivery"
    );
    drop(a);
    drop(b);
    assert_eq!(r.after.track_notes, 100_000);
    drop(r);
    assert_eq!(quota.used(), 0);
}
#[test]
fn equal_count_note_event_and_setting_edits_are_reported_by_value_or_revision() {
    let mut a = doc();
    a.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&[note(1)], false)
        .unwrap();
    let event = RawEvent {
        tick: 7,
        anchor: 0,
        kind: RawKind::Channel {
            status: 0xb3,
            data: [7, 80],
        },
    };
    a.track_mut(0).unwrap().events.push(event).unwrap();
    let mut b = a.clone();
    b.track_mut(0).unwrap().notes.select_all();
    b.track_mut(0)
        .unwrap()
        .notes
        .map_selected_attrs(|_| NoteAttrs::new_wide(199, 92, 3, 0, NoteOffKind::NoteOff).unwrap());
    let mut events = EventStore::new();
    events
        .push(RawEvent {
            kind: RawKind::Channel {
                status: 0xb3,
                data: [7, 81],
            },
            ..event
        })
        .unwrap();
    b.track_mut(0).unwrap().events = events;
    b.track_mut(0).unwrap().mute = true;
    let r = report(&a, &b);
    assert_eq!(r.before, r.after);
    assert_eq!(
        (r.changes.notes, r.changes.events, r.changes.settings),
        (1, 1, 1)
    );
    assert_eq!(r.samples.len(), 1);
    assert!(r.samples[0].notes && r.samples[0].events && r.samples[0].settings);
    assert_eq!(r.samples[0].track, a.track(0).unwrap().id());
}
#[test]
fn track_add_remove_reparent_and_reorder_have_full_totals_and_bounded_samples() {
    let mut a = doc();
    for _ in 0..70 {
        a.add_track().unwrap();
    }
    let mut b = a.clone();
    b.delete_track(1).unwrap();
    b.add_track().unwrap();
    apply(
        &mut b,
        Operation::AddSubtracks {
            parent: 0,
            count: 2,
        },
    );
    apply(
        &mut b,
        Operation::MoveTracks {
            from: 3,
            to: 4,
            parent: None,
        },
    );
    let r = report(&a, &b);
    assert_eq!((r.changes.added, r.changes.removed), (3, 1));
    assert!(r.changes.order);
    assert_eq!(r.after.subtracks, 2);
    assert_eq!(r.sampled_tracks, 4);
    let tracks = (0..70)
        .map(|i| Track::named(TrackId(100 + i), &format!("音\n{}", "a".repeat(512))))
        .collect();
    let many =
        PlumDocument::from_parts(SmfFormat::Parallel, Division::ppqn(960), tracks, Vec::new());
    let r = report(&a, &many);
    assert_eq!(r.samples.len(), MAX_SAMPLES);
    assert_eq!(r.sampled_tracks, 141);
    assert_eq!((r.changes.added, r.changes.removed), (70, 71));
    assert!(
        r.samples
            .iter()
            .all(|s| s.label.chars().count() <= LABEL_CHARS + 1 && !s.label.contains('\n'))
    );
}
#[test]
fn clip_links_do_not_duplicate_owned_note_counts_and_same_count_layout_edits_change() {
    use plumspirit_engine::editor::clips;
    let a = doc();
    let mut b = a.clone();
    let op = clips::add_clip(&b, 0, 100, 960).unwrap();
    apply(&mut b, op);
    let content = b.clips().contents()[0].id;
    b.notes_of_mut(plumspirit_engine::model::ContentRef::Owned(content))
        .unwrap()
        .insert_sorted_batch(&[note(3)], false)
        .unwrap();
    let id = b.clips().explicit_placements()[0].id;
    let op = clips::duplicate_clips(&b, &[(id, 0, 3000)], true).unwrap();
    apply(&mut b, op);
    let r = report(&a, &b);
    assert!(r.changes.clips);
    assert_eq!(r.after.contents, 1);
    assert_eq!(r.after.placements, 2);
    assert_eq!((r.after.track_notes, r.after.clip_notes), (0, 1));
    let mut c = b.clone();
    let notes = c
        .notes_of_mut(plumspirit_engine::model::ContentRef::Owned(content))
        .unwrap();
    notes.select_all();
    notes.map_selected_attrs(|_| NoteAttrs::new_wide(201, 80, 3, 0, NoteOffKind::NoteOff).unwrap());
    let r = report(&b, &c);
    assert_eq!(r.before, r.after);
    assert!(r.changes.clips);
    assert_eq!(r.changes.notes, 0);
}
#[test]
fn conductor_groups_attributes_and_project_settings_include_equal_count_edits() {
    use plumspirit_engine::model::conductor::Interp;
    use plumspirit_engine::model::metadata::{Record, Value};
    use plumspirit_engine::model::tempo::TempoValue;
    use plumspirit_engine::model::{GroupInfo, GroupTable};
    let mut a = doc();
    a.project_mut().groups = GroupTable::from_entries(
        vec![GroupInfo {
            id: 1,
            name: Some("Group".into()),
            ..Default::default()
        }],
        2,
    )
    .unwrap();
    a.project_mut()
        .metadata
        .set(
            "author.review",
            Some(Record {
                version: 1,
                value: Value::Integer(1),
            }),
        )
        .unwrap();
    let mut b = a.clone();
    let edit = b
        .conductor()
        .prepare_set_tempo(
            None,
            0,
            TempoValue::Bpm(Bpm::from_integer(144).unwrap()),
            Interp::Step,
        )
        .unwrap();
    apply(
        &mut b,
        Operation::Conductor {
            edit,
            label: "Tempo",
        },
    );
    let edit = b.conductor().prepare_set_signature(None, 0, 3, 2).unwrap();
    apply(
        &mut b,
        Operation::Conductor {
            edit,
            label: "Meter",
        },
    );
    let edit = b
        .conductor()
        .prepare_set_marker(None, 9007199254741001, None, "Staged marker")
        .unwrap();
    apply(
        &mut b,
        Operation::Conductor {
            edit,
            label: "Marker",
        },
    );
    let edit = b.conductor().prepare_set_master(None, 10, 8192).unwrap();
    apply(
        &mut b,
        Operation::Conductor {
            edit,
            label: "Volume",
        },
    );
    b.project_mut().groups = GroupTable::from_entries(
        vec![GroupInfo {
            id: 1,
            name: Some("Locked".into()),
            locked: true,
            ..Default::default()
        }],
        2,
    )
    .unwrap();
    b.project_mut()
        .metadata
        .set(
            "author.review",
            Some(Record {
                version: 2,
                value: Value::Integer(2),
            }),
        )
        .unwrap();
    b.project_mut().name = "Staged project".into();
    let r = report(&a, &b);
    let c = r.changes;
    assert!(c.tempo && c.meter && c.markers && c.master && c.groups && c.attributes && c.project);
    assert_eq!(
        (
            r.before.tempos,
            r.before.signatures,
            r.before.groups,
            r.before.attributes
        ),
        (
            r.after.tempos,
            r.after.signatures,
            r.after.groups,
            r.after.attributes
        )
    );
    assert_eq!((r.after.markers, r.after.master), (1, 1));
    assert!(r.samples.is_empty());
}
#[test]
fn budget_and_mid_payload_cancellation_release_all_reservations() {
    let a = doc();
    let b = a.clone();
    let quota = NativeBudget::isolated(REPORT_BYTES);
    assert!(Report::prepare(&a, &b, &quota, || Ok(())).is_err());
    assert_eq!(quota.used(), 0);
    let mut a = doc();
    let mut b = a.clone();
    for d in [&mut a, &mut b] {
        let t = d.track_mut(0).unwrap();
        let payload = t.events.store_payload(&vec![42; 2 * 1024 * 1024]).unwrap();
        t.events
            .push(RawEvent {
                tick: 1,
                anchor: 0,
                kind: RawKind::SysEx { payload },
            })
            .unwrap();
    }
    let quota = NativeBudget::isolated(128 * 1024);
    let mut checks = 0;
    let error = Report::prepare(&a, &b, &quota, || {
        checks += 1;
        if checks == 12 {
            Err(stop(Category::Canceled, "fixture cancellation"))
        } else {
            Ok(())
        }
    })
    .err()
    .expect("payload comparison must check cancellation");
    assert_eq!(super::super::classify(&error).category, Category::Canceled);
    assert_eq!(checks, 12);
    assert_eq!(quota.used(), 0);
    let r = Report::prepare(&a, &b, &quota, || Ok(())).unwrap();
    assert_eq!(r.changes.events, 0);
    drop(r);
    assert_eq!(quota.used(), 0);
}

#[test]
fn ordinary_api01_transaction_keeps_existing_apply_semantics_without_a_report() {
    use crate::lua::{Capability, Host, Limits, World};
    use std::sync::Arc;
    let mut source = Arc::new(doc());
    let mut history = History::new(HistoryLimits::default());
    history.observe_ids(&source);
    let host = Host::new(
        Limits::default(),
        [Capability::ReadProject, Capability::EditTracks],
    )
    .unwrap();
    host.set_world(Some(World::new(
        Arc::clone(&source),
        history.incarnations().snapshot(),
    )));
    host.run("ordinary-api01.lua",r#"plum.require_api(0,1);local tx=plum.transaction{label='Legacy rename'};tx:rename(plum.project.current():track(1),'Legacy');tx:commit()"#).unwrap();
    let mut commits = host.take_commits().unwrap();
    assert!(commits.format_review().is_none());
    commits.apply(&mut history, &mut source).unwrap();
    drop(commits);
    assert_eq!(source.track(0).unwrap().name(), Some("Legacy"));
    history.undo(Arc::get_mut(&mut source).unwrap()).unwrap();
    assert_eq!(source.track(0).unwrap().name(), None);
}
