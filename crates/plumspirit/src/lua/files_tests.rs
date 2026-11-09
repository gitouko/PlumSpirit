//! Actual OS file I/O with controlled UI replies. Native picker UI is a
//! separate acceptance check; this harness does not simulate that gate.
use super::*;
use crate::lua::{Limits, Plain, ScriptError};
use std::time::Duration;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!(
            "plum-api02-files-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    fn entries(&self) -> usize {
        std::fs::read_dir(&self.0).unwrap().count()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(
    source: &str,
    paths: Vec<Option<PathBuf>>,
    caps: Vec<Capability>,
    memory: usize,
) -> Result<Plain, ScriptError> {
    let (client, ui) = super::super::bridge::channel("test.files");
    let source = source.to_owned();
    let worker = std::thread::spawn(move || {
        let host = Host::new(
            Limits {
                memory,
                ..Limits::default()
            },
            caps,
        )
        .unwrap();
        host.set_plugin(crate::lua::plugin_api::PluginMeta {
            id: "test.files".into(),
            name: "File Conformance".into(),
            version: "1.0.0".into(),
            root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples-lua/file-transform"),
            data_dir: None,
            config_version: 1,
        })
        .unwrap();
        {
            let mut session = host.lua.app_data_mut::<Session>().unwrap();
            session.client = Some(client);
            session.interactive = true;
        }
        host.run("files-test.lua", &source)
    });
    for path in paths {
        ui.recv_timeout(Duration::from_secs(3))
            .unwrap()
            .respond(None, path)
            .unwrap();
    }
    worker.join().unwrap()
}

const MEMORY: usize = 16 * 1024 * 1024;

#[test]
fn permission_and_path_options_fail_before_a_picker_is_queued() {
    let denied = run("return plum.files.open()", vec![], vec![], MEMORY).unwrap_err();
    assert_eq!(denied.category, Category::Capability);
    for options in [
        "{path='C:/private.txt'}",
        "{name='../escape'}",
        "{filters={{name='Bad',extensions={'../png'}}}}",
    ] {
        let error = run(
            &format!("return plum.files.open({options})"),
            vec![],
            vec![Capability::ReadUserFile],
            MEMORY,
        )
        .unwrap_err();
        assert_eq!(error.category, Category::Runtime);
    }
}

#[test]
fn picker_cancellation_is_nil_and_has_no_file_effect() {
    assert_eq!(
        run(
            "return plum.files.open() == nil",
            vec![None],
            vec![Capability::ReadUserFile],
            MEMORY
        )
        .unwrap(),
        Plain::Boolean(true)
    );
    assert_eq!(
        run(
            "return plum.files.save() == nil",
            vec![None],
            vec![Capability::WriteUserFile],
            MEMORY
        )
        .unwrap(),
        Plain::Boolean(true)
    );
}

#[test]
fn binary_chunks_eof_seek_and_small_read_all_are_exact() {
    let temp = Temp::new();
    let path = temp.file("binary.dat");
    std::fs::write(&path, [0, 255, 13, 10, 65, 66, 67]).unwrap();
    let result = run(
        r#"
        local f = plum.files.open()
        local i = f:info()
        assert(i.name == 'binary.dat' and i.size == 7 and i.offset == 0 and i.mode == 'read')
        assert(i.path == nil)
        assert(f:read(2) == string.char(0,255))
        assert(f:read(2) == '\r\n')
        assert(f:seek(4) == 4)
        assert(f:read_all() == 'ABC')
        assert(f:read() == '')
        assert(not pcall(function() f:seek(8) end))
        assert(not pcall(function() f:read(0) end))
        assert(not pcall(function() f:read(plum.files.max_chunk+1) end))
        f:close()
        assert(not pcall(function() f:info() end))
        return true
    "#,
        vec![Some(path)],
        vec![Capability::ReadUserFile],
        MEMORY,
    )
    .unwrap();
    assert_eq!(result, Plain::Boolean(true));
}

#[test]
fn write_is_staged_then_atomically_replaces_exact_binary_bytes() {
    let temp = Temp::new();
    let path = temp.file("result.bin");
    std::fs::write(&path, b"original").unwrap();
    assert_eq!(
        run(
            r#"
        local w = plum.files.save{name='result.bin'}
        assert(w:write('a\0b') == 3)
        assert(w:write(string.char(255,13,10)) == 3)
        assert(w:info().size == 6)
        assert(w:commit())
        assert(not pcall(function() w:write('bad') end))
        return true
    "#,
            vec![Some(path.clone())],
            vec![Capability::WriteUserFile],
            MEMORY
        )
        .unwrap(),
        Plain::Boolean(true)
    );
    assert_eq!(std::fs::read(path).unwrap(), [97, 0, 98, 255, 13, 10]);
    assert_eq!(temp.entries(), 1);
}

#[test]
fn failed_or_abandoned_staging_keeps_the_original_and_removes_temps() {
    let temp = Temp::new();
    let path = temp.file("original.txt");
    std::fs::write(&path, b"keep me").unwrap();
    for source in [
        "local w=plum.files.save(); w:write('partial'); error('failure halfway')",
        "local w=plum.files.save(); w:write('partial'); w:close()",
        "local w=plum.files.save(); w:write('partial'); assert(not pcall(function() w:write(string.rep('x',plum.files.max_chunk+1)) end)); assert(not pcall(function() w:commit() end))",
    ] {
        let _ = run(
            source,
            vec![Some(path.clone())],
            vec![Capability::WriteUserFile],
            MEMORY,
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"keep me");
        assert_eq!(temp.entries(), 1, "staging file leaked");
    }
}

#[test]
fn a_retained_lua_file_is_closed_and_expired_in_the_next_run() {
    let temp = Temp::new();
    let path = temp.file("input.txt");
    std::fs::write(&path, b"test").unwrap();
    let (client, ui) = super::super::bridge::channel("test.files");
    let worker = std::thread::spawn(move || {
        let host = Host::new(Limits::default(), [Capability::ReadUserFile]).unwrap();
        {
            let mut session = host.lua.app_data_mut::<Session>().unwrap();
            session.client = Some(client);
            session.interactive = true;
        }
        host.run(
            "one.lua",
            "held=plum.files.open(); assert(held:read_all()=='test')",
        )
        .unwrap();
        host.run("two.lua", "return held:read_all()").unwrap_err()
    });
    ui.recv_timeout(Duration::from_secs(3))
        .unwrap()
        .respond(None, Some(path))
        .unwrap();
    assert_eq!(worker.join().unwrap().category, Category::Expired);
    assert_eq!(temp.entries(), 1);
}

#[test]
fn a_256_mib_file_streams_with_an_8_mib_lua_quota() {
    let temp = Temp::new();
    let path = temp.file("large.bin");
    File::create(&path)
        .unwrap()
        .set_len(256 * 1024 * 1024)
        .unwrap();
    let result = run(
        r#"
        local f=plum.files.open()
        assert(not pcall(function() f:read_all() end))
        assert(f:info().offset == 0)
        local total=0
        repeat
            local bytes=f:read(plum.files.max_chunk)
            total=total+#bytes
            if #bytes==0 then break end
        until false
        return total
    "#,
        vec![Some(path)],
        vec![Capability::ReadUserFile],
        8 * 1024 * 1024,
    )
    .unwrap();
    assert_eq!(result, Plain::Integer(256 * 1024 * 1024));
}

#[test]
fn unsafe_windows_names_and_streams_are_rejected() {
    for name in [
        "..",
        "CON",
        "nul.txt",
        "COM1.mid",
        "lpt9",
        "a:b",
        "trailing.",
        "x/../y",
        "\\server\\file",
        "x\n",
    ] {
        assert!(!valid_name(name), "{name}");
    }
    assert!(valid_name("notes.test.bin"));
    assert!(valid_name("Café.ttf"));
}

#[test]
fn output_rename_terminates_every_utf16_buffer_alignment() {
    let temp = Temp::new();
    for length in 1..=8 {
        let path = temp.file(&format!("{}.bin", "x".repeat(length)));
        assert_eq!(
            run(
                "local w=plum.files.save(); w:write('exact'); return w:commit()",
                vec![Some(path.clone())],
                vec![Capability::WriteUserFile],
                MEMORY
            )
            .unwrap(),
            Plain::Boolean(true)
        );
        assert_eq!(std::fs::read(path).unwrap(), b"exact");
    }
    assert_eq!(temp.entries(), 8);
}

#[test]
fn api02_file_sample_streams_utf8_boundaries_and_rejects_invalid_text() {
    let temp = Temp::new();
    let input = temp.file("source.txt");
    let output = temp.file("output.txt");
    let mut bytes = vec![b'a'; 65_535];
    bytes.extend_from_slice("é\r\nbZ".as_bytes());
    std::fs::write(&input, &bytes).unwrap();
    let sample = include_str!("../../examples-lua/file-transform/main.lua");
    let source = format!(
        "{sample}\nlocal command; register{{command=function(d) command=d.run end}}; command(); return true"
    );
    assert_eq!(
        run(
            &source,
            vec![Some(input.clone()), Some(output.clone())],
            vec![Capability::ReadUserFile, Capability::WriteUserFile],
            MEMORY
        )
        .unwrap(),
        Plain::Boolean(true)
    );
    let mut expected = vec![b'A'; 65_535];
    expected.extend_from_slice("é\r\nBZ".as_bytes());
    assert_eq!(std::fs::read(&output).unwrap(), expected);
    std::fs::write(&input, [b'a', 255, b'b']).unwrap();
    assert!(
        run(
            &source,
            vec![Some(input), Some(output.clone())],
            vec![Capability::ReadUserFile, Capability::WriteUserFile],
            MEMORY
        )
        .is_err()
    );
    assert_eq!(std::fs::read(output).unwrap(), expected);
    assert_eq!(temp.entries(), 2);
}

#[test]
fn both_01_and_02_version_requirements_work_and_03_is_rejected() {
    let host = Host::new(Limits::default(), []).unwrap();
    assert_eq!(
        host.run(
            "version.lua",
            "assert(plum.require_api(0,1)); assert(plum.require_api(0,2)); return plum.api_version"
        )
        .unwrap(),
        Plain::String("0.2.0".into())
    );
    assert_eq!(
        host.run("new.lua", "plum.require_api(0,3)")
            .unwrap_err()
            .category,
        Category::Compatibility
    );
}

#[cfg(windows)]
#[test]
fn junction_parents_are_rejected_before_opening_the_file() {
    let temp = Temp::new();
    let target = temp.file("target");
    let link = temp.file("junction");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("data.txt"), b"private fixture").unwrap();
    // Fixture setup only, never reachable from Lua or production code.
    let created = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&link)
        .arg(&target)
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "Could not create the Windows junction test fixture"
    );
    let error = run(
        "return plum.files.open()",
        vec![Some(link.join("data.txt"))],
        vec![Capability::ReadUserFile],
        MEMORY,
    )
    .unwrap_err();
    assert!(
        error.message.contains("Reparse") || error.message.contains("junction"),
        "{error}"
    );
    assert_eq!(
        std::fs::read(target.join("data.txt")).unwrap(),
        b"private fixture"
    );
}

#[test]
fn cumulative_io_budget_rejects_the_next_chunk_and_discards_output() {
    let temp = Temp::new();
    let path = temp.file("output.txt");
    std::fs::write(&path, b"original").unwrap();
    let (client, ui) = super::super::bridge::channel("test.files");
    let worker = std::thread::spawn(move || {
        let host = Host::new(Limits::default(), [Capability::WriteUserFile]).unwrap();
        {
            let mut s = host.lua.app_data_mut::<Session>().unwrap();
            s.client = Some(client);
            s.interactive = true;
        }
        host.register("near_budget_for_test", |lua, _| {
            lua.app_data_ref::<FileSlot>()
                .unwrap()
                .0
                .as_ref()
                .unwrap()
                .written
                .set(MAX_JOB_BYTES - 1);
            Ok(mlua::MultiValue::new())
        })
        .unwrap();
        host.run("budget.lua","local w=plum.files.save(); plum.near_budget_for_test(); w:write('a'); w:write('b'); w:commit()").unwrap_err()
    });
    ui.recv_timeout(Duration::from_secs(3))
        .unwrap()
        .respond(None, Some(path.clone()))
        .unwrap();
    assert_eq!(worker.join().unwrap().category, Category::Budget);
    assert_eq!(std::fs::read(path).unwrap(), b"original");
    assert_eq!(temp.entries(), 1);
}

#[cfg(windows)]
#[test]
fn parent_pins_are_released_at_job_end_even_with_retained_userdata() {
    let temp = Temp::new();
    let folder = temp.file("folder");
    let moved = temp.file("moved");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("a.txt"), b"a").unwrap();
    let (client, ui) = super::super::bridge::channel("test.files");
    let from = folder.clone();
    let to = moved.clone();
    let worker = std::thread::spawn(move || {
        let host = Host::new(Limits::default(), [Capability::ReadUserFile]).unwrap();
        {
            let mut s = host.lua.app_data_mut::<Session>().unwrap();
            s.client = Some(client);
            s.interactive = true;
        }
        host.register("check_pins_for_test", move |_, _| {
            assert!(
                std::fs::rename(&from, &to).is_err(),
                "Parent was not pinned"
            );
            Ok(mlua::MultiValue::new())
        })
        .unwrap();
        host.run(
            "pins.lua",
            "retained=plum.files.open(); plum.check_pins_for_test()",
        )
        .unwrap();
        host.run("expired.lua", "return retained:info()")
            .unwrap_err()
    });
    ui.recv_timeout(Duration::from_secs(3))
        .unwrap()
        .respond(None, Some(folder.join("a.txt")))
        .unwrap();
    assert_eq!(worker.join().unwrap().category, Category::Expired);
    std::fs::rename(&folder, &moved).unwrap();
    assert_eq!(std::fs::read(moved.join("a.txt")).unwrap(), b"a");
}

#[test]
fn changed_destination_and_canceled_staging_never_publish() {
    for cancel in [false, true] {
        let temp = Temp::new();
        let path = temp.file("output.txt");
        std::fs::write(&path, b"original").unwrap();
        let chosen_path = path.clone();
        let changed = path.clone();
        let (client, ui) = super::super::bridge::channel("test.files");
        let worker = std::thread::spawn(move || {
            let host = Host::new(Limits::default(), [Capability::WriteUserFile]).unwrap();
            {
                let mut s = host.lua.app_data_mut::<Session>().unwrap();
                s.client = Some(client);
                s.interactive = true;
            }
            let flag = host.cancel_handle();
            host.register("interfere_for_test", move |_, _| {
                if cancel {
                    flag.store(true, Ordering::Release);
                } else {
                    std::fs::write(&changed, b"new external contents").unwrap();
                }
                Ok(mlua::MultiValue::new())
            })
            .unwrap();
            host.run("interrupted.lua","local w=plum.files.save(); w:write('partial'); plum.interfere_for_test(); w:commit()").unwrap_err()
        });
        ui.recv_timeout(Duration::from_secs(3))
            .unwrap()
            .respond(None, Some(chosen_path))
            .unwrap();
        let error = worker.join().unwrap();
        if cancel {
            assert_eq!(error.category, Category::Canceled);
            assert_eq!(std::fs::read(path).unwrap(), b"original");
        } else {
            assert!(error.message.contains("destination changed"), "{error}");
            assert_eq!(std::fs::read(path).unwrap(), b"new external contents");
        }
        assert_eq!(temp.entries(), 1);
    }
}

#[test]
fn a_lifecycle_context_cannot_prompt_even_with_file_permission() {
    let host = Host::new(Limits::default(), [Capability::ReadUserFile]).unwrap();
    let (client, ui) = super::super::bridge::channel("test.files");
    host.lua.app_data_mut::<Session>().unwrap().client = Some(client);
    let error = host
        .run("on_load.lua", "return plum.files.open()")
        .unwrap_err();
    assert_eq!(error.category, Category::Capability);
    assert!(ui.try_recv().is_err());
}

#[test]
fn sample_manifest_uses_only_file_authorities_and_both_api_minors_load() {
    let mut manifest = crate::lua::package::Manifest::parse(include_str!(
        "../../examples-lua/file-transform/plum-plugin.toml"
    ))
    .unwrap();
    assert_eq!(manifest.api, (0, 2));
    assert_eq!(
        manifest.capabilities,
        vec!["file.read_user_choice", "file.write_user_choice"]
    );
    assert!(manifest.compatible().is_ok());
    manifest.api = (0, 1);
    assert!(manifest.compatible().is_ok());
    manifest.api = (0, 3);
    assert!(
        manifest
            .compatible()
            .unwrap_err()
            .contains("needs scripting API")
    );
}

#[test]
fn excessively_deep_paths_fail_before_any_directory_is_opened() {
    let mut path = std::env::temp_dir();
    for _ in 0..80 {
        path.push("x");
    }
    path.push("input.txt");
    let error = pin_parents(&path).unwrap_err();
    assert!(super::super::error_text(&error).contains("64-directory"));
}

#[test]
fn native_read_consumers_cannot_bypass_cancellation_or_cumulative_bytes() {
    let temp = Temp::new();
    let path = temp.file("native.bin");
    std::fs::write(&path, [0u8; 128]).unwrap();
    for cancel in [false, true] {
        let (client, ui) = super::super::bridge::channel("test.files");
        let worker = std::thread::spawn(move || {
            let host = Host::new(Limits::default(), [Capability::ReadUserFile]).unwrap();
            {
                let mut s = host.lua.app_data_mut::<Session>().unwrap();
                s.client = Some(client);
                s.interactive = true;
            }
            let flag = host.cancel_handle();
            host.register("native_read_for_test", move |lua, args| {
                let Some(Value::UserData(file)) = args.front() else {
                    panic!("expected file fixture");
                };
                if !cancel {
                    lua.app_data_ref::<FileSlot>()
                        .unwrap()
                        .0
                        .as_ref()
                        .unwrap()
                        .read
                        .set(MAX_JOB_BYTES - 1);
                }
                let result = with_reader(lua, file, |reader, _, _| {
                    let mut bytes = [0; 2];
                    assert!(reader.read(&mut bytes).unwrap() > 0);
                    if cancel {
                        flag.store(true, Ordering::Release);
                    }
                    // Even a consumer ignoring the read error cannot report success.
                    assert!(reader.read(&mut bytes).is_err());
                    Ok(())
                });
                assert_eq!(file.borrow::<LuaFile>().unwrap().state.borrow().position, 4);
                result?;
                Ok(mlua::MultiValue::new())
            })
            .unwrap();
            host.run(
                "native.lua",
                "local f=plum.files.open();f:seek(4);plum.native_read_for_test(f)",
            )
            .unwrap_err()
        });
        ui.recv_timeout(Duration::from_secs(3))
            .unwrap()
            .respond(None, Some(path.clone()))
            .unwrap();
        assert_eq!(
            worker.join().unwrap().category,
            if cancel {
                Category::Canceled
            } else {
                Category::Budget
            }
        );
    }
}
