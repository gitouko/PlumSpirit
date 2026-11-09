use super::*;
use crate::app::{commands::Command, test_support::Harness};
use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord};
use std::path::PathBuf;
use std::time::{Duration, Instant};
const ID: &str = "plumspirit.example.note-exchange";
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let p = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "plum-format-app-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let p = self.0.canonicalize().unwrap();
        let base = std::env::temp_dir().canonicalize().unwrap();
        assert!(
            p.parent() == Some(base.as_path())
                && p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("plum-format-app-")
        );
        std::fs::remove_dir_all(p).unwrap();
    }
}
fn frames(h: &mut Harness, what: &str, ready: impl Fn(&Harness) -> bool) {
    let start = Instant::now();
    while !ready(h) {
        h.app_frame(egui::vec2(1366., 768.), 1.);
        assert!(start.elapsed() < Duration::from_secs(20), "{what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn setup(keys: u16) -> (Harness, Temp) {
    let temp = Temp::new();
    let mut d = PlumDocument::new_project("Format App".into(), Bpm::DEFAULT, 960);
    if keys == 256 {
        d.project_mut().keys = plumspirit_engine::model::project::KeyRange::Extended256;
    }
    let records = (0..8)
        .map(|i| NoteRecord {
            start_tick: i * 240,
            duration_ticks: 96,
            attrs: NoteAttrs::new_wide(
                if keys == 256 { 200 } else { 60 },
                80,
                0,
                0,
                NoteOffKind::NoteOff,
            )
            .unwrap(),
            sub: Default::default(),
            group: 0,
        })
        .collect::<Vec<_>>();
    d.track_mut(0)
        .unwrap()
        .notes
        .insert_sorted_batch(&records, true)
        .unwrap();
    let mut h = Harness::new(d);
    h.app.settings.lua_plugins.folder = Some(temp.0.join("plugins"));
    h.app.settings.lua_plugins.enable_on_install = true;
    h.app_frame(egui::vec2(1366., 768.), 1.);
    let mut prefs = h.app.settings.lua_plugins.clone();
    h.app
        .lua_plugins()
        .install_example(crate::lua::examples::by_id(ID).unwrap(), &mut prefs)
        .unwrap();
    h.app.settings.lua_plugins = prefs;
    frames(&mut h, "codec loads", |h| {
        h.app.lua_plugins.as_ref().is_some_and(|m| {
            !m.is_busy() && m.plugin(ID).is_some_and(|p| p.status == Status::Ready)
        })
    });
    h.app.main_view = crate::app::track_list::MainView::PianoRoll;
    h.app_frame(egui::vec2(1366., 768.), 1.);
    (h, temp)
}
fn request(h: &mut Harness, mode: Mode) -> crate::lua::bridge::UiRequest {
    h.app.run_lua_command(ID, &action(mode, "notes"), true);
    assert!(h.app.lua_formats.pending.is_some());
    let start = Instant::now();
    // No App frame until this controlled reply: a real picker must not open.
    loop {
        if let Some(request) = h.app.lua_plugins.as_ref().unwrap().poll_ui_request() {
            assert_eq!(request.spec.save, mode == Mode::Export);
            assert_eq!(request.spec.filters[0].1, vec!["pnotes"]);
            return request;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "format picker request missing"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn choose(h: &mut Harness, mode: Mode, path: Option<PathBuf>) {
    let request = request(h, mode);
    let stamp = request.lease.project;
    request.respond(stamp, path).unwrap();
}
fn import(h: &mut Harness, temp: &Temp, keys: u16) {
    let path = temp.0.join("input.pnotes");
    let pitch = if keys == 256 { 200 } else { 60 };
    std::fs::write(
        &path,
        format!("PNOTES1\r\n19,29,{pitch},93\r\n9007199254741001,24,{pitch},96"),
    )
    .unwrap();
    choose(h, Mode::Import, Some(path));
    frames(h, "private format review", |h| {
        h.app.lua_formats.review.is_some()
    });
}
#[test]
fn actual_native_menu_review_store_preview_apply_cancel_and_one_undo_redo_both_modes() {
    for keys in [128, 256] {
        let (mut h, temp) = setup(keys);
        let original = h.doc().export_fingerprint();
        let menu = h.app.lua_menu();
        let entry = menu.iter().find(|m| m.0 == ID).unwrap();
        assert_eq!(entry.2.len(), 2);
        assert_eq!(entry.2[0].0, action(Mode::Import, "notes"));
        assert!(entry.2[0].1.starts_with("Import Note Exchange"));
        import(&mut h, &temp, keys);
        assert_eq!(h.doc().export_fingerprint(), original);
        assert!(!h.app.enabled(Command::Undo));
        let r = h.app.lua_formats.review.as_ref().unwrap();
        let expected = r.commits.result().export_fingerprint();
        assert_eq!(r.commits.result().note_count(), 10);
        let summary = r
            .commits
            .format_review()
            .expect("native worker prepares review before delivery");
        assert_eq!(
            (summary.before.track_notes, summary.after.track_notes),
            (8, 10)
        );
        assert_eq!(
            (
                summary.changes.notes,
                summary.changes.added,
                summary.changes.removed
            ),
            (1, 0, 0)
        );
        assert_eq!(summary.samples.len(), 1);
        assert_eq!(r.commits.steps.len(), 1);
        let p = preview(
            &h.app.lua_formats,
            &h.app.open.as_ref().unwrap().doc,
            NoteTarget::Track(0),
        )
        .unwrap();
        let crate::piano_roll::ToolPreview::Store { notes, .. } = p else {
            panic!("native Store preview expected")
        };
        assert_eq!(notes.len(), 10);
        for _ in 0..3 {
            h.app_frame(egui::vec2(1366., 900.), 1.);
        }
        let pos = h
            .app
            .lua_formats
            .review
            .as_ref()
            .unwrap()
            .inspect_button
            .unwrap()
            .center();
        h.app_frame_events(
            egui::vec2(1366., 900.),
            1.,
            vec![egui::Event::PointerMoved(pos)],
        );
        for pressed in [true, false] {
            h.app_frame_events(
                egui::vec2(1366., 900.),
                1.,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                }],
            );
        }
        assert!(
            h.app
                .lua_formats
                .review
                .as_ref()
                .unwrap()
                .browser
                .as_ref()
                .is_some_and(|b| b.open),
            "native Inspect button opens the staged browser"
        );
        h.app_frame(egui::vec2(820., 620.), 1.5);
        assert_eq!(h.doc().export_fingerprint(), original);
        h.app
            .lua_formats
            .review
            .as_mut()
            .unwrap()
            .browser
            .as_mut()
            .unwrap()
            .open = false;
        h.app_frame(egui::vec2(820., 620.), 1.5);
        assert!(
            h.app.lua_formats.review.as_ref().unwrap().browser.is_none(),
            "closing only the browser releases its caches and keeps review"
        );
        h.app.apply_lua_format();
        assert!(
            h.app.lua_formats.review.is_none(),
            "{:?}",
            h.app
                .lua_formats
                .review
                .as_ref()
                .and_then(|r| r.error.as_ref())
        );
        assert_eq!(h.doc().export_fingerprint(), expected);
        assert!(h.command(Command::Undo));
        assert_eq!(h.doc().export_fingerprint(), original);
        assert!(h.command(Command::Redo));
        assert_eq!(h.doc().export_fingerprint(), expected);
        import(&mut h, &temp, keys);
        h.app.cancel_lua_format();
        assert!(!h.app.lua_formats.busy());
        assert_eq!(h.doc().export_fingerprint(), expected);
        drop(h);
        drop(temp);
    }
}
#[test]
fn source_change_and_permission_revoke_expire_preview_without_applying_import() {
    for revoke in [false, true] {
        let (mut h, temp) = setup(128);
        let original = h.doc().export_fingerprint();
        import(&mut h, &temp, 128);
        if revoke {
            let mut prefs = h.app.settings.lua_plugins.clone();
            h.app
                .lua_plugins()
                .set_granted(ID, "notes.edit", false, &mut prefs);
            h.app.settings.lua_plugins = prefs;
        } else {
            let o = h.app.open.as_mut().unwrap();
            Arc::make_mut(&mut o.doc)
                .track_mut(0)
                .unwrap()
                .notes
                .insert_sorted_batch(
                    &[NoteRecord {
                        start_tick: 3,
                        duration_ticks: 8,
                        attrs: NoteAttrs::new_wide(70, 90, 0, 0, NoteOffKind::NoteOff).unwrap(),
                        sub: Default::default(),
                        group: 0,
                    }],
                    true,
                )
                .unwrap();
        }
        assert!(
            preview(
                &h.app.lua_formats,
                &h.app.open.as_ref().unwrap().doc,
                NoteTarget::Track(0)
            )
            .is_none()
        );
        let current = h.doc().export_fingerprint();
        h.app.maintain_lua_format();
        assert!(h.app.lua_formats.review.is_none());
        h.app.apply_lua_format();
        assert_eq!(h.doc().export_fingerprint(), current);
        if revoke {
            assert_eq!(current, original);
        } else {
            assert_ne!(current, original);
        }
        drop(h);
        drop(temp);
    }
}
#[test]
fn actual_native_export_and_malformed_import_keep_musical_state_and_release_dialog_scope() {
    let (mut h, temp) = setup(256);
    let original = h.doc().export_fingerprint();
    let output = temp.0.join("export.pnotes");
    std::fs::write(&output, b"prior").unwrap();
    choose(&mut h, Mode::Export, Some(output.clone()));
    frames(&mut h, "export completes", |h| {
        !h.app.lua_formats.busy() && h.app.lua_plugins.as_ref().is_some_and(|m| !m.is_busy())
    });
    let data = std::fs::read_to_string(output).unwrap();
    assert!(data.starts_with("PNOTES1\n"));
    assert_eq!(data.lines().count(), 9);
    assert!(data.contains(",200,80\n"));
    assert_eq!(h.doc().export_fingerprint(), original);
    let input = temp.0.join("bad.pnotes");
    std::fs::write(&input, b"PNOTES1\n0,24,60,96\nbroken\n").unwrap();
    choose(&mut h, Mode::Import, Some(input));
    frames(&mut h, "failed import ends", |h| !h.app.lua_formats.busy());
    assert_eq!(h.doc().export_fingerprint(), original);
    assert!(h.app.lua_formats.review.is_none());
    choose(&mut h, Mode::Import, None);
    frames(&mut h, "picker cancellation ends", |h| {
        !h.app.lua_formats.busy()
    });
    assert_eq!(h.doc().export_fingerprint(), original);
    drop(h);
    drop(temp);
}

#[test]
fn busy_apply_keeps_exact_preview_and_can_apply_after_other_reader_releases() {
    let (mut h, temp) = setup(128);
    let original = h.doc().export_fingerprint();
    import(&mut h, &temp, 128);
    let expected = h
        .app
        .lua_formats
        .review
        .as_ref()
        .unwrap()
        .commits
        .result()
        .export_fingerprint();
    let reader = Arc::clone(&h.app.open.as_ref().unwrap().doc);
    h.app.apply_lua_format();
    assert!(
        h.app
            .lua_formats
            .review
            .as_ref()
            .is_some_and(|r| r.error.is_some())
    );
    assert_eq!(h.doc().export_fingerprint(), original);
    drop(reader);
    h.app.apply_lua_format();
    assert!(h.app.lua_formats.review.is_none());
    assert_eq!(h.doc().export_fingerprint(), expected);
    drop(h);
    drop(temp);
}
#[test]
fn reload_while_picker_waits_retires_only_old_request_and_new_plugin_still_loads() {
    let (mut h, temp) = setup(128);
    let original = h.doc().export_fingerprint();
    let old = request(&mut h, Mode::Import);
    let prefs = h.app.settings.lua_plugins.clone();
    h.app.lua_plugins().reload(ID, &prefs);
    h.app.maintain_lua_format();
    assert!(!h.app.lua_formats.busy());
    let stamp = old.lease.project;
    assert!(old.lease.check().is_err());
    let _ = old.respond(stamp, None);
    frames(&mut h, "reloaded codec remains usable", |h| {
        h.app.lua_plugins.as_ref().is_some_and(|m| {
            !m.is_busy() && m.plugin(ID).is_some_and(|p| p.status == Status::Ready)
        })
    });
    import(&mut h, &temp, 128);
    h.app.cancel_lua_format();
    assert_eq!(h.doc().export_fingerprint(), original);
    drop(h);
    drop(temp);
}
#[test]
fn pending_cancel_and_project_close_release_review_without_a_project_write() {
    let (mut h, temp) = setup(128);
    let original = h.doc().export_fingerprint();
    let pending = request(&mut h, Mode::Import);
    h.app.cancel_lua_format();
    let stamp = pending.lease.project;
    assert!(pending.lease.check().is_err());
    let _ = pending.respond(stamp, None);
    frames(&mut h, "canceled request worker stops", |h| {
        h.app.lua_plugins.as_ref().is_some_and(|m| !m.is_busy())
    });
    assert_eq!(h.doc().export_fingerprint(), original);
    import(&mut h, &temp, 128);
    h.app.open = None;
    h.app.maintain_lua_format();
    assert!(!h.app.lua_formats.busy());
    drop(h);
    drop(temp);
}

#[test]
fn an_empty_project_can_review_and_apply_its_first_imported_notes() {
    let (mut h, temp) = setup(128);
    Arc::make_mut(&mut h.app.open.as_mut().unwrap().doc)
        .track_mut(0)
        .unwrap()
        .notes = NoteStore::default();
    h.app_frame(egui::vec2(1366., 768.), 1.);
    let original = h.doc().export_fingerprint();
    assert_eq!(h.doc().note_count(), 0);
    import(&mut h, &temp, 128);
    assert_eq!(h.doc().export_fingerprint(), original);
    assert_eq!(
        h.app
            .lua_formats
            .review
            .as_ref()
            .unwrap()
            .commits
            .result()
            .note_count(),
        2
    );
    h.app_frame(egui::vec2(820., 620.), 1.5);
    h.app.apply_lua_format();
    assert!(h.app.lua_formats.review.is_none());
    assert_eq!(h.doc().note_count(), 2);
    assert!(h.command(Command::Undo));
    assert_eq!(h.doc().export_fingerprint(), original);
    drop(h);
    drop(temp);
}
