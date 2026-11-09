//! Native integration: real LUP files and persistent history, no mocked store.
use plumspirit_engine::history::journal::{Journal, identity, path_key};
use plumspirit_engine::history::{History, HistoryLimits, Operation};
use plumspirit_engine::model::metadata::{MAX_NAMESPACES, Metadata, Record, Value};
use plumspirit_engine::model::{Bpm, PlumDocument};
use plumspirit_engine::native::{LupControl, load_project, save_project};

fn table(version: u32) -> Metadata {
    let mut m = Metadata::default();
    m.set(
        "author.demo",
        Some(Record {
            version,
            value: Value::Integer(9_007_199_254_740_993),
        }),
    )
    .unwrap();
    m.set_at(
        "author.demo",
        plumspirit_engine::model::metadata::Target::Track(plumspirit_engine::model::TrackId(0)),
        Some(Record {
            version,
            value: Value::Integer(version as i64),
        }),
    )
    .unwrap();
    m
}
fn op(table: Metadata) -> Operation {
    Operation::SetMetadata {
        table: Box::new(table),
        label: "Author metadata",
    }
}

#[test]
fn disk_save_reopen_and_persistent_undo_redo_restore_typed_metadata() {
    let base = std::env::temp_dir().canonicalize().unwrap();
    let root = base.join(format!(
        "plumspirit-api02-metadata-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("song.lup");
    let jpath = root.join("song.plh");
    let mut doc = PlumDocument::new_project("Metadata".into(), Bpm::DEFAULT, 960);
    doc.add_track().unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    h.attach_new_journal(
        Journal::create(&jpath, path_key(&file)).unwrap(),
        Some(identity(&doc)),
    )
    .unwrap();
    h.apply(&mut doc, op(table(1))).unwrap();
    h.apply(&mut doc, op(table(2))).unwrap();
    h.apply(&mut doc, Operation::DeleteTrack { index: 0 })
        .unwrap();
    let expected = table(2)
        .removing_tracks(&[plumspirit_engine::model::TrackId(0)])
        .unwrap();
    save_project(&doc, &file, LupControl::default()).unwrap();
    let state = h.current_state();
    h.mark_saved(state);
    h.persist_saved(state, identity(&doc));
    h.journal_mut().unwrap().sync().unwrap();
    drop(h);
    let (mut opened, _) = load_project(&file, LupControl::default()).unwrap();
    assert_eq!(opened.project().metadata, expected);
    let (j, scan) = Journal::open(&jpath, path_key(&file)).unwrap().unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    h.restore_journal(j, scan, identity(&opened)).unwrap();
    assert_eq!(h.undo_len(), 3);
    h.undo(&mut opened).unwrap();
    assert_eq!(opened.project().metadata, table(2));
    h.undo(&mut opened).unwrap();
    assert_eq!(opened.project().metadata, table(1));
    h.undo(&mut opened).unwrap();
    assert!(opened.project().metadata.is_empty());
    h.redo(&mut opened).unwrap();
    h.redo(&mut opened).unwrap();
    h.redo(&mut opened).unwrap();
    assert_eq!(opened.project().metadata, expected);
    assert!(h.is_at_saved_state());
    drop(h);
    let resolved = root.canonicalize().unwrap();
    assert_eq!(resolved.parent(), Some(base.as_path()));
    assert!(
        resolved
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("plumspirit-api02-metadata-")
    );
    std::fs::remove_dir_all(resolved).unwrap();
}

#[test]
fn namespace_record_and_total_limits_are_atomic_and_unknown_owners_survive() {
    let mut m = Metadata::default();
    for i in 0..MAX_NAMESPACES {
        m.set(
            &format!("owner.{i:03}"),
            Some(Record {
                version: 1,
                value: Value::Bool(true),
            }),
        )
        .unwrap();
    }
    let before = m.clone();
    assert!(
        m.set(
            "owner.extra",
            Some(Record {
                version: 1,
                value: Value::Null
            })
        )
        .is_err()
    );
    assert_eq!(m, before);
    m.set("owner.000", None).unwrap();
    m.set(
        "owner.extra",
        Some(Record {
            version: u32::MAX,
            value: Value::Bool(false),
        }),
    )
    .unwrap();
    assert_eq!(m.len(), MAX_NAMESPACES);
    let mut doc = PlumDocument::new_project("Unknown plugins".into(), Bpm::DEFAULT, 960);
    doc.project_mut().metadata = m.clone();
    doc.project_mut().plugin_data.push((
        "owner.legacy".into(),
        "{\"version\":1,\"values\":{\"old\":true}}".into(),
    ));
    let b = plumspirit_engine::native::project_to_bytes(&doc).unwrap();
    let (reopened, _) = plumspirit_engine::native::project_from_bytes(&b).unwrap();
    assert_eq!(reopened.project().metadata, m);
    assert_eq!(reopened.project().plugin_data, doc.project().plugin_data);
    let big = Value::Array((0..5).map(|_| Value::Text("x".repeat(16384))).collect());
    let mut m = Metadata::default();
    assert!(
        m.set(
            "author.demo",
            Some(Record {
                version: 1,
                value: big
            })
        )
        .is_err()
    );
    assert!(m.is_empty());
    let value = Value::Array((0..3).map(|_| Value::Text("x".repeat(16384))).collect());
    for i in 0..21 {
        m.set(
            &format!("owner.{i:03}"),
            Some(Record {
                version: 1,
                value: value.clone(),
            }),
        )
        .unwrap();
    }
    let old = m.clone();
    assert!(
        m.set("owner.overflow", Some(Record { version: 1, value }))
            .is_err()
    );
    assert_eq!(m, old);
}

#[test]
fn tiny_history_budget_refuses_before_metadata_changes_and_noop_is_clean() {
    let mut doc = PlumDocument::new_project("Metadata".into(), Bpm::DEFAULT, 960);
    let mut h = History::new(HistoryLimits {
        max_bytes: 1,
        max_entries: 1,
    });
    h.reset(true);
    assert!(h.apply(&mut doc, op(table(1))).is_err());
    assert!(doc.project().metadata.is_empty());
    assert!(h.is_at_saved_state());
    assert_eq!(h.undo_len(), 0);
    h.apply(&mut doc, op(Metadata::default())).unwrap();
    assert!(h.is_at_saved_state());
}

#[test]
fn native_subtree_deletion_prunes_all_owners_and_undo_restores_associations() {
    use plumspirit_engine::model::metadata::Target;
    let mut d = PlumDocument::new_project("Targets".into(), Bpm::DEFAULT, 960);
    d.add_track().unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    h.apply(
        &mut d,
        Operation::AddSubtracks {
            parent: 1,
            count: 2,
        },
    )
    .unwrap();
    let parent = d.track(1).unwrap().id();
    let child = d.track(2).unwrap().id();
    let mut table = Metadata::default();
    for owner in ["author.one", "author.two"] {
        for id in [parent, child] {
            table
                .set_at(
                    owner,
                    Target::Track(id),
                    Some(Record {
                        version: 1,
                        value: Value::Text(owner.into()),
                    }),
                )
                .unwrap();
        }
    }
    table
        .set(
            "author.project",
            Some(Record {
                version: 1,
                value: Value::Bool(true),
            }),
        )
        .unwrap();
    h.apply(&mut d, op(table.clone())).unwrap();
    let b = plumspirit_engine::native::project_to_bytes(&d).unwrap();
    let (reopened, _) = plumspirit_engine::native::project_from_bytes(&b).unwrap();
    assert_eq!(reopened.project().metadata, table);
    h.apply(&mut d, Operation::DeleteTrack { index: 1 })
        .unwrap();
    assert_eq!(d.project().metadata.len(), 1);
    h.undo(&mut d).unwrap();
    assert_eq!(d.project().metadata, table);
    h.redo(&mut d).unwrap();
    assert_eq!(d.project().metadata.len(), 1);
    h.undo(&mut d).unwrap();
    h.apply_without_undo(&mut d, Operation::DeleteTrack { index: 1 })
        .unwrap();
    assert_eq!(d.project().metadata.len(), 1);
    assert!(!h.can_undo());
}

#[test]
fn native_missing_targets_are_refused_and_track_copy_does_not_inherit() {
    use plumspirit_engine::model::{Track, TrackId, metadata::Target};
    let mut d = PlumDocument::new_project("Targets".into(), Bpm::DEFAULT, 960);
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    let mut invalid = Metadata::default();
    invalid
        .set_at(
            "author.demo",
            Target::Track(TrackId(99)),
            Some(Record {
                version: 1,
                value: Value::Null,
            }),
        )
        .unwrap();
    assert!(h.apply(&mut d, op(invalid.clone())).is_err());
    assert!(d.project().metadata.is_empty());
    d.project_mut().metadata = invalid;
    assert!(plumspirit_engine::native::project_to_bytes(&d).is_err());
    d.project_mut().metadata = Metadata::default();
    let source = d.track(0).unwrap();
    let source_id = source.id();
    let new_id = TrackId(d.next_track_id());
    let copy = Track::from_parts(
        new_id,
        source.notes.clone(),
        source.events.clone(),
        source.end_of_track_tick,
    );
    let mut m = Metadata::default();
    m.set_at(
        "author.demo",
        Target::Track(source_id),
        Some(Record {
            version: 1,
            value: Value::Integer(7),
        }),
    )
    .unwrap();
    h.apply(&mut d, op(m)).unwrap();
    h.apply(
        &mut d,
        Operation::InsertTracks {
            index: 1,
            tracks: vec![copy],
            label: "Independent copy",
        },
    )
    .unwrap();
    assert!(
        d.project()
            .metadata
            .get_at("author.demo", Target::Track(new_id))
            .is_none()
    );
    assert!(
        d.project()
            .metadata
            .get_at("author.demo", Target::Track(source_id))
            .is_some()
    );
}

#[test]
fn group_metadata_prunes_foreign_owners_and_temporary_transitions_with_native_history() {
    use plumspirit_engine::model::{GroupInfo, GroupTable, metadata::Target};
    let mut d = PlumDocument::new_project("Groups".into(), Bpm::DEFAULT, 960);
    let id = d.project_mut().groups.add(GroupInfo::default());
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    let mut m = Metadata::default();
    for owner in ["author.one", "author.two"] {
        m.set_at(
            owner,
            Target::Group(id),
            Some(Record {
                version: 1,
                value: Value::Text(owner.into()),
            }),
        )
        .unwrap();
    }
    h.apply(&mut d, op(m.clone())).unwrap();
    let bytes = plumspirit_engine::native::project_to_bytes(&d).unwrap();
    let (opened, _) = plumspirit_engine::native::project_from_bytes(&bytes).unwrap();
    assert_eq!(opened.project().metadata, m);
    let mut temporary = d.project().groups.clone();
    temporary.get_mut(id).unwrap().temporary = true;
    h.apply(
        &mut d,
        Operation::SetGroups {
            table: Box::new(temporary.clone()),
            label: "Temporary group",
        },
    )
    .unwrap();
    assert!(d.project().metadata.is_empty());
    let bytes = plumspirit_engine::native::project_to_bytes(&d).unwrap();
    let (opened, _) = plumspirit_engine::native::project_from_bytes(&bytes).unwrap();
    assert!(opened.project().groups.is_empty());
    assert!(opened.project().metadata.is_empty());
    h.undo(&mut d).unwrap();
    assert_eq!(d.project().metadata, m);
    h.redo(&mut d).unwrap();
    assert!(d.project().metadata.is_empty());
    h.undo(&mut d).unwrap();
    h.apply(
        &mut d,
        Operation::SetGroups {
            table: Box::new(GroupTable::new()),
            label: "Delete groups",
        },
    )
    .unwrap();
    assert!(d.project().metadata.is_empty());
    h.undo(&mut d).unwrap();
    assert_eq!(d.project().metadata, m);
    h.apply_without_undo(
        &mut d,
        Operation::SetGroups {
            table: Box::new(temporary),
            label: "Temporary without Undo",
        },
    )
    .unwrap();
    assert!(d.project().metadata.is_empty());
    assert!(!h.can_undo());
    let mut invalid = Metadata::default();
    invalid
        .set_at(
            "author.demo",
            Target::Group(id),
            Some(Record {
                version: 1,
                value: Value::Null,
            }),
        )
        .unwrap();
    assert!(h.apply(&mut d, op(invalid)).is_err());
}

#[test]
fn group_deletion_metadata_journal_survives_real_file_restart() {
    use plumspirit_engine::model::{GroupInfo, GroupTable, metadata::Target};
    let base = std::env::temp_dir().canonicalize().unwrap();
    let root = base.join(format!(
        "plumspirit-api02-group-metadata-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("song.lup");
    let journal = root.join("song.plh");
    let mut d = PlumDocument::new_project("Groups".into(), Bpm::DEFAULT, 960);
    let id = d.project_mut().groups.add(GroupInfo::default());
    let mut m = Metadata::default();
    m.set_at(
        "author.unknown",
        Target::Group(id),
        Some(Record {
            version: 42,
            value: Value::Integer(i64::MAX),
        }),
    )
    .unwrap();
    d.project_mut().metadata = m.clone();
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    h.attach_new_journal(
        Journal::create(&journal, path_key(&file)).unwrap(),
        Some(identity(&d)),
    )
    .unwrap();
    h.apply(
        &mut d,
        Operation::SetGroups {
            table: Box::new(GroupTable::new()),
            label: "Delete attributed group",
        },
    )
    .unwrap();
    assert!(d.project().metadata.is_empty());
    save_project(&d, &file, LupControl::default()).unwrap();
    let state = h.current_state();
    h.mark_saved(state);
    h.persist_saved(state, identity(&d));
    h.journal_mut().unwrap().sync().unwrap();
    drop(h);
    let (mut d, _) = load_project(&file, LupControl::default()).unwrap();
    let (j, scan) = Journal::open(&journal, path_key(&file)).unwrap().unwrap();
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    h.restore_journal(j, scan, identity(&d)).unwrap();
    h.undo(&mut d).unwrap();
    assert!(d.project().groups.get(id).is_some());
    assert_eq!(d.project().metadata, m);
    h.redo(&mut d).unwrap();
    assert!(d.project().metadata.is_empty());
    drop(h);
    let resolved = root.canonicalize().unwrap();
    assert_eq!(resolved.parent(), Some(base.as_path()));
    assert!(
        resolved
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("plumspirit-api02-group-metadata-")
    );
    std::fs::remove_dir_all(resolved).unwrap();
}
