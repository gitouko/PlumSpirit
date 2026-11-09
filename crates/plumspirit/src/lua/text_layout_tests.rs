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
    h.lua.set_app_data(NativeBudget::isolated(8 * 1024 * 1024));
    h
}
fn run(h: &Host, s: &str) {
    h.run("text-layout.lua", s).unwrap();
}
fn clear(h: &Host) {
    h.lua.gc_collect().unwrap();
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}
#[test]
fn known_bidi_visual_runs_keep_original_utf8_offsets_levels_and_direction() {
    let h = host();
    run(
        &h,
        r#"
      local t=plum.text;assert(t.bidi_unicode_version=='16.0.0'and t.linebreak_unicode_version=='15.0.0')
      local a=t.bidi_runs('abc אבג');assert(a.text=='abc אבג'and a.count==2 and a.line_count==1 and a.has_rtl)
      assert(a.first[1]==1 and a.last[1]==5 and a.level[1]==0 and a.direction[1]=='ltr')
      assert(a.first[2]==5 and a.last[2]==11 and a.level[2]==1 and a.direction[2]=='rtl')
      assert(a.lines[1].first==1 and a.lines[1].content_last==11 and a.lines[1].last==11 and a.lines[1].first_run==1 and a.lines[1].last_run==3)
      local b=t.bidi_runs('אבג abc');assert(b.count==2 and b.lines[1].level==1)
      assert(b.first[1]==8 and b.last[1]==11 and b.direction[1]=='ltr'and b.level[1]==2)
      assert(b.first[2]==1 and b.last[2]==8 and b.direction[2]=='rtl'and b.level[2]==1)
      local forced=t.bidi_runs('abc',{direction='rtl'});assert(forced.lines[1].level==1 and forced.has_rtl and forced.direction[1]=='ltr'and forced.level[1]==2)
      local num=t.bidi_runs('123',{direction='ltr'});assert(not num.has_rtl and num.direction[1]=='ltr')
      local source='a '..utf8.char(0x2067)..'אבג'..utf8.char(0x2069)..' z';local controls=t.bidi_runs(source)
      local seen={};for i=1,controls.count do for byte=controls.first[i],controls.last[i]-1 do assert(not seen[byte]);seen[byte]=true end end
      for i=1,#source do assert(seen[i])end;assert(controls.text==source and controls.has_rtl)
    "#,
    );
    clear(&h);
}
#[test]
fn hard_lines_preserve_crlf_empty_trailing_lines_and_original_graphemes() {
    let h = host();
    run(
        &h,
        r#"
      local t=plum.text;local source='\r\nA\n\nאב'..utf8.char(0x2028);local a=t.bidi_runs(source)
      assert(a.text==source and a.line_count==5 and a.count==2)
      assert(a.lines[1].first==1 and a.lines[1].content_last==1 and a.lines[1].last==3)
      assert(a.lines[2].first==3 and a.lines[2].content_last==4 and a.lines[2].last==5)
      assert(a.lines[3].first==5 and a.lines[3].content_last==5 and a.lines[3].last==6)
      assert(a.lines[4].first==6 and a.lines[4].content_last==10 and a.lines[4].last==13 and a.lines[4].direction=='rtl')
      assert(a.lines[5].first==13 and a.lines[5].last==13 and a.lines[5].first_run==a.lines[5].last_run)
      local empty=t.bidi_runs('');assert(empty.count==0 and empty.line_count==1 and empty.lines[1].first==1 and empty.lines[1].last==1)
      for _,s in ipairs({'é é','👨‍👩‍👧‍👦 👍🏽 🇺🇸 1️⃣','אבְג','العربية 123','a\tאב'})do
        local valid={[#s+1]=true};for _,first,last in t.graphemes(s)do valid[first]=true;valid[last]=true end
        local b=t.bidi_runs(s);assert(b.text==s)
        for i=1,b.count do assert(valid[b.first[i]]and valid[b.last[i]])end
      end
    "#,
    );
    clear(&h);
}
#[test]
fn unicode_line_breaks_match_known_crlf_space_cjk_and_grapheme_boundaries() {
    let h = host();
    run(
        &h,
        r#"
      local t=plum.text;local a=t.line_breaks('a b \nc')
      assert(a.count==3 and a.position[1]==3 and a.kind[1]=='allowed'and a.position[2]==6 and a.kind[2]=='mandatory'and a.position[3]==7 and a.kind[3]=='mandatory')
      local cr=t.line_breaks('A\r\nB');assert(cr.count==2 and cr.position[1]==4 and cr.kind[1]=='mandatory'and cr.position[2]==5)
      local cjk=t.line_breaks('漢字');assert(cjk.count==2 and cjk.position[1]==4 and cjk.kind[1]=='allowed'and cjk.position[2]==7)
      for _,s in ipairs({'é é','👨‍👩‍👧‍👦X','🇺🇸🇨🇦','👍🏽 1️⃣','\r\n','a'..utf8.char(0x2028)..'b'})do
        local valid={[#s+1]=true};for _,first,last in t.graphemes(s)do valid[first]=true;valid[last]=true end
        local b=t.line_breaks(s);assert(b.text==s and b.count>=1 and b.position[b.count]==#s+1 and b.kind[b.count]=='mandatory')
        local previous=0;for i=1,b.count do assert(valid[b.position[i]]and b.position[i]>previous);previous=b.position[i]end
      end
      local empty=t.line_breaks('');assert(empty.count==0 and empty.text=='')
    "#,
    );
    clear(&h);
}
#[test]
fn layout_limits_strict_options_and_native_preflight_are_atomic() {
    let h = host();
    run(
        &h,
        r#"
      local t=plum.text;assert(t.max_layout_bytes==16384 and t.max_layout_scalars==4096 and t.max_layout_lines==256 and t.max_layout_runs==4096)
      assert(t.bidi_runs(string.rep('a',4096)).count==1)
      assert(t.bidi_runs(string.rep('😀',4096)).text==string.rep('😀',4096))
      assert(t.bidi_runs(string.rep('\n',255)).line_count==256)
      assert(not pcall(t.bidi_runs,string.rep('\n',256)))
      for _,s in ipairs({string.rep('a',4097),string.rep('😀',4097),string.char(255),1,true,{}})do assert(not pcall(t.bidi_runs,s));assert(not pcall(t.line_breaks,s))end
      for _,o in ipairs({{direction='left'},{direction=1},{max_runs=0},{max_runs=4097},{max_runs=1.0},{max_lines=0},{max_lines=257},{extra=1},setmetatable({},{})})do assert(not pcall(t.bidi_runs,'abc',o))end
      assert(not pcall(t.bidi_runs,'abc אבג',{max_runs=1}));assert(t.bidi_runs('abc אבג',{max_runs=2}).count==2)
      assert(not pcall(t.bidi_runs,'a\nb\n',{max_lines=2}));assert(t.bidi_runs('a\nb',{max_lines=2}).line_count==2)
      assert(not pcall(t.line_breaks,'a b c',{max_breaks=1}));assert(t.line_breaks('a b c',{max_breaks=3}).count==3)
      for _,o in ipairs({{max_breaks=0},{max_breaks=4097},{max_breaks=1.0},{extra=1},setmetatable({},{})})do assert(not pcall(t.line_breaks,'a',o))end
    "#,
    );
    clear(&h);
    h.lua.set_app_data(NativeBudget::isolated(1024));
    for s in [
        "plum.text.bidi_runs('abc אבג')",
        "plum.text.line_breaks('a b c')",
    ] {
        assert_eq!(
            h.run("layout-quota.lua", s).unwrap_err().category,
            Category::Budget
        );
        clear(&h);
    }
}
#[test]
fn deep_bidi_and_linebreak_cancellation_discards_private_outputs_and_allows_reuse() {
    let h = host();
    for bidi in [true, false] {
        let cancel = h.cancel_handle();
        h.lua
            .globals()
            .set(
                "controlled_layout",
                h.lua
                    .create_function(move |lua, value: Value| {
                        let mut calls = 0;
                        let target = if bidi { 15 } else { 150 };
                        let check = || {
                            calls += 1;
                            if calls == target {
                                cancel.store(true, std::sync::atomic::Ordering::Release);
                            }
                            notes::check_budget(lua)
                        };
                        if bidi {
                            bidi_controlled(lua, value, None, check)
                        } else {
                            breaks_controlled(lua, value, None, check)
                        }
                    })
                    .unwrap(),
            )
            .unwrap();
        let s = if bidi {
            r"saved=string.rep('abc אבג\n',32);controlled_layout(saved)"
        } else {
            "saved=string.rep('漢',4096);controlled_layout(saved)"
        };
        assert_eq!(
            h.run("layout-cancel.lua", s).unwrap_err().category,
            Category::Canceled
        );
        clear(&h);
        run(
            &h,
            "assert(type(saved)=='string' and plum.text.bidi_runs('abc').count==1 and plum.text.line_breaks('a b').count==2)",
        );
        clear(&h);
    }
}
#[test]
fn actual_vm_output_failures_preserve_original_text_and_native_workspace() {
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
        r#"
      local t=plum.text;local s=string.rep('aא',2048)
      limit_vm();local ok=pcall(t.bidi_runs,s);restore_vm();assert(not ok and #s==6144)
      local runs=t.bidi_runs(s);assert(runs.count==4096 and runs.text==s)
      local cjk=string.rep('漢',4096);limit_vm();ok=pcall(t.line_breaks,cjk);restore_vm();assert(not ok and #cjk==12288)
      local breaks=t.line_breaks(cjk);assert(breaks.count==4096 and breaks.text==cjk)
    "#,
    );
    clear(&h);
}
#[test]
fn published_author_layout_uses_original_offsets_and_two_native_shaped_baselines() {
    let h = host();
    h.lua.set_app_data(NativeBudget::isolated(64 * 1024 * 1024));
    let source = include_str!("../../../../docs/LUA_API.md")
        .split("### Bidirectional runs and Unicode line breaks (API 0.2)")
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
        r#"
      assert(layout.text=='office é\r\nTwo lines'and layout.count==15)
      local seen_lower=false
      for i=1,layout.count do
        assert(layout.first[i]>=1 and layout.last[i]>layout.first[i]and layout.last[i]<=#layout.text+1)
        local cluster=layout.text:sub(layout.first[i],layout.last[i]-1)
        assert(not cluster:find('\r')and not cluster:find('\n'))
        if layout.y[i]==-1.5 then seen_lower=true else assert(layout.y[i]==0)end
      end
      assert(seen_lower and layout.first[7]==12)
    "#,
    );
    clear(&h);
}
