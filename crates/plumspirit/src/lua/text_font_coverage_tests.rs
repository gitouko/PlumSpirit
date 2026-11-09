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
    h.run("font-coverage.lua", s)
        .unwrap_or_else(|e| panic!("{e}"));
}
fn clear(h: &Host) {
    h.lua.gc_collect().unwrap();
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}
#[test]
fn whole_grapheme_priority_matches_independent_scalar_cmap_reference_and_original_bytes() {
    let h = host();
    run(
        &h,
        r#"
      local t=plum.text;local p=t.builtin_font();local e=t.builtin_font('emoji');local m=t.builtin_font('monospace')
      local function verify(text,fonts,ignored)
        local options={ignored_codepoints=ignored or{}};local skip={};for _,c in ipairs(options.ignored_codepoints)do skip[c]=true end
        local a=t.font_coverage_runs(text,fonts,options);assert(a.text==text and a.font_count==#fonts)
        local pos,count,covered,missing,empty,skipped=1,0,0,0,0,0
        for cluster,first,last in t.graphemes(text)do
          assert(first==pos);pos=last;count=count+1
          local wanted=0;local required=false
          for _,c in utf8.codes(cluster)do if skip[c]then skipped=skipped+1 else required=true end end
          if required then for i,font in ipairs(fonts)do local yes=true
            for _,c in utf8.codes(cluster)do if not skip[c]and not font:glyph_id(c)then yes=false;break end end
            if yes then wanted=i;break end
          end end
          local status=not required and'ignored'or wanted==0 and'missing'or'covered'
          if status=='covered'then covered=covered+1 elseif status=='missing'then missing=missing+1 else empty=empty+1 end
          local found=false;for i=1,a.count do if a.first[i]<=first and first<a.last[i]then
            assert(a.last[i]>=last and a.font[i]==wanted and a.status[i]==status);found=true;break end end;assert(found)
        end
        assert(pos==#text+1 and a.grapheme_count==count and a.covered_graphemes==covered and a.missing_graphemes==missing and a.ignored_graphemes==empty and a.ignored_scalar_count==skipped)
        local total,ig=0,0;for i=1,a.count do assert(a.first[i]==(i==1 and 1 or a.last[i-1]));total=total+a.graphemes[i];ig=ig+a.ignored[i]end
        assert(total==count and ig==skipped)
        return a
      end
      assert(not p:glyph_id('😀')and e:glyph_id('😀'))
      local a=verify('A😀A',{p,e,p});assert(a.count==3 and a.font[1]==1 and a.font[2]==2 and a.font[3]==1)
      local b=verify('😀',{e,p});assert(b.font[1]==1)
      for _,text in ipairs({'ffi AV éΩЖ','👍🏽👨‍👩‍👧‍👦🇺🇸1️⃣','a\r\nאالعربية漢',utf8.char(0x10ffff),'a'..utf8.char(0x301)..'😀'})do
        verify(text,{p,m,e});verify(text,{e,m,p},{0x200d,0xfe0f})
      end
      local empty=verify('',{p,e});assert(empty.count==0)
      local x=verify('aA',{p,p});assert(x.count==1 and x.font[1]==1 and x.graphemes[1]==2)
    "#,
    );
    clear(&h);
}
#[test]
fn ignored_policy_is_explicit_and_never_implies_composed_emoji_appearance() {
    let h = host();
    run(
        &h,
        r#"
      local t=plum.text;local e=t.builtin_font('emoji')
      local original=utf8.char(0x10ffff);local missing=t.font_coverage_runs(original,{e})
      assert(missing.status[1]=='missing'and missing.font[1]==0 and missing.missing_graphemes==1 and missing.text==original)
      local ignored=t.font_coverage_runs(original,{e},{ignored_codepoints={0x10ffff}})
      assert(ignored.font[1]==0 and ignored.status[1]=='ignored'and ignored.ignored[1]==1 and ignored.ignored_graphemes==1 and ignored.text==original)
      local family='👨‍👩‍👧‍👦';local covered=t.font_coverage_runs(family,{e},{ignored_codepoints={0x200d}})
      assert(covered.grapheme_count==1 and covered.text==family and covered.ignored_scalar_count==3)
      -- This font maps the component people but does not compose the family.
      assert(covered.font[1]==1 and covered.status[1]=='covered')
      local shaped=e:shape_run(family);assert(shaped.count>1 and shaped.text==family)
      local modifier=t.font_coverage_runs('👍🏽',{e});assert(modifier.grapheme_count==1 and modifier.status[1]=='missing'and modifier.font[1]==0)
    "#,
    );
    clear(&h);
}
#[test]
fn strict_lists_options_limits_and_recoverable_native_preflight_are_atomic() {
    let h = host();
    run(
        &h,
        r#"
      local t=plum.text;local f=t.builtin_font();assert(t.max_coverage_fonts==8 and t.max_coverage_runs==4096 and t.max_coverage_ignored_codepoints==32)
      for _,fonts in ipairs({{}, {f,f,f,f,f,f,f,f,f}, {[0]=f},{[1]=f,[3]=f},{f,name='x'},setmetatable({f},{}),{'not a font'},{plum.images.from_rgba(1,1,string.char(0,0,0,0))}})do
        assert(not pcall(t.font_coverage_runs,'a',fonts))
      end
      for _,opts in ipairs({{extra=true},{max_runs=0},{max_runs=4097},{max_runs=1.0},setmetatable({},{}),
        {ignored_codepoints=true},{ignored_codepoints={65.0}},{ignored_codepoints={0xd800}},{ignored_codepoints={-1}},
        {ignored_codepoints={0x110000}},{ignored_codepoints={65,65}},{ignored_codepoints={[0]=65}},
        {ignored_codepoints=setmetatable({65},{})}})do assert(not pcall(t.font_coverage_runs,'a',{f},opts))end
      local ignored={};for i=1,33 do ignored[i]=i end;assert(not pcall(t.font_coverage_runs,'a',{f},{ignored_codepoints=ignored}))
      for _,text in ipairs({string.char(255),1,true,{},string.rep('a',4097),string.rep('😀',4097)})do assert(not pcall(t.font_coverage_runs,text,{f}))end
      local s=string.rep('A'..utf8.char(0x10ffff),2048);local r=t.font_coverage_runs(s,{f});assert(r.count==4096 and r.text==s and r.covered_graphemes==2048 and r.missing_graphemes==2048)
      assert(not pcall(t.font_coverage_runs,s,{f},{max_runs=4095}))
      assert(t.font_coverage_runs(string.rep('😀',4096),{f}).grapheme_count==4096)
      assert(t.font_coverage_runs(string.rep('a',4096),{f,f,f,f,f,f,f,f}).count==1)
    "#,
    );
    clear(&h);
    let h = host();
    let budget = h.lua.app_data_ref::<NativeBudget>().unwrap().clone();
    let held = Rc::new(RefCell::new(None));
    let hold = held.clone();
    let b = budget.clone();
    h.lua
        .globals()
        .set(
            "hold_native",
            h.lua
                .create_function(move |_, ()| {
                    *hold.borrow_mut() = Some(
                        b.reserve(64 * 1024 * 1024 - b.used())
                            .map_err(|e| e.lua())?,
                    );
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    h.lua
        .globals()
        .set(
            "release_native",
            h.lua
                .create_function(move |_, ()| {
                    held.borrow_mut().take();
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        "local f=plum.text.builtin_font();hold_native();local ok,e=pcall(plum.text.font_coverage_runs,'a',{f});release_native();assert(not ok and tostring(e):lower():find('budget'));assert(plum.text.font_coverage_runs('a',{f}).font[1]==1)",
    );
    clear(&h);
}
#[test]
fn closed_expired_and_unused_installed_fonts_retain_their_native_scope_checks() {
    let h = host();
    h.lua
        .globals()
        .set(
            "tag_installed",
            h.lua
                .create_function(|_, font: AnyUserData| {
                    font.borrow::<Font>()?
                        .data
                        .borrow_mut()
                        .as_mut()
                        .unwrap()
                        .kind = "installed";
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        r#"
      local t=plum.text;local a=t.builtin_font();local b=t.builtin_font();b:close()
      assert(not pcall(t.font_coverage_runs,'A',{a,b}))
      local c=t.builtin_font();tag_installed(c)
      local ok,e=pcall(t.font_coverage_runs,'A',{a,c});assert(not ok and tostring(e):find('fonts.list'))
      held=a
    "#,
    );
    assert!(h.lua.app_data_ref::<NativeBudget>().unwrap().used() >= HANDLE_BYTES);
    let error = h
        .run(
            "expired-coverage.lua",
            "return plum.text.font_coverage_runs('',{held})",
        )
        .unwrap_err();
    assert_eq!(error.category, Category::Expired);
    h.lua.globals().set("held", Value::Nil).unwrap();
    clear(&h);
}
#[test]
fn cancellation_during_font_scan_and_output_never_publishes_partial_runs() {
    for at in [200, 4370] {
        let h = host();
        let cancel = h.cancel_handle();
        h.lua
            .globals()
            .set(
                "controlled_coverage",
                h.lua
                    .create_function(move |lua, (text, fonts): (Value, Table)| {
                        let mut calls = 0;
                        controlled(lua, text, fonts, None, || {
                            calls += 1;
                            if calls == at {
                                cancel.store(true, std::sync::atomic::Ordering::Release);
                            }
                            notes::check_budget(lua)
                        })
                    })
                    .unwrap(),
            )
            .unwrap();
        let error=h.run("cancel-coverage.lua","original=string.rep('A'..utf8.char(0x10ffff),2048);controlled_coverage(original,{plum.text.builtin_font()})").unwrap_err();
        assert_eq!(error.category, Category::Canceled);
        clear(&h);
        run(
            &h,
            "assert(#original==10240 and plum.text.font_coverage_runs('A',{plum.text.builtin_font()}).font[1]==1)",
        );
        clear(&h);
    }
}

#[test]
fn actual_vm_output_failure_keeps_input_and_retry_releases_native_scratch() {
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
        "local t=plum.text;local f=t.builtin_font();local text=string.rep('A'..utf8.char(0x10ffff),2048);local fonts={f};limit_vm();local ok=pcall(t.font_coverage_runs,text,fonts);restore_vm();assert(not ok and #text==10240);assert(t.font_coverage_runs(text,fonts).count==4096)",
    );
    clear(&h);
}
#[test]
fn published_font_selection_shapes_real_fallback_glyphs_without_new_authority() {
    let h = host();
    let source = include_str!("../../../../docs/LUA_API.md")
        .split("## Font coverage runs (API 0.2)")
        .nth(1)
        .unwrap()
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    run(&h, source);
    assert!(h.take_commits().is_none());
    clear(&h);
}
