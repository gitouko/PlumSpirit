use super::*;
use crate::app::commands::Command;
use crate::app::test_support::Harness;
use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord};

fn project(ppqn: u32, with_notes: bool) -> Harness {
    let mut doc = PlumDocument::new_project(format!("{ppqn} PPQN"), Bpm::DEFAULT, ppqn);
    if with_notes {
        let attrs = NoteAttrs::new(66, 111, 0, 77, NoteOffKind::NoteOff).unwrap();
        doc.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(
                &[
                    NoteRecord::new(80, 12, attrs),
                    NoteRecord::new(110, 15, attrs),
                ],
                false,
            )
            .unwrap();
    }
    let mut h = Harness::new(doc);
    h.app.main_view = MainView::PianoRoll;
    if with_notes {
        h.set_notes(0);
    }
    h
}

fn drain(h: &mut Harness) {
    let deadline = Instant::now() + std::time::Duration::from_secs(4);
    while h.app.job.is_some() {
        assert!(Instant::now() < deadline, "clipboard job did not finish");
        h.poll_background_jobs();
        std::thread::yield_now();
    }
}

#[test]
fn shortcuts_paste_once_and_cut_waits_for_publication() {
    let mut a = project(480, true);
    let mut b = project(960, false);
    // A blank track's invisible primary is intentionally read-only. Make
    // the primary a real empty Clip so the Piano Roll can accept notes.
    let id = ClipId::Primary(b.id(0));
    let op = clips::resize_clip(b.doc(), id, 0, 0, Some(3_840)).unwrap();
    b.app.apply_op(op).unwrap();
    let before_paste = b.app.open.as_ref().unwrap().history.undo_len();
    let mock = Arc::new(std::sync::Mutex::new(MockClipboard::default()));
    a.app.clipboard_backend.mock = Some(mock.clone());
    b.app.clipboard_backend.mock = Some(mock.clone());
    mock.lock().unwrap().busy = true;
    let source = a.doc().state_fingerprint();
    a.shortcut_events(vec![egui::Event::Cut]);
    drain(&mut a);
    assert_eq!(a.doc().state_fingerprint(), source);
    assert!(a.status().contains("busy"));
    mock.lock().unwrap().busy = false;
    a.shortcut_events(vec![egui::Event::Cut]);
    drain(&mut a);
    assert_eq!(a.doc().track(0).unwrap().notes.len(), 0);
    assert!(a.command(Command::Undo));
    assert_eq!(a.doc().state_fingerprint(), source);
    drop(a);

    b.app.roll.cursor_tick = 420;
    let release = egui::Event::Key {
        key: egui::Key::V,
        physical_key: None,
        pressed: false,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND,
    };
    b.shortcut_events(vec![egui::Event::Paste(String::new()), release.clone()]);
    drain(&mut b);
    let notes: Vec<_> = b
        .doc()
        .track(0)
        .unwrap()
        .notes
        .iter()
        .map(|n| (n.start_tick, n.duration_ticks, n.attrs.off_velocity()))
        .collect();
    assert_eq!(notes, [(420, 24, 77), (480, 30, 77)], "{}", b.status());
    assert_eq!(
        b.app.open.as_ref().unwrap().history.undo_len(),
        before_paste + 1
    );
    assert!(b.command(Command::Undo));
    assert!(b.doc().track(0).unwrap().notes.is_empty());
    assert!(b.command(Command::Redo));
    assert_eq!(b.doc().track(0).unwrap().notes.len(), 2);

    // Ordinary clipboard text replaces the registered format.
    {
        let mut clipboard = mock.lock().unwrap();
        clipboard.sequence += 1;
        clipboard.bytes = None;
    }
    b.shortcut_events(vec![egui::Event::Paste("external text".into()), release]);
    drain(&mut b);
    assert_eq!(b.doc().track(0).unwrap().notes.len(), 2);
    assert!(b.status().contains("No musical data"));
}

#[test]
fn a_pending_paste_cannot_follow_a_changed_destination_or_clipboard() {
    let mut a = project(480, true);
    let mut b = project(960, false);
    let mock = Arc::new(std::sync::Mutex::new(MockClipboard::default()));
    a.app.clipboard_backend.mock = Some(mock.clone());
    b.app.clipboard_backend.mock = Some(mock.clone());
    a.app.request_note_copy(false);
    drain(&mut a);
    b.app.paste_music();
    b.app.roll.cursor_tick = 480;
    drain(&mut b);
    assert!(b.status().contains("destination changed"));
    assert!(b.doc().track(0).unwrap().notes.is_empty());

    let destination = Destination::get(&b.app);
    let sequence = mock.lock().unwrap().sequence;
    mock.lock().unwrap().sequence += 1;
    b.app.finish_clipboard(
        Ok(Completion::Paste {
            sequence,
            destination,
            prepared: Paste::Notes(PreparedEdit::Identity, None),
        }),
        Instant::now(),
    );
    assert!(
        b.status()
            .contains("clipboard or paste destination changed")
    );
}

#[test]
fn clip_menu_cut_and_paste_commit_once() {
    let mut a = project(480, true);
    let mut b = project(960, false);
    let mock = Arc::new(std::sync::Mutex::new(MockClipboard::default()));
    a.app.clipboard_backend.mock = Some(mock.clone());
    b.app.clipboard_backend.mock = Some(mock);
    let id = ClipId::Primary(a.id(0));
    a.set_clips(&[id]);
    a.app.main_view = MainView::TrackList;
    assert!(a.command(Command::Cut));
    drain(&mut a);
    assert!(a.doc().clips().placement(id).is_none());
    drop(a);

    b.app.main_view = MainView::TrackList;
    b.app.roll.cursor_tick = 360;
    assert!(b.command(Command::Paste));
    drain(&mut b);
    assert_eq!(b.app.open.as_ref().unwrap().history.undo_len(), 1);
    let layout = b.doc().clips().layout();
    assert_eq!(layout.placements[0].start, 360);
    let notes = b.doc().notes_of(layout.placements[0].content).unwrap();
    assert_eq!(notes.len(), 2);
    assert_eq!(notes.iter().next().unwrap().start_tick, 160);
    assert!(b.command(Command::Undo));
    assert!(b.doc().clips().layout().placements.is_empty());
}

#[test]
fn controller_cut_keeps_the_source_channel_and_rescales_for_the_target() {
    use plumspirit_engine::model::events::{RawEvent, RawKind};

    let mut a = project(480, true);
    let mut b = project(960, false);
    let mock = Arc::new(std::sync::Mutex::new(MockClipboard::default()));
    a.app.clipboard_backend.mock = Some(mock.clone());
    b.app.clipboard_backend.mock = Some(mock);
    for h in [&mut a, &mut b] {
        h.app.update_graph_slot(0, |slot| {
            slot.target = GraphTarget::Cc(10);
            slot.channel = 3;
        });
        h.app.graph_focus = true;
    }
    for (tick, value) in [(12, 39), (32, 80)] {
        a.app
            .doc_mut()
            .unwrap()
            .track_mut(0)
            .unwrap()
            .events
            .push(RawEvent {
                tick,
                anchor: 0,
                kind: RawKind::Channel {
                    status: 0xB3,
                    data: [10, value],
                },
            })
            .unwrap();
    }
    a.app.graphs.views[0].select_controllers([0, 1], false);
    assert!(a.command(Command::Cut));
    drain(&mut a);
    assert!(a.doc().track(0).unwrap().events.events().is_empty());
    drop(a);
    b.app.roll.cursor_tick = 120;
    b.app.update_graph_slot(0, |slot| slot.channel = 2);
    b.app.paste_music();
    drain(&mut b);
    assert!(b.status().contains("MIDI channel 4"));
    assert!(b.doc().track(0).unwrap().events.events().is_empty());

    b.app.update_graph_slot(0, |slot| slot.channel = 3);
    b.app.paste_music();
    drain(&mut b);
    let events = b.doc().track(0).unwrap().events.events();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].tick, 120);
    assert_eq!(events[1].tick, 160);
    assert_eq!(
        events[0].kind,
        RawKind::Channel {
            status: 0xB3,
            data: [10, 39]
        }
    );
    assert!(b.command(Command::Undo));
    assert!(b.doc().track(0).unwrap().events.events().is_empty());
}
