use super::*;
use crate::lua::{Limits, Plain, plugin_api::PluginMeta};
use plumspirit_engine::model::{Bpm, PlumDocument};

fn fixture(owner: &str, caps: &[Capability]) -> (Host, Arc<PlumDocument>, History) {
    let doc = Arc::new(PlumDocument::new_project(
        "Metadata".into(),
        Bpm::DEFAULT,
        960,
    ));
    let mut h = History::new(HistoryLimits::default());
    h.reset(true);
    h.observe_ids(&doc);
    let host = Host::new(Limits::default(), caps.iter().copied()).unwrap();
    host.set_plugin(PluginMeta {
        id: owner.into(),
        name: "Metadata".into(),
        version: "1".into(),
        root: std::env::temp_dir(),
        data_dir: None,
        config_version: 1,
    })
    .unwrap();
    host.lua
        .set_app_data(NativeBudget::isolated(32 * 1024 * 1024));
    bind(&host, &doc, &h);
    (host, doc, h)
}
fn bind(host: &Host, doc: &Arc<PlumDocument>, h: &History) {
    host.set_world(Some(World::new(
        Arc::clone(doc),
        h.incarnations().snapshot(),
    )));
}
const CAPS: [Capability; 2] = [Capability::ReadProject, Capability::EditMetadata];
fn run(host: &Host, s: &str) -> Plain {
    host.run("metadata.lua", s)
        .unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn sparse_typed_metadata_has_real_native_apply_undo_redo_lup_and_unchanged_midi() {
    let (host, mut doc, mut h) = fixture("author.demo", &CAPS);
    let original = plumspirit_engine::midi::export_to_bytes(&doc).unwrap();
    run(
        &host,
        r#"
      assert(select(2,plum.metadata.project())==nil)
      local tx=plum.transaction{label='Author attributes'}
      tx:set_project_metadata({tick=9007199254740993,minimum=-9223372036854775807-1,text='音🎵',values={true,3.25}}, {version=2,expect_version=0})
      local d,v=plum.metadata.project();assert(v==2 and d.tick==9007199254740993 and d.minimum==-9223372036854775807-1 and d.text=='音🎵')
      tx:commit()
    "#,
    );
    assert!(doc.project().metadata.is_empty());
    let mut c = host.take_commits().unwrap();
    assert_eq!(c.steps.len(), 1);
    c.apply(&mut h, &mut doc).unwrap();
    drop(c);
    assert_eq!(h.undo_label(), Some("Author attributes"));
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
    let table = doc.project().metadata.clone();
    assert_eq!(table.len(), 1);
    let b = plumspirit_engine::native::project_to_bytes(&doc).unwrap();
    plumspirit_engine::native::verify_project(&doc, &b[..], Some(b.len() as u64)).unwrap();
    let (opened, _) = plumspirit_engine::native::project_from_bytes(&b).unwrap();
    assert_eq!(opened.project().metadata, table);
    assert_eq!(
        plumspirit_engine::midi::export_to_bytes(&opened).unwrap(),
        original
    );
    h.undo(Arc::get_mut(&mut doc).unwrap()).unwrap();
    assert!(doc.project().metadata.is_empty());
    assert!(h.is_at_saved_state());
    h.redo(Arc::get_mut(&mut doc).unwrap()).unwrap();
    assert_eq!(doc.project().metadata, table);
}

#[test]
fn namespaces_are_private_and_explicit_migration_removal_are_undoable() {
    let (host, mut doc, mut h) = fixture("author.demo", &CAPS);
    run(
        &host,
        "local tx=plum.transaction();tx:set_project_metadata({old=1},{version=1});tx:commit()",
    );
    host.take_commits()
        .unwrap()
        .apply(&mut h, &mut doc)
        .unwrap();
    bind(&host, &doc, &h);
    run(
        &host,
        r#"local d,v=plum.metadata.project();assert(v==1);local tx=plum.transaction();tx:set_project_metadata({new=d.old+1},{version=2,expect_version=v});tx:commit()"#,
    );
    host.take_commits()
        .unwrap()
        .apply(&mut h, &mut doc)
        .unwrap();
    bind(&host, &doc, &h);
    let before = doc.project().metadata.clone();
    run(
        &host,
        "local d=plum.metadata.project();d.new=999;assert(plum.metadata.project().new==2)",
    );
    let e=host.run("stale-version.lua","local tx=plum.transaction();tx:set_project_metadata({}, {version=3,expect_version=1});tx:commit()").unwrap_err();
    assert!(e.message.contains("version changed"));
    assert!(host.take_commits().is_none());
    host.set_plugin(PluginMeta {
        id: "another.author".into(),
        name: "Other".into(),
        version: "1".into(),
        root: std::env::temp_dir(),
        data_dir: None,
        config_version: 1,
    })
    .unwrap();
    run(
        &host,
        "plum.plugin.id='author.demo';assert(select(2,plum.metadata.project())==nil);local tx=plum.transaction();tx:remove_project_metadata();tx:commit()",
    );
    host.take_commits()
        .unwrap()
        .apply(&mut h, &mut doc)
        .unwrap();
    assert_eq!(doc.project().metadata, before);
    host.set_plugin(PluginMeta {
        id: "author.demo".into(),
        name: "Original".into(),
        version: "1".into(),
        root: std::env::temp_dir(),
        data_dir: None,
        config_version: 1,
    })
    .unwrap();
    bind(&host, &doc, &h);
    run(
        &host,
        "local tx=plum.transaction();tx:remove_project_metadata{expect_version=2};assert(select(2,plum.metadata.project())==nil);tx:commit()",
    );
    host.take_commits()
        .unwrap()
        .apply(&mut h, &mut doc)
        .unwrap();
    assert!(doc.project().metadata.is_empty());
    host.set_world(None);
    h.undo(Arc::get_mut(&mut doc).unwrap()).unwrap();
    assert_eq!(doc.project().metadata, before);
}

#[test]
fn invalid_values_denials_canceled_failed_and_stale_jobs_leave_no_metadata() {
    let (host, mut doc, mut h) = fixture("author.demo", &CAPS);
    for value in [
        "0/0",
        "math.huge",
        "function()end",
        "setmetatable({}, {})",
        "(function()local t={};t.self=t;return t end)()",
        "string.rep('x',16385)",
        "{[2]=1}",
    ] {
        let s = format!(
            "local tx=plum.transaction();tx:set_project_metadata({value},{{version=1}});tx:commit()"
        );
        assert!(host.run("invalid.lua", &s).is_err(), "{value}");
        assert!(host.take_commits().is_none());
    }
    for opts in [
        "{version=0}",
        "{version=-1}",
        "{version=1.5}",
        "{version=4294967296}",
        "{version=1,expect_version=1.5}",
    ] {
        assert!(
            host.run(
                "bad-version.lua",
                &format!(
                    "local tx=plum.transaction();tx:set_project_metadata({{}},{opts});tx:commit()"
                )
            )
            .is_err()
        );
        assert!(host.take_commits().is_none());
    }
    run(
        &host,
        "local tx=plum.transaction();tx:set_project_metadata(true,{version=1});tx:cancel()",
    );
    assert!(host.take_commits().is_none());
    assert!(host.run("failed.lua","local tx=plum.transaction();tx:set_project_metadata(true,{version=1});tx:commit();error('fail')").is_err());
    assert!(host.take_commits().is_none());
    run(
        &host,
        "local tx=plum.transaction();tx:set_project_metadata(true,{version=1});tx:commit()",
    );
    let mut commits = host.take_commits().unwrap();
    host.set_world(None);
    doc = Arc::new((*doc).clone());
    assert_eq!(
        commits.apply(&mut h, &mut doc),
        Err(CommitError::ProjectChanged)
    );
    drop(commits);
    assert!(doc.project().metadata.is_empty());
    let (denied, _, _) = fixture("author.demo", &[Capability::ReadProject]);
    let e = denied
        .run(
            "denied.lua",
            "local tx=plum.transaction();tx:set_project_metadata(true,{version=1});tx:commit()",
        )
        .unwrap_err();
    assert_eq!(e.category, Category::Capability);
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}

#[test]
fn per_job_write_quota_is_bounded_and_resets_and_native_quota_preflights() {
    let (host, _, _) = fixture("author.demo", &CAPS);
    run(
        &host,
        r#"local tx=plum.transaction();for i=1,32 do tx:set_project_metadata(i,{version=1}) end;local ok,e=pcall(function()tx:set_project_metadata(33,{version=1})end);assert(not ok and tostring(e):find('32'));tx:cancel()"#,
    );
    run(
        &host,
        "local tx=plum.transaction();tx:set_project_metadata(1,{version=1});tx:cancel()",
    );
    host.lua.set_app_data(NativeBudget::isolated(1024 * 1024));
    let e = host
        .run(
            "native-quota.lua",
            "local tx=plum.transaction();tx:set_project_metadata(1,{version=1});tx:commit()",
        )
        .unwrap_err();
    assert_eq!(e.category, Category::Budget);
    assert!(host.take_commits().is_none());
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}

#[test]
fn track_subtrack_targets_delete_cleanup_and_native_undo_redo_are_one_boundary() {
    use plumspirit_engine::model::metadata::Target;
    let (host, mut doc, mut h) = fixture(
        "author.demo",
        &[
            Capability::ReadProject,
            Capability::EditMetadata,
            Capability::EditTracks,
        ],
    );
    run(
        &host,
        r#"
      local p=plum.project.current();local tx=plum.transaction{label='Track attributes'}
      local parent=tx:add_track{name='Parent'};local child=tx:add_subtracks(parent,1)[1]
      tx:set_metadata(parent,{role='parent'},{version=1,expect_version=0})
      tx:set_metadata(child,{role='child',tick=9007199254740993},{version=2})
      assert(plum.metadata.get(parent).role=='parent' and select(2,plum.metadata.get(child))==2)
      tx:set_metadata(p,{project=true},{version=1});assert(plum.metadata.project().project)
      _G.saved_parent=parent;tx:commit()
    "#,
    );
    host.take_commits()
        .unwrap()
        .apply(&mut h, &mut doc)
        .unwrap();
    let before = doc.project().metadata.clone();
    assert_eq!(before.len(), 3);
    let parent = doc.track(1).unwrap().id();
    let child = doc.track(2).unwrap().id();
    assert!(before.get_at("author.demo", Target::Track(child)).is_some());
    bind(&host, &doc, &h);
    run(
        &host,
        r#"local p=plum.project.current();local t=p:track(2);local tx=plum.transaction{label='Delete attributed subtree'};tx:delete_tracks(t);assert(not pcall(function()plum.metadata.get(t)end));assert(not pcall(function()tx:set_metadata(t,{}, {version=1})end));tx:commit()"#,
    );
    host.take_commits()
        .unwrap()
        .apply(&mut h, &mut doc)
        .unwrap();
    assert_eq!(doc.track_count(), 1);
    assert_eq!(doc.project().metadata.len(), 1);
    assert_eq!(h.undo_label(), Some("Delete attributed subtree"));
    host.set_world(None);
    h.undo(Arc::get_mut(&mut doc).unwrap()).unwrap();
    assert_eq!(doc.project().metadata, before);
    assert!(doc.index_of(parent).is_some());
    h.redo(Arc::get_mut(&mut doc).unwrap()).unwrap();
    assert_eq!(doc.project().metadata.len(), 1);
}

#[test]
fn unsupported_targets_readonly_authority_and_old_handles_cannot_write_other_objects() {
    let (host, mut doc, mut h) = fixture(
        "author.demo",
        &[
            Capability::ReadProject,
            Capability::EditMetadata,
            Capability::EditTracks,
            Capability::EditNotes,
        ],
    );
    run(
        &host,
        r#"local p=plum.project.current();local tx=plum.transaction();local t=tx:add_track();tx:set_metadata(t,42,{version=1});_G.stale=t;tx:cancel();local other=plum.transaction();local fresh=other:add_track();assert(select(2,plum.metadata.get(fresh))==nil);assert(not stale:valid());assert(not pcall(function()other:set_metadata(stale,1,{version=1})end));other:commit()"#,
    );
    host.take_commits()
        .unwrap()
        .apply(&mut h, &mut doc)
        .unwrap();
    assert!(doc.project().metadata.is_empty());
    bind(&host, &doc, &h);
    run(
        &host,
        r#"local p=plum.project.current();local tx=plum.transaction();local g=tx:create_group{temporary=true};local clip=tx:add_clip(p:track(1),0,100);for _,v in ipairs({1,'author.other',g,clip,clip:content(),p:conductor()}) do assert(not pcall(function()tx:set_metadata(v,{}, {version=1})end));assert(not pcall(function()plum.metadata.get(v)end)) end;tx:cancel()"#,
    );
    assert!(host.take_commits().is_none());
    assert!(doc.project().metadata.is_empty());
}

#[test]
fn transaction_births_survive_success_but_expire_on_implicit_cancel_failure_and_discard() {
    let (host, mut doc, mut h) = fixture(
        "author.demo",
        &[
            Capability::ReadProject,
            Capability::EditMetadata,
            Capability::EditTracks,
        ],
    );
    run(
        &host,
        "local tx=plum.transaction();_G.implicit=tx:add_track()",
    );
    run(
        &host,
        "assert(not implicit:valid());local tx=plum.transaction();local t=tx:add_track();assert(not pcall(function()tx:set_metadata(implicit,1,{version=1})end));tx:cancel()",
    );
    assert!(host.run("failure.lua","local p=plum.project.current();local tx=plum.transaction();_G.failed=tx:add_track();tx:commit();_G.failed_reacquired=p:track(2);assert(failed_reacquired:valid());error('fail')").is_err());
    run(
        &host,
        "local tx=plum.transaction();tx:add_track();assert(not failed:valid() and not failed_reacquired:valid());tx:cancel()",
    );
    run(
        &host,
        "local tx=plum.transaction();_G.discarded=tx:add_track();tx:commit()",
    );
    let pending = host.take_commits().unwrap();
    bind(&host, &doc, &h);
    run(
        &host,
        "local tx=plum.transaction();tx:add_track();assert(not discarded:valid());tx:cancel()",
    );
    drop(pending);
    run(
        &host,
        "local p=plum.project.current();local tx=plum.transaction();_G.accepted=tx:add_track();tx:commit();_G.reacquired=p:track(2);local next=plum.transaction();next:set_metadata(accepted,{ok=true},{version=1});assert(reacquired:valid());next:commit()",
    );
    host.take_commits()
        .unwrap()
        .apply(&mut h, &mut doc)
        .unwrap();
    bind(&host, &doc, &h);
    run(
        &host,
        "assert(accepted:valid() and reacquired:valid());assert(plum.metadata.get(accepted).ok)",
    );
}

#[test]
fn permanent_groups_obey_ancestor_locks_and_native_delete_temporary_cleanup() {
    use plumspirit_engine::model::metadata::Target;
    let (host, mut doc, mut h) = fixture(
        "author.demo",
        &[
            Capability::ReadProject,
            Capability::EditMetadata,
            Capability::EditNotes,
        ],
    );
    run(
        &host,
        r#"
      local tx=plum.transaction{label='Group attributes'}
      local parent=tx:create_group{name='Parent'};local child=tx:create_group{name='Child',parent=parent};local peer=tx:create_group{name='Peer'}
      tx:link_groups({child,peer});tx:set_metadata(child,{role='child'},{version=1})
      assert(select(2,plum.metadata.get(peer))==nil)
      tx:edit_group(parent,{locked=true})
      assert(plum.metadata.get(child).role=='child')
      local ok,e=pcall(function()tx:set_metadata(child,{}, {version=1})end);assert(not ok and tostring(e):find('locked'))
      assert(not pcall(function()tx:remove_metadata(child)end))
      tx:edit_group(parent,{locked=false});tx:commit()
    "#,
    );
    host.take_commits()
        .unwrap()
        .apply(&mut h, &mut doc)
        .unwrap();
    let table = doc.project().metadata.clone();
    assert!(table.get_at("author.demo", Target::Group(2)).is_some());
    bind(&host, &doc, &h);
    run(
        &host,
        "local tx=plum.transaction{label='Delete attributed group'};local child=plum.project.current():group(2);tx:delete_group(child);tx:commit()",
    );
    host.take_commits()
        .unwrap()
        .apply(&mut h, &mut doc)
        .unwrap();
    assert!(doc.project().metadata.is_empty());
    host.set_world(None);
    h.undo(Arc::get_mut(&mut doc).unwrap()).unwrap();
    assert_eq!(doc.project().metadata, table);
    h.redo(Arc::get_mut(&mut doc).unwrap()).unwrap();
    assert!(doc.project().metadata.is_empty());
    h.undo(Arc::get_mut(&mut doc).unwrap()).unwrap();
    bind(&host, &doc, &h);
    run(
        &host,
        "local tx=plum.transaction();tx:edit_group(plum.project.current():group(2),{temporary=true});assert(select(2,plum.metadata.project())==nil);tx:commit()",
    );
    host.take_commits()
        .unwrap()
        .apply(&mut h, &mut doc)
        .unwrap();
    assert!(doc.project().metadata.is_empty());
    host.set_world(None);
    h.undo(Arc::get_mut(&mut doc).unwrap()).unwrap();
    assert_eq!(doc.project().metadata, table);
}
