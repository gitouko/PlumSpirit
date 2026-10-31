//! The built-in MIDI Player (v0.9 V09-S17, `docs/PLAYER.md`): the file the
//! project exports to, falling onto a keyboard, on the project's transport.
//!
//! This module holds what the Player reads; the window, the rainfall draw
//! role, the stats panel and the keyboard build on it.
//! * [`snapshot`]: the `ExportSnapshot` the Player shows (P1).
//! * [`Sources`]: its notes as tile sources, read in place (P3). Identity
//!   tracks are the snapshot's own stores, and clip tracks are their
//!   unmuted placements. Every source is read as
//!   `Reading::Played(track correction)`, so the notes are never copied.
//! * [`family`]: the dense renderer's family for the Player role
//!   (identity colours, border = fill × 0.2: P7) and its content id.
//! * [`PlayerView`]: keeps the snapshot current while the Player is shown
//!   (P5: taken on a worker, the previous one drawing meanwhile) and counts
//!   the frames drawn ([`stats::FrameRate`]).
//! * [`PlayerSettings`] and its window (P10), the stats panel ([`stats`]).

pub mod keys;
pub mod rain;
pub mod stats;

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use serde_json::{Value, json};

use plumspirit_engine::midi::{ExportOptions, ExportSnapshot};
use plumspirit_engine::model::project::KeyRange;
use plumspirit_engine::model::{NoteStore, PlumDocument};
use plumspirit_engine::view::tiles::{self, Outline, Reading, SourcePlace, TileOptions};

use crate::piano_roll::dense::{self, Colors, Family, FamilyId, SourceColors, Tint};
use crate::ui::theme::NotePalette;

/// The Player's background (P6: near-black, so every note's dark border
/// reads as a separation).
pub const BACKGROUND: egui::Color32 = egui::Color32::from_rgb(8, 8, 10);

/// The snapshot the Player shows (P1): the default export of `doc`, and in
/// 256-Key Mode every key kept (`ExportOptions::for_player`).
pub fn snapshot(doc: &PlumDocument) -> Arc<ExportSnapshot> {
    ExportSnapshot::take(doc, ExportOptions::for_player(doc.project().keys))
}

/// The Player's tile sources: source `i` is `stores[i]` drawn where
/// `places[i]` puts it and read as `readings[i]`. The tracks come in
/// document order, and a clip track's unmuted placements in the order the
/// export plays them. A content shown by several placements (linked clips)
/// is one shared store, and sources without notes are left out.
#[derive(Clone, Debug, Default)]
pub struct Sources {
    pub stores: Vec<Arc<NoteStore>>,
    pub places: Vec<SourcePlace>,
    pub readings: Vec<Reading>,
    /// The exported track of each source (its colours, P7).
    pub tracks: Vec<usize>,
    /// (palette id of subchannel 0, subchannel count) of every track
    /// ([`track_bases`]).
    pub bases: Vec<(u32, u32)>,
    /// The keyboard's key mask of each source (`tiles::lit_mask`, v1.0-beta1
    /// B1-0145), taken with the snapshot ([`Prepared::of`]); empty: every
    /// key (correct, only slower in dense passages).
    pub lit_masks: Vec<[u64; 4]>,
    /// [`Sources::content_key`], computed once by [`Sources::of`].
    key: u64,
}

impl Sources {
    /// The sources of `s`'s notes. O(tracks + placements + chunks of the
    /// stores shown): nothing per note.
    pub fn of(s: &ExportSnapshot) -> Sources {
        let doc = s.document();
        let plan = s.plan();
        let tracks = doc.tracks().len();
        let mut out = Sources::default();
        // Only clip projects pay for the per-track placement lists.
        let placed = (0..tracks).any(|i| plan.placed(i).is_some());
        let by_track = if placed {
            doc.placed_contents_by_track()
        } else {
            Vec::new()
        };
        // One `Arc` per content (linked clips share their content's chunks).
        let mut shared: HashMap<*const NoteStore, Arc<NoteStore>> = HashMap::new();
        for i in 0..tracks {
            let reading = Reading::Played(plan.correction(i).copied());
            if plan.placed(i).is_none() {
                let notes = &doc.tracks()[i].notes;
                if !notes.is_empty() {
                    out.stores.push(Arc::new(notes.clone()));
                    out.places.push(SourcePlace::IDENTITY);
                    out.readings.push(reading);
                    out.tracks.push(i);
                }
                continue;
            }
            for pc in by_track.get(i).into_iter().flatten() {
                if pc.placement.muted || pc.notes.is_empty() {
                    continue;
                }
                let store = shared
                    .entry(std::ptr::from_ref(pc.notes))
                    .or_insert_with(|| Arc::new(pc.notes.clone()));
                out.stores.push(Arc::clone(store));
                out.places.push(SourcePlace::of(&pc.placement));
                out.readings.push(reading);
                out.tracks.push(i);
            }
        }
        out.bases = track_bases(doc);
        out.key = out.hash_content();
        out
    }

    pub fn len(&self) -> usize {
        self.stores.len()
    }

    /// Content identity of what the tiles of these sources show: every
    /// source's notes (store revision and size), place and reading — the
    /// corrections by value. A snapshot taken again after an edit that
    /// changes none of them (a tempo change, say) keeps every built tile.
    pub fn content_key(&self) -> u64 {
        self.key
    }

    fn hash_content(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.len().hash(&mut h);
        for ((s, p), r) in self.stores.iter().zip(&self.places).zip(&self.readings) {
            s.revision().hash(&mut h);
            s.len().hash(&mut h);
            p.hash(&mut h);
            r.hash(&mut h);
        }
        self.tracks.hash(&mut h);
        self.bases.hash(&mut h);
        h.finish()
    }
}

/// The Player's tile options for a project of `keys` (P6 / P7): outlined
/// along time at [`dense::PLAYED_OUTLINE_DARKNESS`], one texel row per key.
/// There are no outlines across keys: the rainfall draws each key's row as
/// its own inset column, which gives every note its dark sides, so a
/// vertical outline would only turn a chord's outer keys into border.
pub fn tile_options(keys: KeyRange) -> TileOptions {
    TileOptions {
        outline: Some(Outline {
            alpha: 255,
            darkness: dense::PLAYED_OUTLINE_DARKNESS,
            selection: None,
            vertical: false,
            borders: false,
        }),
        keys,
        fold: None,
        window: None,
    }
}

/// Palette ids of every track's subchannels (P7): tracks in document order,
/// each taking as many ids as its highest subchannel id + 1, so track `t`'s
/// subchannel `s` is id `base + s` — one track with one subchannel per
/// track gives every track its own colour, and track 1's subchannels keep
/// the editor's colours.
pub fn track_bases(doc: &PlumDocument) -> Vec<(u32, u32)> {
    let mut next = 0u32;
    doc.tracks()
        .iter()
        .map(|t| {
            let n = t.subchannels().max_id().0.saturating_add(1);
            let b = next;
            next = next.saturating_add(n);
            (b, n)
        })
        .collect()
}

/// The Player's colours (P7) for sources of `tracks` (each source's track
/// index into `bases`, [`track_bases`]): identity colours of `palette` per
/// (track, subchannel), nothing drawn as selected (a played reading has no
/// selection), one layer (within a key, the note drawn later — the later
/// track, then the later start — is in front).
pub fn colors(
    palette: Arc<NotePalette>,
    dark: bool,
    bases: &[(u32, u32)],
    tracks: &[usize],
) -> Colors {
    let per_source = tracks
        .iter()
        .map(|&t| bases.get(t).copied().unwrap_or((0, 1)))
        .collect();
    let played = dense::PlayedColors::new(palette, dark, per_source);
    Colors {
        sources: vec![SourceColors::new(Tint::Played(Arc::new(played)))],
        selection: false,
        muted_fill: None,
        muted: 255,
        ranked: false,
    }
}

/// Tag of Player families in [`dense::hash_words`] ids (the roll's track
/// family is 1, the Onion 2).
const FAMILY_TAG: u64 = 4;

/// The Player family of `sources` (from `Sources::of` of the Player's
/// snapshot) and its id: the sources' content, the colours, the theme and
/// the options.
pub fn family(
    sources: &Sources,
    keys: KeyRange,
    palette: &Arc<NotePalette>,
    dark: bool,
) -> (FamilyId, Family) {
    let opt = tile_options(keys);
    let id = family_id(sources, keys, palette, dark);
    let fam = Family {
        stores: sources.stores.clone(),
        places: sources.places.clone(),
        readings: sources.readings.clone(),
        colors: colors(Arc::clone(palette), dark, &sources.bases, &sources.tracks),
        opt,
    };
    (id, fam)
}

/// The id of the Player family of `sources` (O(1): the sources' content
/// key is computed once), without building it.
pub fn family_id(sources: &Sources, keys: KeyRange, palette: &NotePalette, dark: bool) -> FamilyId {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    tile_options(keys).hash(&mut h);
    dense::hash_words([
        FAMILY_TAG,
        sources.content_key(),
        palette.key(),
        u64::from(dark),
        h.finish(),
    ])
}

// ------------------------------------------------------------- settings

/// Falling time shown above the keyboard (P10), seconds. v1.0-beta2
/// (B2-F20): any finite positive value (the 0.25 – 20 s limits are gone).
pub const DEFAULT_FALL_SECONDS: f64 = 2.0;

/// B2-F20: a falling time the Player can use — finite and above 0.
pub fn valid_fall_seconds(s: f64) -> bool {
    s.is_finite() && s > 0.0
}

/// B2-F20: the sentence for a falling time that is not finite and above 0.
pub const FALL_SECONDS_SENTENCE: &str =
    "The falling time must be a positive number of seconds (above 0).";

/// The MIDI Player's application settings (P10; never in `.lup`, never
/// dirty a project). Font, colours and number format follow the
/// Progressor's.
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerSettings {
    /// The Player replaces the editor area (View → MIDI Player, F10).
    pub enabled: bool,
    /// The stats panel (`stats::PlayerField`s).
    pub template: String,
    /// Seconds of song time between the top of the rainfall and the
    /// keyboard.
    pub fall_seconds: f64,
    /// v1.0-beta2 (B2-F11): Time-Based or Tick-Based rainfall.
    pub mapping: rain::Mapping,
    /// v1.0-beta1 (B1-0143) Player-only effects, percent (0 = off, the
    /// default): note bodies shade towards the right (PFA's primary → dark
    /// body at about 40 %), and a glow rises from each lit key above the
    /// keyboard. The Piano Roll never shows either.
    pub gradient: u8,
    pub glow: u8,
}

impl Default for PlayerSettings {
    fn default() -> Self {
        PlayerSettings {
            enabled: false,
            template: stats::DEFAULT_TEMPLATE.into(),
            fall_seconds: DEFAULT_FALL_SECONDS,
            mapping: rain::Mapping::Time,
            gradient: 0,
            glow: 0,
        }
    }
}

impl PlayerSettings {
    pub fn to_json(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "template": self.template,
            "fall_seconds": self.fall_seconds,
            "mapping": self.mapping.key(),
            "gradient": self.gradient,
            "glow": self.glow,
        })
    }

    /// Each field falls back to its default on its own (with a problem
    /// line).
    pub fn from_json(v: &Value, problems: &mut Vec<String>) -> PlayerSettings {
        let mut s = PlayerSettings::default();
        let Some(o) = v.as_object() else {
            problems.push("player is not an object; using the defaults".into());
            return s;
        };
        let mut bad = |k: &str| problems.push(format!("player.{k} is invalid; using the default"));
        if let Some(x) = o.get("enabled") {
            match x.as_bool() {
                Some(b) => s.enabled = b,
                None => bad("enabled"),
            }
        }
        if let Some(x) = o.get("template") {
            match x.as_str() {
                Some(t) => s.template = t.to_owned(),
                None => bad("template"),
            }
        }
        if let Some(x) = o.get("fall_seconds") {
            // B2-F20: kept as written when finite and above 0, never
            // clamped; anything else falls back with a problem line.
            match x.as_f64().filter(|f| valid_fall_seconds(*f)) {
                Some(f) => s.fall_seconds = f,
                None => bad("fall_seconds"),
            }
        }
        if let Some(x) = o.get("mapping") {
            match x.as_str().and_then(rain::Mapping::from_key) {
                Some(m) => s.mapping = m,
                None => bad("mapping"),
            }
        }
        for (k, field) in [("gradient", &mut s.gradient), ("glow", &mut s.glow)] {
            if let Some(x) = o.get(k) {
                match x.as_u64() {
                    Some(v) => *field = v.min(100) as u8,
                    None => bad(k),
                }
            }
        }
        s
    }
}

/// View → MIDI Player Settings…: show / hide, the panel's lines (the
/// Progressor's template editor) and the falling time.
pub fn settings_window(
    ctx: &egui::Context,
    s: &mut PlayerSettings,
) -> crate::app::progressor::WindowOutcome {
    use crate::app::progressor::{self, WindowOutcome};
    use crate::ui::theme::{self, space};
    use egui::RichText;
    let mut outcome = WindowOutcome::Open;
    let mut open = true;
    egui::Window::new("MIDI Player Settings")
        .id(egui::Id::new("midi-player-settings"))
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .default_pos(ctx.content_rect().left_top() + egui::Vec2::new(236.0, 96.0))
        .show(ctx, |ui| {
            ui.set_width(380.0);
            ui.checkbox(
                &mut s.enabled,
                format!(
                    "Show the MIDI Player{}",
                    crate::app::keybinds::hint(crate::app::commands::Command::ToggleMidiPlayer)
                ),
            );
            ui.add_space(space::MICRO);
            progressor::template_editor::<stats::PlayerField>(
                ui,
                "player",
                &mut s.template,
                &stats::FIELDS,
            );
            ui.add_space(space::MICRO);
            ui.horizontal(|ui| {
                ui.label("Falling time");
                ui.add(
                    crate::ui::stepper::Stepper::new(&mut s.fall_seconds)
                        .range(f64::MIN_POSITIVE..=f64::MAX)
                        .range_sentence(FALL_SECONDS_SENTENCE)
                        .speed(0.05)
                        .min_decimals(2)
                        .suffix(" s"),
                )
                .on_hover_text("Song time between the top of the Player and the keyboard (in Tick-Based, at the song's opening tempo).");
            });
            // v1.0-beta2 (B2-F11): what equal heights stand for.
            ui.horizontal(|ui| {
                ui.label("Notes fall by");
                ui.radio_value(&mut s.mapping, rain::Mapping::Time, "Time")
                    .on_hover_text("Time-Based: equal seconds are equal heights, so notes fall at one speed through every tempo change.");
                ui.radio_value(&mut s.mapping, rain::Mapping::Tick, "Ticks")
                    .on_hover_text("Tick-Based: equal ticks (beats) are equal heights whatever the tempo, so a note is as long as its ticks; the falling speed follows the tempo. Playback, the keyboard and the statistics are the same in both.");
            });
            ui.add_space(space::MICRO);
            // v1.0-beta1 (B1-0143): optional effects of the Player alone.
            ui.horizontal(|ui| {
                ui.label("Note gradient");
                ui.add(egui::Slider::new(&mut s.gradient, 0..=100).suffix(" %"))
                    .on_hover_text("Each falling note shades from its color on the left to darker on the right (about 40 % is Piano From Above's look). 0 = flat. MIDI Player only: the Piano Roll never shows it.");
            });
            ui.horizontal(|ui| {
                ui.label("Hit glow");
                ui.add(egui::Slider::new(&mut s.glow, 0..=100).suffix(" %"))
                    .on_hover_text("A soft glow in each sounding key's color rises above the keyboard. 0 = off. MIDI Player only.");
            });
            ui.add_space(space::MICRO);
            ui.label(
                RichText::new("Font, colors and numbers follow the Progressor Settings.")
                    .size(theme::size::SMALL)
                    .weak(),
            );
            ui.add_space(space::SECTION);
            ui.horizontal(|ui| {
                if ui.button("Reset to Default").clicked() {
                    let enabled = s.enabled;
                    *s = PlayerSettings {
                        enabled,
                        ..PlayerSettings::default()
                    };
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("OK").clicked() {
                        outcome = WindowOutcome::Ok;
                    }
                    if ui.button("Cancel").clicked() {
                        outcome = WindowOutcome::Cancel;
                    }
                });
            });
        });
    if !open {
        outcome = WindowOutcome::Cancel;
    }
    outcome
}

// ----------------------------------------------------------------- view

/// A snapshot ready to draw: the file's notes and their tile sources.
pub struct Prepared {
    pub snapshot: Arc<ExportSnapshot>,
    pub sources: Arc<Sources>,
    /// The snapshot's song time (the rainfall falls in seconds, P6).
    pub timing: Arc<plumspirit_engine::analysis::Timing>,
    /// The exported file's length in song nanoseconds
    /// ([`ExportSnapshot::end_tick`]: its latest End of Track, P10).
    pub length_ns: u64,
}

impl Prepared {
    /// Takes the Player's snapshot of `doc` and its sources (O(tracks +
    /// placements + chunks), plus O(notes) for the export plan when tracks
    /// have corrections and for the keyboard's key masks).
    pub fn of(doc: &PlumDocument) -> Prepared {
        Prepared::of_until(doc, &AtomicBool::new(false)).expect("never canceled")
    }

    /// [`Prepared::of`] on the snapshot worker, `None` once `cancel` is set
    /// (L-H03: a closed project or a newer snapshot stops it between stages
    /// and between sources, so Close Project never waits for a whole
    /// O(notes) pass of a project that is gone).
    pub fn of_until(doc: &PlumDocument, cancel: &AtomicBool) -> Option<Prepared> {
        let stop = || cancel.load(Ordering::Relaxed);
        let snapshot = snapshot(doc);
        if stop() {
            return None;
        }
        let mut sources = Sources::of(&snapshot);
        // O(notes) of the attribute columns, on the snapshot worker; linked
        // contents (one shared store) are read once.
        let mut seen: HashMap<*const NoteStore, [u64; 4]> = HashMap::new();
        let mut masks = Vec::with_capacity(sources.stores.len());
        for (st, r) in sources.stores.iter().zip(&sources.readings) {
            if stop() {
                return None;
            }
            masks.push(match r {
                Reading::Played(Some(_)) => tiles::lit_mask(st, *r),
                _ => *seen
                    .entry(Arc::as_ptr(st))
                    .or_insert_with(|| tiles::lit_mask(st, *r)),
            });
        }
        sources.lit_masks = masks;
        let sources = Arc::new(sources);
        let timing = Arc::new(plumspirit_engine::analysis::Timing::for_document(
            snapshot.document(),
        ));
        let length_ns = timing.nanos_at(snapshot.end_tick());
        Some(Prepared {
            snapshot,
            sources,
            timing,
            length_ns,
        })
    }

    fn is_for(&self, key: u64, options: ExportOptions) -> bool {
        self.snapshot.key() == key && self.snapshot.options() == options
    }
}

struct Job {
    rx: Receiver<Prepared>,
    key: u64,
    /// Set when the job is dropped (replaced by a newer one, the Player
    /// hidden, the project closed): its worker stops at its next check.
    cancel: Arc<AtomicBool>,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The Player's state while it is shown (P5): the snapshot drawn, the one
/// being taken, the frame rate and the parsed panel template.
#[derive(Default)]
pub struct PlayerView {
    current: Option<Prepared>,
    job: Option<Job>,
    /// Take snapshots inline (tests: deterministic, no thread).
    sync: bool,
    pub frames: stats::FrameRate,
    /// The song time the rainfall is drawn at (P9).
    pub clock: rain::SmoothClock,
    pub template: Option<(String, crate::app::progressor::Template<stats::PlayerField>)>,
    /// Snapshots started (tests, diagnostics).
    pub taken: u64,
}

impl PlayerView {
    /// L-H03: stops a snapshot being taken without waiting (Close Project,
    /// on the GUI thread, before [`PlayerView::release`]).
    pub fn cancel_job(&self) {
        if let Some(j) = &self.job {
            j.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Close Project, on the reaper: drain any in-flight snapshot before
    /// reporting completion. Ordinary hide/replacement stays nonblocking.
    pub fn release(mut self) {
        if let Some(j) = self.job.take() {
            // L-H03: stop it, then wait only for its current stage.
            j.cancel.store(true, Ordering::Relaxed);
            while j.rx.recv().is_ok() {}
        }
    }

    /// `sync`: snapshots are taken inline (tests).
    #[cfg(test)]
    pub fn new(sync: bool) -> PlayerView {
        PlayerView {
            sync,
            ..PlayerView::default()
        }
    }

    /// The snapshot drawn (possibly one edit behind while the next one is
    /// taken; `None` until the first one is ready).
    pub fn current(&self) -> Option<&Prepared> {
        self.current.as_ref()
    }

    /// Keeps the snapshot current for `doc` (D5: re-taken when `(export
    /// key, options)` changes). The next one is taken on a worker from a
    /// copy-on-write clone of the document while the previous one keeps
    /// drawing (D6); a result for a document that changed again meanwhile
    /// is dropped and taken anew. O(tracks + contents) per call (the export
    /// key). Returns true while a snapshot is being taken (poll again).
    pub fn sync(&mut self, doc: &PlumDocument) -> bool {
        let options = ExportOptions::for_player(doc.project().keys);
        let key = doc.export_key();
        if let Some(j) = &self.job {
            match j.rx.try_recv() {
                Ok(p) => {
                    self.job = None;
                    if p.is_for(key, options) {
                        self.current = Some(p);
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.job = None,
            }
        }
        if self
            .current
            .as_ref()
            .is_some_and(|p| p.is_for(key, options))
        {
            return false;
        }
        if self.job.as_ref().is_some_and(|j| j.key == key) {
            return true;
        }
        self.taken += 1;
        if self.sync {
            self.current = Some(Prepared::of(doc));
            return false;
        }
        let copy = doc.clone();
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&cancel);
        let spawned = std::thread::Builder::new()
            .name("plumspirit-player-snapshot".into())
            .spawn(move || {
                if let Some(p) = Prepared::of_until(&copy, &stop) {
                    let _ = tx.send(p);
                }
            });
        match spawned {
            Ok(_) => {
                // A job for an older document is dropped: it is cancelled
                // (its thread stops at its next check, its result goes
                // nowhere).
                self.job = Some(Job { rx, key, cancel });
                true
            }
            Err(e) => {
                log::warn!(
                    "the MIDI Player snapshot thread could not start ({e}); taking it inline"
                );
                self.current = Some(Prepared::of(doc));
                false
            }
        }
    }

    /// The Player was hidden or the project closed: nothing is kept.
    pub fn clear(&mut self) {
        self.current = None;
        self.job = None;
        self.frames.reset();
        self.clock = rain::SmoothClock::default();
    }
}

#[cfg(test)]
mod tests;
