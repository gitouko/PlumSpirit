//! The shared helpers of the v0.9 tool expansion (V09-S34,
//! `docs/v0_9/TOOL_DESIGN.md` "Shared helpers for S34+"): one implementation
//! of each piece of plumbing the new tool functions have in common, so no
//! tool carries its own copy. Each module is named after the catalog's
//! `shared = […]` entry that uses it:
//!
//! | helper | module | over |
//! |---|---|---|
//! | `euclid` | [`euclid`] | Bjorklund's algorithm, built once per setting |
//! | `tempo_time` | [`tempo_time`] | the song's tempo map at 1 MHz ([`super::Ctx::tempo`], `TempoCursor`) |
//! | `metric_position` | [`metric_position`] | the meter map ([`super::Ctx::meter`]) |
//! | `chord_groups` | [`chord_groups`] | onset groups by tolerance (the FL tools' chords) |
//! | `phrase_detect` | [`phrase_detect`] | rests and bar lines between onsets |
//! | `density_window` | [`density_window`] | the canonical NPS / Poly definitions (`analysis/canonical.rs`) |
//! | `scale_lattice` | [`scale_lattice`] | [`super::theory::Scale`] pitch-class masks |
//! | `seeded_stream` | [`seeded_stream`] | [`super::rng`] (per-key streams, shuffles, weights) |
//! | `fast_hash` | [`fast_hash`] | maps / sets of grid cells with a fast deterministic hasher (V09-S39) |
//! | `reference` | [`reference`] | reference notes by track number or subchannel position (V09-S41) |
//!
//! Rules every helper keeps (rubric `perf`): no second copy of the notes
//! (they take `&[TNote]` and index lists), no heap per note (flat index
//! lists with group bounds), linear or `n log n` work, deterministic results
//! that depend only on their inputs.

pub mod chord_groups;
pub mod density_window;
pub mod euclid;
pub mod fast_hash;
pub mod metric_position;
pub mod phrase_detect;
pub mod reference;
pub mod scale_lattice;
pub mod seeded_stream;
pub mod tempo_time;
