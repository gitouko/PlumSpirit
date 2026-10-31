//! B1-0020 / B1-0021 / B1-0024: the host's versions, sandbox, limits and
//! error categories.

use super::*;

fn host() -> Host {
    Host::new(Limits::default(), [Capability::ReadProject]).expect("host")
}

#[test]
fn versions_and_capability_discovery_are_published() {
    let h = host();
    assert!(
        h.lua_version().starts_with("Lua 5.4"),
        "{}",
        h.lua_version()
    );
    assert_eq!(
        h.run("v.lua", "return plum.version").unwrap(),
        Plain::String(env!("CARGO_PKG_VERSION").into())
    );
    assert_eq!(
        h.run("v.lua", "return plum.api_version").unwrap(),
        Plain::String(api_version())
    );
    assert_eq!(
        h.run("v.lua", "return plum.api.major * 1000 + plum.api.minor")
            .unwrap(),
        Plain::Integer(i64::from(API_VERSION.0) * 1000 + i64::from(API_VERSION.1))
    );
    assert_eq!(
        h.run("v.lua", "return plum.has('project.read')").unwrap(),
        Plain::Boolean(true)
    );
    assert_eq!(
        h.run("v.lua", "return plum.has('notes.edit')").unwrap(),
        Plain::Boolean(false)
    );
    assert_eq!(
        h.run("v.lua", "return plum.has('no.such')").unwrap(),
        Plain::Boolean(false)
    );
    // Every capability is listed, granted or not.
    let n = h
        .run(
            "v.lua",
            "local n = 0 for k, v in pairs(plum.capabilities()) do n = n + 1 end return n",
        )
        .unwrap();
    assert_eq!(n, Plain::Integer(Capability::ALL.len() as i64));
    for c in Capability::ALL {
        assert_eq!(Capability::parse(c.name()), Some(c));
    }
    // Compatibility: same major, equal or older minor.
    assert_eq!(
        h.run(
            "v.lua",
            &format!("return plum.require_api({})", API_VERSION.0)
        )
        .unwrap(),
        Plain::Boolean(true)
    );
    let e = h
        .run(
            "v.lua",
            &format!("plum.require_api({}, {})", API_VERSION.0, API_VERSION.1 + 1),
        )
        .unwrap_err();
    assert_eq!(e.category, Category::Compatibility, "{e:?}");
    let e = h
        .run("v.lua", &format!("plum.require_api({})", API_VERSION.0 + 1))
        .unwrap_err();
    assert_eq!(e.category, Category::Compatibility);
    assert!(e.message.contains(&api_version()), "{e}");
}

/// Nothing outside the sandbox is reachable (B1-0021).
#[test]
fn the_sandbox_keeps_out_files_processes_modules_and_bytecode() {
    let h = host();
    for (what, src) in [
        ("io", "return io == nil"),
        ("os.execute", "return os.execute == nil"),
        ("os.remove", "return os.remove == nil"),
        ("os.getenv", "return os.getenv == nil"),
        ("os.exit", "return os.exit == nil"),
        ("require", "return require == nil"),
        ("package", "return package == nil"),
        ("debug", "return debug == nil"),
        ("dofile", "return dofile == nil"),
        ("loadfile", "return loadfile == nil"),
        ("string.dump", "return string.dump == nil"),
    ] {
        assert_eq!(h.run("s.lua", src).unwrap(), Plain::Boolean(true), "{what}");
    }
    // What is there works.
    assert_eq!(
        h.run(
            "s.lua",
            "return #string.rep('ab', 3) + math.floor(2.5) + utf8.len('é')"
        )
        .unwrap(),
        Plain::Integer(9)
    );
    assert!(matches!(
        h.run("s.lua", "return os.clock()").unwrap(),
        Plain::Number(_)
    ));
    // load reads text only: a binary chunk is refused.
    let r = h
        .run(
            "s.lua",
            "local f, err = load('\\27Lua' .. string.rep('x', 40)) return f == nil and err ~= nil",
        )
        .unwrap();
    assert_eq!(r, Plain::Boolean(true));
    assert_eq!(
        h.run("s.lua", "return load('return 1 + 2')()").unwrap(),
        Plain::Integer(3)
    );
    // collectgarbage: reading / collecting only.
    assert!(matches!(
        h.run("s.lua", "return collectgarbage('count')").unwrap(),
        Plain::Number(_)
    ));
    let e = h.run("s.lua", "collectgarbage('stop')").unwrap_err();
    assert_eq!(e.category, Category::Capability, "{e:?}");
    // print goes to the host.
    h.run("s.lua", "print('hello', 42, nil)").unwrap();
    assert_eq!(h.take_output(), vec!["hello\t42\tnil".to_owned()]);
    assert!(h.take_output().is_empty());
}

/// Syntax and runtime errors name the file and line (B1-0024).
#[test]
fn errors_name_the_category_file_and_line() {
    let h = host();
    let e = h.run("bad.lua", "local x = \n\n  = 3").unwrap_err();
    assert_eq!(e.category, Category::Syntax, "{e:?}");
    assert_eq!(e.file.as_deref(), Some("bad.lua"));
    assert_eq!(e.line, Some(3));
    let e = h
        .run("boom.lua", "local t = nil\nlocal y = 1\nreturn t.x")
        .unwrap_err();
    assert_eq!(e.category, Category::Runtime, "{e:?}");
    assert_eq!((e.file.as_deref(), e.line), (Some("boom.lua"), Some(3)));
    assert!(e.message.contains("nil"), "{e}");
    assert!(
        e.to_string().starts_with("Error in boom.lua, line 3: "),
        "{e}"
    );
    let e = h.run("err.lua", "\nerror('my own message')").unwrap_err();
    assert_eq!((e.category, e.line), (Category::Runtime, Some(2)));
    assert_eq!(e.message, "my own message");
    // error with a non-string value
    let e = h.run("err.lua", "error({code = 7})").unwrap_err();
    assert_eq!(e.category, Category::Runtime);
}

/// The memory quota and the time budget stop a run; the state stays usable
/// (B1-0021).
#[test]
fn memory_and_time_limits_stop_a_script_and_the_host_goes_on() {
    // The memory quota first, with time to spare even on a loaded machine
    // (a 0.2 s budget sometimes ran out before 8 MB were filled).
    let m = Host::new(
        Limits {
            memory: 8 * 1024 * 1024,
            time: Duration::from_secs(60),
            ..Limits::default()
        },
        [],
    )
    .unwrap();
    let e = m
        .run(
            "mem.lua",
            "local t = {} for i = 1, 1e9 do t[i] = string.rep('x', 64) .. i end",
        )
        .unwrap_err();
    assert_eq!(e.category, Category::Memory, "{e:?}");
    assert_eq!(m.run("ok.lua", "return 1").unwrap(), Plain::Integer(1));
    let h = Host::new(
        Limits {
            memory: 8 * 1024 * 1024,
            time: Duration::from_millis(200),
            ..Limits::default()
        },
        [],
    )
    .unwrap();
    let t = Instant::now();
    let e = h
        .run("loop.lua", "local n = 0\nwhile true do n = n + 1 end")
        .unwrap_err();
    assert_eq!(e.category, Category::TimeLimit, "{e:?}");
    assert_eq!(e.file.as_deref(), Some("loop.lua"));
    assert_eq!(e.line, Some(2));
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    // The next run has its own budget and works.
    assert_eq!(h.run("ok.lua", "return 6 * 7").unwrap(), Plain::Integer(42));
    // B1-0040: a run the user started gets a longer budget for itself; the
    // runs after it are back to the host's 0.2 s.
    let spin = "local t = os.clock() while os.clock() - t < 0.5 do end return 1";
    h.set_time_limit(Some(Duration::from_secs(30)));
    assert_eq!(h.run("long.lua", spin).unwrap(), Plain::Integer(1));
    h.set_time_limit(None);
    let e = h.run("long.lua", spin).unwrap_err();
    assert_eq!(e.category, Category::TimeLimit);
    assert!(e.to_string().contains("longer than 0.2 s"), "{e}");
}

/// Cancellation from another thread (the user, or closing PlumSpirit).
#[test]
fn a_running_script_is_cancelled_from_another_thread() {
    let h = Host::new(
        Limits {
            time: Duration::from_secs(60),
            ..Limits::default()
        },
        [],
    )
    .unwrap();
    let flag = h.cancel_handle();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        flag.store(true, Ordering::SeqCst);
    });
    let started = Instant::now();
    let e = h.run("spin.lua", "while true do end").unwrap_err();
    t.join().unwrap();
    assert_eq!(e.category, Category::Canceled, "{e:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
    // The flag is lowered for the next run.
    assert_eq!(h.run("ok.lua", "return 1").unwrap(), Plain::Integer(1));
}

/// A panicking host function never crosses into the app; capability checks
/// refuse with their own category (B1-0024).
#[test]
fn a_panic_in_a_host_function_is_an_internal_error_and_capabilities_are_checked() {
    let h = host();
    h.register("explode", |_, _| -> mlua::Result<mlua::MultiValue> {
        panic!("test panic")
    })
    .unwrap();
    let e = h.run("p.lua", "plum.explode()").unwrap_err();
    assert_eq!(e.category, Category::Internal, "{e:?}");
    assert!(e.message.contains("test panic"), "{e}");
    assert_eq!(
        h.run("ok.lua", "return 'still here'").unwrap(),
        Plain::String("still here".into())
    );
    let granted = h.granted();
    h.register("edit_something", move |_, _| {
        Host::require(&granted, Capability::EditNotes)?;
        Ok(mlua::MultiValue::new())
    })
    .unwrap();
    let e = h.run("c.lua", "plum.edit_something()").unwrap_err();
    assert_eq!(e.category, Category::Capability, "{e:?}");
    assert!(e.message.contains("notes.edit"), "{e}");
}

/// 64-bit integers stay exact (ticks, counts) — no float rounding.
#[test]
fn integers_are_exact_64_bit() {
    let h = host();
    assert_eq!(
        h.run("i.lua", "return plum.max_integer").unwrap(),
        Plain::Integer(i64::MAX)
    );
    let big = (1i64 << 62) + 1;
    assert_eq!(
        h.run("i.lua", &format!("return {big} + 0")).unwrap(),
        Plain::Integer(big)
    );
    assert_eq!(
        h.run("i.lua", "return math.type(9007199254740993)")
            .unwrap(),
        Plain::String("integer".into())
    );
    assert_eq!(
        h.run("i.lua", "return 9007199254740993 - 9007199254740992")
            .unwrap(),
        Plain::Integer(1)
    );
}

#[test]
fn places_are_split_from_lua_messages() {
    assert_eq!(
        split_place("a/b.lua:12: oops"),
        (Some("a/b.lua".into()), Some(12), "oops".into())
    );
    assert_eq!(
        split_place("C:\\x\\y.lua:3: bad"),
        (Some("C:\\x\\y.lua".into()), Some(3), "bad".into())
    );
    assert_eq!(
        split_place("no place here"),
        (None, None, "no place here".into())
    );
    assert_eq!(
        split_place("[string \"x\"]:1: e\nstack traceback:\n\t..."),
        (Some("[string \"x\"]".into()), Some(1), "e".into())
    );
}

/// B1-0024: closing PlumSpirit cancels every running script through the
/// app's registry without joining it.
#[test]
fn closing_cancels_running_scripts_without_waiting() {
    let scripts = Scripts::default();
    let h = Host::new(
        Limits {
            time: Duration::from_secs(60),
            ..Limits::default()
        },
        [],
    )
    .unwrap();
    scripts.add(&h);
    let reg = scripts.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        let started = Instant::now();
        let told = reg.cancel_all();
        (told, started.elapsed())
    });
    let started = Instant::now();
    let e = h.run("spin.lua", "while true do end").unwrap_err();
    let (told, took) = t.join().unwrap();
    assert_eq!(e.category, Category::Canceled, "{e:?}");
    assert_eq!(told, 1);
    assert!(took < Duration::from_millis(50), "cancel_all never waits");
    assert!(started.elapsed() < Duration::from_secs(5));
    // A dropped host is forgotten.
    drop(h);
    assert_eq!(scripts.cancel_all(), 0);
}
