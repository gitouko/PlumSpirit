use super::*;
use std::time::{Duration, Instant};

fn fixture() -> (Request, Vec<u8>) {
    let defs = egui::FontDefinitions::default();
    let font = &defs.font_data[&defs.families[&egui::FontFamily::Proportional][0]];
    (
        Request {
            face: font.index,
            text: "office e\u{301} éΣ".into(),
            direction: None,
            script: None,
            language: Some("en".into()),
            features: vec![],
            cap: 4096,
        },
        font.font.to_vec(),
    )
}
fn wire(request: &Request, font: &[u8]) -> Vec<u8> {
    let h = request.header(font.len()).unwrap();
    let mut out = REQUEST.to_vec();
    out.extend_from_slice(&(h.len() as u32).to_le_bytes());
    out.extend_from_slice(&h);
    out.extend_from_slice(font);
    out
}
#[test]
fn strict_protocol_preserves_native_glyph_positions_clusters_and_rejects_malformed_data() {
    let (request, font) = fixture();
    let bytes = wire(&request, &font);
    let (parsed, copy) = read_request(&mut bytes.as_slice()).unwrap();
    assert_eq!(parsed.text, request.text);
    assert_eq!(copy, font);
    let run = shape(&request, &font).unwrap();
    assert!(!run.glyphs.is_empty());
    assert_eq!(decode(&encode(&run).unwrap()).unwrap(), run);
    for n in [0, 1, 7, 8, 10, 12, bytes.len() - 1] {
        assert!(read_request(&mut &bytes[..n]).is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(read_request(&mut trailing.as_slice()).is_err());
    let reply = encode(&run).unwrap();
    for n in [0, 1, 7, 8, 13, 17, reply.len() - 1] {
        assert!(decode(&reply[..n]).is_err());
    }
    let mut unknown = reply.clone();
    unknown[8] = 9;
    assert!(decode(&unknown).is_err());
    let mut extra = reply;
    extra.push(0);
    assert!(decode(&extra).is_err());
    assert!(decode(&vec![0; MAX_REPLY + 1]).is_err());
    let mut invalid = request.clone();
    invalid.text = "a".repeat(257);
    assert!(invalid.header(font.len()).is_err());
    invalid = request.clone();
    invalid.features = vec![("liga".into(), 1), ("liga".into(), 0)];
    assert!(invalid.header(font.len()).is_err());
    invalid = request.clone();
    invalid.language = Some("en_US".into());
    assert!(invalid.header(font.len()).is_err());
    invalid = request.clone();
    invalid.cap = 0;
    assert!(invalid.header(font.len()).is_err());
    assert!(request.header(MAX_FONT + 1).is_err());
    assert!(shape(&request, b"malformed").is_err());
}
#[cfg(windows)]
#[test]
fn actual_isolated_native_shapes_equal_trusted_native_pipeline_for_directions_features_and_clusters()
 {
    let (mut request, font) = fixture();
    for (text, direction) in [
        ("office e\u{301} éΣ", None),
        ("office", Some("rtl")),
        ("ABC", Some("ttb")),
        ("", None),
    ] {
        request.text = text.into();
        request.direction = direction.map(str::to_owned);
        request.features = vec![("liga".into(), 0)];
        let expected = shape(&request, &font).unwrap();
        let actual = run(&request, &font, || Ok::<_, ()>(())).unwrap();
        assert_eq!(actual, expected);
    }
}
#[cfg(windows)]
#[test]
fn actual_windows_job_refuses_a_commit_above_its_memory_limit() {
    let (request, font) = fixture();
    let result = process::test_run(
        &request,
        &font,
        "memory",
        Duration::from_millis(MAX_MILLIS),
        || Ok::<_, ()>(()),
    )
    .unwrap();
    assert_eq!(
        result.glyphs[0].id, 1,
        "The Windows job must refuse a256MiBallocation before touching font caches"
    );
}
#[cfg(windows)]
#[test]
fn cpu_bound_helper_obeys_parent_wall_deadline_and_windows_job_refuses_descendants() {
    let (request, font) = fixture();
    let start = Instant::now();
    let cpu = process::test_run(
        &request,
        &font,
        "cpu",
        Duration::from_millis(MAX_MILLIS),
        || Ok::<_, ()>(()),
    );
    assert!(
        matches!(cpu, Err(Error::Timeout) | Err(Error::Refused(_))),
        "CPU-bound work must stop at the parent wall deadline or earlier OS refusal: {cpu:?}"
    );
    assert!(start.elapsed() < Duration::from_secs(3));
    let child = process::test_run(
        &request,
        &font,
        "process",
        Duration::from_millis(MAX_MILLIS),
        || Ok::<_, ()>(()),
    )
    .unwrap();
    assert_eq!(
        child.glyphs[0].id, 1,
        "An external font helper cannot create descendants"
    );
}
#[cfg(windows)]
#[test]
fn timeout_cancellation_reply_overflow_and_crash_terminate_and_join_all_pipe_workers() {
    let (request, font) = fixture();
    let start = Instant::now();
    let timeout = process::test_run(&request, &font, "sleep", Duration::from_millis(80), || {
        Ok::<_, ()>(())
    });
    assert!(matches!(timeout, Err(Error::Timeout)));
    assert!(start.elapsed() < Duration::from_secs(3));
    let mut checks = 0;
    let start = Instant::now();
    let cancel = process::test_run(&request, &font, "sleep", Duration::from_secs(5), || {
        checks += 1;
        if checks == 8 {
            Err("captured cancel")
        } else {
            Ok(())
        }
    });
    assert!(matches!(cancel, Err(Error::Stopped("captured cancel"))));
    assert!(start.elapsed() < Duration::from_secs(3));
    let flood = process::test_run(
        &request,
        &font,
        "flood",
        Duration::from_millis(MAX_MILLIS),
        || Ok::<_, ()>(()),
    );
    assert!(matches!(flood, Err(Error::Refused(_))));
    let crash = process::test_run(
        &request,
        &font,
        "crash",
        Duration::from_millis(MAX_MILLIS),
        || Ok::<_, ()>(()),
    );
    assert!(matches!(crash, Err(Error::Refused(_))));
    // Same worker can launch and validate a normal native job after every refusal.
    assert_eq!(
        run(&request, &font, || Ok::<_, ()>(())).unwrap(),
        shape(&request, &font).unwrap()
    );
}

// A fresh test binary runs only this entrypoint. The native process test path
// strips libtest's one fixed header; production uses the GUI-free main branch.
#[test]
fn child_entrypoint() {
    let Ok(mode) = std::env::var("PLUMSPIRIT_NATIVE_FONT_TEST") else {
        return;
    };
    if mode == "shape" {
        std::process::exit(i32::from(child_main()));
    }
    process::prepare_child();
    let (_request, _font) = read_request(&mut std::io::stdin().lock()).unwrap();
    process::verify_fence().unwrap();
    match mode.as_str() {
        "sleep" => std::thread::sleep(Duration::from_secs(30)),
        "memory" => {
            let mut bytes = Vec::<u8>::new();
            let refused = bytes.try_reserve_exact(MEMORY_BYTES * 2).is_err();
            let run = Run {
                direction: "ltr".into(),
                script: "Latn".into(),
                language: None,
                glyphs: vec![Glyph {
                    id: u32::from(refused),
                    cluster: 0,
                    xa: 0,
                    ya: 0,
                    xo: 0,
                    yo: 0,
                    unsafe_break: false,
                }],
            };
            std::io::stdout()
                .lock()
                .write_all(&encode(&run).unwrap())
                .unwrap();
        }
        "flood" => {
            let mut out = std::io::stdout().lock();
            let chunk = [0u8; 4096];
            for _ in 0..1024 {
                if out.write_all(&chunk).is_err() {
                    break;
                }
            }
        }
        "crash" => std::process::exit(7),
        "cpu" => loop {
            std::hint::black_box(871231u64.wrapping_mul(812367));
        },
        "process" => {
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                let child = std::process::Command::new("cmd.exe")
                    .args(["/d", "/c", "exit", "0"])
                    .creation_flags(0x0800_0000)
                    .spawn();
                let refused = match child {
                    Err(_) => true,
                    Ok(mut child) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        false
                    }
                };
                let run = Run {
                    direction: "ltr".into(),
                    script: "Latn".into(),
                    language: None,
                    glyphs: vec![Glyph {
                        id: u32::from(refused),
                        cluster: 0,
                        xa: 0,
                        ya: 0,
                        xo: 0,
                        yo: 0,
                        unsafe_break: false,
                    }],
                };
                std::io::stdout()
                    .lock()
                    .write_all(&encode(&run).unwrap())
                    .unwrap();
            }
        }
        _ => std::process::exit(8),
    }
    std::process::exit(0);
}
