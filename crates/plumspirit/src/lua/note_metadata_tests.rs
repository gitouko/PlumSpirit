use super::*;
use plumspirit_engine::history::{History, HistoryLimits};
use plumspirit_engine::model::{Bpm, GroupInfo, NoteAttrs, NoteOffKind, NoteRecord};

fn fixture() -> (Host, Arc<plumspirit_engine::model::PlumDocument>, History) {
    let mut d =
        plumspirit_engine::model::PlumDocument::new_project("Metadata".into(), Bpm::DEFAULT, 960);
    d.project_mut().groups.add(GroupInfo {
        name: Some("Editable".into()),
        ..Default::default()
    });
    d.project_mut().groups.add(GroupInfo {
        locked: true,
        ..Default::default()
    });
    let d = Arc::new(d);
    let mut h = History::new(HistoryLimits::default());
    h.reset(false);
    h.observe_ids(&d);
    let host = Host::new(
        Limits::default(),
        [
            Capability::ReadProject,
            Capability::EditNotes,
            Capability::EditTracks,
        ],
    )
    .unwrap();
    host.lua
        .set_app_data(bridge::NativeBudget::isolated(256 * 1024 * 1024));
    host.set_world(Some(World::new(
        Arc::clone(&d),
        h.incarnations().snapshot(),
    )));
    (host, d, h)
}
fn run(host: &Host, src: &str) -> Plain {
    host.run("metadata.lua", src)
        .unwrap_or_else(|e| panic!("{e}"))
}
fn records(d: &plumspirit_engine::model::PlumDocument) -> Vec<NoteRecord> {
    d.track(0)
        .unwrap()
        .notes
        .iter()
        .map(|n| n.record())
        .collect()
}

#[test]
fn optional_metadata_columns_each_and_writer_match_rows_without_changing_defaults() {
    let (host, mut doc, mut history) = fixture();
    let preview = run(
        &host,
        r#"
        local p=plum.project.current();local t=p:track(1);local tx=plum.transaction{}
        tx:insert_notes(t,{{start=0,gate=12,key=60,muted=true,off_kind='note_off',off_velocity=97,group=1}})
        local w=tx:note_writer(t)
        w:append{start={20,40},gate={12,12},key={61,62},muted={false,false},
            off_kind={'note_on_zero','note_off'},off_velocity={0,127},group={1,0}}
        w:finish()
        local legacy=t:notes():batches(8)();local count=0
        for name in pairs(legacy) do count=count+1 end
        assert(count==9 and legacy.muted==nil and legacy.group==nil)
        local row=t:notes():each()();count=0;for name in pairs(row) do count=count+1 end
        assert(count==8 and row.off_kind==nil)
        local b=t:notes():batches{columns={'muted','off_kind','off_velocity','group','locked','track_id','content_id','host_kind'}}()
        assert(b.start==nil and b.count==3 and b.muted[1] and not b.muted[2])
        assert(b.off_velocity[1]==97 and b.off_kind[2]=='note_on_zero' and b.off_velocity[2]==0)
        assert(b.group[1]==1 and b.group[3]==0 and not b.locked[1])
        assert(b.track_id[1]==t:id() and b.content_id[1]==0 and b.host_kind[1]=='track')
        local r=t:notes():each{columns={'key','muted','off_velocity','group'}}()
        assert(r.key==60 and r.muted and r.off_velocity==97 and r.group==1 and r.start==nil)
        assert(t:notes{muted=true,group=1}:count()==1)
        local hash=p:content_hash();tx:commit();return hash
    "#,
    );
    let mut commits = host.take_commits().unwrap();
    let expected = records(commits.result());
    commits.apply(&mut history, &mut doc).unwrap();
    drop(commits);
    assert_eq!(records(&doc), expected);
    assert_eq!(
        Plain::String(format!("{:016x}", doc.export_fingerprint())),
        preview
    );
    let bytes = plumspirit_engine::native::lup::project_to_bytes(&doc).unwrap();
    let (reopened, _) = plumspirit_engine::native::lup::project_from_bytes(&bytes).unwrap();
    assert_eq!(records(&reopened), expected);
    let mid = plumspirit_engine::midi::export_to_bytes(&doc).unwrap();
    let (imported, _) = plumspirit_engine::midi::import_bytes(&mid).unwrap();
    let audible = records(&imported);
    assert_eq!(
        audible.len(),
        2,
        "muted note omitted by existing MIDI exporter"
    );
    assert_eq!(
        audible
            .iter()
            .map(|r| (r.attrs.off_kind(), r.attrs.off_velocity()))
            .collect::<Vec<_>>(),
        vec![(NoteOffKind::NoteOnZero, 0), (NoteOffKind::NoteOff, 127)]
    );
    history
        .undo(Arc::get_mut(&mut doc).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(doc.note_count(), 0);
    history
        .redo(Arc::get_mut(&mut doc).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(records(&doc), expected);
}

#[test]
fn native_and_callback_changes_preserve_packed_fields_and_validate_off_combinations() {
    let (host, _, _) = fixture();
    run(
        &host,
        r#"
        local t=plum.project.current():track(1);local tx=plum.transaction{}
        tx:insert_notes(t,{start={0,10},gate={1,1},key={60,61},off_velocity={12,34},group={1,1}})
        assert(tx:modify_notes(t:notes(),{muted=true,off_kind='note_on_zero'})==2)
        local b=t:notes():batches{columns={'muted','off_kind','off_velocity','group'}}()
        assert(b.muted[1] and b.off_kind[1]=='note_on_zero' and b.off_velocity[1]==0 and b.group[1]==1)
        tx:modify_notes(t:notes(),function(n) return {muted=false,off_kind='note_off',off_velocity=99,group=0} end)
        b=t:notes():batches{columns={'muted','off_kind','off_velocity','group'}}()
        assert(not b.muted[1] and b.off_velocity[2]==99 and b.group[2]==0)
        tx:duplicate_notes(t:notes(),{shift=100,off_velocity=17,group=1})
        assert(t:notes{group=1}:count()==2)
        local before=plum.project.current():content_hash()
        assert(not pcall(function() tx:modify_notes(t:notes(),{off_kind='note_on_zero',off_velocity=1}) end))
        assert(plum.project.current():content_hash()==before)
        tx:cancel()
    "#,
    );
    assert!(host.take_commits().is_none());
}

#[test]
fn invalid_metadata_poison_writer_and_existing_note_locks_refuse_changes() {
    let (host, _, _) = fixture();
    for fields in [
        "muted=1",
        "off_kind='invalid'",
        "off_velocity=128",
        "off_kind='note_on_zero',off_velocity=1",
        "group=999",
        "group=2",
        "group=-1",
    ] {
        run(
            &host,
            &format!(
                r#"
            local t=plum.project.current():track(1);local tx=plum.transaction{{}}
            local w=tx:note_writer(t)
            assert(not pcall(function() w:append({{{{start=0,gate=1,key=60,{fields}}}}}) end))
            assert(w:info().state=='failed' and t:notes():count()==0);w:cancel();tx:cancel()
        "#
            ),
        );
    }
    // Install a native locked note; Lua cannot bypass the native lock via metadata.
    let (host, mut d, mut h) = fixture();
    host.set_world(None);
    Arc::get_mut(&mut d)
        .unwrap()
        .track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(
            &[NoteRecord {
                start_tick: 0,
                duration_ticks: 1,
                attrs: NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                sub: Default::default(),
                group: 2,
            }],
            false,
        )
        .unwrap();
    h.observe_ids(&d);
    host.set_world(Some(World::new(
        Arc::clone(&d),
        h.incarnations().snapshot(),
    )));
    run(
        &host,
        r#"
        local t=plum.project.current():track(1);local tx=plum.transaction{}
        local b=t:notes():batches{columns={'locked'}}();assert(b.locked[1])
        assert(not pcall(function() tx:delete_notes(t:notes()) end))
        assert(not pcall(function() tx:modify_notes(t:notes(),{group=0,muted=true}) end))
        assert(not pcall(function() tx:modify_notes(t:notes(),function(n) return false end) end))
        assert(t:notes():count()==1);tx:cancel()
    "#,
    );
}

#[test]
fn packed_columns_stream_under_eight_mib_vm_in_extended_key_mode() {
    let (baseline, mut doc, mut h) = fixture();
    drop(baseline);
    Arc::get_mut(&mut doc).unwrap().project_mut().keys =
        plumspirit_engine::model::project::KeyRange::Extended256;
    h.observe_ids(&doc);
    let host = Host::new(
        Limits {
            memory: 8 * 1024 * 1024,
            ..Limits::default()
        },
        [Capability::ReadProject, Capability::EditNotes],
    )
    .unwrap();
    host.lua
        .set_app_data(bridge::NativeBudget::isolated(256 * 1024 * 1024));
    host.set_world(Some(World::new(
        Arc::clone(&doc),
        h.incarnations().snapshot(),
    )));
    host.lua.gc_stop();
    run(
        &host,
        r#"
        local t=plum.project.current():track(1);local tx=plum.transaction{};local w=tx:note_writer(t)
        local b={start={},gate={},key={},muted={},off_kind={},off_velocity={},group={}}
        for i=1,32768 do
            b.start[i]=i;b.gate[i]=1;b.key[i]=200;b.muted[i]=i%2==0
            b.off_kind[i]='note_off';b.off_velocity[i]=i%128;b.group[i]=1
        end
        assert(w:append(b)==32768);w:finish();assert(t:notes{group=1,muted=true}:count()==16384)
        local n=t:notes():each{columns={'key','muted','off_kind','off_velocity','group'}}()
        assert(n.key==200 and not n.muted and n.off_velocity==1 and n.group==1)
        tx:commit()
    "#,
    );
    let c = host.take_commits().unwrap();
    assert_eq!(c.result().stored_note_count(), 32768);
    drop(c);
    assert_eq!(
        host.lua
            .app_data_ref::<bridge::NativeBudget>()
            .unwrap()
            .used(),
        0
    );
}

#[test]
fn legacy_strict_index_handlers_never_receive_new_optional_field_requests() {
    let (host, _, _) = fixture();
    run(
        &host,
        r#"
        local new={muted=true,off_kind=true,off_velocity=true,group=true}
        local function legacy(data)
            return setmetatable(data,{__index=function(_,key)
                assert(not new[key],'API01 getter was asked for new field '..key)
                return nil
            end})
        end
        local t=plum.project.current():track(1);local tx=plum.transaction{}
        tx:insert_notes(t,{legacy{start=0,gate=1,key=60}})
        tx:insert_notes(t,legacy{start={10},gate={1},key={61}})
        assert(t:notes(legacy{}):count()==2)
        tx:modify_notes(t:notes(),legacy{velocity_add=1})
        tx:modify_notes(t:notes(),function(n) return legacy{velocity=22} end)
        tx:insert_notes(t,{legacy{start=20,gate=1,key=62,muted=true,off_velocity=42}})
        local n=t:notes{muted=true}:each{columns={'muted','off_velocity'}}()
        assert(n.muted and n.off_velocity==42)
        assert(t:notes():count()==3);tx:cancel()
    "#,
    );
    assert!(host.take_commits().is_none());
}
