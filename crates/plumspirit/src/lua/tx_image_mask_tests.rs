use super::*;
#[test]
fn unscaled_gate_changes_keep_exact_native_integer_values() {
    let original = NoteRecord {
        start_tick: 0,
        duration_ticks: 9007199254740993,
        attrs: NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId::MAIN,
        group: 0,
    };
    let unchanged = Changes::default().apply(&original, 127).unwrap().unwrap();
    assert_eq!(unchanged, original);
    let add = Changes {
        gate_add: 1,
        ..Default::default()
    }
    .apply(&original, 127)
    .unwrap()
    .unwrap();
    assert_eq!({ add.duration_ticks }, 9007199254740994);
    let set = Changes {
        gate: Some(i64::MAX as u64),
        ..Default::default()
    }
    .apply(&original, 127)
    .unwrap()
    .unwrap();
    assert_eq!({ set.duration_ticks }, i64::MAX as u64);
}

use crate::lua::{Capability, Limits, World};

use plumspirit_engine::model::{Bpm, project::KeyRange};

fn fixture(keys: u16, limits: Limits) -> (Host, Arc<PlumDocument>, History, NativeBudget) {
    let mut doc = PlumDocument::new_project("Mask edits".into(), Bpm::DEFAULT, 960);
    doc.add_track().unwrap();
    doc.project_mut().keys = if keys == 256 {
        KeyRange::Extended256
    } else {
        KeyRange::Standard
    };
    let first = if keys == 256 { 200 } else { 60 };
    let records = (0..3)
        .map(|i| NoteRecord {
            start_tick: i * 4,
            duration_ticks: 4,
            attrs: NoteAttrs::new_wide(
                first + i as u8,
                80 + i as u8 * 10,
                0,
                0,
                NoteOffKind::NoteOff,
            )
            .unwrap(),
            sub: SubchannelId::MAIN,
            group: 0,
        })
        .collect::<Vec<_>>();
    doc.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&records, false)
        .unwrap();
    let mut selected = RankSetBuilder::default();
    selected.push(2);
    doc.track_mut(0)
        .unwrap()
        .notes
        .select_ranks(&selected.finish())
        .unwrap();
    let doc = Arc::new(doc);
    let mut history = History::new(HistoryLimits::default());
    history.reset(false);
    history.observe_ids(&doc);
    let h = Host::new(
        limits,
        [
            Capability::ReadProject,
            Capability::EditNotes,
            Capability::EditTracks,
        ],
    )
    .unwrap();
    let budget = NativeBudget::isolated(256 * 1024 * 1024);
    h.lua.set_app_data(budget.clone());
    h.set_world(Some(World::new(
        Arc::clone(&doc),
        history.incarnations().snapshot(),
    )));
    (h, doc, history, budget)
}
const SETUP: &str = "local t=plum.project.current():track(1);local image=plum.images.from_rgba(2,1,string.char(0,0,0,255,0,0,0,0));local mask=image:mask{kind='alpha'};local sampler=mask:note_sampler{start_tick=0,end_tick=12,first_key=0,last_key=255,sample='start'};image:close();mask:close();";
const SPAN_SETUP: &str = "local t=plum.project.current():track(1);local spans=plum.geometry.rasterize_polygon({x={0,1,1,0},y={0,0,1,1}},{columns=2,rows=1});local mask=plum.images.mask_from_spans(spans,{width=2,height=1});local sampler=mask:note_sampler{start_tick=0,end_tick=12,first_key=0,last_key=255,sample='start'};mask:close();";
#[test]
fn masked_musical_actions_have_identical_preview_apply_and_single_undo_redo_both_modes() {
    for setup in [SETUP, SPAN_SETUP] {
        for keys in [128, 256] {
            for action in ["delete", "duplicate", "modify"] {
                let (h, mut doc, mut history, budget) = fixture(keys, Limits::default());
                let original = doc.export_fingerprint();
                let changes = if action == "delete" {
                    ""
                } else {
                    ",changes={shift=2,transpose=1,velocity=72,gate_add=1}"
                };
                let source = format!(
                    "{setup}local tx=plum.transaction{{label='Masked edit'}};assert(tx:mask_notes(t:notes(),sampler,{{action='{action}'{changes}}})==2);tx:commit()"
                );
                h.run("mask-preview.lua", &source).unwrap();
                let preview = h.take_commits().unwrap();
                let expected = preview.result().export_fingerprint();
                let count = if action == "delete" {
                    1
                } else if action == "duplicate" {
                    5
                } else {
                    3
                };
                assert_eq!(preview.result().note_count(), count);
                assert_eq!(doc.export_fingerprint(), original);
                drop(preview);
                h.set_world(Some(World::new(
                    Arc::clone(&doc),
                    history.incarnations().snapshot(),
                )));
                h.run("mask-apply.lua", &source).unwrap();
                let mut commits = h.take_commits().unwrap();
                assert_eq!(commits.result().export_fingerprint(), expected);
                assert_eq!(commits.steps.len(), 1);
                h.set_world(None);
                commits.apply(&mut history, &mut doc).unwrap();
                drop(commits);
                assert_eq!(doc.note_count(), count);
                history
                    .undo(Arc::get_mut(&mut doc).unwrap())
                    .unwrap()
                    .unwrap();
                assert_eq!(doc.export_fingerprint(), original);
                assert!(!history.can_undo());
                history
                    .redo(Arc::get_mut(&mut doc).unwrap())
                    .unwrap()
                    .unwrap();
                assert_eq!(doc.export_fingerprint(), expected);
                h.lua.gc_collect().unwrap();
                assert_eq!(budget.used(), 0);
            }
        }
    }
}
#[test]
fn masked_selection_modes_and_empty_replace_preserve_native_selection_only_semantics() {
    for mode in ["replace", "add", "remove", "toggle"] {
        let (h, mut doc, mut history, _) = fixture(128, Limits::default());
        let expected = if mode == "replace" {
            2
        } else if mode == "remove" {
            1
        } else {
            3
        };
        h.run("mask-selection.lua",&format!("{SETUP}local tx=plum.transaction{{}};assert(tx:mask_notes(t:notes(),sampler,{{action='select',mode='{mode}'}})==2);assert(t:notes{{selected=true}}:count()=={expected});tx:commit()")).unwrap();
        let mut commits = h.take_commits().unwrap();
        assert_eq!(commits.steps[0].operations, 0);
        h.set_world(None);
        commits.apply(&mut history, &mut doc).unwrap();
        drop(commits);
        assert_eq!(doc.track(0).unwrap().notes.selected_count(), expected);
        assert!(!history.can_undo());
    }
    let (h, _, _, _) = fixture(128, Limits::default());
    h.run("empty-selection.lua",&format!("{SETUP}local tx=plum.transaction{{}};assert(tx:mask_notes(t:notes{{start_tick=100}},sampler,{{action='select'}})==0);assert(t:notes{{selected=true}}:count()==0);tx:cancel()")).unwrap();
}
#[test]
fn masked_validation_failures_leave_the_private_transaction_intact() {
    let invalid = [
        "{action='bad'}",
        "{action='modify'}",
        "{action='delete',changes={}}",
        "{action='duplicate',mode='add'}",
        "{action='modify',changes={unknown=1}}",
        "{action='modify',changes={shift=function()end}}",
        "{action='modify',changes=setmetatable({},{})}",
        "{action='modify',changes={gate_scale=-1}}",
        "{action='modify',changes={gate_scale=0/0}}",
        "{action='modify',changes={subchannel=999}}",
        "{action='modify',changes={transpose=2147483647}}",
        "{action='modify',changes={gate_scale=1e308,gate_add=1}}",
        "setmetatable({action='delete'},{})",
    ];
    for options in invalid {
        let (h, doc, _, budget) = fixture(128, Limits::default());
        let original = doc.export_fingerprint();
        h.run("invalid-mask-edit.lua",&format!("{SETUP}local tx=plum.transaction{{}};assert(not pcall(function()tx:mask_notes(t:notes(),sampler,{options})end));assert(tx:is_open() and t:notes():count()==3);tx:cancel()")).unwrap();
        assert!(h.take_commits().is_none());
        assert_eq!(doc.export_fingerprint(), original);
        h.lua.gc_collect().unwrap();
        assert_eq!(budget.used(), 0, "{options}");
    }
}
#[test]
fn masked_post_mutation_cancellation_discards_whole_transaction_even_when_caught() {
    use std::sync::atomic::Ordering;
    let (h, doc, _, budget) = fixture(128, Limits::default());
    let cancel = Arc::clone(&h.cancel);
    h.lua
        .globals()
        .set(
            "cancel_after_first",
            h.lua
                .create_function(
                    move |lua,
                          (tx, query, sampler, options): (
                        AnyUserData,
                        Value,
                        AnyUserData,
                        Table,
                    )| {
                        let tx = tx.borrow::<LuaTx>()?;
                        let result = edit_controlled(lua, &tx, query, sampler, options, || {
                            cancel.store(true, Ordering::Release)
                        });
                        cancel.store(false, Ordering::Release);
                        result
                    },
                )
                .unwrap(),
        )
        .unwrap();
    h.run("mask-cancel.lua",&format!("{SETUP}local tx=plum.transaction{{}};tx:insert_notes(t,{{{{start=100,gate=1,key=70}}}});assert(not pcall(cancel_after_first,tx,t:notes(),sampler,{{action='modify',changes={{velocity=72}}}}));assert(not tx:is_open() and not pcall(function()tx:commit()end));assert(t:notes():count()==3)")).unwrap();
    assert!(h.take_commits().is_none());
    assert_eq!(doc.note_count(), 3);
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
}
#[test]
fn extreme_modifiers_do_not_overflow_native_arithmetic() {
    let r = NoteRecord {
        start_tick: 0,
        duration_ticks: 1,
        attrs: NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId::MAIN,
        group: 0,
    };
    assert!(
        Changes {
            transpose: i32::MAX,
            ..Default::default()
        }
        .apply(&r, 127)
        .is_err()
    );
    let v = Changes {
        velocity_scale: Some(f64::MAX),
        velocity_add: i32::MAX,
        ..Default::default()
    }
    .apply(&r, 127)
    .unwrap()
    .unwrap();
    assert_eq!(v.attrs.velocity(), 127);
    assert!(
        Changes {
            gate_scale: Some(f64::MAX),
            gate_add: 1,
            ..Default::default()
        }
        .apply(&r, 127)
        .is_err()
    );
}

#[test]
fn source_ancestor_and_destination_group_locks_refuse_before_mutation() {
    for destination in [false, true] {
        let (h, doc, _, _) = fixture(128, Limits::default());
        let prepare = if destination {
            ""
        } else {
            "tx:modify_notes(t:notes{start_tick=0,end_tick=1},{group=child:id()})"
        };
        let changes = if destination {
            "{group=child:id()}"
        } else {
            "{velocity=72}"
        };
        h.run("masked-group-lock.lua",&format!("{SETUP}local tx=plum.transaction{{}};local parent=tx:create_group{{name='Parent'}};local child=tx:create_group{{name='Child',parent=parent}};{prepare};tx:edit_group(parent,{{locked=true}});assert(not pcall(function()tx:mask_notes(t:notes(),sampler,{{action='modify',changes={changes}}})end));assert(tx:is_open() and t:notes():count()==3);tx:cancel()")).unwrap();
        assert!(h.take_commits().is_none());
        assert_eq!(doc.note_count(), 3);
    }
}
#[test]
fn masked_subtrack_linked_clip_local_edits_keep_native_ownership_and_subchannels() {
    for setup in [SETUP, SPAN_SETUP] {
        for keys in [128, 256] {
            let (h, doc, _, _) = fixture(keys, Limits::default());
            let key = if keys == 256 { 200 } else { 60 };
            h.run("masked-clip.lua",&format!("{setup}local tx=plum.transaction{{}};local child=tx:add_subtracks(t,1)[1];local sub=tx:add_subchannel(child,{{channel=15}});local clip=tx:add_clip(child,10000,1000);local linked=tx:duplicate_clips(clip,{{start=20000,linked=true}})[1];tx:insert_notes(clip:content(),{{{{start=1,gate=2,key={key}}}}});assert(tx:mask_notes(linked:notes(),sampler,{{action='modify',changes={{shift=2,subchannel=sub:id(),channel=15,velocity=88}}}})==1);local b=clip:notes():batches(1)();assert(b.start[1]==3 and b.key[1]=={key} and b.channel[1]==15 and b.subchannel[1]==sub:id() and b.velocity[1]==88);assert(linked:to_song(3)==20003 and clip:to_song(3)==10003);tx:commit()")).unwrap();
            let commits = h.take_commits().unwrap();
            assert_eq!(commits.result().stored_note_count(), 4);
            assert_eq!(doc.stored_note_count(), 3);
        }
    }
}
#[test]
fn masked_native_refusal_preserves_transaction_and_output_budget_cannot_be_bypassed() {
    use crate::lua::bridge::Reservation;
    use std::{cell::RefCell, rc::Rc};
    let (h, doc, _, budget) = fixture(128, Limits::default());
    let held = Rc::new(RefCell::new(None::<Reservation>));
    let sink = held.clone();
    let pool = budget.clone();
    h.lua
        .globals()
        .set(
            "hold_native",
            h.lua
                .create_function(move |_, ()| {
                    *sink.borrow_mut() = Some(
                        pool.reserve(256 * 1024 * 1024 - pool.used() - 16)
                            .map_err(|e| e.lua())?,
                    );
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    let sink = held.clone();
    h.lua
        .globals()
        .set(
            "release_native",
            h.lua
                .create_function(move |_, ()| {
                    sink.borrow_mut().take();
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    h.run("masked-native-budget.lua",&format!("{SETUP}local tx=plum.transaction{{}};hold_native();local ok=pcall(function()tx:mask_notes(t:notes(),sampler,{{action='delete'}})end);release_native();assert(not ok and tx:is_open() and t:notes():count()==3);assert(tx:mask_notes(t:notes(),sampler,{{action='delete'}})==2);tx:cancel()")).unwrap();
    assert!(h.take_commits().is_none());
    assert_eq!(doc.note_count(), 3);
    let (h, _, _, _) = fixture(
        128,
        Limits {
            max_output_notes: 1,
            ..Default::default()
        },
    );
    h.run("masked-output-budget.lua",&format!("{SETUP}local tx=plum.transaction{{}};assert(not pcall(function()tx:mask_notes(t:notes(),sampler,{{action='duplicate'}})end));assert(tx:is_open() and t:notes():count()==3);tx:cancel()")).unwrap();
}
#[test]
fn native_mask_modification_preserves_large_gates_and_note_metadata_without_scale() {
    let (h, _, _, _) = fixture(128, Limits::default());
    h.run("masked-exact-gate.lua",&format!("{SETUP}local tx=plum.transaction{{}};tx:insert_notes(t,{{{{start=1,gate='9007199254740993',key=70,muted=true,off_kind='note_on_zero'}}}});assert(tx:mask_notes(t:notes{{keys=70}},sampler,{{action='modify',changes={{velocity=44}}}})==1);local b=t:notes{{keys=70}}:batches{{columns={{'gate','muted','off_kind','velocity'}}}}();assert(b.gate[1]==9007199254740993 and b.muted[1] and b.off_kind[1]=='note_on_zero' and b.velocity[1]==44);tx:cancel()")).unwrap();
}

#[test]
fn one_million_native_mask_deletions_fit_eight_mib_vm_and_keep_confirmation_gate() {
    let (h, mut doc, mut history, budget) = fixture(
        128,
        Limits {
            memory: 8 * 1024 * 1024,
            time: std::time::Duration::from_secs(60),
            ..Default::default()
        },
    );
    h.set_world(None);
    let notes = &mut Arc::get_mut(&mut doc).unwrap().track_mut(0).unwrap().notes;
    *notes = plumspirit_engine::model::NoteStore::new();
    let records = (0..1000000)
        .map(|start| NoteRecord {
            start_tick: start,
            duration_ticks: 1,
            attrs: NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
            sub: SubchannelId::MAIN,
            group: 0,
        })
        .collect::<Vec<_>>();
    notes.insert_sorted_batch(&records, false).unwrap();
    drop(records);
    history.observe_ids(&doc);
    h.set_world(Some(World::new(
        Arc::clone(&doc),
        history.incarnations().snapshot(),
    )));
    h.run("million-masked-delete.lua","local t=plum.project.current():track(1);local i=plum.images.from_rgba(1,1,string.char(0,0,0,255));local m=i:mask{kind='alpha'};local s=m:note_sampler{end_tick=1000000,first_key=60,last_key=60};local tx=plum.transaction{};assert(tx:mask_notes(t:notes(),s,{action='delete'})==1000000 and t:notes():count()==0);tx:commit()").unwrap();
    let commits = h.take_commits().unwrap();
    assert_eq!(commits.result().note_count(), 0);
    assert_eq!(commits.removed_notes(), 1000000);
    assert!(commits.needs_confirmation());
    assert_eq!(doc.note_count(), 1000000);
    assert!(budget.used() < 256 * 1024 * 1024);
    drop(commits);
    h.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
}
#[test]
fn native_insert_refusal_after_mask_erase_discards_the_whole_transaction() {
    let (h, doc, _, _) = fixture(128, Limits::default());
    h.lua.globals().set("refuse_after_erase",h.lua.create_function(|lua,(tx,query,sampler,options):(AnyUserData,Value,AnyUserData,Table)|{let tx=tx.borrow::<LuaTx>()?;edit_controlled(lua,&tx,query,sampler,options,||{let mut slot=lua.app_data_mut::<TxSlot>().unwrap();slot.open.as_mut().unwrap().history=History::new(HistoryLimits{max_bytes:0,max_entries:0});})}).unwrap()).unwrap();
    h.run("refused-after-mask-erase.lua",&format!("{SETUP}local tx=plum.transaction{{}};tx:insert_notes(t,{{{{start=100,gate=1,key=70}}}});assert(not pcall(refuse_after_erase,tx,t:notes(),sampler,{{action='modify',changes={{velocity=72}}}}));assert(not tx:is_open() and t:notes():count()==3);assert(not pcall(function()tx:commit()end))")).unwrap();
    assert!(h.take_commits().is_none());
    assert_eq!(doc.note_count(), 3);
}
#[test]
fn outside_drop_counts_deletion_and_creation_by_native_result_and_requires_edit_authority() {
    let (h, _, _, _) = fixture(
        128,
        Limits {
            max_output_notes: 0,
            ..Default::default()
        },
    );
    h.run("masked-drop.lua",&format!("{SETUP}local tx=plum.transaction{{}};assert(tx:mask_notes(t:notes(),sampler,{{action='duplicate',changes={{transpose=200,outside='drop'}}}})==2);assert(t:notes():count()==3);assert(tx:mask_notes(t:notes(),sampler,{{action='modify',changes={{transpose=200,outside='drop'}}}})==2);assert(t:notes():count()==1);tx:commit()")).unwrap();
    let commits = h.take_commits().unwrap();
    assert_eq!(commits.removed_notes(), 2);
    assert_eq!(commits.steps[0].inserted_notes, 0);
    let (_, doc, history, _) = fixture(128, Limits::default());
    let restricted = Host::new(Limits::default(), [Capability::ReadProject]).unwrap();
    restricted.set_world(Some(World::new(doc, history.incarnations().snapshot())));
    let err=restricted.run("masked-denied.lua",&format!("{SETUP}local tx=plum.transaction{{}};return tx:mask_notes(t:notes(),sampler,{{action='delete'}})")).unwrap_err();
    assert_eq!(err.category, Category::Capability);
}
#[test]
fn published_native_mask_transaction_example_executes() {
    let (h, doc, _, _) = fixture(128, Limits::default());
    let original = doc.export_fingerprint();
    let source = include_str!("../../../../docs/LUA_API.md")
        .split("## Native mask-conditioned note transactions (API 0.2)")
        .nth(1)
        .unwrap()
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    h.run("published-mask-transactions.lua", source).unwrap();
    let commits = h.take_commits().unwrap();
    assert_ne!(commits.result().export_fingerprint(), original);
    assert_eq!(doc.export_fingerprint(), original);
}
