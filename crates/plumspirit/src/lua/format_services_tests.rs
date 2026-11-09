//! Installed native codec/provider workers and real files, controlled file choice.
use super::*;
use crate::lua::formats::Mode;
use crate::lua::tx::Commits;
use std::path::Path;
const CODEC: &str = "test.codec";
const PROVIDER: &str = "test.codec-provider";
const CODEC_SOURCE: &str = r#"
local function invoke(ctx,op)
  assert(plum.has('services.call'))
  assert(not plum.has('ui.panels') and not plum.has('midi.input') and not plum.has('playback.control'))
  assert(not pcall(function()plum.config.set('blocked',true)end))
  assert(not pcall(function()plum.presets.save('blocked',{})end))
  assert(not pcall(function()plum.services.call('test.codec-provider/codec',{file=ctx.file})end))
  local out=plum.services.call('test.codec-provider/codec',{op=op,keys=ctx.keys,tick=9007199254741001,text=op=='import'and ctx.file:read(128)or'output'}, {seed=17})
  assert(out.seed==17 and math.type(out.tick)=='integer' and out.tick==9007199254741001)
  return out
end
function register(p)
 p.format{id='wire',name='Wire Codec',extensions={'pnotes'},import=function(ctx)
   local w=ctx.tx:note_writer(ctx.project:track(1));w:append{{start=1,gate=1,key=1,velocity=1}}
   local out=invoke(ctx,'import')
   w:append{{start=out.tick,gate=out.gate,key=out.key,velocity=out.velocity}};w:finish()
 end,export=function(ctx)
   ctx.file:write('WIRE\n') -- private prefix must disappear if the service fails
   local out=invoke(ctx,'export');ctx.file:write(out.text)
   assert(not pcall(function()ctx.file:commit()end))
 end}
 p.command{id='base',name='Inspect restored authority',run=function()
   assert(plum.has('ui.panels') and plum.has('midi.input') and plum.has('notes.edit') and plum.has('file.read_user_choice') and plum.has('file.write_user_choice'))
   print('CODEC_BASE_AUTHORITY_RESTORED')
 end}
end
"#;
const PROVIDER_SOURCE: &str = r#"
function register(p)
 p.service{id='codec',version='1.2',run=function(data,ctx)
   assert(type(data)=='table' and type(data.text)=='string' and ctx.seed==17)
   assert(not plum.has('ui.panels') and not plum.has('midi.input') and not plum.has('playback.control'))
   assert(plum.project.current()==nil) -- provider receives plain data, never caller World
   assert(not pcall(function()plum.transaction{label='forbidden provider edit'}end))
   assert(not pcall(function()plum.config.set('forbidden',true)end))
   assert(not pcall(function()plum.presets.save('forbidden',{})end))
   assert(not pcall(function()plum.plugin.__run_format()end))
   if data.op=='import' then
     assert(plum.has('notes.edit') and plum.has('file.read_user_choice') and not plum.has('file.write_user_choice'))
     assert(not pcall(function()plum.files.open()end))
     assert(data.text=='encoded input')
   else
     assert(not plum.has('notes.edit') and not plum.has('file.read_user_choice') and plum.has('file.write_user_choice'))
     assert(not pcall(function()plum.files.save()end))
   end
   return {tick=data.tick,gate=31,key=data.keys==256 and 200 or 60,velocity=93,seed=ctx.seed,text=string.format('%d/%d',data.tick,data.keys)}
 end}
end
"#;
fn install(r: &mut Rig, provider: &str, declared: bool, call_granted: bool) {
    let caps = Capability::ALL
        .into_iter()
        .map(Capability::name)
        .collect::<Vec<_>>();
    r.install(PROVIDER, &caps, &[], provider);
    r.install(
        CODEC,
        &caps,
        if declared {
            &["test.codec-provider/codec@1.0"]
        } else {
            &[]
        },
        CODEC_SOURCE,
    );
    if !call_granted {
        r.manager
            .set_granted(CODEC, "services.call", false, &mut r.prefs);
        r.settle();
    }
}
fn source(r: &mut Rig, keys: u16) -> (Arc<PlumDocument>, History) {
    let mut doc = PlumDocument::new_project("Format service composition".into(), Bpm::DEFAULT, 960);
    if keys == 256 {
        doc.project_mut().keys = plumspirit_engine::model::project::KeyRange::Extended256;
    }
    let doc = Arc::new(doc);
    let mut history = History::new(HistoryLimits::default());
    history.observe_ids(&doc);
    r.manager.project_opened(world(&doc, &mut history));
    r.settle();
    (doc, history)
}
fn world(doc: &Arc<PlumDocument>, history: &mut History) -> World {
    history.observe_ids(doc);
    World::new(Arc::clone(doc), history.incarnations().snapshot()).with_info(
        crate::lua::handles::ProjectInfo {
            revision: history.current_state(),
            ..Default::default()
        },
    )
}
fn pump(
    r: &mut Rig,
    doc: &Arc<PlumDocument>,
    history: &mut History,
    path: &Path,
) -> Vec<PluginEvent> {
    pump_with(r, doc, history, path, |_, _, _| {})
}
fn pump_with(
    r: &mut Rig,
    doc: &Arc<PlumDocument>,
    history: &mut History,
    path: &Path,
    mut hook: impl FnMut(&mut Rig, &Arc<PlumDocument>, &mut History),
) -> Vec<PluginEvent> {
    let until = Instant::now() + Duration::from_secs(5);
    let mut events = Vec::new();
    let mut choices = 0;
    loop {
        for event in r.manager.poll(&r.prefs) {
            if matches!(event, PluginEvent::NeedWorld) {
                r.manager.supply_world(world(doc, history));
            }
            events.push(event);
        }
        hook(r, doc, history);
        if let Some(request) = r.manager.poll_ui_request() {
            choices += 1;
            assert_eq!(choices, 1, "only the codec can choose a file");
            let stamp = request.lease.project;
            request.respond(stamp, Some(path.to_path_buf())).unwrap();
        }
        assert!(
            r.manager.poll_midi_input_request().is_none(),
            "services cannot request devices"
        );
        if !r.manager.is_busy() {
            events.extend(r.manager.poll(&r.prefs));
            assert_eq!(choices, 1);
            return events;
        }
        assert!(
            Instant::now() < until,
            "format/service timeout: {:?}",
            r.log(CODEC)
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn waiting_format_service_timeout_provider_revoke_reload_caller_cancel_and_revision_never_publish_staging()
 {
    const BLOCKED: &str = "function register(p)p.service{id='codec',version='1.2',run=function()plum.progress(0,'format-provider-running');while true do plum.yield()end end}end";
    for mode in [Mode::Import, Mode::Export] {
        for action in 0..6 {
            let mut r = Rig::new();
            install(&mut r, BLOCKED, true, true);
            let (doc, mut history) = source(&mut r, 128);
            let before = doc.export_fingerprint();
            let path = r.root.join("wire.pnotes");
            std::fs::write(&path, b"encoded input").unwrap();
            r.manager
                .run_format(CODEC, "wire", mode, Some(world(&doc, &mut history)))
                .unwrap();
            let mut changed = false;
            let events = pump_with(&mut r, &doc, &mut history, &path, |r, doc, history| {
                if action == 0
                    || changed
                    || !r.manager.plugin(PROVIDER).is_some_and(|p| {
                        p.progress()
                            .is_some_and(|p| p.message == "format-provider-running")
                    })
                {
                    return;
                }
                changed = true;
                match action {
                    1 => r
                        .manager
                        .set_granted(PROVIDER, "services.provide", false, &mut r.prefs),
                    2 => r.manager.reload(PROVIDER, &r.prefs),
                    3 => r.manager.cancel(CODEC),
                    4 => {
                        let w = world(doc, history);
                        r.manager
                            .observation_context(Some(crate::lua::bridge::ProjectStamp {
                                serial: w.project_serial(),
                                revision: w.info.revision + 1,
                            }));
                    }
                    _ => r
                        .manager
                        .set_granted(CODEC, "services.call", false, &mut r.prefs),
                }
            });
            if action != 0 {
                assert!(
                    changed,
                    "intervention must occur while provider is active: {mode:?}/{action}"
                );
            }
            assert!(
                !events.iter().any(|e| matches!(
                    e,
                    PluginEvent::Commits { .. }
                        | PluginEvent::ProjectData { .. }
                        | PluginEvent::Select { .. }
                        | PluginEvent::FormatResult { result: Ok(_), .. }
                )),
                "no successful result after revoked composition"
            );
            assert_eq!(std::fs::read(&path).unwrap(), b"encoded input");
            assert_eq!(doc.export_fingerprint(), before);
            assert!(!history.can_undo());
            assert_eq!(
                Arc::strong_count(&doc),
                1,
                "native caller/provider must drop source snapshots"
            );
            // Reset the native context and demonstrate the surviving codec worker
            // can run an ordinary command after the failed native format scope.
            r.manager
                .observation_context(Some(crate::lua::bridge::ProjectStamp {
                    serial: world(&doc, &mut history).project_serial(),
                    revision: history.current_state(),
                }));
            if r.manager
                .plugin(CODEC)
                .is_some_and(|p| p.status == Status::Ready)
            {
                r.manager
                    .run_command(CODEC, "base", Some(world(&doc, &mut history)))
                    .unwrap();
                r.settle();
                assert!(
                    r.log(CODEC)
                        .iter()
                        .any(|s| s.contains("CODEC_BASE_AUTHORITY_RESTORED"))
                );
            }
        }
    }
}
fn outcome(events: Vec<PluginEvent>, mode: Mode) -> Result<Option<Commits>, String> {
    assert!(
        !events.iter().any(|e| matches!(
            e,
            PluginEvent::Commits { .. }
                | PluginEvent::ProjectData { .. }
                | PluginEvent::Select { .. }
        )),
        "no automatic side effects from pure service/format"
    );
    events
        .into_iter()
        .find_map(|e| match e {
            PluginEvent::FormatResult {
                plugin,
                mode: m,
                selected,
                result,
                ..
            } if plugin == CODEC => {
                assert_eq!(m, mode);
                assert_eq!(selected,result.is_ok(),"selected reports successful native completion, not the earlier chooser decision");
                Some(result)
            }
            _ => None,
        })
        .expect("native format result")
}
#[test]
fn installed_format_and_pure_provider_import_preview_apply_undo_redo_and_export_keep_narrow_authority()
 {
    for keys in [128, 256] {
        let mut r = Rig::new();
        install(&mut r, PROVIDER_SOURCE, true, true);
        let (mut doc, mut history) = source(&mut r, keys);
        let path = r.root.join("wire.pnotes");
        std::fs::write(&path, b"encoded input").unwrap();
        let before = doc.export_fingerprint();
        r.manager
            .run_format(CODEC, "wire", Mode::Import, Some(world(&doc, &mut history)))
            .unwrap();
        let preview = outcome(pump(&mut r, &doc, &mut history, &path), Mode::Import)
            .unwrap()
            .unwrap();
        assert_eq!(doc.export_fingerprint(), before);
        assert_eq!(preview.result().note_count(), 2);
        let n = preview
            .result()
            .track(0)
            .unwrap()
            .notes
            .iter()
            .nth(1)
            .unwrap();
        assert_eq!(n.start_tick, 9_007_199_254_741_001);
        assert_eq!(n.attrs.pitch(), if keys == 256 { 200 } else { 60 });
        assert_eq!(n.attrs.velocity(), 93);
        let expected = preview.result().export_fingerprint();
        let summary = preview
            .format_review()
            .expect("format worker delivers bounded native review");
        assert_eq!(
            (summary.before.track_notes, summary.after.track_notes),
            (0, 2)
        );
        assert_eq!(summary.changes.notes, 1);
        drop(preview);
        r.manager
            .run_format(CODEC, "wire", Mode::Import, Some(world(&doc, &mut history)))
            .unwrap();
        let mut apply = outcome(pump(&mut r, &doc, &mut history, &path), Mode::Import)
            .unwrap()
            .unwrap();
        assert_eq!(apply.result().export_fingerprint(), expected);
        assert_eq!(apply.steps.len(), 1);
        apply.apply(&mut history, &mut doc).unwrap();
        drop(apply);
        history
            .undo(Arc::get_mut(&mut doc).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(doc.export_fingerprint(), before);
        history
            .redo(Arc::get_mut(&mut doc).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(doc.export_fingerprint(), expected);
        r.manager.project_opened(world(&doc, &mut history));
        r.settle();
        std::fs::write(&path, b"old destination").unwrap();
        r.manager
            .run_format(CODEC, "wire", Mode::Export, Some(world(&doc, &mut history)))
            .unwrap();
        assert!(
            outcome(pump(&mut r, &doc, &mut history, &path), Mode::Export)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            format!("WIRE\n9007199254741001/{keys}").as_bytes()
        );
        assert_eq!(doc.export_fingerprint(), expected);
        r.manager
            .run_command(CODEC, "base", Some(world(&doc, &mut history)))
            .unwrap();
        r.settle();
        assert!(
            r.log(CODEC)
                .iter()
                .any(|s| s.contains("CODEC_BASE_AUTHORITY_RESTORED"))
        );
        assert_eq!(Arc::strong_count(&doc), 1);
    }
}
#[test]
fn missing_service_dependency_or_grant_and_provider_failure_discard_export_prefix_and_import_transaction()
 {
    for scenario in 0..3 {
        let mut r = Rig::new();
        install(
            &mut r,
            if scenario == 2 {
                "function register(p)p.service{id='codec',version='1.2',run=function()error('intentional codec provider failure')end}end"
            } else {
                PROVIDER_SOURCE
            },
            scenario != 0,
            scenario != 1,
        );
        let (doc, mut history) = source(&mut r, 128);
        let before = doc.export_fingerprint();
        let path = r.root.join("wire.pnotes");
        for mode in [Mode::Import, Mode::Export] {
            std::fs::write(&path, b"encoded input").unwrap();
            r.manager
                .run_format(CODEC, "wire", mode, Some(world(&doc, &mut history)))
                .unwrap();
            assert!(outcome(pump(&mut r, &doc, &mut history, &path), mode).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), b"encoded input");
            assert_eq!(doc.export_fingerprint(), before);
            assert!(!history.can_undo());
            assert_eq!(Arc::strong_count(&doc), 1);
        }
    }
}

#[test]
fn opaque_executable_or_metatable_provider_replies_cannot_escape_into_format_import_or_export() {
    for body in [
        "return plum.text.builtin_font('proportional')",
        "return function()end",
        "return setmetatable({tick=1},{})",
    ] {
        let mut r = Rig::new();
        let provider = format!(
            "function register(p)p.service{{id='codec',version='1.2',run=function() {body} end}}end"
        );
        install(&mut r, &provider, true, true);
        let (doc, mut history) = source(&mut r, 128);
        let original = doc.export_fingerprint();
        let path = r.root.join("wire.pnotes");
        for mode in [Mode::Import, Mode::Export] {
            std::fs::write(&path, b"encoded input").unwrap();
            r.manager
                .run_format(CODEC, "wire", mode, Some(world(&doc, &mut history)))
                .unwrap();
            assert!(outcome(pump(&mut r, &doc, &mut history, &path), mode).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), b"encoded input");
            assert_eq!(doc.export_fingerprint(), original);
            assert_eq!(Arc::strong_count(&doc), 1);
        }
    }
}

#[test]
fn format_call_tree_keeps_the_existing_sixty_four_call_budget_and_discards_accepted_staging() {
    const ECHO_PROVIDER: &str = "function register(p)p.service{id='codec',version='1.2',run=function(data)return data end}end";
    const CLIENT: &str = r#"
      local function calls(ctx)
        if ctx.mode=='import' then ctx.tx:insert_notes(ctx.project:track(1),{{start=1,gate=1,key=1,velocity=1}})
        else ctx.file:write('PRIVATE PREFIX')end
        for i=1,65 do
          local out=plum.services.call('test.codec-provider/codec',{index=i})
          assert(out.index==i)
        end
      end
      function register(p)p.format{id='wire',name='Wire Codec',extensions={'pnotes'},import=calls,export=calls}end
    "#;
    let caps = Capability::ALL
        .into_iter()
        .map(Capability::name)
        .collect::<Vec<_>>();
    let mut r = Rig::new();
    r.install(PROVIDER, &caps, &[], ECHO_PROVIDER);
    r.install(CODEC, &caps, &["test.codec-provider/codec@1.0"], CLIENT);
    let (doc, mut history) = source(&mut r, 128);
    let original = doc.export_fingerprint();
    let path = r.root.join("wire.pnotes");
    for mode in [Mode::Import, Mode::Export] {
        std::fs::write(&path, b"encoded input").unwrap();
        r.manager
            .run_format(CODEC, "wire", mode, Some(world(&doc, &mut history)))
            .unwrap();
        let error = outcome(pump(&mut r, &doc, &mut history, &path), mode)
            .err()
            .expect("root call budget");
        assert!(error.contains("64 service calls"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), b"encoded input");
        assert_eq!(doc.export_fingerprint(), original);
        assert!(!history.can_undo());
        assert_eq!(Arc::strong_count(&doc), 1);
    }
}
