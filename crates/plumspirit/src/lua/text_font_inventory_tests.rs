use super::*;
use crate::lua::Limits;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
#[path = "text_font_picker_tests.rs"]
mod picker;
static NEXT: AtomicU64 = AtomicU64::new(1);
struct Temp {
    root: PathBuf,
}
impl Temp {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "plum-api02-font-catalog-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("fonts")).unwrap();
        Self { root }
    }
    fn fonts(&self) -> PathBuf {
        self.root.join("fonts")
    }
    fn bundled(&self) {
        let d = egui::FontDefinitions::default();
        for (file, name) in [("a.ttf", "Ubuntu-Light"), ("b.ttf", "Hack")] {
            std::fs::write(self.fonts().join(file), &d.font_data[name].font).unwrap();
        }
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let root = self.root.canonicalize().unwrap();
        let base = std::env::temp_dir().canonicalize().unwrap();
        assert!(
            root.starts_with(&base)
                && root != base
                && root
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("plum-api02-font-catalog-")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
fn host(granted: bool) -> Host {
    let h = Host::new(
        Limits {
            memory: 16 * 1024 * 1024,
            ..Default::default()
        },
        if granted {
            vec![Capability::ListFonts]
        } else {
            vec![]
        },
    )
    .unwrap();
    h.lua.set_app_data(NativeBudget::isolated(64 * 1024 * 1024));
    h
}
fn expose(h: &Host, root: PathBuf) {
    h.lua
        .globals()
        .set(
            "fixture_fonts",
            h.lua
                .create_function(move |lua, opts: Option<Table>| {
                    create_controlled(lua, opts, vec![root.clone()], || notes::check_budget(lua))
                })
                .unwrap(),
        )
        .unwrap();
}
fn run(h: &Host, s: &str) {
    h.run("inventory.lua", s).unwrap_or_else(|e| panic!("{e}"));
}
fn clear(h: &Host) {
    h.lua.gc_collect().unwrap();
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}

fn collection(t: &Temp, name: &str, faces: usize) {
    let defs = egui::FontDefinitions::default();
    let font = &defs.font_data["Ubuntu-Light"].font;
    let base = 12 + faces * 4;
    let mut bytes = vec![0u8; base];
    bytes[..4].copy_from_slice(b"ttcf");
    bytes[4..8].copy_from_slice(&0x10000u32.to_be_bytes());
    bytes[8..12].copy_from_slice(&(faces as u32).to_be_bytes());
    for i in 0..faces {
        bytes[12 + i * 4..16 + i * 4].copy_from_slice(&(base as u32).to_be_bytes());
    }
    bytes.extend_from_slice(font);
    let count = u16::from_be_bytes(font[4..6].try_into().unwrap()) as usize;
    for k in 0..count {
        let at = base + 12 + k * 16 + 8;
        let off = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
        bytes[at..at + 4].copy_from_slice(&(off + base as u32).to_be_bytes());
    }
    std::fs::write(t.fonts().join(name), bytes).unwrap();
}

#[test]
fn opt_in_pages_reach_768_real_collection_faces_and_keep_legacy_default_and_checked_loading() {
    let t = Temp::new();
    for name in ["a.ttc", "b.ttc", "c.ttc"] {
        collection(&t, name, 256);
    }
    let h = host(true);
    expose(&h, t.fonts());
    run(
        &h,
        r#"
        assert(plum.text.max_font_pages==64)
        for _,v in ipairs({{paged=1},{paged='yes'},{paged=true,max_faces=0},{paged=true,extra=1}})do assert(not pcall(fixture_fonts,v))end
        local old=fixture_fonts();assert(old:info().count==512 and old:info().truncated and not old:info().paged and old:info().page==1)
        assert(not pcall(function()old:next_page()end));old:close()
        local c=fixture_fonts{paged=true,max_faces=128};local total=0
        for page=1,6 do
          local info=c:info();assert(info.paged and info.page==page and info.count==128 and not info.scan_truncated and info.read_bytes<=16777216)
          for i=1,128 do local f=c:face(i);assert(f.index==i and f.page==page and f.face_index==((page-1)%2)*128+i-1)
            for k in pairs(f)do assert(k~='path'and k~='root'and k~='token')end
          end
          total=total+info.count
          if page<6 then assert(info.has_more and c:next_page())else assert(not info.has_more and not c:next_page() and not c:next_page())end
        end
        assert(total==768 and c:info().page==6 and c:face(128).face_index==255)
        local font=c:load(128);assert(font:info().face_index==255 and font:glyph_id('A') and font:glyph_advance(font:glyph_id('A'))>0)
        c:close();assert(font:glyph_id('W'));font:close()
        local absent=fixture_fonts{paged=true,max_faces=1,query='not-a-real-family'}
        for i=1,64 do assert(absent:info().count==0);if not absent:info().has_more then break end;assert(absent:next_page()or not absent:info().has_more)end
        assert(not absent:info().has_more);absent:close()
    "#,
    );
    clear(&h);
}

#[test]
fn page_limit_refuses_before_advance_and_leaves_last_page_loadable() {
    let t = Temp::new();
    collection(&t, "many.ttc", 256);
    let h = host(true);
    expose(&h, t.fonts());
    run(
        &h,
        r#"
        local c=fixture_fonts{paged=true,max_faces=1}
        for i=2,64 do assert(c:next_page()and c:info().page==i and c:face(1).face_index==i-1)end
        local ok,e=pcall(function()c:next_page()end);assert(not ok and tostring(e):find('budget'))
        assert(c:info().page==64 and c:info().has_more and c:face(1).face_index==63)
        assert(not pcall(fixture_fonts,{paged=true,max_faces=1}),'new catalogs cannot bypass the job page budget')
        local f=c:load(1);assert(f:glyph_id('A'));f:close();c:close()
        local old=fixture_fonts{max_faces=1};assert(old:info().count==1 and not old:info().paged);old:close()
    "#,
    );
    clear(&h);
}

#[test]
fn native_preflight_denial_preserves_page_and_cancellation_after_attempt_closes_cursor() {
    let t = Temp::new();
    t.bundled();
    let h = host(true);
    expose(&h, t.fonts());
    let held = Rc::new(RefCell::new(None::<Reservation>));
    let quota = Rc::clone(&held);
    h.lua
        .globals()
        .set(
            "hold_catalog_quota",
            h.lua
                .create_function(move |lua, on: bool| {
                    if on {
                        let b = lua.app_data_ref::<NativeBudget>().unwrap().clone();
                        let n = 64 * 1024 * 1024 - b.used() - 1024;
                        *quota.borrow_mut() = Some(b.reserve(n).unwrap());
                    } else {
                        quota.borrow_mut().take();
                    }
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    h.lua
        .globals()
        .set(
            "cancel_catalog_page",
            h.lua
                .create_function(|lua, c: mlua::AnyUserData| {
                    let c = c.borrow::<Catalog>()?;
                    let mut checks = 0;
                    next_controlled(lua, &c, || {
                        checks += 1;
                        if checks == 3 {
                            Err(stop(Category::Canceled, "fixture page canceled"))
                        } else {
                            Ok(())
                        }
                    })
                })
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        r#"
        local c=fixture_fonts{paged=true,max_faces=1};local family=c:face(1).family
        hold_catalog_quota(true);assert(not pcall(function()c:next_page()end));hold_catalog_quota(false)
        assert(c:info().page==1 and c:face(1).family==family and c:next_page()and c:info().page==2);c:close()
        local c=fixture_fonts{paged=true,max_faces=1};assert(not pcall(cancel_catalog_page,c));assert(not pcall(function()c:info()end));assert(not c:close())
    "#,
    );
    assert!(held.borrow().is_none());
    clear(&h);
}

#[test]
fn changed_collection_between_pages_invalidates_cursor_and_end_run_releases_captured_paths() {
    let t = Temp::new();
    collection(&t, "source.ttc", 4);
    let h = host(true);
    expose(&h, t.fonts());
    let path = t.fonts().join("source.ttc");
    h.lua
        .globals()
        .set(
            "replace_catalog_source",
            h.lua
                .create_function(move |_, ()| {
                    std::fs::write(&path, b"changed").unwrap();
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        r#"
        local c=fixture_fonts{paged=true,max_faces=1};replace_catalog_source()
        local ok,e=pcall(function()c:next_page()end);assert(not ok and tostring(e):find('changed between pages'))
        assert(not pcall(function()c:face(1)end));assert(not c:close())
    "#,
    );
    clear(&h);
    collection(&t, "source.ttc", 4);
    run(
        &h,
        "saved_page=fixture_fonts{paged=true,max_faces=1};assert(saved_page:info().has_more)",
    );
    h.lua.gc_collect().unwrap();
    assert_eq!(
        h.lua.app_data_ref::<NativeBudget>().unwrap().used(),
        CATALOG_HANDLE
    );
    run(
        &h,
        "assert(not pcall(function()saved_page:next_page()end));assert(not pcall(function()saved_page:face(1)end));saved_page=nil",
    );
    clear(&h);
}

#[test]
fn documented_paging_loop_executes_verbatim_with_actual_collection_metadata() {
    let t = Temp::new();
    for name in ["a.ttc", "b.ttc", "c.ttc"] {
        collection(&t, name, 256);
    }
    let h = host(true);
    expose(&h, t.fonts());
    let plum = h.lua.globals().get::<Table>("plum").unwrap();
    let text = plum.get::<Table>("text").unwrap();
    text.set(
        "installed_fonts",
        h.lua
            .globals()
            .get::<mlua::Function>("fixture_fonts")
            .unwrap(),
    )
    .unwrap();
    let docs = include_str!("../../../../docs/LUA_API.md");
    let section = docs
        .split("### Paged installed-font catalogs")
        .nth(1)
        .unwrap();
    let snippet = section
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    assert_eq!(
        h.run("documented-font-pages.lua", snippet).unwrap(),
        crate::lua::Plain::Integer(768)
    );
    clear(&h);
}

#[test]
fn decoded_work_bound_can_yield_empty_progress_pages_without_losing_later_faces() {
    let t = Temp::new();
    // Metadata stress collection: each valid face references 4096 repeated name
    // records. A no-match query must pause at the shared per-page work bound.
    let original = crate::ui::fonts::fake_font("Fixture", "Regular");
    let mut names = Vec::new();
    for v in [0u16, 4096, 49158] {
        names.extend(v.to_be_bytes());
    }
    for _ in 0..4096 {
        for v in [3u16, 1, 0x409, 1, 512, 0] {
            names.extend(v.to_be_bytes());
        }
    }
    for _ in 0..256 {
        names.extend((b'A' as u16).to_be_bytes());
    }
    let faces = 10usize;
    let base = 12 + faces * 4;
    let mut bytes = vec![0u8; base];
    bytes[..4].copy_from_slice(b"ttcf");
    bytes[4..8].copy_from_slice(&0x10000u32.to_be_bytes());
    bytes[8..12].copy_from_slice(&(faces as u32).to_be_bytes());
    for i in 0..faces {
        bytes[12 + i * 4..16 + i * 4].copy_from_slice(&(base as u32).to_be_bytes());
    }
    bytes.extend_from_slice(&original[..28]);
    bytes[base + 20..base + 24].copy_from_slice(&((base + 28) as u32).to_be_bytes());
    bytes[base + 24..base + 28].copy_from_slice(&(names.len() as u32).to_be_bytes());
    bytes.extend(names);
    std::fs::write(t.fonts().join("work.ttc"), bytes).unwrap();
    let h = host(true);
    expose(&h, t.fonts());
    run(
        &h,
        r#"
      local c=fixture_fonts{paged=true,max_faces=512,query='absent'};local pages=1
      assert(c:info().count==0 and c:info().has_more and not c:info().scan_truncated)
      while c:info().has_more do
        local advanced=c:next_page();pages=pages+1;local i=c:info()
        assert(i.count==0 and i.read_bytes<=16777216 and not i.scan_truncated)
        if not advanced then assert(not i.has_more)end
      end
      assert(pages>1 and pages<=64 and not c:info().truncated);c:close()
    "#,
    );
    clear(&h);
}
#[test]
fn installed_font_authority_is_explicit_and_strict_descriptors_fail_before_scan() {
    assert_eq!(Capability::parse("fonts.list"), Some(Capability::ListFonts));
    let denied = host(false);
    assert_eq!(
        denied
            .run("deny.lua", "plum.text.installed_fonts()")
            .unwrap_err()
            .category,
        Category::Capability
    );
    clear(&denied);
    let t = Temp::new();
    let h = host(true);
    expose(&h, t.fonts());
    run(
        &h,
        r#"
      assert(plum.has('fonts.list') and not plum.has('file.read_user_choice'))
      assert(plum.text.max_font_catalogs==4 and plum.text.max_installed_faces==512 and plum.text.max_installed_font_bytes==268435456)
      for _,v in ipairs({{query=1},{query=string.char(255)},{query=string.rep('a',129)},{max_faces=0},{max_faces=513},{max_faces=1.0},{extra=true},setmetatable({},{})})do assert(not pcall(fixture_fonts,v))end
      local c=fixture_fonts();assert(c:info().count==0 and not c:info().truncated)
      assert(not pcall(function()c:face(1)end));assert(not pcall(function()c:load(1)end));assert(c:close() and not c:close());assert(not pcall(function()c:info()end))
    "#,
    );
    clear(&h);
}
#[test]
fn real_catalog_metadata_search_scoped_loading_and_sources_are_independent() {
    let t = Temp::new();
    t.bundled();
    let h = host(true);
    expose(&h, t.fonts());
    run(
        &h,
        r#"
      local c=fixture_fonts();local info=c:info();assert(info.count==2 and not info.truncated and info.scanned_files==2 and info.read_bytes>0 and info.read_bytes<=16777216)
      local a,b=c:face(1),c:face(2);assert(a.family~=b.family and a.index==1 and b.index==2 and a.face_index==0 and a.weight>=1 and a.weight<=1000 and type(a.italic)=='boolean')
      for k in pairs(a)do assert(k~='path' and k~='root' and k~='token')end
      a.family='mutated';assert(c:face(1).family~='mutated')
      local fa,fb=c:load(1),c:load(2);assert(fa:info().source=='installed' and fa:info().shaping_supported and fa:info().shape_backend=='isolated_native')
      assert(fa:glyph_advance(fa:glyph_id('W'))~=fb:glyph_advance(fb:glyph_id('W')))
      assert(fa:glyph_outline(fa:glyph_id('O')).count>0 and fb:glyph_outline(fb:glyph_id('O')).count>0)
      assert(not pcall(function()fa:shape_run('test')end));assert(not pcall(function()c:face(1.0)end));assert(not pcall(function()c:load(0)end))
      local s=fixture_fonts{query='hAcK'};assert(s:info().count==1 and s:face(1).family:find('Hack'))
      assert(s:load(1):info().source=='installed');assert(s:close())
      local limited=fixture_fonts{max_faces=1};assert(limited:info().count==1 and limited:info().truncated)
      local absent=fixture_fonts{query='no-such-family'};assert(absent:info().count==0 and not absent:info().truncated)
      c:close();assert(fa:glyph_id('A'));fa:close();fb:close();limited:close();absent:close()
    "#,
    );
    clear(&h);
}
#[test]
fn collection_queries_can_find_a_later_face_even_with_one_result_slot() {
    let t = Temp::new();
    let defs = egui::FontDefinitions::default();
    let mut bytes = vec![0u8; 20];
    bytes[..4].copy_from_slice(b"ttcf");
    bytes[4..8].copy_from_slice(&0x10000u32.to_be_bytes());
    bytes[8..12].copy_from_slice(&2u32.to_be_bytes());
    for (i, name) in ["Ubuntu-Light", "Hack"].iter().enumerate() {
        while !bytes.len().is_multiple_of(4) {
            bytes.push(0);
        }
        let base = bytes.len();
        bytes[12 + i * 4..16 + i * 4].copy_from_slice(&(base as u32).to_be_bytes());
        let font = &defs.font_data[*name].font;
        bytes.extend_from_slice(font);
        let count = u16::from_be_bytes(font[4..6].try_into().unwrap()) as usize;
        for k in 0..count {
            let at = base + 12 + k * 16 + 8;
            let off = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
            bytes[at..at + 4].copy_from_slice(&(off + base as u32).to_be_bytes());
        }
    }
    std::fs::write(t.fonts().join("pair.ttc"), bytes).unwrap();
    let h = host(true);
    expose(&h, t.fonts());
    run(
        &h,
        "local c=fixture_fonts{query='Hack',max_faces=1};assert(c:info().count==1 and c:face(1).face_index==1);local f=c:load(1);assert(f:info().face_index==1 and f:glyph_id('A'));f:close();c:close()",
    );
    clear(&h);
}
#[test]
fn catalog_and_permission_lifetimes_close_expired_resources_without_silent_reauthorization() {
    let t = Temp::new();
    t.bundled();
    let h = host(true);
    expose(&h, t.fonts());
    run(
        &h,
        "saved=fixture_fonts();for i=1,3 do local c=fixture_fonts();assert(c:close())end;assert(not pcall(fixture_fonts))",
    );
    h.lua.gc_collect().unwrap();
    assert_eq!(
        h.lua.app_data_ref::<NativeBudget>().unwrap().used(),
        CATALOG_HANDLE
    );
    run(
        &h,
        "assert(not pcall(function()saved:info()end));assert(not pcall(function()saved:load(1)end));saved=nil",
    );
    clear(&h);
    h.lua
        .globals()
        .set(
            "revoke_fonts",
            h.lua
                .create_function(|lua, ()| {
                    lua.set_app_data(Granted(Arc::default()));
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        "local c=fixture_fonts();local f=c:load(1);revoke_fonts();assert(not pcall(function()f:glyph_id('A')end));f:close();assert(not pcall(function()c:info()end));assert(not pcall(function()c:face(1)end));assert(not pcall(function()c:load(1)end));assert(c:close())",
    );
    clear(&h);
}
#[test]
fn replaced_removed_outside_root_and_mutated_snapshot_sources_are_rejected() {
    let t = Temp::new();
    t.bundled();
    let h = host(true);
    expose(&h, t.fonts());
    let root = t.fonts();
    h.lua
        .globals()
        .set(
            "remove_files",
            h.lua
                .create_function(move |_, ()| {
                    std::fs::remove_file(root.join("a.ttf")).unwrap();
                    std::fs::write(root.join("b.ttf"), b"different").unwrap();
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        "local c=fixture_fonts();remove_files();for i=1,c:info().count do assert(not pcall(function()c:load(i)end))end;c:close()",
    );
    clear(&h);
    t.bundled();
    let mut inventory = native::collect(vec![t.fonts()], "", 512, &mut || true).unwrap();
    let outside = t.root.join("outside.ttf");
    std::fs::write(
        &outside,
        &egui::FontDefinitions::default().font_data["Hack"].font,
    )
    .unwrap();
    let face = &mut inventory.faces[0];
    face.path = outside.canonicalize().unwrap();
    assert!(native::open_checked(face, &inventory.roots).is_err());
    let original = egui::FontDefinitions::default().font_data["Hack"]
        .font
        .to_vec();
    assert!(!native::snapshot_matches(
        &original,
        &inventory.faces[1],
        &mut || true
    ));
}
#[test]
fn inventory_native_quota_and_scan_cancel_reclaim_private_paths_and_allow_reuse() {
    let t = Temp::new();
    t.bundled();
    let h = host(true);
    expose(&h, t.fonts());
    h.lua.set_app_data(NativeBudget::isolated(4096));
    assert_eq!(
        h.run("quota.lua", "fixture_fonts()").unwrap_err().category,
        Category::Budget
    );
    clear(&h);
    h.lua.set_app_data(NativeBudget::isolated(64 * 1024 * 1024));
    let cancel = h.cancel_handle();
    let root = t.fonts();
    h.lua
        .globals()
        .set(
            "cancel_scan",
            h.lua
                .create_function(move |lua, ()| {
                    let mut calls = 0;
                    let r = create_controlled(lua, None, vec![root.clone()], || {
                        calls += 1;
                        if calls == 9 {
                            cancel.store(true, Ordering::Release);
                        }
                        notes::check_budget(lua)
                    });
                    assert_eq!(calls, 9);
                    r
                })
                .unwrap(),
        )
        .unwrap();
    assert_eq!(
        h.run("cancel.lua", "cancel_scan()").unwrap_err().category,
        Category::Canceled
    );
    clear(&h);
    run(
        &h,
        "local c=fixture_fonts();assert(c:info().count==2);c:close()",
    );
    clear(&h);
}
#[test]
fn installed_font_read_cancellation_and_cumulative_load_budget_do_not_publish_partial_fonts() {
    let t = Temp::new();
    t.bundled();
    let h = host(true);
    expose(&h, t.fonts());
    let cancel = h.cancel_handle();
    h.lua
        .globals()
        .set(
            "cancel_load",
            h.lua
                .create_function(move |lua, c: AnyUserData| {
                    let c = c.borrow::<Catalog>()?;
                    let mut calls = 0;
                    let r = load_controlled(lua, &c, Value::Integer(1), || {
                        calls += 1;
                        if calls == 3 {
                            cancel.store(true, Ordering::Release);
                        }
                        notes::check_budget(lua)
                    });
                    assert_eq!(calls, 3);
                    r
                })
                .unwrap(),
        )
        .unwrap();
    assert_eq!(
        h.run("cancel-load.lua", "cancel_load(fixture_fonts())")
            .unwrap_err()
            .category,
        Category::Canceled
    );
    clear(&h);
    h.lua
        .globals()
        .set(
            "exhaust_font_reads",
            h.lua
                .create_function(|lua, ()| {
                    lua.app_data_mut::<Slot>().unwrap().loaded = MAX_LOADED;
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        "local c=fixture_fonts();exhaust_font_reads();local ok,e=pcall(function()c:load(1)end);assert(not ok and tostring(e):find('256MiB'));assert(c:info().count==2);c:close()",
    );
    clear(&h);
}

#[test]
fn bounded_name_parser_preserves_legacy_names_but_rejects_excessive_names_tables_and_decoded_work()
{
    let t = Temp::new();
    let original = crate::ui::fonts::fake_font("Fixture Family", "Regular");
    let file = t.fonts().join("good.ttf");
    std::fs::write(&file, &original).unwrap();
    assert_eq!(
        crate::ui::fonts::read_face_names(&file),
        crate::ui::fonts::inspect_font(&original)
    );
    let got = native::collect(vec![t.fonts()], "", 512, &mut || true).unwrap();
    assert_eq!(got.faces.len(), 1);
    assert_eq!(got.faces[0].names.family, "Fixture Family");
    assert_eq!(got.faces[0].weight, 400);
    assert!(!got.faces[0].italic);
    std::fs::remove_file(file).unwrap();
    let long = crate::ui::fonts::fake_font(&"X".repeat(1024), "Regular");
    std::fs::write(t.fonts().join("long.ttf"), &long).unwrap();
    assert_eq!(crate::ui::fonts::inspect_font(&long).len(), 1);
    let mut huge = original.clone();
    huge[24..28].copy_from_slice(&0x7fffffffu32.to_be_bytes());
    std::fs::write(t.fonts().join("huge.ttf"), huge).unwrap();
    let r = native::collect(vec![t.fonts()], "", 512, &mut || true).unwrap();
    assert!(r.faces.is_empty() && r.truncated && r.read_bytes <= native::MAX_IO);
    let repeated = t.root.join("repeated");
    std::fs::create_dir(&repeated).unwrap();
    let mut names = Vec::new();
    for v in [0u16, 4096, 49158] {
        names.extend(v.to_be_bytes());
    }
    for _ in 0..4096 {
        for v in [3u16, 1, 0x409, 1, 512, 0] {
            names.extend(v.to_be_bytes());
        }
    }
    for _ in 0..256 {
        names.extend((b'A' as u16).to_be_bytes());
    }
    let mut font = original[..28].to_vec();
    font[24..28].copy_from_slice(&(names.len() as u32).to_be_bytes());
    font.extend(names);
    for i in 0..10 {
        std::fs::write(repeated.join(format!("{i:02}.ttf")), &font).unwrap();
    }
    let r = native::collect(vec![repeated], "", 512, &mut || true).unwrap();
    assert!(r.truncated && r.faces.len() <= 8 && r.read_bytes <= native::MAX_IO);
}
#[test]
fn native_file_candidate_cap_is_explicit_even_when_the_query_matches_nothing() {
    let t = Temp::new();
    let bytes = crate::ui::fonts::fake_font("Fixture", "Regular");
    for i in 0..1025 {
        std::fs::write(t.fonts().join(format!("{i:04}.ttf")), &bytes).unwrap();
    }
    let r = native::collect(vec![t.fonts()], "absent", 512, &mut || true).unwrap();
    assert!(
        r.faces.is_empty()
            && r.truncated
            && r.files <= 1024
            && r.entries <= 20000
            && r.read_bytes <= native::MAX_IO
    );
}

#[test]
fn actual_installed_inspector_persists_plain_family_style_and_reports_missing_choice_fallback() {
    let t = Temp::new();
    t.bundled();
    let dir = t.root.join("plugin-data");
    let manifest = crate::lua::package::Manifest::parse(include_str!(
        "../../examples-lua/font-resources/plum-plugin.toml"
    ))
    .unwrap();
    assert!(manifest.capabilities.contains(&"fonts.list".to_owned()));
    let source = include_str!("../../examples-lua/font-resources/main.lua");
    let make = || {
        let h = host(true);
        h.set_plugin(crate::lua::plugin_api::PluginMeta {
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            version: "1.0.0".into(),
            root: t.root.clone(),
            data_dir: Some(dir.clone()),
            config_version: 1,
        })
        .unwrap();
        h.lua.set_app_data(NativeBudget::isolated(64 * 1024 * 1024));
        expose(&h, t.fonts());
        h
    };
    let h = make();
    run(
        &h,
        &format!(
            "{source}\nplum.text.installed_fonts=fixture_fonts;register(plum.plugin);plum.plugin.__phase('ready');plum.config.set('font_choice',{{family='Hack',style='Regular'}});plum.plugin.__run('inspect-installed',{{}});local r=plum.config.get('last_report');assert(r.choice.family=='Hack' and not r.fallback and r.font.source=='installed')"
        ),
    );
    clear(&h);
    let saved = std::fs::read_to_string(dir.join("config.json")).unwrap();
    assert!(!saved.contains("fonts\\") && !saved.contains("path"));
    let h = make();
    run(
        &h,
        &format!(
            "{source}\nplum.text.installed_fonts=fixture_fonts;register(plum.plugin);plum.plugin.__phase('ready');assert(plum.config.get('font_choice').family=='Hack');plum.plugin.__run('inspect-installed',{{}});assert(not plum.config.get('last_report').fallback);plum.config.set('font_choice',{{family='Removed Family',style='Regular'}});plum.plugin.__run('inspect-installed',{{}});assert(plum.config.get('last_report').fallback)"
        ),
    );
    assert!(
        h.take_plugin_out()
            .notices
            .iter()
            .any(|s| s.0.contains("unavailable"))
    );
    clear(&h);
}

#[cfg(windows)]
#[test]
fn controlled_windows_font_handle_blocks_concurrent_mutation_and_releases_the_file() {
    let t = Temp::new();
    t.bundled();
    let inventory = native::collect(vec![t.fonts()], "", 512, &mut || true).unwrap();
    let face = &inventory.faces[0];
    let file = native::open_checked(face, &inventory.roots).unwrap();
    assert!(
        std::fs::OpenOptions::new()
            .write(true)
            .open(&face.path)
            .is_err()
    );
    assert!(std::fs::remove_file(&face.path).is_err());
    drop(file);
    assert!(
        std::fs::OpenOptions::new()
            .write(true)
            .open(&face.path)
            .is_ok()
    );
}
