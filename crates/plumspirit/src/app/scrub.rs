//! Play Selected / Scrub (contract §65): Playback → Play Selected / Scrub…
//! (F9), a modeless window driving the v0.6 audition synthesizer (its own
//! voices: it never disturbs the transport). Ten functions:
//!
//! 1. play only the selected notes; 2. reverse; 3. variable speed (¼× – 4×);
//! 4. snap-based scrub (dragging the cursor sounds the notes starting at each
//!    snap step it crosses); 5. loop the selection; 6. only the active
//!    subchannel; 7. only a pitch band; 8. audible cursor chase (the notes
//!    sounding at the cursor are heard while it moves); 9. velocity-normalized
//!    audition; 10. A / B trail markers (drawn on the roll; Play A→B).
//!
//! The note list is planned once per Play (seconds from the tempo map), then
//! a cursor walks it each frame: O(notes started this frame). Bounded:
//! [`MAX_PLAY_NOTES`] notes per play, [`MAX_PER_FRAME`] starts per frame,
//! [`MAX_CHASE`] notes per cursor step.

use super::PlumSpiritApp;
use crate::ui::status_bar::StatusKind;
use plumspirit_engine::midi::group_digits;
use std::time::{Duration, Instant};

pub const MAX_PLAY_NOTES: usize = 50_000;
const MAX_PER_FRAME: usize = 256;
const MAX_CHASE: usize = 16;

/// One planned note: start and length in seconds (from the first note), key,
/// velocity, channel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Planned {
    pub at: f64,
    pub len: f64,
    pub key: u8,
    pub vel: u8,
    pub ch: u8,
    pub tick: u64,
}

/// Plans a playback of notes given as (start s, length s, key, vel, ch, tick):
/// relative to the first note, reversed (mirrored in time) when asked,
/// divided by `speed`, sorted by start.
pub fn plan(mut v: Vec<Planned>, reverse: bool, speed: f64) -> (Vec<Planned>, f64) {
    if v.is_empty() {
        return (v, 0.0);
    }
    let t0 = v.iter().map(|n| n.at).fold(f64::INFINITY, f64::min);
    let t1 = v.iter().map(|n| n.at + n.len).fold(0.0, f64::max);
    let span = t1 - t0;
    for n in &mut v {
        n.at -= t0;
        if reverse {
            n.at = span - (n.at + n.len);
        }
        n.at /= speed;
        n.len /= speed;
    }
    v.sort_by(|a, b| a.at.total_cmp(&b.at).then(a.key.cmp(&b.key)));
    (v, span / speed)
}

struct Playing {
    notes: Vec<Planned>,
    next: usize,
    started: Instant,
    span: f64,
}

pub struct Scrub {
    pub open: bool,
    pub speed: f64,
    pub reverse: bool,
    pub looped: bool,
    pub sub_only: bool,
    pub band_on: bool,
    pub band: (u8, u8),
    pub chase: bool,
    pub snap_scrub: bool,
    pub norm_on: bool,
    pub norm_vel: u8,
    pub a: Option<u64>,
    pub b: Option<u64>,
    playing: Option<Playing>,
    last_cursor: Option<u64>,
}

impl Default for Scrub {
    fn default() -> Self {
        Scrub {
            open: false,
            speed: 1.0,
            reverse: false,
            looped: false,
            sub_only: false,
            band_on: false,
            band: (48, 84),
            chase: true,
            snap_scrub: false,
            norm_on: false,
            norm_vel: 100,
            a: None,
            b: None,
            playing: None,
            last_cursor: None,
        }
    }
}

impl PlumSpiritApp {
    fn scrub_filter(&self, sub: u32, key: u8) -> bool {
        let s = &self.scrub;
        if s.band_on && !(s.band.0.min(s.band.1)..=s.band.0.max(s.band.1)).contains(&key) {
            return false;
        }
        if s.sub_only
            && let Some(a) = self.active_sub_of(self.active_track())
            && a.id.0 != sub
        {
            return false;
        }
        true
    }

    /// Starts playing the selected notes (or, with `range`, every note of the
    /// active track starting in it).
    pub(super) fn scrub_play(&mut self, range: Option<(u64, u64)>) {
        let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) else {
            return;
        };
        // The notes the roll edits (an opened clip's content plays at its
        // song position: content tick + shift; V09-S11).
        let (target, shift) = (self.edit_target(), self.edit_shift());
        let Ok(t) = plumspirit_engine::editor::note_view(&doc, target) else {
            return;
        };
        let song = |c: u64| super::clip_session::shifted(c, shift);
        let range = range.map(|r| self.range_to_content(r));
        let tempo = plumspirit_engine::model::TempoMap::for_document_at(
            &doc,
            plumspirit_engine::transform::TEMPO_RATE,
            plumspirit_engine::model::Speed::NORMAL,
        );
        let mut v = Vec::new();
        let mut skipped = 0usize;
        let mut push = |n: &plumspirit_engine::model::NoteView, v: &mut Vec<Planned>| {
            if !self.scrub_filter(n.sub.0, n.attrs.pitch()) {
                return;
            }
            if v.len() >= MAX_PLAY_NOTES {
                skipped += 1;
                return;
            }
            let at = tempo.seconds_at_tick(song(n.start_tick));
            let len = (tempo.seconds_at_tick(song(n.end_tick())) - at).max(0.01);
            v.push(Planned {
                at,
                len,
                key: n.attrs.pitch(),
                vel: if self.scrub.norm_on {
                    self.scrub.norm_vel
                } else {
                    n.attrs.velocity()
                },
                ch: n.attrs.channel(),
                tick: song(n.start_tick),
            });
        };
        match range {
            Some((a, b)) => {
                t.notes.query(a, b, |_, n| {
                    if n.start_tick >= a && n.start_tick < b && !n.attrs.muted() {
                        push(&n, &mut v);
                    }
                });
            }
            None => {
                t.notes.for_each_selected(|n| {
                    if !n.attrs.muted() {
                        push(&n, &mut v);
                    }
                });
            }
        }
        if v.is_empty() {
            self.set_status(
                if range.is_some() {
                    "No notes between the A and B markers (with the scrub filters)."
                } else {
                    "Select notes to play (with the scrub filters)."
                },
                StatusKind::Info,
            );
            return;
        }
        self.scrub_start(v, skipped);
    }

    /// Plays planned notes (Spiderweb's Listen) with the scrub speed.
    pub(super) fn scrub_play_planned(&mut self, v: Vec<Planned>, skipped: usize) {
        if v.is_empty() {
            return;
        }
        self.scrub_start(v, skipped);
    }

    fn scrub_start(&mut self, v: Vec<Planned>, skipped: usize) {
        let (notes, span) = plan(v, self.scrub.reverse, self.scrub.speed.clamp(0.25, 4.0));
        let count = notes.len();
        self.transport.audition_all_off();
        self.scrub.playing = Some(Playing {
            notes,
            next: 0,
            started: Instant::now(),
            span,
        });
        self.set_status(
            if skipped > 0 {
                format!(
                    "Playing {} note(s) (the first {}; {} more not played).",
                    group_digits(count as u64),
                    group_digits(MAX_PLAY_NOTES as u64),
                    group_digits(skipped as u64)
                )
            } else {
                format!("Playing {} note(s).", group_digits(count as u64))
            },
            StatusKind::Info,
        );
    }

    /// Whether planned notes (Scrub, Spiderweb's Listen) are playing.
    #[cfg(test)]
    pub(super) fn scrub_is_playing(&self) -> bool {
        self.scrub.playing.is_some()
    }

    pub(super) fn scrub_stop(&mut self) {
        if self.scrub.playing.take().is_some() {
            self.transport.audition_all_off();
        }
    }

    fn scrub_sound(&mut self, key: u8, vel: u8, ch: u8, tick: u64, len: Duration) {
        let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) else {
            return;
        };
        let generation = self.doc_generation();
        let ti = self.active_track();
        if let Err(e) =
            self.transport
                .audition_on(&doc, generation, ti, ch, key, vel, tick, Some(len))
            && !self.audition_warned
        {
            self.audition_warned = true;
            self.set_status(e, StatusKind::Warning);
        }
    }

    /// Per frame: starts the planned notes that are due, and the cursor chase /
    /// snap scrub when the edit cursor moved.
    pub(super) fn scrub_frame(&mut self, ctx: &egui::Context) {
        // Playback of the plan.
        if let Some(p) = self.scrub.playing.as_mut() {
            let now = p.started.elapsed().as_secs_f64();
            let mut due = Vec::new();
            while p.next < p.notes.len() && p.notes[p.next].at <= now && due.len() < MAX_PER_FRAME {
                due.push(p.notes[p.next]);
                p.next += 1;
            }
            let finished = p.next >= p.notes.len() && now >= p.span;
            for n in due {
                self.scrub_sound(
                    n.key,
                    n.vel,
                    n.ch,
                    n.tick,
                    Duration::from_secs_f64(n.len.max(0.02)),
                );
            }
            if finished {
                if self.scrub.looped {
                    if let Some(p) = self.scrub.playing.as_mut() {
                        p.next = 0;
                        p.started = Instant::now();
                    }
                } else {
                    self.scrub.playing = None;
                }
            }
            ctx.request_repaint_after(Duration::from_millis(8));
        }
        // Cursor chase / snap scrub.
        if !self.scrub.open || self.transport.state().is_running() {
            self.scrub.last_cursor = None;
            return;
        }
        let cur = self.roll.cursor_tick;
        let prev = self.scrub.last_cursor.replace(cur);
        let Some(prev) = prev.filter(|&p| p != cur) else {
            return;
        };
        // The cursor is song time; the notes are on the roll's axis.
        let (cur_c, prev_c) = (self.to_content(cur), self.to_content(prev));
        let Some(notes) = self.edit_notes() else {
            return;
        };
        let mut hits: Vec<(u8, u8, u8, u64)> = Vec::new();
        if self.scrub.snap_scrub {
            // Notes starting between the two positions (each snap step crossed).
            let (a, b) = (prev_c.min(cur_c), prev_c.max(cur_c));
            notes.query(a, b.saturating_add(1), |_, n| {
                if hits.len() < MAX_CHASE && n.start_tick > a && n.start_tick <= b {
                    hits.push((
                        n.attrs.pitch(),
                        n.attrs.velocity(),
                        n.attrs.channel(),
                        n.sub.0 as u64,
                    ));
                }
            });
        } else if self.scrub.chase {
            notes.query(cur_c, cur_c.saturating_add(1), |_, n| {
                if hits.len() < MAX_CHASE && n.start_tick <= cur_c && n.end_tick() > cur_c {
                    hits.push((
                        n.attrs.pitch(),
                        n.attrs.velocity(),
                        n.attrs.channel(),
                        n.sub.0 as u64,
                    ));
                }
            });
        }
        let hits: Vec<_> = hits
            .into_iter()
            .filter(|h| self.scrub_filter(h.3 as u32, h.0))
            .collect();
        for (key, vel, ch, _) in hits {
            let vel = if self.scrub.norm_on {
                self.scrub.norm_vel
            } else {
                vel
            };
            self.scrub_sound(key, vel, ch, cur, Duration::from_millis(160));
        }
    }

    /// Playback → Play Selected / Scrub… window.
    pub(super) fn scrub_window_ui(&mut self, ctx: &egui::Context) {
        if !self.scrub.open || self.open.is_none() {
            return;
        }
        let mut open = true;
        let (mut play, mut play_ab, mut stop) = (false, false, false);
        let (mut set_a, mut set_b, mut clear_ab) = (false, false, false);
        let playing = self.scrub.playing.is_some();
        let cursor = self.roll.cursor_tick;
        // v0.9: the pitch band spans the project's keys (0–255 in 256-Key Mode).
        let top = self
            .doc()
            .map_or(plumspirit_engine::model::note::MAX_PITCH, |d| {
                d.project().keys.max_pitch()
            });
        let mut s = std::mem::take(&mut self.scrub);
        egui::Window::new("Play Selected / Scrub")
            .id(egui::Id::new("plumspirit-scrub"))
            .default_pos(ctx.content_rect().right_top() + egui::vec2(-340.0, 120.0))
            .open(&mut open)
            .default_width(300.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .button(if playing {
                            "Restart"
                        } else {
                            "▶ Play Selected"
                        })
                        .clicked()
                    {
                        play = true;
                    }
                    if ui
                        .add_enabled(playing, egui::Button::new("■ Stop"))
                        .clicked()
                    {
                        stop = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Speed");
                    crate::ui::stepper::slider(ui, &mut s.speed, 0.25..=4.0, "×");
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut s.reverse, "Reverse");
                    ui.checkbox(&mut s.looped, "Loop");
                });
                ui.separator();
                ui.checkbox(&mut s.sub_only, "Only the active subchannel");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut s.band_on, "Only pitches");
                    ui.add_enabled(
                        s.band_on,
                        crate::ui::stepper::Stepper::new(&mut s.band.0).range(0..=top),
                    );
                    ui.label("to");
                    ui.add_enabled(
                        s.band_on,
                        crate::ui::stepper::Stepper::new(&mut s.band.1).range(0..=top),
                    );
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut s.norm_on, "Same velocity for all");
                    ui.add_enabled(
                        s.norm_on,
                        crate::ui::stepper::Stepper::new(&mut s.norm_vel).range(1..=127),
                    );
                });
                ui.separator();
                ui.checkbox(
                    &mut s.chase,
                    "Cursor chase (hear the notes at the cursor while it moves)",
                );
                ui.checkbox(
                    &mut s.snap_scrub,
                    "Snap scrub (hear notes starting at each step crossed)",
                );
                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .button("Set A")
                        .on_hover_text("At the edit cursor")
                        .clicked()
                    {
                        set_a = true;
                    }
                    if ui
                        .button("Set B")
                        .on_hover_text("At the edit cursor")
                        .clicked()
                    {
                        set_b = true;
                    }
                    let ok = s.a.zip(s.b).is_some_and(|(a, b)| a != b);
                    if ui.add_enabled(ok, egui::Button::new("Play A→B")).clicked() {
                        play_ab = true;
                    }
                    if ui
                        .add_enabled(s.a.is_some() || s.b.is_some(), egui::Button::new("Clear"))
                        .clicked()
                    {
                        clear_ab = true;
                    }
                });
                let fmt = |v: Option<u64>| v.map_or("—".to_owned(), group_digits);
                ui.label(
                    egui::RichText::new(format!("A: {}   B: {} (ticks)", fmt(s.a), fmt(s.b)))
                        .weak(),
                );
            });
        if set_a {
            s.a = Some(cursor);
        }
        if set_b {
            s.b = Some(cursor);
        }
        if clear_ab {
            s.a = None;
            s.b = None;
        }
        self.scrub = s;
        if !open {
            self.scrub.open = false;
            self.scrub_stop();
        }
        if stop {
            self.scrub_stop();
        }
        if play {
            self.scrub_play(None);
        }
        if play_ab && let (Some(a), Some(b)) = (self.scrub.a, self.scrub.b) {
            self.scrub_play(Some((a.min(b), a.max(b))));
        }
    }

    /// The A / B markers over the Piano Roll (thin lines with labels).
    pub(super) fn draw_scrub_markers(&self, ctx: &egui::Context) {
        if self.scrub.a.is_none() && self.scrub.b.is_none() {
            return;
        }
        let Some(area) = self.roll.notes_rect() else {
            return;
        };
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("scrub-markers"),
        ));
        let p = painter.with_clip_rect(area);
        let pal = &crate::ui::theme::ui_palette(ctx);
        let v = self.roll.view;
        for (label, t) in [("A", self.scrub.a), ("B", self.scrub.b)] {
            let Some(t) = t else { continue };
            let x = area.left() + v.time.tick_to_x(t) as f32;
            p.vline(x, area.y_range(), egui::Stroke::new(1.5, pal.warning));
            p.text(
                egui::pos2(x + 3.0, area.top() + 2.0),
                egui::Align2::LEFT_TOP,
                label,
                egui::FontId::proportional(12.0),
                pal.warning,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(at: f64, len: f64, key: u8) -> Planned {
        Planned {
            at,
            len,
            key,
            vel: 100,
            ch: 0,
            tick: 0,
        }
    }

    #[test]
    fn plans_are_relative_reversible_and_scaled_by_speed() {
        let v = vec![n(2.0, 0.5, 60), n(3.0, 1.0, 62), n(2.5, 0.25, 64)];
        let (p, span) = plan(v.clone(), false, 1.0);
        assert_eq!(
            p.iter().map(|x| x.key).collect::<Vec<_>>(),
            vec![60, 64, 62]
        );
        assert_eq!(p[0].at, 0.0);
        assert_eq!(span, 2.0);
        // Reverse: the last note to end plays first.
        let (r, _) = plan(v.clone(), true, 1.0);
        assert_eq!(r[0].key, 62);
        assert!((r[0].at - 0.0).abs() < 1e-9);
        assert!((r.last().unwrap().at - 1.5).abs() < 1e-9, "{r:?}");
        // Double speed halves every time.
        let (f, s2) = plan(v, false, 2.0);
        assert_eq!(s2, 1.0);
        assert!((f[2].at - 0.5).abs() < 1e-9 && (f[2].len - 0.5).abs() < 1e-9);
    }
}
