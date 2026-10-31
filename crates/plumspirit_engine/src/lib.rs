//! # PlumSpirit engine
//!
//! The GUI-independent core of PlumSpirit, an original Black MIDI editor:
//!
//! * [`model`] – the authoritative document: tracks, compact chunked note storage,
//!   preserved non-note events, selection bitsets.
//! * [`midi`] – streaming Standard MIDI File import, export and validation.
//! * [`editor`] – note editing, copy/paste and exact Flip/Scale transforms.
//! * [`history`] – undo/redo with compact reversible patches and savepoints.
//! * [`native`] – the versioned binary `.lup` project format.
//! * [`playback`] – audio-backend-independent scheduling from note snapshots.
//! * [`view`] – viewport math that renderers use: grid lines, density rasters for
//!   level-of-detail, coordinate helpers.
//!
//! The engine uses only the Rust standard library and never depends on GUI types, so
//! future front ends (and future plugin systems) can consume the same model.

pub mod analysis;
pub mod codec;
pub mod editor;
pub mod fixtures;
pub mod history;
pub mod json;
pub mod midi;
pub mod model;
pub mod native;
pub mod playback;
pub mod plugins;
pub mod spiderweb;
pub mod transform;
pub mod view;
pub(crate) mod wide;

/// PlumSpirit version string (from Cargo metadata).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
