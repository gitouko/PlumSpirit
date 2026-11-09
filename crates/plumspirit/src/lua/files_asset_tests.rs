use super::*;
use crate::lua::{Limits, plugin_api::PluginMeta};
use std::sync::atomic::{AtomicU64, Ordering};

struct Temp {
    root: PathBuf,
    parent: PathBuf,
}
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let parent = std::env::temp_dir().canonicalize().unwrap();
        let root = parent.join(format!(
            "plum-api02-assets-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self { root, parent }
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        if let Ok(p) = self.root.canonicalize()
            && p.parent() == Some(self.parent.as_path())
            && p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("plum-api02-assets-"))
        {
            let _ = std::fs::remove_dir_all(p);
        }
    }
}
fn host(root: PathBuf) -> Host {
    let h = Host::new(
        Limits {
            memory: 8 * 1024 * 1024,
            ..Default::default()
        },
        [],
    )
    .unwrap();
    h.set_plugin(PluginMeta {
        id: "test.assets".into(),
        name: "Asset fixture".into(),
        version: "1".into(),
        root,
        data_dir: None,
        config_version: 1,
    })
    .unwrap();
    h.lua
        .set_app_data(NativeBudget::isolated(128 * 1024 * 1024));
    h
}
fn run(h: &Host, s: &str) {
    h.run("asset-test.lua", s).unwrap();
}
fn clear(h: &Host) {
    h.lua.gc_collect().unwrap();
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}

#[test]
fn installed_package_binary_png_and_font_assets_use_existing_native_streams_without_file_grants() {
    use image::{DynamicImage, ImageFormat, RgbaImage};
    let temp = Temp::new();
    let source = temp.root.join("source");
    let assets = source.join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(source.join("plum-plugin.toml"),"id = \"test.assets\"\nname = \"Assets\"\nversion = \"1.0\"\napi = \"0.2\"\nentry = \"main.lua\"\ncapabilities = []\n").unwrap();
    std::fs::write(source.join("main.lua"), "function register(plugin) end").unwrap();
    std::fs::write(assets.join("binary.bin"), b"A\0B\r\nC\xFF").unwrap();
    let mut png = std::io::Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(
        RgbaImage::from_raw(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 128]).unwrap(),
    )
    .write_to(&mut png, ImageFormat::Png)
    .unwrap();
    std::fs::write(assets.join("art.png"), png.into_inner()).unwrap();
    let defs = egui::FontDefinitions::default();
    let font = defs
        .font_data
        .get(&defs.families[&egui::FontFamily::Proportional][0])
        .unwrap();
    std::fs::write(assets.join("face.ttf"), font.font.as_ref()).unwrap();
    let store = crate::lua::package::Store::new(temp.root.join("store"));
    store.install_folder(&source, false).unwrap();
    let h = host(store.dir_of("test.assets").unwrap());
    run(
        &h,
        r#"
      assert(not plum.has('file.read_user_choice')and not plum.has('file.write_user_choice'))
      local f=plum.plugin.asset('assets/binary.bin');local i=f:info();assert(i.name=='binary.bin'and i.source=='package_asset'and i.size==7 and i.mode=='read')
      assert(f:read(4)=='A\0B\r');assert(f:seek(0)==0 and f:read_all()=='A\0B\r\nC\255')
      assert(not pcall(function()f:write('bad')end)and not pcall(function()f:commit()end));f:close();assert(not pcall(function()f:read()end))
      local p=plum.plugin.asset('assets/art.png');p:seek(3);local image=plum.images.load(p);assert(p:info().offset==3)
      assert(image:info().width==2 and image:info().height==1);local r,g,b,a=image:pixel(1,0);assert(r==0 and g==255 and b==0 and a==128);p:close()
      local input=plum.plugin.asset('assets/face.ttf');input:seek(5);local face=plum.text.load_font(input);assert(input:info().offset==5)
      local info=face:info();assert(info.source=='package_asset'and info.shaping_supported and info.shape_backend=='isolated_native' and info.source_name=='face.ttf')
      local original=plum.text.builtin_font();local id=face:glyph_id('O');assert(id==original:glyph_id('O')and face:glyph_advance(id)==original:glyph_advance(id))
      assert(face:glyph_outline(id).count==original:glyph_outline(id).count)
      input:close();assert(face:glyph_id('A')~=nil);face:close();image:close()
    "#,
    );
    clear(&h);
}

#[test]
fn traversal_unknown_paths_and_types_cannot_leave_the_own_package() {
    let temp = Temp::new();
    let package = temp.root.join("package");
    std::fs::create_dir(&package).unwrap();
    std::fs::write(temp.root.join("private.txt"), b"outside").unwrap();
    std::fs::write(package.join("ok.bin"), b"inside").unwrap();
    let h = host(package);
    run(
        &h,
        r#"
      for _,p in ipairs({'../private.txt','..\\private.txt','/private.txt','C:/private.txt','\\\\server\\share\\private.txt','ok.bin:stream','./ok.bin','folder//ok.bin','CON','COM1.txt','ok.bin.','ok.bin ','','missing.bin',string.rep('x',1025),false,1,{}})do assert(not pcall(plum.plugin.asset,p))end
      assert(plum.plugin.asset('ok.bin'):read_all()=='inside')
      assert(not pcall(plum.files.open))
    "#,
    );
    clear(&h);
    assert_eq!(
        std::fs::read(temp.root.join("private.txt")).unwrap(),
        b"outside"
    );
}

#[test]
fn asset_attempt_size_native_and_vm_limits_cleanup_and_reset_at_the_next_job() {
    let temp = Temp::new();
    std::fs::write(temp.root.join("data.bin"), b"fixture").unwrap();
    let big = File::create(temp.root.join("big.bin")).unwrap();
    big.set_len(crate::lua::package::MAX_FILE_BYTES + 1)
        .unwrap();
    drop(big);
    std::fs::write(temp.root.join("buffer.bin"), vec![7u8; 1024 * 1024]).unwrap();
    let h = host(temp.root.clone());
    run(
        &h,
        r#"
      assert(plum.plugin.max_asset_bytes==16777216 and plum.plugin.max_asset_opens==32)
      assert(not pcall(plum.plugin.asset,'big.bin'))
      for i=1,31 do plum.plugin.asset('data.bin'):close()end
      local ok,e=pcall(plum.plugin.asset,'data.bin');assert(not ok and tostring(e):find('32'))
    "#,
    );
    clear(&h);
    run(
        &h,
        "assert(plum.plugin.asset('data.bin'):read_all()=='fixture')",
    );
    clear(&h);
    h.lua
        .set_app_data(NativeBudget::isolated(METADATA_BYTES - 1));
    assert_eq!(
        h.run("asset-quota.lua", "plum.plugin.asset('data.bin')")
            .unwrap_err()
            .category,
        Category::Budget
    );
    clear(&h);
    h.lua
        .set_app_data(NativeBudget::isolated(128 * 1024 * 1024));
    h.lua
        .globals()
        .set(
            "limit_vm",
            h.lua
                .create_function(|lua, ()| {
                    lua.gc_stop();
                    lua.set_memory_limit(lua.used_memory() + 32)?;
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
        "local f=plum.plugin.asset('buffer.bin');limit_vm();local ok=pcall(function()return f:read_all()end);restore_vm();assert(not ok);f:seek(0);assert(#f:read_all()==1048576)",
    );
    clear(&h);
    run(
        &h,
        "assert(plum.plugin.asset('data.bin'):read_all()=='fixture')",
    );
    clear(&h);
}

#[test]
fn retained_assets_expire_on_success_error_cancel_and_plugin_authority_change() {
    let temp = Temp::new();
    std::fs::write(temp.root.join("data.bin"), b"fixture").unwrap();
    let h = host(temp.root.clone());
    run(
        &h,
        "held=plum.plugin.asset('data.bin');assert(held:read(2)=='fi')",
    );
    assert_eq!(
        h.lua.app_data_ref::<NativeBudget>().unwrap().used(),
        METADATA_BYTES
    );
    run(&h, "assert(not pcall(function()held:read()end));held=nil");
    clear(&h);
    assert_eq!(
        h.run(
            "asset-error.lua",
            "held=plum.plugin.asset('data.bin');error('fixture')"
        )
        .unwrap_err()
        .category,
        Category::Runtime
    );
    run(&h, "assert(not pcall(function()held:info()end));held=nil");
    clear(&h);
    let cancel = h.cancel_handle();
    h.lua
        .globals()
        .set(
            "cancel_now",
            h.lua
                .create_function(move |_, ()| {
                    cancel.store(true, Ordering::Release);
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    assert_eq!(
        h.run(
            "asset-cancel.lua",
            "held=plum.plugin.asset('data.bin');cancel_now();held:read() "
        )
        .unwrap_err()
        .category,
        Category::Canceled
    );
    run(&h, "assert(not pcall(function()held:read()end));held=nil");
    clear(&h);
    h.lua
        .globals()
        .set(
            "other_owner",
            h.lua
                .create_function(|lua, other: bool| {
                    lua.app_data_mut::<crate::lua::plugin_api::PluginSlot>()
                        .unwrap()
                        .meta
                        .id = if other { "test.other" } else { "test.assets" }.into();
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        "local f=plum.plugin.asset('data.bin');other_owner(true);assert(not pcall(function()f:read()end));other_owner(false);assert(f:read_all()=='fixture')",
    );
    clear(&h);
}

#[cfg(windows)]
#[test]
fn actual_windows_junction_hardlink_and_pinned_rename_refusals_preserve_outside_data() {
    use std::os::windows::process::CommandExt;
    let temp = Temp::new();
    let package = temp.root.join("package");
    let outside = temp.root.join("outside");
    std::fs::create_dir(&package).unwrap();
    std::fs::create_dir(&outside).unwrap();
    let private = outside.join("private.bin");
    std::fs::write(&private, b"outside fixture").unwrap();
    std::fs::hard_link(&private, package.join("hard.bin")).unwrap();
    let junction = package.join("junction");
    let made = std::process::Command::new("cmd.exe")
        .creation_flags(0x0800_0000)
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&junction)
        .arg(&outside)
        .output()
        .unwrap();
    assert!(made.status.success());
    std::fs::write(package.join("ok.bin"), b"inside").unwrap();
    let h = host(package.clone());
    let pinned = package.clone();
    h.lua
        .globals()
        .set(
            "try_native_replace",
            h.lua
                .create_function(move |_, ()| {
                    assert!(
                        std::fs::rename(pinned.join("ok.bin"), pinned.join("moved.bin")).is_err()
                    );
                    assert!(
                        OpenOptions::new()
                            .write(true)
                            .open(pinned.join("ok.bin"))
                            .is_err()
                    );
                    assert!(
                        std::fs::rename(&pinned, pinned.with_file_name("moved-package")).is_err()
                    );
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        r#"
      local ok,e=pcall(plum.plugin.asset,'hard.bin');assert(not ok and tostring(e):find('hard links'))
      assert(not pcall(plum.plugin.asset,'junction/private.bin'))
      local f=plum.plugin.asset('ok.bin');try_native_replace();assert(f:read_all()=='inside');f:close()
    "#,
    );
    clear(&h);
    assert_eq!(std::fs::read(&private).unwrap(), b"outside fixture");
    // Remove only this verified fixture junction itself, never recurse through it.
    assert_eq!(junction.parent(), Some(package.as_path()));
    std::fs::remove_dir(&junction).unwrap();
    std::fs::rename(package.join("ok.bin"), package.join("moved.bin")).unwrap();
}
