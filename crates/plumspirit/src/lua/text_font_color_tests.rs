use super::*;
use crate::lua::{Capability, Host, Limits};
use std::collections::BTreeMap;
fn word(n: usize) -> [u8; 2] {
    (n as u16).to_be_bytes()
}
// A valid checksummed SFNT with licensed bundled glyf tables and controlled
// COLR/CPAL metadata. This is a fixture writer, not another production decoder.
pub(in crate::lua::text::fonts) fn fixture(layers: usize, mutate: usize) -> Vec<u8> {
    let defs = egui::FontDefinitions::default();
    let original = &defs.font_data["Ubuntu-Light"].font;
    let font = FontRef::new(original).unwrap();
    let o = font.charmap().map('O').unwrap().to_u32() as usize;
    let a = font.charmap().map('A').unwrap().to_u32() as usize;
    let mut tables = BTreeMap::new();
    let count = u16::from_be_bytes(original[4..6].try_into().unwrap()) as usize;
    for i in 0..count {
        let at = 12 + 16 * i;
        let tag: [u8; 4] = original[at..at + 4].try_into().unwrap();
        let offset = u32::from_be_bytes(original[at + 8..at + 12].try_into().unwrap()) as usize;
        let len = u32::from_be_bytes(original[at + 12..at + 16].try_into().unwrap()) as usize;
        tables.insert(tag, original[offset..offset + len].to_vec());
    }
    let mut colr = Vec::new();
    colr.extend(word(if mutate == 1 { 9 } else { 0 }));
    colr.extend(word(1));
    colr.extend(14u32.to_be_bytes());
    colr.extend(20u32.to_be_bytes());
    colr.extend(word(layers));
    colr.extend(word(o));
    colr.extend(word(if mutate == 2 { layers + 1 } else { 0 }));
    colr.extend(word(layers));
    for i in 0..layers {
        colr.extend(word(if mutate == 3 {
            65535
        } else if i.is_multiple_of(2) {
            o
        } else {
            a
        }));
        colr.extend(word(if mutate == 4 {
            2
        } else {
            match i % 3 {
                0 => 0,
                1 => 1,
                _ => 65535,
            }
        }));
    }
    let mut cpal = Vec::new();
    for n in [0, 2, 2, 4] {
        cpal.extend(word(n));
    }
    cpal.extend(16u32.to_be_bytes());
    cpal.extend(word(0));
    cpal.extend(word(if mutate == 5 { 3 } else { 2 }));
    cpal.extend([
        0, 0, 255, 255, 0, 255, 0, 128, 255, 0, 0, 0, 255, 255, 255, 255,
    ]);
    tables.insert(*b"COLR", colr);
    if mutate != 6 {
        tables.insert(*b"CPAL", cpal);
    }
    tables.get_mut(b"head").unwrap()[8..12].fill(0);
    let n = tables.len();
    let pow = 1usize << n.ilog2();
    let mut out = vec![0; 12 + n * 16];
    out[..4].copy_from_slice(&original[..4]);
    out[4..6].copy_from_slice(&word(n));
    out[6..8].copy_from_slice(&word(pow * 16));
    out[8..10].copy_from_slice(&word(n.ilog2() as usize));
    out[10..12].copy_from_slice(&word(n * 16 - pow * 16));
    let mut head = 0;
    fn sum(bytes: &[u8]) -> u32 {
        bytes.chunks(4).fold(0u32, |sum, p| {
            let mut b = [0; 4];
            b[..p.len()].copy_from_slice(p);
            sum.wrapping_add(u32::from_be_bytes(b))
        })
    }
    for (i, (tag, bytes)) in tables.into_iter().enumerate() {
        let at = 12 + i * 16;
        let offset = out.len();
        if tag == *b"head" {
            head = offset;
        }
        out[at..at + 4].copy_from_slice(&tag);
        out[at + 4..at + 8].copy_from_slice(&sum(&bytes).to_be_bytes());
        out[at + 8..at + 12].copy_from_slice(&(offset as u32).to_be_bytes());
        out[at + 12..at + 16].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
        out.extend(&bytes);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
    }
    let adjustment = 0xb1b0afbau32.wrapping_sub(sum(&out));
    out[head + 8..head + 12].copy_from_slice(&adjustment.to_be_bytes());
    out
}
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
    h.lua
        .globals()
        .set(
            "fixture_font",
            h.lua
                .create_function(|lua, (layers, mutate): (Option<usize>, Option<usize>)| {
                    let bytes = fixture(layers.unwrap_or(3), mutate.unwrap_or(0));
                    let (job, budget) = context(lua)?;
                    let native = budget
                        .reserve(bytes.len() * 2 + 1024 * 1024)
                        .map_err(|e| e.lua())?;
                    store(
                        lua,
                        job,
                        bytes,
                        0,
                        "selected_file",
                        "controlled-colr.ttf".into(),
                        native,
                    )
                })
                .unwrap(),
        )
        .unwrap();
    h
}
fn run(h: &Host, s: &str) {
    h.run("font-color.lua", s).unwrap_or_else(|e| panic!("{e}"));
}
fn clear(h: &Host) {
    h.lua.gc_collect().unwrap();
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}
#[test]
fn original_layer_order_palette_alpha_and_foreground_are_exact_and_outline_composable() {
    let h = host();
    run(
        &h,
        r#"
      local f=fixture_font();local o=f:glyph_id('O');local a=f:glyph_id('A');local i=f:color_info()
      assert(i.format=='colr_v0'and i.table_version==0 and i.base_glyph_count==1 and i.layer_count==3 and i.palette_count==2 and i.palette_entries==2 and i.v0_layers_available)
      local r=f:color_layers(o,{foreground={3,5,7,9}});assert(r.count==3 and r.glyph_id[1]==o and r.glyph_id[2]==a and r.glyph_id[3]==o)
      assert(r.red[1]==255 and r.green[1]==0 and r.blue[1]==0 and r.alpha[1]==255)
      assert(r.red[2]==0 and r.green[2]==255 and r.alpha[2]==128)
      assert(r.palette_index[3]==65535 and r.foreground[3]and not r.foreground[1]and r.red[3]==3 and r.green[3]==5 and r.blue[3]==7 and r.alpha[3]==9)
      local p=f:color_layers(o,{palette=1});assert(p.red[1]==0 and p.blue[1]==255 and p.alpha[1]==0 and p.red[2]==255 and p.alpha[2]==255 and p.red[3]==0 and p.alpha[3]==255)
      for j=1,r.count do assert(f:glyph_outline(r.glyph_id[j]).count>0)end
      assert(f:color_layers(a)==nil)
      r.red[1]=1;assert(f:color_layers(o).red[1]==255)
      local plain=plum.text.builtin_font();assert(plain:color_info()==nil and plain:color_layers(plain:glyph_id('A'))==nil)
      held=f
    "#,
    );
    h.lua.gc_collect().unwrap();
    assert_eq!(
        h.lua.app_data_ref::<NativeBudget>().unwrap().used(),
        HANDLE_BYTES
    );
    assert_eq!(
        h.run("expired-color.lua", "return held:color_layers(0)")
            .unwrap_err()
            .category,
        Category::Expired
    );
    h.lua.globals().set("held", Value::Nil).unwrap();
    clear(&h);
}
#[test]
fn strict_options_caps_corrupt_tables_and_closed_handles_fail_without_partial_output() {
    let h = host();
    run(
        &h,
        r#"
      local f=fixture_font();local id=f:glyph_id('O')
      for _,o in ipairs({{palette=-1},{palette=2},{palette=0.0},{max_layers=2},{max_layers=0},{max_layers=257},{max_layers=3.0},{unknown=1},setmetatable({},{}),{foreground={0,0,0}},{foreground={0,0,0,256}},{foreground={0,0,0,255.0}},{foreground={0,0,0,255,extra=1}},{foreground=setmetatable({0,0,0,255},{})}})do assert(not pcall(function()f:color_layers(id,o)end))end
      for _,id in ipairs({-1,1.0,f:info().glyph_count,{},true})do assert(not pcall(function()f:color_layers(id)end))end
      for mode=1,6 do local bad=fixture_font(3,mode);local ok,e=pcall(function()bad:color_layers(bad:glyph_id('O'),{palette=mode==5 and 1 or 0})end);assert(not ok,tostring(mode));bad:close()end
      assert(f:color_layers(id,{max_layers=3}).count==3)
      local large=fixture_font(256);assert(not pcall(function()large:color_layers(large:glyph_id('O'))end));assert(large:color_layers(large:glyph_id('O'),{max_layers=256}).count==256)
      f:close();assert(not pcall(function()f:color_info()end)and not pcall(function()f:color_layers(0)end))
    "#,
    );
    clear(&h);
}
#[test]
fn native_quota_and_vm_output_failure_preserve_font_and_allow_retry() {
    let h = host();
    let b = h.lua.app_data_ref::<NativeBudget>().unwrap().clone();
    let held = Rc::new(RefCell::new(None));
    let sink = held.clone();
    h.lua
        .globals()
        .set(
            "hold_native",
            h.lua
                .create_function(move |_, ()| {
                    *sink.borrow_mut() = Some(
                        b.reserve(64 * 1024 * 1024 - b.used() - PARSER_WORK - 1024)
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
        "local f=fixture_font(256);local id=f:glyph_id('O');hold_native();local ok,e=pcall(function()f:color_layers(id,{max_layers=256})end);release_native();assert(not ok and tostring(e):lower():find('budget'));local opts={max_layers=256};limit_vm();ok=pcall(function()f:color_layers(id,opts)end);restore_vm();assert(not ok);assert(f:color_layers(id,opts).count==256)",
    );
    clear(&h);
}
#[test]
fn cancellation_during_layer_scan_and_output_discards_every_partial_result() {
    for target in [4, 20] {
        let h = host();
        let cancel = h.cancel_handle();
        h.lua
            .globals()
            .set(
                "controlled_layers",
                h.lua
                    .create_function(move |lua, (f, id): (AnyUserData, Value)| {
                        let f = f.borrow::<Font>()?;
                        let opts = lua.create_table()?;
                        opts.raw_set("max_layers", 256)?;
                        let mut checks = 0;
                        layers_controlled(lua, &f, id, Some(opts), || {
                            checks += 1;
                            if checks == target {
                                cancel.store(true, std::sync::atomic::Ordering::Release);
                            }
                            notes::check_budget(lua)
                        })
                    })
                    .unwrap(),
            )
            .unwrap();
        let e = h
            .run(
                "cancel-color.lua",
                "local f=fixture_font(256);controlled_layers(f,f:glyph_id('O'))",
            )
            .unwrap_err();
        assert_eq!(e.category, Category::Canceled);
        clear(&h);
        run(
            &h,
            "local f=fixture_font();assert(f:color_layers(f:glyph_id('O')).count==3)",
        );
        clear(&h);
    }
}
#[cfg(windows)]
#[test]
fn actual_windows_installed_emoji_exposes_compatible_v0_layers_without_v1_paint_execution() {
    let h = Host::new(
        Limits {
            memory: 8 * 1024 * 1024,
            ..Default::default()
        },
        [Capability::ListFonts],
    )
    .unwrap();
    h.lua
        .set_app_data(NativeBudget::isolated(128 * 1024 * 1024));
    h.lua
        .globals()
        .set(
            "revoke_fonts",
            h.lua
                .create_function(|lua, ()| {
                    lua.set_app_data(crate::lua::handles::Granted(Arc::default()));
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    h.run("actual-windows-color-font.lua",r#"
      local catalog=plum.text.installed_fonts{query='Segoe UI Emoji',max_faces=64};local f
      for j=1,catalog:info().count do if catalog:face(j).family=='Segoe UI Emoji'then f=catalog:load(j);break end end
      assert(f,'Configured Windows Segoe UI Emoji was not found')
      local info=f:color_info();local id=assert(f:glyph_id('😀'));local layers=assert(f:color_layers(id,{max_layers=256}))
      assert(f:info().source=='installed'and info.v0_layers_available and layers.count>1)
      local outlines=0;for i=1,layers.count do local o=f:glyph_outline(layers.glyph_id[i]);if o.count>0 then outlines=outlines+1 end end
      assert(outlines>0)
      native_color_result={table_version=info.table_version,base_glyph_count=info.base_glyph_count,layer_count=layers.count,original_outlines=outlines}
      revoke_fonts();assert(not pcall(function()f:color_info()end)and not pcall(function()f:color_layers(id)end))
      f:close();catalog:close();return true
    "#).unwrap();
    let result = h.lua.globals().get::<Table>("native_color_result").unwrap();
    println!(
        "Actual Windows installed SegoeUIEmoji: table_version={},base_glyphs={},glyph_layers={},original_outlines={}",
        result.raw_get::<u16>("table_version").unwrap(),
        result.raw_get::<usize>("base_glyph_count").unwrap(),
        result.raw_get::<usize>("layer_count").unwrap(),
        result.raw_get::<usize>("original_outlines").unwrap()
    );
    clear(&h);
}

#[test]
fn published_original_layer_example_executes_over_real_glyf_contours() {
    let h = host();
    let source = include_str!("../../../../docs/LUA_API.md")
        .split("## Color glyph layers (API 0.2)")
        .nth(1)
        .unwrap()
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    run(
        &h,
        &format!(
            "{source}\nlocal f=fixture_font();local layers,points=inspect_color(f,'O');assert(layers.count==3 and points>0);f:close()"
        ),
    );
    assert!(h.take_commits().is_none());
    clear(&h);
}
