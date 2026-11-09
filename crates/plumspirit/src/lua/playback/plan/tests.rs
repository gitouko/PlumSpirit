use super::*;
use crate::lua::{Host, Limits, Plain};
use std::time::Duration;

fn host() -> Host {
    Host::new(
        Limits {
            time: Duration::from_secs(10),
            ..Limits::default()
        },
        vec![],
    )
    .unwrap()
}
#[test]
fn published_offline_plan_example_executes_verbatim() {
    let docs = include_str!("../../../../../../docs/LUA_API.md");
    let section = docs
        .split("## Bounded MIDI event plans (API 0.2)")
        .nth(1)
        .unwrap();
    let example = section
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    host().run("published-plan.lua", example).unwrap();
}
#[test]
fn packed_plans_preserve_exact_ticks_all_voice_messages_and_stable_equal_tick_order() {
    let host = host();
    assert_eq!(
        host.run(
            "plan.lua",
            r#"
        local w=plum.playback.plan_writer{max_events=16}
        local tick=9007199254740993
        local columns={tick={},status={},data1={},data2={}}
        for i,s in ipairs{0x80,0x91,0xa2,0xb3,0xc4,0xd5,0xe6,0x9f}do
            columns.tick[i]=tick;columns.status[i]=s;columns.data1[i]=i
            columns.data2[i]=(s==0xc4 or s==0xd5)and 0 or 127
        end
        assert(w:append(columns)==8)
        columns.tick[1]=0 -- appended records are native immutable copies
        local p=w:finish();assert(p:info().count==8 and p:info().finished)
        assert(p:info().first_tick==tick and math.type(p:info().last_tick)=='integer')
        local first=p:read{limit=3};assert(first.offset==0 and first.next_offset==3)
        assert(first.total==8 and first.count==3 and first.status[2]==0x91)
        for i=1,3 do assert(first.tick[i]==tick and first.data1[i]==i)end
        first.status[1]=0;assert(p:read{limit=1}.status[1]==0x80)
        local last=p:read{offset=3,limit=4096};assert(last.count==5 and last.next_offset==nil)
        assert(last.status[1]==0xb3 and last.status[5]==0x9f)
        assert(p:read{offset=8}.count==0)
        assert(not pcall(function()w:finish()end))
        assert(not pcall(function()w:cancel()end))
        assert(not pcall(function()w:append(columns)end))
        p:close();p:close();assert(not pcall(function()p:info()end))
        return 'exact'
    "#
        )
        .unwrap(),
        Plain::String("exact".into())
    );
    assert!(host.take_commits().is_none());
}
#[test]
fn strict_inputs_invalid_bytes_modes_order_and_holes_leave_previous_batch_unchanged() {
    host().run("invalid.lua", r#"
        for _,o in ipairs{{},{max_events=0},{max_events=262145},{max_events=1.0},
            {max_events=2,extra=true},setmetatable({max_events=2},{})}do
            assert(not pcall(function()plum.playback.plan_writer(o)end))
        end
        local w=plum.playback.plan_writer{max_events=16}
        local function row(t,s,a,b)return{tick={t},status={s},data1={a},data2={b}}end
        assert(w:append(row('9007199254740993',0x90,60,100))==1)
        for _,c in ipairs{
          row(0,0x90,60,1),row(1/0,0x90,60,1),row(-1,0x90,60,1),
          row(9007199254740993.0,0x90,60,1),row('9223372036854775808',0x90,60,1),
          row('9007199254740993',0x7f,60,1),row('9007199254740993',0xf0,60,1),
          row('9007199254740993',0xff,60,1),row('9007199254740993',0x90,128,1),
          row('9007199254740993',0x90,60,-1),row('9007199254740993',0xc0,60,1),
          row('9007199254740993',0xd0,60,1),row('9007199254740993',0xb0,120,0),
          row('9007199254740993',0xbf,127,0),row('9007199254740993',0x90,60,0.0),
          {tick={9007199254740993},status={0x90},data1={60}},
          {tick={9007199254740993},status={0x90,0x90},data1={60},data2={1}},
          {tick={9007199254740993},status={[1]=0x90,extra=1},data1={60},data2={1}},
          {tick={9007199254740993},status=setmetatable({0x90},{}),data1={60},data2={1}},
          setmetatable(row('9007199254740993',0x90,60,1),{}),
          {tick={9007199254740993},status={0x90},data1={60},data2={1},extra=true},
          {tick={9007199254740993,9007199254740993},status={0x90,0xf0},data1={60,60},data2={1,1}},
          {tick={9007199254740993,9007199254740993,9007199254740993},status={[1]=0x90,[3]=0x90},data1={60,60,60},data2={1,1,1}}
        }do
            assert(not pcall(function()w:append(c)end))
            assert(w:info().count==1)
        end
        assert(w:append(row('9007199254740994',0x80,60,0))==2)
        local p=w:finish()
        for _,o in ipairs{{offset=-1},{offset=3},{offset=0.0},{limit=0},{limit=4097},
            {extra=true},setmetatable({},{})}do assert(not pcall(function()p:read(o)end))end
        assert(p:read().status[2]==0x80)
    "#).unwrap();
}
#[test]
fn resource_and_reserved_event_quotas_survive_cancel_close_and_garbage_collection() {
    let host = host();
    host.run(
        "limits.lua",
        r#"
        for i=1,4 do local w=plum.playback.plan_writer{max_events=262144};w:cancel()end
        assert(not pcall(function()plum.playback.plan_writer{max_events=1}end))
    "#,
    )
    .unwrap();
    host.run(
        "resources.lua",
        r#"
        for i=1,32 do local w=plum.playback.plan_writer{max_events=1};w:finish():close()end
        assert(not pcall(function()plum.playback.plan_writer{max_events=1}end))
    "#,
    )
    .unwrap();
    host.run(
        "batch.lua",
        r#"
        local w=plum.playback.plan_writer{max_events=1}
        local c={tick={},status={},data1={},data2={}}
        assert(not pcall(function()w:append(c)end))
        for i=1,4097 do c.tick[i]=i;c.status[i]=0x90;c.data1[i]=60;c.data2[i]=1 end
        assert(not pcall(function()w:append(c)end));assert(w:info().count==0)
        c={tick={1,2},status={0x90,0x80},data1={60,60},data2={1,0}}
        assert(not pcall(function()w:append(c)end));assert(w:info().count==0)
        w:cancel();w:cancel();assert(not pcall(function()w:finish()end))
    "#,
    )
    .unwrap();
}
#[test]
fn native_preflight_eager_scope_cleanup_retained_metadata_and_failed_jobs() {
    let host = host();
    let budget = NativeBudget::isolated(32 * 1024);
    host.lua.set_app_data(budget.clone());
    host.run(
        "retained.lua",
        r#"
        held_writer=plum.playback.plan_writer{max_events=1024}
        held_writer:append{tick={0},status={0x90},data1={60},data2={1}}
        held_plan=held_writer:finish()
    "#,
    )
    .unwrap();
    assert_eq!(budget.used(), METADATA_BYTES);
    host.run(
        "expired.lua",
        r#"
        assert(not pcall(function()held_writer:info()end))
        assert(not pcall(function()held_plan:read()end))
        held_writer=nil;held_plan=nil;collectgarbage('collect')
    "#,
    )
    .unwrap();
    assert_eq!(budget.used(), 0);
    assert!(
        host.run(
            "failed.lua",
            "failed=plum.playback.plan_writer{max_events=1024};error('fail')"
        )
        .is_err()
    );
    assert_eq!(budget.used(), METADATA_BYTES);
    host.run("clean.lua", "failed=nil;collectgarbage('collect')")
        .unwrap();
    assert_eq!(budget.used(), 0);
    let tiny = NativeBudget::isolated(METADATA_BYTES + 15);
    host.lua.set_app_data(tiny.clone());
    assert_eq!(
        host.run("denied.lua", "plum.playback.plan_writer{max_events=1}")
            .unwrap_err()
            .category,
        Category::Budget
    );
    assert_eq!(tiny.used(), 0);
}
#[test]
fn native_scratch_denial_is_atomic_and_cancellation_is_checked_before_access() {
    let host = host();
    let budget = NativeBudget::isolated(METADATA_BYTES + size_of::<Event>());
    host.lua.set_app_data(budget.clone());
    host.run(
        "scratch.lua",
        r#"
        local w=plum.playback.plan_writer{max_events=1}
        assert(not pcall(function()w:append{tick={0},status={0x90},data1={60},data2={1}}end))
        assert(w:info().count==0)
    "#,
    )
    .unwrap();
    assert!(budget.used() <= METADATA_BYTES);
    // Capture the live handle in a native test hook, cancel its host lease, then access it.
    host.lua.set_app_data(NativeBudget::isolated(4096));
    host.lua
        .globals()
        .set(
            "cancel_plan",
            host.lua
                .create_function(|_, w: mlua::AnyUserData| {
                    w.borrow::<Writer>()?.0.lease.cancel_from_surface();
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    assert_eq!(
        host.run(
            "cancel.lua",
            r#"
        held=plum.playback.plan_writer{max_events=4};cancel_plan(held);held:finish()
    "#
        )
        .unwrap_err()
        .category,
        Category::Canceled
    );
    let writer: mlua::AnyUserData = host.lua.globals().get("held").unwrap();
    assert!(
        writer
            .borrow::<Writer>()
            .unwrap()
            .0
            .stored
            .borrow()
            .is_none()
    );
}
#[test]
fn quarter_million_events_stay_native_under_small_lua_heap_and_batch_independent_pages() {
    let host = Host::new(
        Limits {
            memory: 8 * 1024 * 1024,
            time: Duration::from_secs(15),
            ..Limits::default()
        },
        vec![],
    )
    .unwrap();
    host.run(
        "large.lua",
        r#"
        local function make(n,batch)
            local w=plum.playback.plan_writer{max_events=n}
            for base=0,n-1,batch do
                local c={tick={},status={},data1={},data2={}}
                for j=1,math.min(batch,n-base)do
                    local k=base+j-1;c.tick[j]=k//2;c.status[j]=k%2==0 and 0x90 or 0x80
                    c.data1[j]=k%128;c.data2[j]=k%2==0 and 100 or 0
                end
                w:append(c)
            end
            return w:finish()
        end
        local a,b=make(8192,1),make(8192,4096)
        for offset=0,8191,257 do
            local x,y=a:read{offset=offset,limit=257},b:read{offset=offset,limit=257}
            for _,col in ipairs{'tick','status','data1','data2'}do
                for i=1,x.count do assert(x[col][i]==y[col][i])end
            end
        end
        a:close();b:close()
        local p=make(262144,4096);assert(p:info().count==262144)
        for offset=0,262143,4096 do
            local c=p:read{offset=offset}
            for i=1,c.count do assert(c.tick[i]==(offset+i-1)//2)end
        end
        p:close()
    "#,
    )
    .unwrap();
}
