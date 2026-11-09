//! Document model: the authoritative state of an open MIDI project.

pub mod bitset;
pub mod clip;
pub mod conductor;
pub mod correction;
pub mod document;
pub mod duration;
pub mod events;
pub mod folder;
pub mod group;
pub mod metadata;
pub mod note;
pub mod note_store;
pub mod patch;
pub mod placed;
pub mod project;
pub(crate) mod sub_column;
pub mod tempo;
pub mod timing;
pub mod track;

pub use clip::{ClipId, ClipLayout, ClipPlacement, ClipTable, ContentId, ContentRef, OwnedContent};
pub use conductor::{
    AdoptReport, Conductor, ConductorEdit, ConductorError, ConductorScope, Interp, Marker,
    MasterVolumePoint, TempoPoint, TimeSignature,
};
pub use document::{DocumentError, ExtraChunk, PlacedContent, PlumDocument, SmfFormat};
pub use events::{ArenaRange, EventStore, RawEvent, RawKind};
pub use folder::{FolderInfo, TrackFolders};
pub use group::{GroupInfo, GroupTable};
pub use note::{NoteAttrs, NoteOffKind, NoteRecord, SubchannelId};
pub use note_store::{
    NoteLoc, NoteSnapshot, NoteStore, NoteView, Overlapping, PatchError, QueryStats,
    SelectionBounds, SelectionUniformity, SnapshotPos,
};
pub use patch::{CompactRecords, ListPatch, RankSet, RankSetBuilder};
pub use placed::{PlacedLane, PlacedNotes};
pub use project::{
    Bpm, BpmError, DensityBasis, GraphConfig, GraphSlot, GraphTarget, LoopConfig, MidiTempo,
    OnionConfig, OnionMode, OnionOrder, ProjectInfo, SoundFontRef, TempoFidelity,
};
pub use tempo::{Speed, TempoMap, TempoValue};
pub use timing::{Division, MAX_DELTA_TICKS, Snap, Tick, TimingKind};
pub use track::{
    GatePercent, OutputRoute, Subchannel, SubchannelTable, Track, TrackId, TrackKind, TrackProps,
    next_midi_channel,
};
