//! Generate disposable Windows ruler and clipboard acceptance projects.
//! Pass an empty directory path. No existing projects are overwritten.

use std::io::Write;
use std::path::PathBuf;

use plumspirit_engine::editor::clips;
use plumspirit_engine::history::{History, HistoryLimits, Operation};
use plumspirit_engine::model::events::{RawEvent, RawKind};
use plumspirit_engine::model::track::Subchannel;
use plumspirit_engine::model::{
    Bpm, ClipId, GraphTarget, GroupInfo, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument,
    SubchannelId,
};
use plumspirit_engine::native::project_to_bytes;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("Pass an output directory.")?,
    );
    std::fs::create_dir_all(&output)?;
    let mut source = PlumDocument::new_project("Clipboard A".into(), Bpm::DEFAULT, 480);
    let mut target = PlumDocument::new_project("Clipboard B".into(), Bpm::DEFAULT, 960);
    for doc in [&mut source, &mut target] {
        doc.project_mut().graphs.slots[0].target = GraphTarget::Cc(10);
    }
    let mut history = History::new(HistoryLimits::default());
    history.apply(
        &mut source,
        Operation::EditSubchannel {
            track: 0,
            sub: Subchannel {
                id: SubchannelId::MAIN,
                midi_channel: 3,
                name: Some("Fixture strings".into()),
            },
            move_notes: false,
        },
    )?;
    let group = source.project_mut().groups.add(GroupInfo {
        id: 0,
        parent: 0,
        name: Some("Fixture group".into()),
        color: Some([42, 211, 136]),
        locked: false,
        temporary: false,
        link: 0,
    });
    let track = source.track_mut(0).unwrap();
    let attrs = NoteAttrs::new(65, 109, 3, 45, NoteOffKind::NoteOff)?;
    track.notes.insert_sorted_batch(
        &[(0, 240), (480, 240), (960, 3_040)].map(|(tick, gate)| NoteRecord {
            group,
            ..NoteRecord::new(tick, gate, attrs)
        }),
        false,
    )?;
    for (tick, value) in [(480, 36), (960, 90)] {
        track.events.push(RawEvent {
            tick,
            anchor: 0,
            kind: RawKind::Channel {
                status: 0xB0,
                data: [10, value],
            },
        })?;
    }
    let primary = ClipId::Primary(track.id());
    let op = clips::duplicate_clips(&source, &[(primary, 0, 200_000)], true)
        .map_err(|e| e.to_string())?;
    history.apply(&mut source, op)?;
    // A real empty primary lets the destination accept notes and graph
    // events while starting without any musical content.
    let primary = ClipId::Primary(target.track(0).unwrap().id());
    let op = clips::resize_clip(&target, primary, 0, 0, Some(7_680)).map_err(|e| e.to_string())?;
    History::new(HistoryLimits::default()).apply(&mut target, op)?;
    for (name, doc) in [("Clipboard A.lup", source), ("Clipboard B.lup", target)] {
        let bytes = project_to_bytes(&doc)?;
        let mut file = std::fs::File::create_new(output.join(name))?;
        file.write_all(&bytes)?;
    }
    Ok(())
}
