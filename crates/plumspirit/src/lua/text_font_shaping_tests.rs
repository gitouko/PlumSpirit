use super::*;
use crate::lua::{Host, Limits};
fn host(quota: usize) -> Host {
    let h = Host::new(
        Limits {
            memory: 16 * 1024 * 1024,
            ..Default::default()
        },
        [],
    )
    .unwrap();
    h.lua.set_app_data(NativeBudget::isolated(quota));
    h
}
fn run(h: &Host, s: &str) {
    h.run("shape.lua", s).unwrap_or_else(|e| panic!("{e}"));
}
fn clear(h: &Host) {
    h.lua.gc_collect().unwrap();
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}
#[test]
fn real_ligatures_kerning_combining_marks_and_utf8_clusters_are_shaped_not_scalar_mapped() {
    let h = host(64 * 1024 * 1024);
    run(
        &h,
        r#"
      local f=plum.text.builtin_font();assert(f:info().shaping_supported)
      local r=f:shape_run('ffi',{script='Latn',language='en'});local plain=f:shape_run('ffi',{features={liga=false}})
      assert(r.count<plain.count and plain.count==3,'real ffi ligature')
      assert(r.first[1]==1 and r.last[1]==4 and r.text=='ffi' and r.units=='em' and r.direction=='ltr' and r.script=='Latn' and r.language=='en')
      local kern=f:shape_run('AV');local loose=f:shape_run('AV',{features={kern=false}})
      assert(kern.advance_x<loose.advance_x,'native kerning changes layout')
      local decomposed=f:shape_run('é');local composed=f:shape_run('é')
      assert(decomposed.count==1 and composed.count==1 and decomposed.glyph_id[1]==composed.glyph_id[1])
      assert(decomposed.first[1]==1 and decomposed.last[1]==4 and composed.last[1]==3)
      assert(decomposed.text=='é' and composed.text=='é')
      local p=f:shape_run('éΣ');assert(p.first[1]==1 and p.last[1]==3 and p.first[2]==3 and p.last[2]==5)
      local again=f:shape_run('ffi',{features={liga=false}})
      for _,k in ipairs({'glyph_id','first','last','x','y','x_advance','y_advance','x_offset','y_offset','missing','unsafe_to_break'})do for i=1,plain.count do assert(again[k][i]==plain[k][i])end end
      for i=1,p.count do local glyph=f:glyph_outline(p.glyph_id[i]);assert(glyph.count>0);assert(p.x[i]==(i==1 and 0 or p.x_advance[1])+p.x_offset[i])end
      plain.x[1]=99;assert(again.x[1]~=99);f:close()
    "#,
    );
    clear(&h);
}
#[test]
fn directions_empty_missing_and_emoji_sequences_retain_original_grapheme_clusters() {
    let h = host(64 * 1024 * 1024);
    run(
        &h,
        r#"
      local f=plum.text.builtin_font();local r=f:shape_run('ABC',{direction='rtl'})
      assert(r.direction=='rtl' and r.count==3 and r.first[1]==3 and r.first[3]==1)
      local a=f:shape_run('A');local b=f:shape_run('ABC',{direction='ttb'});assert(b.direction=='ttb' and b.advance_y<0 and b.y_advance[1]<0)
      local up=f:shape_run('ABC',{direction='btt'});assert(up.direction=='btt' and up.first[1]==3)
      local empty=f:shape_run('');assert(empty.count==0 and empty.advance_x==0 and empty.advance_y==0 and empty.text=='' and empty.missing_count==0)
      local missing=f:shape_run(utf8.char(0x10ffff));assert(missing.text==utf8.char(0x10ffff) and missing.count==1 and missing.glyph_id[1]==0 and missing.missing[1] and missing.missing_count==1 and missing.last[1]==5)
      local e=plum.text.builtin_font('emoji')
      for _,text in ipairs({'😀','👍🏽','👨‍👩‍👧‍👦','🇺🇸','1️⃣'})do
        local r=e:shape_run(text);assert(r.text==text and r.count>0)
        for i=1,r.count do assert(r.first[i]==1 and r.last[i]==#text+1)end
        if text=='😀'then assert(r.count==1 and r.missing_count==0 and e:glyph_outline(r.glyph_id[1]).count>0)end
      end
      f:close();e:close()
    "#,
    );
    clear(&h);
}
#[test]
fn strict_options_input_caps_output_cap_and_native_quota_fail_without_a_partial_run() {
    let h = host(64 * 1024 * 1024);
    run(
        &h,
        r#"
      local t=plum.text;assert(t.max_shape_bytes==4096 and t.max_shape_scalars==256 and t.max_shaped_glyphs==4096)
      local f=t.builtin_font()
      for _,v in ipairs({1,true,{},string.char(255),string.rep('a',4097),string.rep('a',257),'one\ntwo','one\rtwo','one'..utf8.char(0x2028)..'two'})do assert(not pcall(function()f:shape_run(v)end))end
      for _,o in ipairs({setmetatable({},{}),{extra=1},{direction=1},{direction='left'},{script='Latin'},{script='1234'},{language=''},{language='en_US'},{language=string.rep('a',64)},{max_glyphs=0},{max_glyphs=4097},{max_glyphs=4.0},{features={abc=true}},{features={liga=1.0}},{features={liga=-1}},{features={liga=65536}},{features=setmetatable({},{})},{features={[1]=true}}})do assert(not pcall(function()f:shape_run('test',o)end))end
      local features={};for i=1,17 do features[string.format('cv%02d',i)]=true end;assert(not pcall(function()f:shape_run('test',{features=features})end))
      local ok,e=pcall(function()f:shape_run('AB',{max_glyphs=1})end);assert(not ok and tostring(e):find('max_glyphs'))
      assert(f:shape_run(string.rep('A',256)).count==256)
      assert(f:shape_run('AB',{max_glyphs=2}).count==2);f:close()
    "#,
    );
    clear(&h);
    let low = host(2 * 1024 * 1024);
    run(
        &low,
        "local f=plum.text.builtin_font();local ok,e=pcall(function()f:shape_run('test')end);assert(not ok and tostring(e):lower():find('budget'),tostring(e));assert(f:glyph_advance(f:glyph_id('A'))>0);f:close()",
    );
    clear(&low);
}
#[test]
fn selected_layout_authority_is_not_inferred_from_font_bytes_and_expired_handles_stay_expired() {
    let h = host(64 * 1024 * 1024);
    h.lua
        .globals()
        .set(
            "mark_external",
            h.lua
                .create_function(|_, f: AnyUserData| {
                    f.borrow::<Font>()?.data.borrow_mut().as_mut().unwrap().kind = "selected_file";
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        "local f=plum.text.builtin_font();mark_external(f);assert(f:info().shaping_supported and f:info().shape_backend=='isolated_native');local ok,e=pcall(function()f:shape_run('test')end);assert(not ok and tostring(e):find('native resource budget'));assert(f:glyph_outline(f:glyph_id('A')).count>0);f:close();saved=plum.text.builtin_font()",
    );
    run(
        &h,
        "assert(not pcall(function()saved:shape_run('test')end));saved=nil;local f=plum.text.builtin_font();f:close();assert(not pcall(function()f:shape_run('test')end))",
    );
    clear(&h);
}
#[test]
fn cancellation_after_native_shaping_discards_the_run_reclaims_workspace_and_preserves_host() {
    let h = host(64 * 1024 * 1024);
    let cancel = h.cancel_handle();
    h.lua
        .globals()
        .set(
            "controlled_shape",
            h.lua
                .create_function(move |lua, f: AnyUserData| {
                    let f = f.borrow::<Font>()?;
                    let mut calls = 0;
                    let r = shape_controlled(
                        lua,
                        &f,
                        Value::String(lua.create_string("ffi")?),
                        None,
                        || {
                            calls += 1;
                            if calls == 6 {
                                cancel.store(true, std::sync::atomic::Ordering::Release);
                            }
                            notes::check_budget(lua)
                        },
                    );
                    assert_eq!(calls, 6);
                    r
                })
                .unwrap(),
        )
        .unwrap();
    assert_eq!(
        h.run(
            "shape-cancel.lua",
            "controlled_shape(plum.text.builtin_font())"
        )
        .unwrap_err()
        .category,
        Category::Canceled
    );
    clear(&h);
    run(
        &h,
        "local f=plum.text.builtin_font();assert(f:shape_run('ffi').count==1);f:close()",
    );
    clear(&h);
}

#[test]
fn bundled_emoji_composition_limits_are_verified_and_not_confused_with_cluster_preservation() {
    let h = host(64 * 1024 * 1024);
    run(
        &h,
        r#"
      local f=plum.text.builtin_font('emoji')
      local flag=f:shape_run('🇺🇸');assert(flag.count==1 and flag.missing_count==0 and f:glyph_outline(flag.glyph_id[1]).count>0)
      local tone=f:shape_run('👍🏽');assert(tone.count==2 and tone.missing_count==1,'bundled modifier is missing')
      local family=f:shape_run('👨‍👩‍👧‍👦');assert(family.count>1 and family.missing_count==0,'covered components do not establish a joined family appearance')
      local keycap=f:shape_run('1️⃣');assert(keycap.count>1 and keycap.missing_count==0,'bundled keycap remains uncomposed')
      for _,r in ipairs({flag,tone,family,keycap})do for i=1,r.count do assert(r.first[i]==1 and r.last[i]==#r.text+1)end end
      f:close()
    "#,
    );
    clear(&h);
}

#[cfg(windows)]
struct ExternalFixture {
    root: std::path::PathBuf,
    parent: std::path::PathBuf,
}
#[cfg(windows)]
impl ExternalFixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let parent = std::env::temp_dir().canonicalize().unwrap();
        let root = parent.join(format!(
            "plum-external-shape-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let defs = egui::FontDefinitions::default();
        for (name, family) in [
            ("face.ttf", egui::FontFamily::Proportional),
            ("mono.ttf", egui::FontFamily::Monospace),
        ] {
            let f = &defs.font_data[&defs.families[&family][0]];
            std::fs::write(root.join(name), f.font.as_ref()).unwrap();
        }
        Self { root, parent }
    }
    fn host(&self) -> Host {
        let h = host(256 * 1024 * 1024);
        h.set_plugin(crate::lua::plugin_api::PluginMeta {
            id: "test.external-shape".into(),
            name: "External shapes".into(),
            version: "1".into(),
            root: self.root.clone(),
            data_dir: None,
            config_version: 1,
        })
        .unwrap();
        h.lua
            .set_app_data(NativeBudget::isolated(256 * 1024 * 1024));
        h
    }
}
#[cfg(windows)]
impl Drop for ExternalFixture {
    fn drop(&mut self) {
        if let Ok(p) = self.root.canonicalize()
            && p.parent() == Some(self.parent.as_path())
            && p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("plum-external-shape-"))
        {
            let _ = std::fs::remove_dir_all(p);
        }
    }
}
#[cfg(windows)]
#[test]
fn actual_package_font_shaping_matches_bundled_runs_without_inheriting_builtin_trust() {
    let fixture = ExternalFixture::new();
    let h = fixture.host();
    run(
        &h,
        r#"
      local t=plum.text
      assert(t.max_external_shape_calls==8 and t.max_external_shape_millis==1500 and t.external_shape_memory_bytes==134217728)
      for _,pair in ipairs({{'face.ttf','proportional'},{'mono.ttf','monospace'}})do
        local input=plum.plugin.asset(pair[1]);local external=t.load_font(input);input:close()
        local builtin=t.builtin_font(pair[2]);assert(external:info().shape_backend=='isolated_native'and builtin:info().shape_backend=='bundled_native')
        for _,case in ipairs({{'office é éΣ',{}},{'office',{features={liga=false}}},{'ABC',{direction='rtl'}}})do
          local a,b=external:shape_run(case[1],case[2]),builtin:shape_run(case[1],case[2])
          assert(a.text==b.text and a.count==b.count and a.direction==b.direction and a.script==b.script and a.language==b.language and a.advance_x==b.advance_x and a.advance_y==b.advance_y)
          for _,name in ipairs({'glyph_id','first','last','x','y','x_advance','y_advance','x_offset','y_offset','missing','unsafe_to_break'})do
            for i=1,a.count do assert(a[name][i]==b[name][i])end
          end
        end
        external:close();builtin:close()
      end
      assert(not plum.has('file.read_user_choice'))
    "#,
    );
    clear(&h);
}
#[cfg(windows)]
#[test]
fn external_shape_attempt_cap_native_reservation_and_new_job_cleanup_are_authoritative() {
    let fixture = ExternalFixture::new();
    let h = fixture.host();
    run(
        &h,
        r#"
      local f=plum.text.load_font(plum.plugin.asset('face.ttf'))
      for i=1,8 do assert(f:shape_run('A').count==1)end
      local ok,e=pcall(function()return f:shape_run('A')end);assert(not ok and tostring(e):find('8external'))
      saved=f
    "#,
    );
    run(
        &h,
        "assert(not pcall(function()saved:shape_run('A')end));saved=nil;local f=plum.text.load_font(plum.plugin.asset('face.ttf'));assert(f:shape_run('A').count==1)",
    );
    clear(&h);
    h.lua.set_app_data(NativeBudget::isolated(64 * 1024 * 1024));
    assert_eq!(
        h.run(
            "external-quota.lua",
            "local f=plum.text.load_font(plum.plugin.asset('face.ttf'));f:shape_run('A')"
        )
        .unwrap_err()
        .category,
        Category::Budget
    );
    clear(&h);
}
#[cfg(windows)]
#[test]
fn external_worker_cancellation_kills_helper_and_vm_output_refusal_keeps_font_usable() {
    let fixture = ExternalFixture::new();
    let h = fixture.host();
    let cancel = h.cancel_handle();
    h.lua
        .globals()
        .set(
            "controlled_external",
            h.lua
                .create_function(move |lua, f: AnyUserData| {
                    let f = f.borrow::<Font>()?;
                    let mut calls = 0;
                    shape_controlled(
                        lua,
                        &f,
                        Value::String(lua.create_string("office")?),
                        None,
                        || {
                            calls += 1;
                            if calls == 5 {
                                cancel.store(true, std::sync::atomic::Ordering::Release);
                            }
                            notes::check_budget(lua)
                        },
                    )
                })
                .unwrap(),
        )
        .unwrap();
    assert_eq!(
        h.run(
            "external-cancel.lua",
            "local f=plum.text.load_font(plum.plugin.asset('face.ttf'));controlled_external(f)"
        )
        .unwrap_err()
        .category,
        Category::Canceled
    );
    clear(&h);
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
      local f=plum.text.load_font(plum.plugin.asset('face.ttf'));local source=string.rep('A',256)
      limit_vm();local ok=pcall(function()return f:shape_run(source)end);restore_vm();assert(not ok)
      assert(f:shape_run(source).count==256 and f:shape_run('office').count==4)
      local ok,e=pcall(function()return f:shape_run('ABC',{max_glyphs=1})end);assert(not ok and tostring(e):find('max_glyphs'))
    "#,
    );
    clear(&h);
}
