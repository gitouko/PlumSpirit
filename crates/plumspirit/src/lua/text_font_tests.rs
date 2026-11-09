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
    h.run("font.lua", s).unwrap_or_else(|e| panic!("{e}"));
}
fn clear(h: &Host) {
    h.lua.gc_collect().unwrap();
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}
#[test]
fn bundled_faces_metrics_and_scalar_coverage_are_real_and_distinct() {
    let h = host();
    run(
        &h,
        r#"
      local t=plum.text;local p=t.builtin_font();local m=t.builtin_font('monospace')
      local pi,mi=p:info(),m:info();assert(pi.source=='builtin' and pi.source_name~=mi.source_name)
      assert(pi.units_per_em>0 and pi.glyph_count>0 and pi.bytes>0 and pi.line_height>0)
      assert(p:glyph_advance(p:glyph_id('W'))~=m:glyph_advance(m:glyph_id('W')))
      assert(p:glyph_id('A')==p:glyph_id(65) and p:glyph_id('é')~=nil and p:glyph_id(0x10ffff)==nil)
      assert(p:glyph_advance(p:glyph_id(' '))>0 and p:glyph_advance(0)>=0)
      local again=t.builtin_font();assert(again:info().source_name==pi.source_name and again:glyph_id('W')==p:glyph_id('W') and again:glyph_advance(again:glyph_id('W'))==p:glyph_advance(p:glyph_id('W')))
      assert(p:close() and not p:close() and not pcall(function()p:info()end))
    "#,
    );
    clear(&h);
}
#[test]
fn strict_font_inputs_handle_caps_quota_and_expiry_preserve_the_host() {
    let h = host();
    run(
        &h,
        r#"
      local t=plum.text;assert(t.max_font_bytes==67108864 and t.max_font_handles==32 and t.max_font_faces==256)
      for _,kind in ipairs({'other',false,1,{}})do assert(not pcall(t.builtin_font,kind))end
      local f=t.builtin_font()
      for _,v in ipairs({'','ab','👩‍💻',0xd800,-1,0x110000,65.0,false,{}})do assert(not pcall(function()f:glyph_id(v)end))end
      for _,v in ipairs({-1,f:info().glyph_count,1.5,1.0,'1',false,{}})do assert(not pcall(function()f:glyph_advance(v)end))end
      assert(not pcall(t.load_font,'C:/Windows/Fonts/arial.ttf'))
      held=f
    "#,
    );
    h.lua.gc_collect().unwrap();
    assert_eq!(
        h.lua.app_data_ref::<NativeBudget>().unwrap().used(),
        HANDLE_BYTES
    );
    run(
        &h,
        "assert(not pcall(function()held:info()end));assert(not held:close());held=nil;assert(plum.text.builtin_font():glyph_id('A'))",
    );
    clear(&h);
    run(
        &h,
        "for i=1,32 do local f=plum.text.builtin_font();f:close()end;assert(not pcall(plum.text.builtin_font))",
    );
    clear(&h);
    h.lua.set_app_data(NativeBudget::isolated(10));
    assert_eq!(
        h.run("quota.lua", "plum.text.builtin_font()")
            .unwrap_err()
            .category,
        Category::Budget
    );
    clear(&h);
}

struct Temp {
    root: std::path::PathBuf,
    parent: std::path::PathBuf,
}
impl Temp {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let parent = std::env::temp_dir().canonicalize().unwrap();
        let root = parent.join(format!(
            "plum-lua-fonts-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self { root, parent }
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        if let Ok(path) = self.root.canonicalize()
            && path.parent() == Some(self.parent.as_path())
            && path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("plum-lua-fonts-"))
        {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}
fn selected_run(source: &str, paths: Vec<Option<std::path::PathBuf>>) -> crate::lua::Plain {
    use crate::lua::{Capability, bridge};
    let (client, ui) = bridge::channel("test.font-file");
    let source = source.to_owned();
    let worker = std::thread::spawn(move || {
        let h = Host::new(
            Limits {
                memory: 8 * 1024 * 1024,
                ..Default::default()
            },
            [Capability::ReadUserFile],
        )
        .unwrap();
        h.set_plugin(crate::lua::plugin_api::PluginMeta {
            id: "test.font-file".into(),
            name: "Font File Conformance".into(),
            version: "1".into(),
            root: std::env::temp_dir(),
            data_dir: None,
            config_version: 1,
        })
        .unwrap();
        // Production deliberately shares a plugin's counter across reload workers.
        // Parallel independent fixtures must assert cleanup on their own ledger.
        h.lua
            .set_app_data(NativeBudget::isolated(256 * 1024 * 1024));
        {
            let mut session = h.lua.app_data_mut::<Session>().unwrap();
            session.client = Some(client);
            session.interactive = true;
        }
        let result = h.run("font-file.lua", &source);
        h.lua.gc_collect().unwrap();
        assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
        result
    });
    for path in paths {
        ui.recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .respond(None, path)
            .unwrap();
    }
    worker.join().unwrap().unwrap_or_else(|e| panic!("{e}"))
}
#[test]
fn selected_font_uses_real_file_io_restores_cursor_and_rejects_malformed_faces() {
    let temp = Temp::new();
    let defs = egui::FontDefinitions::default();
    let data = defs
        .font_data
        .get(&defs.families[&egui::FontFamily::Proportional][0])
        .unwrap();
    let good = temp.root.join("bundled.ttf");
    std::fs::write(&good, data.font.as_ref()).unwrap();
    let bad = temp.root.join("invalid.otf");
    std::fs::write(&bad, [0u8; 16]).unwrap();
    let huge = temp.root.join("oversized.ttf");
    std::fs::File::create(&huge)
        .unwrap()
        .set_len(MAX_BYTES as u64 + 1)
        .unwrap();
    selected_run(
        r#"
      local file=assert(plum.files.open());file:seek(7)
      local f=plum.text.load_font(file);local i=f:info()
      assert(i.source=='selected_file' and i.source_name=='bundled.ttf' and i.path==nil and i.units_per_em>0)
      assert(file:info().offset==7 and f:glyph_id('A') and f:glyph_advance(f:glyph_id('A'))>0)
      assert(f:info().shape_backend=='isolated_native' and f:shape_run('office é').missing_count==0 and file:info().offset==7)
      assert(not pcall(plum.text.load_font,file,1));assert(file:info().offset==7)
      for _,index in ipairs({-1,256,1.5,0.0,'0'})do assert(not pcall(plum.text.load_font,file,index))end
      local invalid=assert(plum.files.open());invalid:seek(3)
      assert(not pcall(plum.text.load_font,invalid));assert(invalid:info().offset==3)
      local huge=assert(plum.files.open());local ok,e=pcall(plum.text.load_font,huge);assert(not ok and tostring(e):find('64MiB'))
      assert(huge:info().offset==0)
      assert(plum.files.open()==nil)
      f:close();assert(not pcall(function()f:glyph_id('A')end))
    "#,
        vec![Some(good), Some(bad), Some(huge), None],
    );
}
#[test]
fn native_font_read_cancels_between_chunks_and_reclaims_preflight() {
    let h = host();
    let cancel = h.cancel_handle();
    h.lua
        .globals()
        .set(
            "controlled_font_read",
            h.lua
                .create_function(move |lua, ()| {
                    let (_, budget) = context(lua)?;
                    let _allocation = budget.reserve(1024 * 1024).map_err(|e| e.lua())?;
                    let mut source = std::io::Cursor::new(vec![0u8; 4 * 64 * 1024]);
                    let mut calls = 0;
                    let result = read_bytes(&mut source, 4 * 64 * 1024, || {
                        calls += 1;
                        if calls == 2 {
                            cancel.store(true, std::sync::atomic::Ordering::Release);
                        }
                        notes::check_budget(lua)
                    });
                    assert_eq!(calls, 2);
                    assert_eq!(source.position(), 64 * 1024);
                    result.map(|_| ())
                })
                .unwrap(),
        )
        .unwrap();
    assert_eq!(
        h.run("cancel-native-font.lua", "controlled_font_read()")
            .unwrap_err()
            .category,
        Category::Canceled
    );
    clear(&h);
    run(&h, "assert(plum.text.builtin_font():glyph_id('A'))");
    clear(&h);
}
#[test]
fn malformed_collection_limits_do_not_publish_font_handles() {
    let h = host();
    h.lua
        .globals()
        .set(
            "invalid_collection",
            h.lua
                .create_function(|lua, ()| {
                    let (job, budget) = context(lua)?;
                    let allocation = budget.reserve(2048).map_err(|e| e.lua())?;
                    let mut bytes = vec![0u8; 16];
                    bytes[..4].copy_from_slice(b"ttcf");
                    bytes[8..12].copy_from_slice(&257u32.to_be_bytes());
                    store(
                        lua,
                        job,
                        bytes,
                        0,
                        "selected_file",
                        "test.ttc".into(),
                        allocation,
                    )
                })
                .unwrap(),
        )
        .unwrap();
    assert_eq!(
        h.run("collection.lua", "invalid_collection()")
            .unwrap_err()
            .category,
        Category::Budget
    );
    clear(&h);
    run(&h, "assert(plum.text.builtin_font():info().glyph_count>0)");
    clear(&h);
}
#[test]
fn approved_two_face_collection_selects_distinct_native_faces() {
    let temp = Temp::new();
    let defs = egui::FontDefinitions::default();
    let faces = [egui::FontFamily::Proportional, egui::FontFamily::Monospace]
        .map(|family| defs.font_data[&defs.families[&family][0]].font.to_vec());
    let mut collection = vec![0u8; 20];
    collection[..4].copy_from_slice(b"ttcf");
    collection[4..8].copy_from_slice(&0x10000u32.to_be_bytes());
    collection[8..12].copy_from_slice(&2u32.to_be_bytes());
    for (face, bytes) in faces.iter().enumerate() {
        while !collection.len().is_multiple_of(4) {
            collection.push(0);
        }
        let base = collection.len();
        collection[12 + face * 4..16 + face * 4].copy_from_slice(&(base as u32).to_be_bytes());
        collection.extend_from_slice(bytes);
        let tables = u16::from_be_bytes(bytes[4..6].try_into().unwrap()) as usize;
        for i in 0..tables {
            let at = base + 12 + i * 16 + 8;
            let offset = u32::from_be_bytes(collection[at..at + 4].try_into().unwrap());
            collection[at..at + 4].copy_from_slice(&(offset + base as u32).to_be_bytes());
        }
    }
    let file = temp.root.join("bundled-collection.ttc");
    std::fs::write(&file, collection).unwrap();
    selected_run(
        r#"
      local f=assert(plum.files.open());f:seek(9)
      local a=plum.text.load_font(f,0);local b=plum.text.load_font(f,1)
      assert(a:info().face_index==0 and b:info().face_index==1 and f:info().offset==9)
      assert(a:glyph_advance(a:glyph_id('W'))~=b:glyph_advance(b:glyph_id('W')))
      assert(not pcall(plum.text.load_font,f,2));assert(f:info().offset==9)
    "#,
        vec![Some(file)],
    );
}
#[test]
fn installable_font_inspector_command_runs_without_project_authority() {
    let manifest = crate::lua::package::Manifest::parse(include_str!(
        "../../examples-lua/font-resources/plum-plugin.toml"
    ))
    .unwrap();
    assert_eq!(manifest.id, "plumspirit.example.font-resources");
    let h = host();
    h.set_plugin(crate::lua::plugin_api::PluginMeta {
        id: manifest.id,
        name: manifest.name,
        version: "1.0.0".into(),
        root: std::env::temp_dir(),
        data_dir: None,
        config_version: 1,
    })
    .unwrap();
    let source = include_str!("../../examples-lua/font-resources/main.lua");
    run(
        &h,
        &format!(
            "{source}\nregister(plum.plugin);plum.plugin.__phase('ready');plum.plugin.__run('inspect-bundled',{{}});local r=plum.config.get('last_report');assert(#r==2 and r[1].font.source_name~=r[2].font.source_name and #r[1].coverage==6)"
        ),
    );
    clear(&h);
}
#[test]
fn actual_selected_font_example_command_runs_with_controlled_picker_and_real_file() {
    let temp = Temp::new();
    let defs = egui::FontDefinitions::default();
    let face = &defs.font_data[&defs.families[&egui::FontFamily::Proportional][0]];
    let file = temp.root.join("bundled-selected.ttf");
    std::fs::write(&file, face.font.as_ref()).unwrap();
    let source = include_str!("../../examples-lua/font-resources/main.lua");
    selected_run(
        &format!(
            "{source}\nregister(plum.plugin);plum.plugin.__phase('ready');plum.plugin.__run('inspect-selected',{{}});local r=plum.config.get('last_report');assert(r.font.source_name=='bundled-selected.ttf' and r.font.source=='selected_file' and #r.coverage==6)"
        ),
        vec![Some(file)],
    );
}
