use super::*;
use crate::lua::{Host, Limits};
use mlua::AnyUserData;
use std::rc::Rc;
const SPEC: &str = "{title='Fixture',content={id='root',kind='column',children={{id='name',kind='text',label='Name',value='original'},{id='toggle',kind='toggle',value=true},{id='canvas',kind='canvas',primitives={{kind='line',x1=0,y1=0,x2=1,y2=1}}}}}}";
pub(super) fn host(grant: bool, interactive: bool) -> (Host, Receiver<Arc<Shared>>) {
    let h = Host::new(
        Limits {
            memory: 16 * 1024 * 1024,
            ..Default::default()
        },
        if grant {
            vec![Capability::UiPanels]
        } else {
            vec![]
        },
    )
    .unwrap();
    h.lua.set_app_data(NativeBudget::isolated(32 * 1024 * 1024));
    let (client, rx) = channel("script");
    h.lua.app_data_mut::<Slot>().unwrap().client = Some(client);
    let (file, _) = super::super::bridge::channel("script");
    let mut s = h.lua.app_data_mut::<Session>().unwrap();
    s.client = Some(file);
    s.interactive = interactive;
    drop(s);
    (h, rx)
}
pub(super) fn run(h: &Host, s: &str) {
    h.run("ui.lua", s).unwrap_or_else(|e| panic!("{e}"));
}
pub(super) fn clear(h: &Host) {
    h.lua.gc_collect().unwrap();
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}
pub(super) fn driver(
    h: &Host,
    rx: Receiver<Arc<Shared>>,
    f: impl Fn(&Arc<Shared>) -> mlua::Result<()> + 'static,
) {
    let rx = Rc::new(rx);
    h.lua
        .globals()
        .set(
            "drive",
            h.lua
                .create_function(move |_, p: AnyUserData| {
                    let p = p.borrow::<Panel>()?;
                    let queued = rx.try_recv().unwrap();
                    assert!(Arc::ptr_eq(&queued, &p.0));
                    f(&p.0)
                })
                .unwrap(),
        )
        .unwrap();
}
fn event(kind: &'static str, id: &str) -> Event {
    Event {
        kind,
        id: id.into(),
        value: InputValue::None,
        pointer: None,
        revision: 1,
        sequence: 0,
    }
}
#[test]
fn panel_permission_user_invocation_and_plain_schema_limits_fail_without_effects() {
    let (h, _) = host(false, true);
    assert_eq!(
        h.run("denied.lua", &format!("plum.ui.open_panel({SPEC})"))
            .unwrap_err()
            .category,
        Category::Capability
    );
    clear(&h);
    let (h, _) = host(true, false);
    assert_eq!(
        h.run("lifecycle.lua", &format!("plum.ui.open_panel({SPEC})"))
            .unwrap_err()
            .category,
        Category::Capability
    );
    clear(&h);
    let (h, _) = host(true, true);
    h.lua
        .globals()
        .set(
            "validate",
            h.lua
                .create_function(|lua, t: Table| schema::parse(lua, t).map(|_| true))
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        r#"
      local function fail(t)assert(not pcall(validate,t))end
      fail(setmetatable({content={id='x',kind='label'}},{}))
      fail{extra=1,content={id='x',kind='label'}}
      fail{width=279,content={id='x',kind='label'}}
      fail{content={id='x',kind='column',children={{id='x',kind='label'}}}}
      fail{content={id='bad id',kind='label'}}
      fail{content={id='x',kind='slider',min=0,max=1,value=math.huge}}
      fail{content={id='x',kind='choice',choices={}}}
      fail{content={id='x',kind='choice',choices={'one'},value=1.0}}
      fail{content={id='x',kind='canvas',primitives={{kind='circle',x=0,y=0,radius=33}}}}
      fail{content={id='x',kind='canvas',primitives={{kind='line',x1=0,y1=0,x2=math.huge,y2=0}}}}
      fail{content={id='x',kind='canvas',primitives={{kind='line',x1=0,y1=0,x2=1,y2=1,color={1,2,3,4.0}}}}}
      local deep={id='d1',kind='column',children={}};local p=deep;for i=2,9 do local c={id='d'..i,kind='column',children={}};p.children={c};p=c end;fail{content=deep}
      local children={};for i=1,256 do children[i]={id='n'..i,kind='label'}end;fail{content={id='root',kind='column',children=children}}
      local prim={};for i=1,129 do prim[i]={kind='circle',x=0,y=0}end;fail{content={id='x',kind='canvas',primitives=prim}}
      local many={};for i=1,17 do many[i]={id='t'..i,kind='text',value=string.rep('x',4096)}end;fail{content={id='root',kind='column',children=many}}
      assert(validate{content={id='root',kind='column',children={{id='number',kind='number',value=-2,min=-10,max=10},{id='choice',kind='choice',choices={'one','two'},value=2}}}})
    "#,
    );
    clear(&h);
}
#[test]
fn typed_events_coalesce_without_crossing_gesture_boundaries_and_close_is_accounted() {
    let (h, rx) = host(true, true);
    driver(&h, rx, |s| {
        assert!(s.valid(None));
        assert_eq!(s.revision(), 1);
        let snapshot = s.snapshot().unwrap();
        assert_eq!(snapshot.title, "Fixture");
        for i in 0..1000 {
            let mut e = event("change", "name");
            e.value = InputValue::Text(i.to_string());
            assert!(s.push(e));
        }
        assert!(s.push(event("pointer_down", "canvas")));
        for _ in 0..100 {
            assert!(s.push(event("pointer_move", "canvas")));
        }
        assert!(s.push(event("pointer_up", "canvas")));
        Ok(())
    });
    run(
        &h,
        &format!(
            "local spec={SPEC};local p=plum.ui.open_panel(spec);spec.title='changed';drive(p);local i=p:info();assert(i.queued==4 and i.coalesced==1098);local a=p:next_event(0);assert(a.kind=='change' and a.value=='999' and a.sequence==1000);assert(p:next_event().kind=='pointer_down');assert(p:next_event().kind=='pointer_move');assert(p:next_event().kind=='pointer_up');assert(p:next_event(0)==nil);assert(not pcall(function()p:next_event(0.0)end));assert(p:update({{title='Updated',content={{id='root',kind='label',label='done'}}}})==2);assert(p:info().revision==2);assert(p:close() and not p:close());assert(p:next_event(0)==nil)"
        ),
    );
    clear(&h);
}
#[test]
fn bounded_queue_overflow_survives_pcall_and_drops_the_entire_job() {
    let (h, rx) = host(true, true);
    driver(&h, rx, |s| {
        for i in 0..65 {
            let e = event("activate", if i % 2 == 0 { "a" } else { "b" });
            let ok = s.push(e);
            assert_eq!(ok, i < 64);
        }
        Ok(())
    });
    assert_eq!(h.run("overflow.lua",&format!("local p=plum.ui.open_panel({SPEC});drive(p);pcall(function()p:next_event()end);return 'caught'")).unwrap_err().category,Category::Budget);
    clear(&h);
}
#[test]
fn surface_cancel_cannot_be_caught_to_publish_previously_staged_commits() {
    use plumspirit_engine::{
        history::{History, HistoryLimits},
        model::{Bpm, PlumDocument},
    };
    let (h, rx) = host(true, true);
    let caps = Arc::new(
        [
            Capability::UiPanels,
            Capability::ReadProject,
            Capability::EditNotes,
        ]
        .into_iter()
        .collect(),
    );
    h.lua.set_app_data(super::super::handles::Granted(caps));
    let document = Arc::new(PlumDocument::new_project(
        "Cancel".into(),
        Bpm::DEFAULT,
        960,
    ));
    let mut history = History::new(HistoryLimits::default());
    history.observe_ids(&document);
    h.set_world(Some(super::super::World::new(
        Arc::clone(&document),
        history.incarnations().snapshot(),
    )));
    driver(&h, rx, |s| {
        s.fail(CANCELED);
        Ok(())
    });
    let script = format!(
        "local tx=plum.transaction{{label='Pending'}};tx:insert_notes(plum.project.current():track(1),{{{{start=0,gate=120,key=60}}}});tx:commit();local p=plum.ui.open_panel({SPEC});drive(p);assert(not pcall(function()p:next_event()end));return true"
    );
    assert_eq!(
        h.run("cancel-commit.lua", &script).unwrap_err().category,
        Category::Canceled
    );
    assert!(h.take_commits().is_none());
    assert_eq!(document.note_count(), 0);
    clear(&h);
}
#[test]
fn worker_wait_wakes_with_typed_data_then_eagerly_expires_retained_panels() {
    let (h, rx) = host(true, true);
    let handle = std::thread::spawn(move || {
        let s = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut e = event("change", "value");
        e.value = InputValue::Number(-3.25);
        assert!(s.push(e));
    });
    run(
        &h,
        &format!(
            "saved=plum.ui.open_panel({SPEC});local e=saved:next_event(2000);assert(e.kind=='change' and e.value==-3.25 and e.revision==1);assert(saved:info().queued==0)"
        ),
    );
    handle.join().unwrap();
    h.lua.gc_collect().unwrap();
    assert_eq!(
        h.lua.app_data_ref::<NativeBudget>().unwrap().used(),
        HANDLE_BYTES
    );
    run(
        &h,
        "assert(not pcall(function()saved:info()end));assert(not pcall(function()saved:update{content={id='x',kind='label'}}end));assert(not saved:close());saved=nil",
    );
    clear(&h);
}
#[test]
fn programmatic_close_allows_apply_and_native_quota_failure_does_not_open_a_surface() {
    let (h, rx) = host(true, true);
    driver(&h, rx, |_| Ok(()));
    run(
        &h,
        &format!("local p=plum.ui.open_panel({SPEC});drive(p);p:close()"),
    );
    clear(&h);
    h.lua.set_app_data(NativeBudget::isolated(4096));
    h.lua.app_data_mut::<Session>().unwrap().interactive = true;
    assert_eq!(
        h.run("quota.lua", &format!("plum.ui.open_panel({SPEC})"))
            .unwrap_err()
            .category,
        Category::Budget
    );
    clear(&h);
}

pub(crate) fn native_panel(
    spec: &str,
    body: impl Fn(&Arc<Shared>) -> mlua::Result<()> + 'static,
) -> Result<crate::lua::Plain, crate::lua::ScriptError> {
    let (h, rx) = host(true, true);
    driver(&h, rx, body);
    let result = h.run(
        "native-egui.lua",
        &format!("local p=plum.ui.open_panel({spec});drive(p);p:close()"),
    );
    h.lua.gc_collect().unwrap();
    result
}
#[test]
fn tabs_and_lists_validate_plain_shapes_values_and_total_option_memory() {
    let (h, _) = host(true, true);
    h.lua
        .globals()
        .set(
            "validate",
            h.lua
                .create_function(|lua, t: Table| schema::parse(lua, t).map(|_| true))
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        r#"
      local function spec(node)return{content=node}end
      local function group(id,label)return{id=id,kind='group',label=label,children={}}end
      assert(validate(spec{id='tabs',kind='tabs',value=2,children={group('a','One'),group('b','Two')}}))
      local choices={};for i=1,512 do choices[i]=''end
      local lists={};for i=1,64 do lists[i]={id='l'..i,kind='list',choices=choices,height=64,value=512}end
      assert(validate(spec{id='root',kind='column',children=lists}))
      lists[65]={id='overflow',kind='list',choices=choices}
      assert(not pcall(validate,spec{id='root',kind='column',children=lists}))
      for _,node in ipairs{
        {id='t',kind='tabs',children={}},
        {id='t',kind='tabs',children={{id='button',kind='button',label='Bad'}}},
        {id='t',kind='tabs',children={group('a','')}},
        {id='t',kind='tabs',children={group('a',string.rep('x',257))}},
        {id='t',kind='tabs',value=2,children={group('a','One')}},
        {id='t',kind='tabs',value=1.0,children={group('a','One')}},
        {id='l',kind='list',choices={}},
        {id='l',kind='list',choices={'One'},value=0},
        {id='l',kind='list',choices={'One'},height=63},
        {id='l',kind='list',choices={'One'},height=math.huge},
        {id='l',kind='list',choices=setmetatable({'One'}, {})},
      }do assert(not pcall(validate,spec(node)))end
      choices[513]='extra';assert(not pcall(validate,spec{id='l',kind='list',choices=choices}))
      local tabs={};for i=1,16 do tabs[i]=group('g'..i,'Tab '..i)end
      assert(validate(spec{id='t',kind='tabs',children=tabs,value=16}))
      tabs[17]=group('g17','Too many');assert(not pcall(validate,spec{id='t',kind='tabs',children=tabs}))
      -- The former maximum possible choice count remains accepted.
      local old={};for i=1,128 do old[i]=''end
      local children={};for i=1,255 do children[i]={id='c'..i,kind='choice',choices=old}end
      assert(validate(spec{id='root',kind='column',children=children}))
    "#,
    );
    clear(&h);
}

#[test]
fn list_event_boundary_delivers_512_to_lua_and_rejects_513() {
    let (h, rx) = host(true, true);
    driver(&h, rx, |s| {
        for n in [127, 128, 511] {
            let mut e = event("change", "data");
            e.value = InputValue::Choice(n);
            assert!(s.push(e));
        }
        Ok(())
    });
    run(
        &h,
        &format!(
            "local p=plum.ui.open_panel({SPEC});drive(p);local e=p:next_event(0);assert(e.value==512 and e.sequence==3);assert(p:info().queued==0);p:close()"
        ),
    );
    clear(&h);
    let (h, rx) = host(true, true);
    driver(&h, rx, |s| {
        let mut e = event("change", "data");
        e.value = InputValue::Choice(512);
        assert!(!s.push(e));
        assert!(s.closed());
        Ok(())
    });
    assert_eq!(
        h.run(
            "bad-list-event.lua",
            &format!("local p=plum.ui.open_panel({SPEC});drive(p);p:next_event(0)")
        )
        .unwrap_err()
        .category,
        Category::Budget
    );
    clear(&h);
}
