//! v0.9 V09-S17: the MIDI Player in the app (`docs/PLAYER.md` P2, P4, P10):
//! View → MIDI Player (F10) replaces the editor area; the toolbar, the
//! transport, the track panel and the status bar stay. Esc or F10 return to
//! the editor.
//!
//! The Player plays the project's transport (one clock) and shows the file
//! the project exports to (`crate::player`). Its statistics come from the
//! project's analysis index with `Scope::Project`.

use super::analysis::Peaks;
use super::keys::KeyDisplay;
use super::progressor::{self, Corner, WindowOutcome};
use super::{PlumSpiritApp, format_clock, live_bpm};
use crate::piano_roll::dense::Columns;
use crate::player;
use crate::player::stats::{self, PlayerField, PlayerReadout};
use crate::ui::theme::{self, Palette};
use egui::{Align2, Color32, FontId, Pos2, Rect};
use plumspirit_engine::analysis::{DEFAULT_WINDOW_NS, Scope};
use plumspirit_engine::view::tiles;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Repaint interval while the Player's snapshot is being taken.
pub(super) const SNAPSHOT_POLL: Duration = Duration::from_millis(30);
/// The audio period assumed before the device reports one (P9).
const DEFAULT_PERIOD: Duration = Duration::from_millis(10);

/// The rainfall and keyboard areas of the Player in `rect` for `keys`
/// keys: the keyboard is about 4.5 white keys tall, between 6 % and 18 % of
/// the height.
pub(super) fn player_layout(rect: Rect, keys: usize) -> (Rect, Rect) {
    let whites = (0..keys)
        .filter(|&k| !player::keys::is_black(k))
        .count()
        .max(1);
    let kb = (rect.width() / whites as f32 * 4.5).clamp(rect.height() * 0.06, rect.height() * 0.18);
    let split = rect.bottom() - kb;
    (
        Rect::from_min_max(rect.min, Pos2::new(rect.right(), split)),
        Rect::from_min_max(Pos2::new(rect.left(), split), rect.max),
    )
}

/// The hit glow (B1-0143): above each lit key, its colour fading from
/// `strength` opacity at the keyboard to nothing over a band of 8 % of the
/// rainfall's height (at most 48 points). Black keys' glows over the white
/// ones, as the keys are drawn.
fn paint_glow(
    p: &egui::Painter,
    rain: Rect,
    spans: &[player::keys::KeySpan],
    lit: &[Option<KeyDisplay>],
    strength: f32,
) {
    let h = (rain.height() * 0.08).min(48.0);
    let (bottom, top) = (rain.bottom(), rain.bottom() - h);
    let mut mesh = egui::Mesh::default();
    for black in [false, true] {
        for s in spans.iter().filter(|s| s.black == black) {
            let Some(Some(d)) = lit.get(s.key) else {
                continue;
            };
            let c = d.bands[0];
            let a = (255.0 * strength.clamp(0.0, 1.0)).round() as u8;
            let base = Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a);
            let i = mesh.vertices.len() as u32;
            for (x, y, col) in [
                (s.x0, top, Color32::TRANSPARENT),
                (s.x1, top, Color32::TRANSPARENT),
                (s.x1, bottom, base),
                (s.x0, bottom, base),
            ] {
                mesh.colored_vertex(Pos2::new(x, y), col);
            }
            mesh.add_triangle(i, i + 1, i + 2);
            mesh.add_triangle(i, i + 2, i + 3);
        }
    }
    if !mesh.is_empty() {
        p.add(egui::Shape::mesh(mesh));
    }
}

/// B2-F11: the label at the top of the rainfall — how far ahead its top is,
/// in seconds (Time-Based) or in beats (Tick-Based; ticks when the file has
/// no beats, SMPTE).
pub(crate) fn rain_unit(
    mapping: player::rain::Mapping,
    fall_nanos: f64,
    map: &plumspirit_engine::model::tempo::TempoMap,
    ppqn: Option<u64>,
) -> String {
    // Two decimals, trailing zeros dropped; below 0.01 or from ten million
    // up, a short exponent (B2-F20: "1e-6 s", "2.5e-6 beats", "1e300 s").
    let trim = |v: f64| {
        let cut = |t: &str| t.trim_end_matches('0').trim_end_matches('.').to_owned();
        if v != 0.0 && !(0.01..1e7).contains(&v.abs()) {
            let t = format!("{v:.2e}");
            let (m, e) = t.split_once('e').unwrap_or((&t, "0"));
            return format!("{}e{e}", cut(m));
        }
        let t = cut(&format!("{v:.2}"));
        if t.is_empty() { "0".to_owned() } else { t }
    };
    match mapping {
        player::rain::Mapping::Time => format!("Top: {} s ahead", trim(fall_nanos / 1e9)),
        player::rain::Mapping::Tick => {
            let span = player::rain::tick_span(map, fall_nanos);
            match ppqn {
                Some(q) if q > 0 => {
                    let beats = span / q as f64;
                    let unit = if (beats - 1.0).abs() < 0.005 {
                        "beat"
                    } else {
                        "beats"
                    };
                    format!("Top: {} {unit} ahead", trim(beats))
                }
                _ => format!("Top: {} ticks ahead", trim(span.round())),
            }
        }
    }
}

impl PlumSpiritApp {
    /// Keys the Player shows: the shown snapshot's key range (else the
    /// document's).
    fn player_keys(&self, doc: &plumspirit_engine::model::PlumDocument) -> usize {
        self.player
            .current()
            .map_or(doc.project().keys, |p| p.snapshot.document().project().keys)
            .keys()
    }

    /// The falling notes (P6) and the keyboard (P8) of the shown snapshot
    /// at the transport position: the window from the playhead to the
    /// falling time later, in song time; the shared tile service draws the
    /// Player family into key columns. Nothing is drawn before the first
    /// snapshot (the caller shows that it is being prepared).
    fn player_rainfall(&mut self, ui: &egui::Ui, rect: Rect, dark: bool) {
        let ctx = ui.ctx().clone();
        let Some((keys, timing, sources, ppqn)) = self.player.current().map(|p| {
            (
                p.snapshot.document().project().keys,
                Arc::clone(&p.timing),
                Arc::clone(&p.sources),
                p.snapshot.document().division().musical_ppqn(),
            )
        }) else {
            return;
        };
        let (rain, keyboard) = player_layout(rect, keys.keys());
        let spans = player::keys::spans(keys.keys(), rain.left(), rain.right());
        let ppp = ctx.pixels_per_point();
        let mut lit_tick = None;
        if rain.height() >= 2.0 {
            let cols = player::keys::columns(
                &spans,
                keys.keys(),
                player::keys::inset_points(rain.width(), ppp),
                1.0 / ppp,
            );
            // P9: drawn at the smooth clock (the transport between audio
            // periods); the statistics stay at the transport's tick.
            let speed = self.settings.speed.to_f64().max(1e-6);
            let period = self
                .transport
                .period()
                .unwrap_or(DEFAULT_PERIOD)
                .as_secs_f64()
                * 1e9
                * speed;
            let running = self.transport.state().is_running();
            let transport = timing.nanos_at(self.transport.position()) as f64;
            let now = self
                .player
                .clock
                .time(transport, running, speed, period, Instant::now());
            // B2-F20: any finite positive falling time (a hand-edited 0
            // falls back to the default rather than dividing by it).
            let secs = self.settings.player.fall_seconds;
            let secs = if player::valid_fall_seconds(secs) {
                secs
            } else {
                player::DEFAULT_FALL_SECONDS
            };
            let fall = secs * 1e9;
            // B2-F11: only the vertical axis depends on the mapping.
            let mapping = self.settings.player.mapping;
            let axis = match mapping {
                player::rain::Mapping::Time => player::rain::RainAxis::time(
                    timing.map(),
                    now,
                    fall,
                    player::rain::ramp_pieces(rain.height() * ppp),
                ),
                player::rain::Mapping::Tick => {
                    player::rain::RainAxis::tick(timing.map(), now, fall)
                }
            };
            let (tick, top) = (axis.lo, axis.hi);
            lit_tick = Some(tick);
            let bends = axis.bends();
            let h = f64::from(rain.height());
            let bottom = rain.bottom();
            // B2-F20: heights far outside the view (a microsecond or an
            // hours-long window) are held just beyond it: the clip hides them
            // and every coordinate stays finite and in order.
            let y = move |t: f64| bottom - (axis.frac(t).clamp(-1.0, 2.0) * h) as f32;
            let level = tiles::level_for(h * f64::from(ppp) / (top - tick) as f64);
            let palette = crate::ui::theme::note_palette(&ctx);
            let id = player::family_id(&sources, keys, &palette, dark);
            let columns = Columns {
                cols: &cols,
                y: &y,
                bends: &bends,
                clip: rain,
                shade: f32::from(self.settings.player.gradient) / 100.0,
            };
            // L-G05: the tile workers wake the UI when a tile is done.
            self.roll.dense_mut().draw_rainfall(
                &ui.painter_at(rain),
                &ctx,
                id,
                || player::family(&sources, keys, &palette, dark).1,
                &columns,
                (tick, top),
                level,
            );
            // B2-F11: what the top of the view stands for.
            ui.painter_at(rain).text(
                Pos2::new(rain.right() - 6.0, rain.top() + 4.0),
                Align2::RIGHT_TOP,
                rain_unit(mapping, fall, timing.map(), ppqn),
                FontId::proportional(theme::size::SMALL),
                Color32::from_gray(150),
            );
        }
        // v1.0-beta1 (B1-0111, PFA): each key in the colour of the note the
        // rainfall shows at the hit line.
        let tick = lit_tick.unwrap_or_else(|| self.transport.position());
        let display = self.player_lit(&ctx, dark, tick);
        // v1.0-beta1 (B1-0143): the optional hit glow, Player only.
        let glow = f32::from(self.settings.player.glow) / 100.0;
        if glow > 0.0 && rain.height() >= 2.0 {
            paint_glow(&ui.painter_at(rain), rain, &spans, &display, glow);
        }
        player::keys::paint_keyboard(&ui.painter_at(keyboard), keyboard, &spans, &|k| {
            display.get(k).copied().flatten()
        });
    }

    /// What each key of the Player's keyboard shows at `tick` (v1.0-beta1
    /// B1-0111, PFA `PFAVIZ_PLAYER.md` §4): the colour of the latest-starting
    /// note sounding on it (`tiles::lit_keys`, the rainfall's own order),
    /// coloured by the Player family's colours — so a key and the note
    /// falling onto it always agree; one colour per key; the same at every
    /// frame at one tick. Empty before the first snapshot.
    pub(super) fn player_lit(
        &self,
        ctx: &egui::Context,
        dark: bool,
        tick: u64,
    ) -> Vec<Option<KeyDisplay>> {
        let Some(p) = self.player.current() else {
            return Vec::new();
        };
        let sources = &p.sources;
        let keys = p.snapshot.document().project().keys.keys();
        let stores: Vec<&plumspirit_engine::model::NoteStore> =
            sources.stores.iter().map(|s| s.as_ref()).collect();
        let mut lit = Vec::new();
        tiles::lit_keys(
            &stores,
            &sources.places,
            &sources.readings,
            &sources.lit_masks,
            tick,
            keys,
            &mut lit,
        );
        let palette = crate::ui::theme::note_palette(ctx);
        let colors = player::colors(palette, dark, &sources.bases, &sources.tracks);
        lit.iter()
            .map(|l| {
                l.map(|(si, v)| {
                    let (c, _) = tiles::TileColors::color(&colors, si, &v);
                    let mut d = KeyDisplay {
                        total: 1,
                        n: 1,
                        ..KeyDisplay::default()
                    };
                    d.bands[0] = Color32::from_rgb(c[0], c[1], c[2]);
                    d
                })
            })
            .collect()
    }

    /// The Player replaces the editor area (it is on and a project is open).
    pub(super) fn player_shown(&self) -> bool {
        self.settings.player.enabled && self.open.is_some()
    }

    /// Shows or hides the Player (the setting is saved). Hiding it keeps no
    /// snapshot.
    pub(super) fn set_player(&mut self, on: bool) {
        if self.settings.player.enabled == on {
            return;
        }
        self.settings.player.enabled = on;
        // The Inspector reads the same snapshot (S18, `docs/INSPECTOR.md` I1).
        if !on && !self.inspector_shown() {
            self.player.clear();
        }
        self.save_settings();
    }

    /// Per frame (from `update_live`): the MIDI Player Settings window.
    pub(super) fn update_player_window(&mut self, ctx: &egui::Context) {
        if self.player_window.is_none() {
            return;
        }
        match crate::player::settings_window(ctx, &mut self.settings.player) {
            WindowOutcome::Open => {}
            WindowOutcome::Ok => {
                self.player_window = None;
                if !self.settings.player.enabled && !self.inspector_shown() {
                    self.player.clear();
                }
                self.save_settings();
            }
            WindowOutcome::Cancel => {
                if let Some(original) = self.player_window.take() {
                    self.settings.player = original;
                }
            }
        }
    }

    /// The panel text at the transport position and whether it describes
    /// an earlier version of the notes (an update is running). `fps`: the
    /// frame rate while playing (`None`: paused or stopped). `None`: nothing
    /// analysed yet.
    pub(super) fn player_text(&mut self, fps: Option<u32>) -> Option<(String, bool)> {
        let src = &self.settings.player.template;
        if self.player.template.as_ref().is_none_or(|(s, _)| s != src) {
            self.player.template = Some((src.clone(), progressor::Template::parse(src)));
        }
        let needs_bpm = self
            .player
            .template
            .as_ref()?
            .1
            .uses(|f| f == PlayerField::Bpm);
        let tick = self.transport.position();
        let snap = self.analysis.query(
            tick,
            &Scope::Project,
            DEFAULT_WINDOW_NS,
            Peaks {
                window: false,
                so_far: true,
            },
        )?;
        let m = &snap.metrics;
        let bpm = match (needs_bpm, self.doc()) {
            (true, Some(d)) => live_bpm(d, m.tick, self.settings.speed).0,
            _ => String::new(),
        };
        let length = self.player.current().map_or_else(
            || "—".to_string(),
            |p| format_clock(p.length_ns as f64 / 1e9),
        );
        let readout = PlayerReadout {
            time: format_clock(m.nanos as f64 / 1e9),
            length,
            fps,
            nps: m.nps,
            peak_nps: snap.peak_nps,
            poly: m.poly,
            peak_poly: snap.peak_poly,
            bpm,
            total: m.total_notes,
            passed: snap.ended,
        };
        let t = &self.player.template.as_ref()?.1;
        Some((
            stats::render_text(t, &readout, self.settings.progressor.number_format),
            snap.stale,
        ))
    }

    /// Draws the Player over `rect` (the editor area): the rainfall above
    /// the keyboard, the stats panel, the hints.
    pub(super) fn player_frame(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        pal: &Palette,
        dark: bool,
        running: bool,
    ) {
        let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) else {
            return;
        };
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, crate::player::BACKGROUND);
        // (The snapshot is kept current in `update_live`, before the panels.)
        self.player_rainfall(ui, rect, dark);
        let now = Instant::now();
        let fps = if running {
            self.player.frames.frame(now);
            Some(self.player.frames.fps(now))
        } else {
            self.player.frames.reset();
            None
        };
        if self.player.current().is_none() {
            // Never blank (§056): the first snapshot is being taken.
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Preparing the MIDI Player…",
                FontId::proportional(theme::size::BODY),
                pal.text_weak,
            );
        }
        // P2: Mute / Solo change only the sound.
        if doc.tracks().iter().any(|t| t.mute || t.solo) {
            painter.text(
                Pos2::new(rect.left() + 10.0, rect.bottom() - 10.0),
                Align2::LEFT_BOTTOM,
                "Mute / Solo change only what you hear: the Player shows every exported track.",
                FontId::proportional(theme::size::SMALL),
                Color32::from_gray(170),
            );
        }
        if let Some((text, stale)) = self.player_text(fps) {
            let rect = player_layout(rect, self.player_keys(&doc)).0;
            let style = progressor::ProgressorSettings {
                corner: Corner::TopLeft,
                offset: [0.0, 0.0],
                ..self.settings.progressor.clone()
            };
            // B1-0065: it fades while the pointer is over it (no chip: the
            // Player is not edited).
            let pointer = ui.input(|i| i.pointer.hover_pos());
            let _ = progressor::paint_overlay(
                &painter,
                rect,
                &text,
                &style,
                pal,
                stale,
                progressor::OverlayState {
                    advanced: None,
                    pointer,
                    collapsed: false,
                    chip: false,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::commands::Command;
    use super::super::progressor::NumberFormat;
    use super::super::test_support::Harness;
    use super::*;
    use egui::Key;
    use plumspirit_engine::analysis::profile::PeakProfile;
    use plumspirit_engine::analysis::{AnalysisIndex, Timing};
    use plumspirit_engine::history::{History, Operation};
    use plumspirit_engine::model::conductor::Interp;
    use plumspirit_engine::model::track::{GatePercent, TrackProps};
    use plumspirit_engine::model::{
        Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument, Speed, TempoValue,
    };

    /// Two tracks of overlapping runs, a muted note, a tick / gate
    /// correction on the second, a tempo change.
    fn doc() -> PlumDocument {
        let mut d = PlumDocument::new_project("player".into(), Bpm::parse("120").unwrap(), 960);
        d.add_track().unwrap();
        for i in 0..2usize {
            let mut recs: Vec<NoteRecord> = (0..300u64)
                .map(|k| {
                    NoteRecord::new(
                        k * 120 + i as u64 * 7,
                        200 + (k % 4) * 90,
                        NoteAttrs::new(
                            30 + (k % 50) as u8,
                            1 + (k % 120) as u8,
                            i as u8,
                            0,
                            NoteOffKind::NoteOff,
                        )
                        .unwrap(),
                    )
                })
                .collect();
            recs[10].attrs = recs[10].attrs.with_muted(true);
            d.track_mut(i)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }
        let id = d.track(1).unwrap().id();
        History::default()
            .apply(
                &mut d,
                Operation::TrackProps {
                    track: id,
                    props: TrackProps {
                        tick_correction: 333,
                        gate: GatePercent::from_hundredths(25_000).unwrap(),
                        ..TrackProps::default()
                    },
                    end: None,
                    label: "Track Properties",
                },
            )
            .unwrap();
        let edit = d
            .conductor()
            .prepare_set_start_tempo(plumspirit_engine::model::TempoValue::Bpm(
                Bpm::parse("150").unwrap(),
            ))
            .unwrap();
        History::default()
            .apply(
                &mut d,
                Operation::Conductor {
                    edit,
                    label: "Change Tempo",
                },
            )
            .unwrap();
        d
    }

    #[test]
    fn the_midi_player_toggles_from_the_menu_f10_and_esc_and_note_keys_do_nothing_there() {
        let mut h = Harness::new(doc());
        assert!(!h.app.player_shown());
        assert!(h.command(Command::ToggleMidiPlayer));
        assert!(h.app.player_shown());
        assert!(h.app.settings.player.enabled, "the setting follows");
        h.press(Key::Escape);
        assert!(!h.app.player_shown(), "Esc returns to the editor");
        h.press(Key::F10);
        assert!(h.app.player_shown(), "F10 shows it again");
        // A note command's key does nothing while the Player is shown …
        h.chord(Command::SelectAll);
        assert_eq!(h.app.selected_count(), 0);
        assert!(h.app.player_shown());
        // … and from the menu it returns to the editor, where it acts: the
        // Piano Roll's notes (B2-F29: in the Track List, Select All takes
        // every clip, as its key does there).
        h.app.main_view = super::super::track_list::MainView::PianoRoll;
        assert!(h.command(Command::SelectAll));
        assert!(!h.app.player_shown());
        assert!(h.app.selected_count() > 0);
        h.press(Key::F10);
        h.press(Key::F10);
        assert!(!h.app.player_shown(), "F10 toggles");
        // The settings window opens with the settings Cancel restores.
        assert!(h.command(Command::MidiPlayerSettings));
        assert_eq!(h.app.player_window.as_ref(), Some(&h.app.settings.player));
    }

    #[test]
    fn the_player_view_is_never_blank_and_shows_its_panel_and_the_mute_hint() {
        let mut h = Harness::new(doc());
        h.command(Command::ToggleMidiPlayer);
        let pal = crate::ui::theme::palette(egui::Theme::Dark);
        let rect = Rect::from_min_size(Pos2::new(200.0, 60.0), egui::vec2(900.0, 600.0));
        let texts = |h: &mut Harness| {
            let ctx = h.ctx.clone();
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                h.app.player_frame(ui, rect, pal, true, false);
            });
            out.textures_delta.clear();
            let mut v: Vec<String> = Vec::new();
            let mut background = false;
            for c in &out.shapes {
                match &c.shape {
                    egui::Shape::Text(t) => v.push(t.galley.text().to_owned()),
                    egui::Shape::Rect(r) if r.fill == crate::player::BACKGROUND => {
                        background = true;
                    }
                    _ => {}
                }
            }
            assert!(background, "the near-black background is drawn");
            v
        };
        // The first frame starts the snapshot: something is shown.
        let first = texts(&mut h);
        assert!(!first.is_empty(), "never blank");
        let mut shown = Vec::new();
        for _ in 0..5000 {
            h.app.update_live(&h.ctx.clone(), true);
            shown = texts(&mut h);
            if h.app.player.current().is_some() && shown.iter().any(|t| t.starts_with("Passed: ")) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(h.app.player.current().is_some(), "the snapshot arrived");
        assert!(
            !shown.iter().any(|t| t.starts_with("Preparing")),
            "{shown:?}"
        );
        assert!(
            shown.iter().any(|t| t.starts_with("Time: 0:00.000 / ")),
            "{shown:?}"
        );
        assert!(!shown.iter().any(|t| t.starts_with("Mute / Solo")));
        // Muting a track only changes the sound: the Player says so.
        if let Some(o) = h.app.open.as_mut() {
            std::sync::Arc::make_mut(&mut o.doc)
                .track_mut(0)
                .unwrap()
                .mute = true;
        }
        let muted = texts(&mut h);
        assert!(
            muted.iter().any(|t| t.starts_with("Mute / Solo")),
            "{muted:?}"
        );
    }

    /// The panel text once the snapshot (the file's length) and the analysis
    /// (peaks included) have caught up at the transport's position.
    /// B2-F11: Tick-Based changes only the rainfall's vertical axis — the
    /// panel (statistics), the keyboard and every other text stay the same
    /// as Time-Based at one transport position; the top label says beats
    /// instead of seconds; the mapping is saved with the settings.
    #[test]
    fn tick_based_rainfall_changes_only_the_vertical_axis() {
        let mut h = Harness::new(doc()); // 150 BPM, 960 PPQN
        h.command(Command::ToggleMidiPlayer);
        let d = h.doc().clone();
        let generation = h.app.doc_generation();
        let _ = h.app.transport.seek(5_000, &d, generation);
        let pal = crate::ui::theme::palette(egui::Theme::Dark);
        let rect = Rect::from_min_size(Pos2::new(200.0, 60.0), egui::vec2(900.0, 600.0));
        let look = |h: &mut Harness, m: player::rain::Mapping| {
            h.app.settings.player.mapping = m;
            let panel = settled_text(h, None);
            let ctx = h.ctx.clone();
            let lit = h.app.player_lit(&ctx, true, h.app.transport.position());
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                h.app.player_frame(ui, rect, pal, true, false);
            });
            out.textures_delta.clear();
            let mut texts: Vec<String> = out
                .shapes
                .iter()
                .filter_map(|c| match &c.shape {
                    egui::Shape::Text(t) => Some(t.galley.text().to_owned()),
                    _ => None,
                })
                .collect();
            texts.sort();
            (panel, lit, texts)
        };
        let (p_time, lit_time, t_time) = look(&mut h, player::rain::Mapping::Time);
        let (p_tick, lit_tick, t_tick) = look(&mut h, player::rain::Mapping::Tick);
        assert_eq!(
            p_time, p_tick,
            "the statistics do not depend on the mapping"
        );
        assert_eq!(lit_time, lit_tick, "nor does the keyboard");
        assert!(
            lit_time.iter().any(Option::is_some),
            "keys are lit at tick 5,000"
        );
        let other = |t: &[String]| -> Vec<String> {
            t.iter()
                .filter(|s| !s.starts_with("Top: "))
                .cloned()
                .collect()
        };
        assert_eq!(other(&t_time), other(&t_tick));
        assert!(t_time.contains(&"Top: 2 s ahead".to_owned()), "{t_time:?}");
        // Two seconds at the opening 150 BPM are five beats.
        assert!(
            t_tick.contains(&"Top: 5 beats ahead".to_owned()),
            "{t_tick:?}"
        );
        let map = Timing::for_document(&d);
        let one = super::rain_unit(player::rain::Mapping::Tick, 0.4e9, map.map(), Some(960));
        assert_eq!(one, "Top: 1 beat ahead");
        let smpte = super::rain_unit(player::rain::Mapping::Tick, 2e9, map.map(), None);
        assert_eq!(smpte, "Top: 4800 ticks ahead");
        let huge = super::rain_unit(player::rain::Mapping::Time, f64::MAX, map.map(), None);
        assert_eq!(huge, "Top: 1.8e299 s ahead");
        let mid = super::rain_unit(player::rain::Mapping::Time, 12.5e9, map.map(), None);
        assert_eq!(mid, "Top: 12.5 s ahead");
        // Saved and read back with the other Player settings.
        let mut p = Vec::new();
        let back = player::PlayerSettings::from_json(&h.app.settings.player.to_json(), &mut p);
        assert_eq!(back.mapping, player::rain::Mapping::Tick);
        assert!(p.is_empty());
        let bad = serde_json::json!({"mapping": "beats", "fall_seconds": 3.0});
        let got = player::PlayerSettings::from_json(&bad, &mut p);
        assert_eq!(
            (got.mapping, got.fall_seconds),
            (player::rain::Mapping::Time, 3.0)
        );
        assert_eq!(p, ["player.mapping is invalid; using the default"]);
    }

    /// B2-F20: a microsecond and ten hours of falling time draw in both
    /// mappings — the label says the window, every coordinate is finite and
    /// the number of shapes stays bounded by the view, not by the song.
    #[test]
    fn tiny_and_hours_long_falling_times_draw_a_bounded_frame() {
        let mut h = Harness::new(doc());
        h.command(Command::ToggleMidiPlayer);
        let d = h.doc().clone();
        let generation = h.app.doc_generation();
        let _ = h.app.transport.seek(5_000, &d, generation);
        let pal = crate::ui::theme::palette(egui::Theme::Dark);
        let rect = Rect::from_min_size(Pos2::new(200.0, 60.0), egui::vec2(900.0, 600.0));
        for (secs, mapping, label) in [
            (1e-6, player::rain::Mapping::Time, "Top: 1e-6 s ahead"),
            (36_000.0, player::rain::Mapping::Time, "Top: 36000 s ahead"),
            (1e-6, player::rain::Mapping::Tick, "Top: 2.5e-6 beats ahead"),
            (
                36_000.0,
                player::rain::Mapping::Tick,
                "Top: 90000 beats ahead",
            ),
        ] {
            h.app.settings.player.fall_seconds = secs;
            h.app.settings.player.mapping = mapping;
            let _ = settled_text(&mut h, None);
            let ctx = h.ctx.clone();
            let mut shapes = Vec::new();
            for _ in 0..50 {
                let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                    h.app.player_frame(ui, rect, pal, true, false);
                });
                out.textures_delta.clear();
                shapes = out.shapes;
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            let texts: Vec<String> = shapes
                .iter()
                .filter_map(|c| match &c.shape {
                    egui::Shape::Text(t) => Some(t.galley.text().to_owned()),
                    _ => None,
                })
                .collect();
            assert!(
                texts.iter().any(|t| t == label),
                "{secs} {mapping:?}: {texts:?}"
            );
            let mut vertices = 0usize;
            for c in &shapes {
                let r = c.shape.visual_bounding_rect();
                assert!(
                    !r.is_positive() || (r.min.y.is_finite() && r.max.y.is_finite()),
                    "{secs} {mapping:?}: {r:?}"
                );
                if let egui::Shape::Mesh(m) = &c.shape {
                    vertices += m.vertices.len();
                    assert!(m.vertices.iter().all(|v| v.pos.y.is_finite()));
                }
            }
            assert!(
                shapes.len() < 4_000,
                "{secs} {mapping:?}: {} shapes",
                shapes.len()
            );
            assert!(
                vertices < 400_000,
                "{secs} {mapping:?}: {vertices} vertices"
            );
        }
    }

    fn settled_text(h: &mut Harness, fps: Option<u32>) -> String {
        for _ in 0..5000 {
            h.app.update_live(&h.ctx.clone(), true);
            if let Some((t, stale)) = h.app.player_text(fps)
                && !stale
                && !t.contains('…')
                && h.app.player.current().is_some()
            {
                return t;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the analysis never caught up");
    }

    /// The panel's text for the exported file of `d` at tick `t`, computed
    /// from scratch (the export snapshot, a fresh index and peak profile).
    fn expected(d: &PlumDocument, t: u64, fps: Option<u32>, speed: Speed) -> String {
        let snap = crate::player::snapshot(d);
        let mut notes: Vec<(u64, u64)> = Vec::new();
        snap.for_each_note(|n| notes.push((n.start, n.end)));
        let idx = AnalysisIndex::for_document(d);
        let timing = Timing::for_document(d);
        let scope = Scope::Project;
        let profile = PeakProfile::build(&idx, &scope, &timing, DEFAULT_WINDOW_NS, None).unwrap();
        let m = idx.metrics(&scope, &timing, t, DEFAULT_WINDOW_NS);
        let passed = notes.iter().filter(|n| n.1 <= t).count() as u64;
        assert_eq!(idx.ends_in(&scope, None, t).0, passed);
        assert_eq!(
            m.total_notes,
            notes.len() as u64,
            "the index counts the file's notes"
        );
        stats::render_text(
            &progressor::Template::parse(stats::DEFAULT_TEMPLATE),
            &PlayerReadout {
                time: format_clock(m.nanos as f64 / 1e9),
                length: format_clock(timing.nanos_at(snap.end_tick()) as f64 / 1e9),
                fps,
                nps: m.nps,
                peak_nps: Some(profile.peak_nps_so_far(&idx, &scope, &timing, t)),
                poly: m.poly,
                peak_poly: Some(profile.peak_poly_so_far(&idx, &scope, t)),
                bpm: live_bpm(d, t, speed).0,
                total: notes.len() as u64,
                passed,
            },
            NumberFormat::Plain,
        )
    }

    #[test]
    fn the_player_panel_is_the_exported_files_statistics_at_the_transport_position() {
        let mut h = Harness::new(doc());
        h.command(Command::ToggleMidiPlayer);
        h.app.settings.progressor.number_format = NumberFormat::Plain;
        let d = h.doc().clone();
        let generation = h.app.doc_generation();
        let snap = crate::player::snapshot(&d);
        let mut notes: Vec<(u64, u64)> = Vec::new();
        snap.for_each_note(|n| notes.push((n.start, n.end)));
        let end = notes.iter().map(|n| n.1).max().unwrap();
        // Seek, a loop wrap's target, a note end exactly, the song end,
        // beyond it, and back to the start (exact after any jump).
        let ticks = [5_000, 960, notes[40].1, end, end + 10_000, 0, 17_777];
        for (k, &t) in ticks.iter().enumerate() {
            let _ = h.app.transport.seek(t, &d, generation);
            let fps = (k % 2 == 1).then_some(57);
            let got = settled_text(&mut h, fps);
            let passed = notes.iter().filter(|n| n.1 <= t).count();
            assert_eq!(
                got,
                expected(&d, t, fps, h.app.settings.speed),
                "at tick {t}"
            );
            assert!(got.contains(&format!("Notes: {}\n", notes.len())), "{got}");
            assert!(got.contains(&format!("Passed: {passed}")), "{got}");
            assert!(got.contains(if fps.is_some() {
                "FPS: 57\n"
            } else {
                "FPS: —\n"
            }));
        }
        // Playback speed changes what you hear, not the file's BPM.
        h.app.settings.speed = plumspirit_engine::model::Speed::parse("2").unwrap();
        let fast = settled_text(&mut h, None);
        assert!(fast.contains("BPM: 150\n"), "{fast}");
    }

    /// v1.0-beta1 B1-0144: the panel stays the exported file's statistics
    /// under a tempo ramp, Mute / Solo (sound only, P2) and an edit while
    /// the Player is shown.
    #[test]
    fn the_player_panel_follows_tempo_ramps_ignores_mute_solo_and_follows_edits() {
        let mut d = doc();
        // A ramp: 150 BPM at 0 rising linearly to 240 BPM at 20,000.
        for (tick, bpm, interp) in [(0, "150", Interp::Linear), (20_000, "240", Interp::Step)] {
            let edit = d
                .conductor()
                .prepare_set_tempo(
                    None,
                    tick,
                    TempoValue::Bpm(Bpm::parse(bpm).unwrap()),
                    interp,
                )
                .unwrap();
            History::default()
                .apply(
                    &mut d,
                    Operation::Conductor {
                        edit,
                        label: "Change Tempo",
                    },
                )
                .unwrap();
        }
        let mut h = Harness::new(d);
        h.command(Command::ToggleMidiPlayer);
        h.app.settings.progressor.number_format = NumberFormat::Plain;
        let check = |h: &mut Harness, label: &str| {
            let d = h.doc().clone();
            let generation = h.app.doc_generation();
            for t in [0u64, 7_777, 19_999, 20_000, 31_000] {
                let _ = h.app.transport.seek(t, &d, generation);
                let got = settled_text(h, None);
                // The panel describes the file: Mute / Solo play no part.
                let mut plain = d.clone();
                for i in 0..plain.tracks().len() {
                    let tr = plain.track_mut(i).unwrap();
                    tr.mute = false;
                    tr.solo = false;
                }
                assert_eq!(
                    got,
                    expected(&plain, t, None, h.app.settings.speed),
                    "{label} at tick {t}"
                );
            }
        };
        check(&mut h, "ramp");
        // Inside the ramp the BPM is neither end's.
        let d = h.doc().clone();
        let _ = h.app.transport.seek(10_000, &d, h.app.doc_generation());
        let mid = settled_text(&mut h, None);
        assert!(
            !mid.contains("BPM: 150\n") && !mid.contains("BPM: 240\n"),
            "{mid}"
        );
        for (mute, solo) in [(true, false), (false, true)] {
            if let Some(o) = h.app.open.as_mut() {
                let d = std::sync::Arc::make_mut(&mut o.doc);
                d.track_mut(0).unwrap().mute = mute;
                d.track_mut(1).unwrap().solo = solo;
            }
            check(&mut h, if mute { "muted" } else { "soloed" });
        }
        // An edit while the Player is shown: the panel follows the new file.
        let before = settled_text(&mut h, None);
        h.app.apply_op(Operation::DeleteTrack { index: 0 });
        assert_eq!(h.doc().tracks().len(), 1);
        check(&mut h, "after deleting a track");
        assert_ne!(settled_text(&mut h, None), before);
    }

    #[test]
    fn the_player_keyboard_lights_the_exported_notes_in_the_players_colours() {
        let mut d = doc();
        // Track 2 (corrected +333 ticks, gate 250 %) also plays key 46 over
        // track 1's note at 1,920 – 2,120: 2,033 – 2,283 once corrected.
        d.track_mut(1)
            .unwrap()
            .notes
            .insert_sorted_batch(
                &[NoteRecord::new(
                    1_700,
                    100,
                    NoteAttrs::new(46, 90, 1, 0, NoteOffKind::NoteOff).unwrap(),
                )],
                false,
            )
            .unwrap();
        let mut h = Harness::new(d);
        h.command(Command::ToggleMidiPlayer);
        let d = h.doc().clone();
        let generation = h.app.doc_generation();
        let snap = crate::player::snapshot(&d);
        // (start, end, track, key).
        type Note = (u64, u64, u32, u8);
        let mut notes: Vec<Note> = Vec::new();
        snap.for_each_note(|n| notes.push((n.start, n.end, n.track, n.key)));
        let bases = crate::player::track_bases(&d);
        let palette = crate::ui::theme::NotePalette::default();
        let colour = |t: u32| {
            let f = palette.entry(bases[t as usize].0, 127, true).fill;
            egui::Color32::from_rgb(f.r(), f.g(), f.b())
        };
        // Both tracks' colours differ (P7).
        assert_ne!(colour(0), colour(1));
        // v1.0-beta1 (B1-0111, PFA): one colour per key — the latest-starting
        // note sounding there (ties: the later track), never an upcoming one.
        let mut both = false;
        for _ in 0..5000 {
            h.app.update_live(&h.ctx.clone(), true);
            if h.app.player.current().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        for t in [0u64, 2_000, 2_050, 5_000, 5_333, 17_777, 36_200] {
            let _ = h.app.transport.seek(t, &d, generation);
            h.app.update_live(&h.ctx.clone(), true);
            let display = h.app.player_lit(&h.ctx.clone(), true, t);
            assert_eq!(display.len(), 128);
            for (pitch, k) in display.iter().enumerate() {
                let sounding: Vec<(usize, &Note)> = notes
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| usize::from(n.3) == pitch && n.0 <= t && t < n.1)
                    .collect();
                let Some(top) = sounding.iter().max_by_key(|(i, n)| (n.0, n.2, *i)) else {
                    assert!(k.is_none(), "tick {t} key {pitch}: nothing sounds");
                    continue;
                };
                let k = k.expect("a sounding key is lit");
                assert_eq!((k.total, k.n), (1, 1), "tick {t} key {pitch}");
                assert_eq!(k.bands[0], colour(top.1.2), "tick {t} key {pitch}");
                let tracks: std::collections::BTreeSet<u32> =
                    sounding.iter().map(|(_, n)| n.2).collect();
                both |= tracks.len() == 2;
            }
            // The same frame twice: the same keys (paused frames are stable).
            assert_eq!(h.app.player_lit(&h.ctx.clone(), true, t), display);
        }
        assert!(both, "a key where both tracks sound shows one color");
    }

    /// Resident memory of this process (Linux; 0 elsewhere), bytes.
    fn rss() -> u64 {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("VmRSS:"))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .and_then(|v| v.parse::<u64>().ok())
            })
            .map_or(0, |kb| kb * 1024)
    }

    /// V09-0042 / P11 bench (release): the Player at 1 M and 10 M notes —
    /// 16 tracks of chords, one with a tick / gate correction, one played
    /// through four linked clips. Snapshot take, the first frames until
    /// every tile is ready, settled frames, and a playing window (the
    /// transport moved 1/60 s per frame), with the process RSS.
    /// v1.0-beta1 B1-0145: a third case puts 10.24 M notes inside the
    /// falling window at once (every note visible in the first frames),
    /// played from its start; frame times also give the 95th percentile.
    #[test]
    #[ignore = "measurement: cargo test --release -p plumspirit player_frame_cost -- --ignored --nocapture"]
    fn player_frame_cost() {
        use std::time::{Duration, Instant};
        for (total, dense) in [
            (1_000_000u64, false),
            (10_000_000, false),
            (10_240_000, true),
        ] {
            let rss0 = rss();
            let per_track = total / 16;
            let mut d = PlumDocument::new_project("bench".into(), Bpm::parse("140").unwrap(), 960);
            for _ in 1..16 {
                d.add_track().unwrap();
            }
            for ti in 0..16usize {
                let recs: Vec<NoteRecord> = (0..per_track)
                    .map(|k| {
                        let chord = k / 16;
                        // Dense: every start inside the first 4,000 ticks
                        // (1.8 s at 140 BPM, under the 2 s falling time).
                        let start = if dense {
                            k * 4_000 / per_track
                        } else {
                            chord * 120 + ti as u64 * 3
                        };
                        NoteRecord::new(
                            start,
                            90 + (k % 5) * 30,
                            NoteAttrs::new(
                                (20 + (chord * 7 + k % 16 * 5 + ti as u64) % 90) as u8,
                                1 + (k % 126) as u8,
                                (ti % 16) as u8,
                                0,
                                NoteOffKind::NoteOff,
                            )
                            .unwrap(),
                        )
                    })
                    .collect();
                let mut recs = recs;
                recs.sort_by_key(|r| r.start_tick);
                d.track_mut(ti)
                    .unwrap()
                    .notes
                    .insert_sorted_batch(&recs, false)
                    .unwrap();
            }
            let id = d.track(3).unwrap().id();
            History::default()
                .apply(
                    &mut d,
                    Operation::TrackProps {
                        track: id,
                        props: TrackProps {
                            tick_correction: 120,
                            gate: GatePercent::from_hundredths(25_000).unwrap(),
                            ..TrackProps::default()
                        },
                        end: None,
                        label: "Track Properties",
                    },
                )
                .unwrap();
            {
                use plumspirit_engine::editor::clips::duplicate_clips;
                let p = plumspirit_engine::model::ClipId::Primary(d.track(5).unwrap().id());
                let span = d.track(5).unwrap().notes.max_end().unwrap_or(0) + 960;
                for k in 1..4u64 {
                    let op = duplicate_clips(&d, &[(p, 5, span * k)], true).unwrap();
                    History::default().apply(&mut d, op).unwrap();
                }
            }
            let rss_doc = rss();
            let t = Instant::now();
            let prep = crate::player::Prepared::of(&d);
            let take = t.elapsed();
            let end = song_end_of(&prep);
            drop(prep);
            let mut h = Harness::new(d);
            h.command(Command::ToggleMidiPlayer);
            let pal = crate::ui::theme::palette(egui::Theme::Dark);
            let rect = Rect::from_min_size(Pos2::new(220.0, 92.0), egui::vec2(1146.0, 650.0));
            let frame = |h: &mut Harness| -> Duration {
                let ctx = h.ctx.clone();
                let t = Instant::now();
                let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                    h.app.update_live(ui.ctx(), true);
                    h.app.player_frame(ui, rect, pal, true, false);
                });
                // Tessellated as the renderer receives it.
                let shapes = std::mem::take(&mut out.shapes);
                let prims = ctx.tessellate(shapes, out.pixels_per_point);
                std::hint::black_box(prims);
                let e = t.elapsed();
                out.textures_delta.clear();
                e
            };
            // Until the snapshot, the analysis and every tile are ready.
            let t = Instant::now();
            let mut first: Vec<Duration> = Vec::new();
            loop {
                first.push(frame(&mut h));
                let st =
                    h.app.roll.dense_mut().stats[crate::piano_roll::dense::Role::Player as usize];
                let stats_ready = h
                    .app
                    .player_text(None)
                    .is_some_and(|(t, stale)| !stale && !t.contains('…'));
                if h.app.player.current().is_some()
                    && stats_ready
                    && st.visible_tiles > 0
                    && st.ready == st.visible_tiles
                    && st.pending == 0
                {
                    break;
                }
                if t.elapsed() > Duration::from_secs(300) {
                    panic!("never ready: {st:?}");
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            let ready = t.elapsed();
            let rss_ready = rss();
            let stat = |mut v: Vec<Duration>| {
                v.sort();
                (
                    v[v.len() / 2].as_secs_f64() * 1e3,
                    v[v.len() - 1].as_secs_f64() * 1e3,
                    v[v.len() * 95 / 100].as_secs_f64() * 1e3,
                )
            };
            let first_max = first
                .iter()
                .max()
                .copied()
                .unwrap_or_default()
                .as_secs_f64()
                * 1e3;
            let settled = stat((0..60).map(|_| frame(&mut h)).collect());
            // Playing: 1/60 s of song per frame from the middle of the song.
            let generation = h.app.doc_generation();
            let doc = h.doc().clone();
            let timing = plumspirit_engine::analysis::Timing::for_document(&doc);
            let start_ns = if dense { 0 } else { timing.nanos_at(end / 2) };
            let built0 = h.app.roll.dense_mut().built;
            let mut playing = Vec::new();
            let mut waits = 0usize;
            let t = Instant::now();
            for f in 0..600u64 {
                let tick = timing.tick_at_nanos(start_ns + f * 1_000_000_000 / 60);
                let _ = h.app.transport.seek(tick, &doc, generation);
                playing.push(frame(&mut h));
                let st =
                    h.app.roll.dense_mut().stats[crate::piano_roll::dense::Role::Player as usize];
                waits += usize::from(st.ready < st.visible_tiles);
                assert!(
                    st.ready + st.stand_ins + st.coarse >= st.visible_tiles,
                    "never blank"
                );
            }
            let wall = t.elapsed();
            let built = h.app.roll.dense_mut().built - built0;
            let playing_stat = stat(playing);
            let st = h.app.roll.dense_mut().stats[crate::piano_roll::dense::Role::Player as usize];
            let mb = |b: u64| b as f64 / 1_048_576.0;
            println!(
                "player bench {} notes{} (16 tracks, 1 corrected, 4 linked clips): snapshot take {:.1} ms; ready (snapshot, analysis with peaks, tiles) after {:.0} ms ({} frames, slowest {:.1} ms); settled median {:.2} / slowest {:.2} ms; playing 600 frames median {:.2} / p95 {:.2} / slowest {:.2} ms, {} frames with a tile still building, {} tiles built ({:.0} / s); tile cache {:.1} MB; RSS doc +{:.1} MB, Player ready +{:.1} MB over the doc (incl. the analysis index the Progressor shares)",
                total,
                if dense { " all visible" } else { "" },
                take.as_secs_f64() * 1e3,
                ready.as_secs_f64() * 1e3,
                first.len(),
                first_max,
                settled.0,
                settled.1,
                playing_stat.0,
                playing_stat.2,
                playing_stat.1,
                waits,
                built,
                built as f64 / wall.as_secs_f64(),
                mb(st.cache_bytes as u64),
                mb(rss_doc.saturating_sub(rss0)),
                mb(rss_ready.saturating_sub(rss_doc)),
            );
        }
    }

    fn song_end_of(p: &crate::player::Prepared) -> u64 {
        let mut end = 1;
        p.snapshot.for_each_note(|n| end = end.max(n.end));
        end
    }
}
