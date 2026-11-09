use super::*;
use crate::lua::{Limits, Plain};
use plumspirit_engine::model::Bpm;

fn fixture() -> (Host, Arc<PlumDocument>, History) {
    let mut d = PlumDocument::new_project("Groups".into(), Bpm::DEFAULT, 960);
    d.add_track().unwrap();
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
        .set_app_data(NativeBudget::isolated(256 * 1024 * 1024));
    bind(&host, &d, &h);
    (host, d, h)
}
fn bind(host: &Host, d: &Arc<PlumDocument>, h: &History) {
    host.set_world(Some(World::new(Arc::clone(d), h.incarnations().snapshot())));
}
fn run(host: &Host, src: &str) -> Plain {
    host.run("groups.lua", src)
        .unwrap_or_else(|e| panic!("{e}"))
}
fn used(host: &Host) -> usize {
    host.lua.app_data_ref::<NativeBudget>().unwrap().used()
}

#[test]
fn native_groups_nested_linked_temporary_cross_track_and_clip_roundtrip_undo() {
    let (host, mut doc, mut history) = fixture();
    let result = run(
        &host,
        r#"
        local p=plum.project.current();local t=p:track(1);local other=p:track(2);local tx=plum.transaction{label='Group author edit'}
        local root=tx:create_group{name='Root',color={r=1,g=2,b=255}}
        local child=tx:create_group{name='Child',parent=root}
        local peer=tx:create_group{name='Peer'};local temp=tx:create_group{temporary=true}
        assert(root:kind()=='group' and root:valid() and root:name()=='Root' and root:id()==1)
        assert(p:group(1)==root and p:group(0)==nil and #p:groups{limit=2}==2)
        assert(p:groups{offset=2,limit=2}[1]==peer)
        assert(child:group_info().parent==root:id() and root:group_info().color.b==255)
        tx:link_groups({root,peer});assert(root:group_info().link==root:id() and peer:group_info().link==root:id())
        tx:insert_notes(t,{{start=0,gate=10,key=60,group=child:id()}})
        tx:insert_notes(other,{{start=20,gate=10,key=61,group=child:id()}})
        local clip=tx:add_clip(t,1000,500);local linked=tx:duplicate_clips(clip,{start=2000,linked=true})[1]
        tx:insert_notes(clip:content(),{{start=3,gate=4,key=62,group=peer:id()}})
        tx:insert_notes(t,{{start=30,gate=10,key=63,group=temp:id()}})
        assert(linked:notes{group=peer:id()}:count()==1)
        local b=clip:content():notes():batches{columns={'group','track_id','content_id','host_kind'}}()
        assert(b.host_kind[1]=='clip_content' and b.content_id[1]==clip:content():id() and b.track_id[1]==t:id())
        tx:edit_group(child,{parent=peer,name='Reparented',color={r=5,g=6,b=7}})
        tx:edit_group(root,{color=false,name=false})
        assert(child:group_info().parent==peer:id() and root:group_info().color==nil and root:name()=='')
        local hash=p:content_hash();tx:commit();return hash
    "#,
    );
    let mut c = host.take_commits().unwrap();
    assert!(used(&host) > 0);
    assert_eq!(c.result().stored_note_count(), 4);
    assert_eq!(c.steps.len(), 1);
    c.apply(&mut history, &mut doc).unwrap();
    drop(c);
    assert_eq!(used(&host), 0);
    assert_eq!(
        Plain::String(format!("{:016x}", doc.export_fingerprint())),
        result
    );
    let groups = doc.project().groups.clone();
    let bytes = plumspirit_engine::native::lup::project_to_bytes(&doc).unwrap();
    let (reopened, _) = plumspirit_engine::native::lup::project_from_bytes(&bytes).unwrap();
    assert!(
        reopened.project().groups.get(4).is_none(),
        "temporary group omitted"
    );
    assert_eq!(reopened.project().groups.get(2).unwrap().parent, 3);
    assert_eq!(reopened.project().groups.get(3).unwrap().link, 1);
    let temp_note = reopened
        .track(0)
        .unwrap()
        .notes
        .iter()
        .find(|n| n.attrs.pitch() == 63)
        .unwrap();
    assert_eq!(temp_note.group, 0);
    history
        .undo(Arc::get_mut(&mut doc).unwrap())
        .unwrap()
        .unwrap();
    assert!(doc.project().groups.is_empty());
    assert_eq!(doc.stored_note_count(), 0);
    history
        .redo(Arc::get_mut(&mut doc).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(doc.project().groups, groups);
    assert_eq!(doc.stored_note_count(), 4);
}

#[test]
fn inherited_locks_protect_notes_writers_subchannels_tracks_and_linked_clip_placements() {
    let (host, doc, _) = fixture();
    run(
        &host,
        r#"
        local p=plum.project.current();local t=p:track(1);local other=p:track(2);local tx=plum.transaction{}
        local root=tx:create_group{};local child=tx:create_group{parent=root};local peer=tx:create_group{}
        local sub=tx:add_subchannel(t,{channel=3})
        tx:insert_notes(t,{{start=0,gate=10,key=60,group=child:id(),subchannel=sub:id()}})
        local clip=tx:add_clip(other,100,100);local linked=tx:duplicate_clips(clip,{start=300,track=t,linked=true})[1]
        tx:insert_notes(clip:content(),{{start=0,gate=10,key=60,group=child:id()}})
        local w=tx:note_writer(t);w:append({{start=20,gate=1,key=61,group=child:id()}})
        tx:edit_group(root,{locked=true})
        assert(child:group_info().effective_locked and not child:group_info().locked)
        local before=p:content_hash()
        local blocked={
            function() tx:delete_notes(t:notes()) end,
            function() tx:modify_notes(t:notes(),{group=0}) end,
            function() tx:duplicate_notes(t:notes(),{group=0}) end,
            function() tx:modify_notes(t:notes(),function(n) return {muted=true,group=0} end) end,
            function() tx:insert_notes(t,{{start=0,gate=1,key=60,group=child:id()}}) end,
            function() w:finish() end,
            function() tx:delete_subchannel(sub) end,
            function() tx:edit_subchannel(sub,{channel=5,move_notes=true}) end,
            function() tx:delete_tracks(t) end,
            function() tx:delete_tracks(other) end,
            function() tx:move_clips(linked,{shift=10}) end,
            function() tx:resize_clip(clip,{length=50}) end,
            function() tx:delete_clips(clip) end,
            function() tx:set_muted(linked,true) end,
            function() tx:duplicate_clips(clip) end,
            function() tx:merge_clips({clip,linked},{mode='stacked'}) end,
            function() tx:edit_group(child,{parent=false,locked=false}) end,
            function() tx:edit_group(child,{temporary=true}) end,
            function() tx:link_groups({child,peer}) end,
        }
        for i,f in ipairs(blocked) do assert(not pcall(f),'lock bypass '..i); assert(p:content_hash()==before,'partial change '..i) end
        w:cancel()
        tx:select_notes(t:notes()) -- Selection is allowed, not a musical edit.
        tx:edit_group(child,{locked=false,name='Still protected'})
        assert(child:group_info().effective_locked)
        tx:edit_group(root,{locked=false})
        assert(tx:modify_notes(t:notes(),{off_velocity=42})==1)
        tx:cancel()
    "#,
    );
    assert!(host.take_commits().is_none());
    assert_eq!(doc.stored_note_count(), 0);
    assert_eq!(used(&host), 0);
}

#[test]
fn mixed_track_delete_preflights_all_locks_and_writer_rechecks_removed_groups() {
    let (host, _, _) = fixture();
    run(
        &host,
        r#"
        local p=plum.project.current();local a=p:track(1);local b=p:track(2);local tx=plum.transaction{}
        local g=tx:create_group{}
        tx:insert_notes(b,{{start=0,gate=1,key=60,group=g:id()}})
        tx:edit_group(g,{locked=true})
        assert(not pcall(function() tx:delete_tracks({a,b}) end))
        assert(a:valid() and b:valid() and p:track_count()==2)
        tx:edit_group(g,{locked=false})
        local empty=tx:create_group{}
        local w=tx:note_writer(a);w:append({{start=0,gate=1,key=60,group=empty:id()}})
        tx:delete_group(empty)
        assert(not pcall(function() w:finish() end));w:cancel()
        assert(a:notes():count()==0);tx:cancel()
    "#,
    );
    assert_eq!(used(&host), 0);
}

#[test]
fn group_cycles_invalid_inputs_safe_deletion_and_handle_expiry_are_explicit() {
    let (host, _, _) = fixture();
    run(
        &host,
        r#"
        local p=plum.project.current();local t=p:track(1);local tx=plum.transaction{}
        local a=tx:create_group{name='A'};local b=tx:create_group{parent=a}
        assert(not pcall(function() tx:edit_group(a,{parent=b}) end))
        assert(not pcall(function() tx:edit_group(a,{parent=a}) end))
        assert(a:group_info().parent==0)
        assert(not pcall(function() tx:delete_group(a) end))
        tx:edit_group(b,{parent=false});tx:delete_group(a);assert(not a:valid())
        assert(not pcall(function() tx:edit_group(a,{name='revive'}) end))
        tx:insert_notes(t,{{start=0,gate=1,key=60,group=b:id()}})
        assert(not pcall(function() tx:delete_group(b) end))
        tx:modify_notes(t:notes{group=b:id()},{group=0});tx:delete_group(b);assert(not b:valid())
        assert(#p:groups()==0)
        local invalid={
            function() tx:create_group{parent=t} end,
            function() tx:create_group{name=string.rep('x',65)} end,
            function() tx:create_group{color={r=-1,g=0,b=0}} end,
            function() tx:create_group{locked=1} end,
            function() tx:create_group{link=5} end,
            function() tx:create_group(setmetatable({},{})) end,
            function() p:groups{limit=4097} end,
        }
        for i,f in ipairs(invalid) do assert(not pcall(f),'invalid accepted '..i) end
        local g=tx:create_group{};local previous=g:group_info();tx:link_groups({g},false)
        assert(not pcall(function() tx:link_groups({g,g}) end))
        tx:cancel()
    "#,
    );
    assert_eq!(used(&host), 0);
}

#[test]
fn group_handle_cannot_revive_after_undo_and_native_id_reuse() {
    let (host, mut doc, mut h) = fixture();
    run(
        &host,
        "local tx=plum.transaction{};old=tx:create_group{name='Old'};oldkey=old:key();tx:commit()",
    );
    let mut c = host.take_commits().unwrap();
    c.apply(&mut h, &mut doc).unwrap();
    drop(c);
    h.undo(Arc::get_mut(&mut doc).unwrap()).unwrap().unwrap();
    bind(&host, &doc, &h);
    run(
        &host,
        r#"
        assert(not old:valid());local tx=plum.transaction{};local new=tx:create_group{name='New'}
        assert(new:id()==1 and new:key()~=oldkey and new~=old and not old:valid())
        assert(not pcall(function() old:group_info() end))
        assert(not pcall(function() tx:edit_group(old,{locked=false}) end));tx:cancel()
    "#,
    );
    assert_eq!(used(&host), 0);
}

#[test]
fn canceled_transaction_handles_expire_before_reused_id_and_published_handles_stay_native() {
    let (host, mut doc, mut h) = fixture();
    run(
        &host,
        r#"
        local p=plum.project.current();local tx=plum.transaction{}
        local canceled=tx:create_group{name='Discarded'};local id=canceled:id();tx:cancel()
        assert(not canceled:valid())
        tx=plum.transaction{};local replacement=tx:create_group{name='Replacement'}
        assert(replacement:id()==id and replacement~=canceled and not canceled:valid())
        assert(not pcall(function() tx:edit_group(canceled,{name='Wrong object'}) end))
        tx:commit();assert(replacement:valid())
    "#,
    );
    let mut c = host.take_commits().unwrap();
    c.apply(&mut h, &mut doc).unwrap();
    drop(c);
    bind(&host, &doc, &h);
    run(
        &host,
        r#"
        local p=plum.project.current();published=p:group(1);published_key=published:key()
        local tx=plum.transaction{};tx:edit_group(published,{name='Native published handle'})
        tx:commit();assert(published:valid())
    "#,
    );
    let mut c = host.take_commits().unwrap();
    c.apply(&mut h, &mut doc).unwrap();
    drop(c);
    bind(&host, &doc, &h);
    run(
        &host,
        "assert(published:valid() and published:name()=='Native published handle' and published:key()==published_key)",
    );
    host.set_world(None);
    h.undo(Arc::get_mut(&mut doc).unwrap()).unwrap().unwrap();
    h.undo(Arc::get_mut(&mut doc).unwrap()).unwrap().unwrap();
    h.redo(Arc::get_mut(&mut doc).unwrap()).unwrap().unwrap();
    bind(&host, &doc, &h);
    run(
        &host,
        "assert(not published:valid() and plum.project.current():group(1):key()~=published_key)",
    );
}

#[test]
fn group_authority_native_quota_job_limit_and_retained_cleanup_are_bounded() {
    let (host, doc, mut h) = fixture();
    host.lua.set_app_data(NativeBudget::isolated(0));
    let error = host
        .run("quota.lua", "local tx=plum.transaction{};tx:create_group{}")
        .unwrap_err();
    assert_eq!(error.category, Category::Budget);
    assert!(host.take_commits().is_none());
    assert_eq!(used(&host), 0);
    host.lua
        .set_app_data(NativeBudget::isolated(32 * 1024 * 1024));
    run(
        &host,
        r#"
        local tx=plum.transaction{};retained=tx:create_group{}
        for i=1,1023 do tx:edit_group(retained,{locked=false}) end
        assert(not pcall(function() tx:create_group{} end));tx:cancel()
    "#,
    );
    assert_eq!(used(&host), 0);
    let denied = Host::new(Limits::default(), [Capability::ReadProject]).unwrap();
    h.observe_ids(&doc);
    bind(&denied, &doc, &h);
    assert_eq!(
        denied
            .run("denied.lua", "plum.transaction{}:create_group{}")
            .unwrap_err()
            .category,
        Category::Capability
    );
}

#[test]
fn published_native_groups_example_executes_and_matches_apply() {
    let docs = include_str!("../../../../docs/LUA_API.md").replace("\r\n", "\n");
    let section = docs
        .split("### Native note groups (API 0.2)")
        .nth(1)
        .unwrap();
    let source = section
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    let (host, mut doc, mut h) = fixture();
    let preview = run(&host, source);
    let mut c = host.take_commits().unwrap();
    c.apply(&mut h, &mut doc).unwrap();
    drop(c);
    assert_eq!(
        Plain::String(format!("{:016x}", doc.export_fingerprint())),
        preview
    );
    assert_eq!(doc.project().groups.len(), 2);
    assert_eq!(doc.stored_note_count(), 1);
    assert_eq!(used(&host), 0);
}
