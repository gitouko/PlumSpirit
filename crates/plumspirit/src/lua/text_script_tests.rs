use super::*;
use crate::lua::{Host, Limits};
fn host() -> Host {
    let h = Host::new(
        Limits {
            memory: 8 * 1024 * 1024,
            ..Default::default()
        },
        [],
    )
    .unwrap();
    h.lua.set_app_data(NativeBudget::isolated(64 * 1024 * 1024));
    h
}
fn run(h: &Host, s: &str) {
    h.run("script-runs.lua", s).unwrap();
}
fn clear(h: &Host) {
    h.lua.gc_collect().unwrap();
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}
#[test]
fn independent_scripts_and_neutral_context_keep_exact_logical_original_ranges() {
    let h = host();
    run(
        &h,
        r#"
      local t=plum.text;assert(t.script_unicode_version=='17.0.0')
      local a=t.script_runs('aΩЖ');assert(a.count==3 and a.text=='aΩЖ')
      assert(a.first[1]==1 and a.last[1]==2 and a.script[1]=='Latn')
      assert(a.first[2]==2 and a.last[2]==4 and a.script[2]=='Grek')
      assert(a.first[3]==4 and a.last[3]==6 and a.script[3]=='Cyrl')
      for _,v in ipairs({{' abc Δ ',{' abc ','Δ '},{'Latn','Grek'}},
        {'אבالعربية',{'אב','العربية'},{'Hebr','Arab'}},
        {'नमस्ते漢글',{'नमस्ते','漢','글'},{'Deva','Hani','Hang'}},
        {'あーア',{'あー','ア'},{'Hira','Kana'}},
        {'アーあ',{'アー','あ'},{'Kana','Hira'}}})do
        local b=t.script_runs(v[1]);assert(b.count==#v[2]and b.text==v[1]);local pos=1
        for i=1,b.count do assert(b.first[i]==pos and b.last[i]==pos+#v[2][i] and b.script[i]==v[3][i]and b.resolved[i]);pos=b.last[i]end
        assert(pos==#v[1]+1)
      end
      local rtl=t.bidi_runs('אבج');assert(rtl.count==1 and rtl.direction[1]=='rtl')
      local scripts=t.script_runs(rtl.text);assert(scripts.count==2 and scripts.script[1]=='Hebr'and scripts.script[2]=='Arab')
      local visual={};for i=scripts.count,1,-1 do visual[#visual+1]=rtl.text:sub(scripts.first[i],scripts.last[i]-1)end
      assert(visual[1]=='ج'and visual[2]=='אב'and rtl.text=='אבج')
      local empty=t.script_runs('');assert(empty.text==''and empty.count==0)
    "#,
    );
    clear(&h);
}
#[test]
fn clusters_are_indivisible_and_ambiguous_unknown_and_common_are_explicit() {
    let h = host();
    run(
        &h,
        r#"
      local t=plum.text
      for _,v in ipairs({{'é','Latn',true},{'אבְ','Hebr',true},{'ー','Zzzz',false},
        {' 	😀👨‍👩‍👧‍👦👍🏽🇺🇸1️⃣','Zyyy',false},{utf8.char(0xe000),'Zzzz',false},
        {'aा','Zzzz',false},{utf8.char(0x301),'Zzzz',false},{utf8.char(0x200d),'Zinh',false}})do
        local a=t.script_runs(v[1]);assert(a.text==v[1]and a.count==1 and a.script[1]==v[2]and a.resolved[1]==v[3],v[1]..':'..tostring(a.script[1])..':'..tostring(a.resolved[1]));assert(a.first[1]==1 and a.last[1]==#v[1]+1)
      end
      for _,s in ipairs({'é éΩ','👨‍👩‍👧‍👦A👍🏽א🇺🇸','aाΩ','क्‍ष漢','a\r\nΩ'})do
        local valid={[#s+1]=true};for _,first,last in t.graphemes(s)do valid[first]=true;valid[last]=true end
        local a=t.script_runs(s);local pos=1
        for i=1,a.count do assert(a.first[i]==pos and valid[a.first[i]]and valid[a.last[i]]);pos=a.last[i]end
        assert(pos==#s+1 and a.text==s)
      end
    "#,
    );
    clear(&h);
}
#[test]
fn script_run_caps_strict_options_utf8_and_native_preflight_refuse_atomically() {
    let h = host();
    run(
        &h,
        r#"
      local t=plum.text;local s=string.rep('aΩ',2048);local a=t.script_runs(s)
      assert(a.count==4096 and a.text==s);assert(t.script_runs(string.rep('😀',4096)).count==1)
      for _,s in ipairs({string.rep('a',4097),string.rep('😀',4097),string.char(255),1,true,{}})do assert(not pcall(t.script_runs,s))end
      for _,o in ipairs({{max_runs=0},{max_runs=4097},{max_runs=1.0},{extra=1},setmetatable({},{})})do assert(not pcall(t.script_runs,'abc',o))end
      assert(not pcall(t.script_runs,'aΩ',{max_runs=1}));assert(t.script_runs('aΩ',{max_runs=2}).count==2)
    "#,
    );
    clear(&h);
    h.lua.set_app_data(NativeBudget::isolated(1024));
    assert_eq!(
        h.run("script-native-quota.lua", "plum.text.script_runs('aΩ')")
            .unwrap_err()
            .category,
        Category::Budget
    );
    clear(&h);
}
#[test]
fn deep_native_and_output_cancellation_release_workspace_and_allow_new_job() {
    for target in [80, 150] {
        let h = host();
        let cancel = h.cancel.clone();
        h.lua
            .globals()
            .set(
                "controlled_script",
                h.lua
                    .create_function(move |lua, value: Value| {
                        let mut calls = 0;
                        runs_controlled(lua, value, None, || {
                            calls += 1;
                            if calls == target {
                                cancel.store(true, std::sync::atomic::Ordering::Release);
                            }
                            notes::check_budget(lua)
                        })
                    })
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(
            h.run(
                "script-cancel.lua",
                "saved=string.rep('aΩ',2048);controlled_script(saved)"
            )
            .unwrap_err()
            .category,
            Category::Canceled
        );
        clear(&h);
        run(
            &h,
            "assert(#saved==6144 and plum.text.script_runs(saved).count==4096)",
        );
        clear(&h);
    }
}
#[test]
fn actual_vm_output_failure_keeps_source_and_retry_without_native_leak() {
    let h = host();
    h.lua
        .globals()
        .set(
            "limit_vm",
            h.lua
                .create_function(|lua, ()| {
                    lua.gc_stop();
                    lua.set_memory_limit(lua.used_memory() + 4096)?;
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    h.lua
        .globals()
        .set(
            "restore_vm",
            h.lua
                .create_function(|lua, ()| {
                    lua.set_memory_limit(8 * 1024 * 1024)?;
                    lua.gc_restart();
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        r#"local s=string.rep('aΩ',2048);limit_vm();local ok=pcall(plum.text.script_runs,s);restore_vm();assert(not ok and #s==6144);local a=plum.text.script_runs(s);assert(a.count==4096 and a.text==s)"#,
    );
    clear(&h);
}
#[test]
fn published_bidi_script_shape_composition_preserves_source_and_reports_scripts() {
    let h = host();
    let source = include_str!("../../../../docs/LUA_API.md")
        .split("### Script runs for mixed-script shaping (API 0.2)")
        .nth(1)
        .unwrap()
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    run(&h, source);
    run(
        &h,
        "assert(composed.text=='AΩЖ' and composed.count==3 and composed.script[1]=='Latn'and composed.script[2]=='Grek'and composed.script[3]=='Cyrl');assert(composed.first[1]==1 and composed.last[3]==6)",
    );
    clear(&h);
}
