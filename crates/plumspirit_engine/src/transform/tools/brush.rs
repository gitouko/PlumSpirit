//! Stroke tools (contract §57–58): Draw and Paint. While their window is open,
//! dragging in the Piano Roll records a pointer stroke (`Ctx::stroke`,
//! absolute ticks and pitch rows with timestamps); the kernel turns it into
//! notes on the active track. Preview shows the result of the stroke, Apply
//! adds it as one undo step. Deterministic: the same stroke and parameters
//! always give the same notes (randomness comes from the seed).
//!
//! * **Draw** draws a melodic line: one note per step following the pointer.
//! * **Paint** paints the grid cells the pointer passes over (a raster brush).

use super::super::boundary::{self, PitchPolicy};
use super::super::drivers::{Frame, Place};
use super::super::param::*;
use super::super::registry::{BuildCtx, Family, FunctionSpec, ToolDef, ToolSpec};
use super::super::rng::{Rng, hash, salt};
use super::super::shared::fast_hash::{FastMap, FastSet};
use super::super::shared::metric_position::bar_place;
use super::super::shared::scale_lattice::Lattice;
use super::super::sweep::SweptParams;
use super::super::theory::{KEY_NAMES, Scale, SnapMode, parse_chord, scale_names};
use super::super::{
    Ctx, Growth, Kernel, Output, Source, Stroke, StrokePoint, TNote, TransformError,
};
use super::{Contextual, NOTE_VALUES, boxed, note_value};
use std::sync::Arc;

/// The stroke's pitch at `tick` (linear between samples; the last-drawn sample
/// pair covering the tick wins), or None where the stroke never passed.
fn pitch_at(pts: &[StrokePoint], tick: u64) -> Option<f64> {
    if let [only] = pts {
        return (only.tick == tick).then_some(only.pitch);
    }
    let mut found = None;
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if tick < a.tick.min(b.tick) || tick > a.tick.max(b.tick) {
            continue;
        }
        let p = if b.tick == a.tick {
            b.pitch
        } else {
            let t = (tick as f64 - a.tick as f64) / (b.tick as f64 - a.tick as f64);
            a.pitch + (b.pitch - a.pitch) * t
        };
        found = Some(p);
    }
    found
}

/// Pointer speed around `tick` (rows per second plus beats per second), from
/// the samples' timestamps; 0 when unknown.
fn speed_at(pts: &[StrokePoint], tick: u64, ppq: u64) -> f64 {
    let mut best = 0.0;
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if tick < a.tick.min(b.tick) || tick > a.tick.max(b.tick) {
            continue;
        }
        let dt = (b.t - a.t).max(1e-3);
        let beats = (b.tick as f64 - a.tick as f64).abs() / ppq.max(1) as f64;
        let rows = (b.pitch - a.pitch).abs();
        best = (beats + rows) / dt;
    }
    best
}

/// Template of a generated note: the destination track's subchannel at
/// position `sub` (its MIDI channel), velocity `vel`.
fn template(src: &Source, sub: usize, vel: u8) -> TNote {
    let (id, ch) = src
        .tracks
        .first()
        .and_then(|t| t.subs.get(sub.min(t.subs.len().saturating_sub(1))).copied())
        .unwrap_or((0, 0));
    TNote {
        start: 0,
        dur: 1,
        sub: id,
        group: 0,
        track: 0,
        pitch: 60,
        vel,
        rel: 64,
        ch,
        flags: 0,
    }
}

fn steps_of(ctx: &Ctx, step: u64) -> Option<(u64, u64, u64)> {
    let st = ctx.stroke.as_ref()?;
    let lo = st.points.iter().map(|p| p.tick).min()?;
    let hi = st.points.iter().map(|p| p.tick).max()?;
    let step = step.max(1);
    let a = lo / step * step;
    Some((a, hi, (hi - a) / step + 1))
}

fn no_stroke(out: &mut Output) -> Result<(), TransformError> {
    out.message =
        Some("Drag in the Piano Roll to draw with this tool (the window stays open).".into());
    Ok(())
}

// ======================================================================= Draw

pub const DRAW: ToolDef = ToolDef {
    id: "draw",
    spec: draw_spec,
    build: draw_build,
};

const SYMMETRY: &[&str] = &[
    "Mirror around a pitch",
    "Mirror in time (around the start)",
    "Both",
];

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const DRAW_HELP: &[(&str, &str)] = &[
    (
        "step",
        "The length of one drawing step: the line gets one note per step along the stroke (Snap uses the Piano Roll's snap).",
    ),
    (
        "step.ticks",
        "The step length in ticks, used when Note Every is set to Custom Ticks.",
    ),
    (
        "gate",
        "Each note's length as a share of its step (100 % fills the step; above 100 % overlaps the next note).",
    ),
    ("vel", "The velocity of every drawn note."),
    (
        "sub",
        "The subchannel of the active track the drawn notes go to, by its position in the track's list (0 = the first).",
    ),
    (
        "a1",
        "Ignores the path in between and draws a straight line from the stroke's first point to its last.",
    ),
    (
        "a2",
        "Joins consecutive drawn notes on the same pitch and subchannel into one longer note.",
    ),
    (
        "a3",
        "Snaps every drawn pitch to the nearest note of a scale.",
    ),
    ("a3.key", "The root note of the scale the pitches snap to."),
    ("a3.scale", "The scale the drawn pitches snap to."),
    (
        "a4",
        "Sets each note's velocity by how fast the pointer moved there: slow is soft, fast is loud.",
    ),
    ("a4.lo", "The velocity where the pointer moved slowest."),
    ("a4.hi", "The velocity where the pointer moved fastest."),
    (
        "a5",
        "Sets each note's gate by how fast the pointer moved there: slow gives long notes, fast gives short ones.",
    ),
    (
        "a5.slow",
        "The gate (% of the step) where the pointer moved slowest.",
    ),
    (
        "a5.fast",
        "The gate (% of the step) where the pointer moved fastest.",
    ),
    (
        "a6",
        "Gives the drawn notes the listed subchannels in turn, one per note.",
    ),
    (
        "a6.subs",
        "The subchannel positions to cycle through, one per drawn note (0 = the track's first subchannel).",
    ),
    ("a7", "Also draws mirrored copies of the line."),
    (
        "a7.mode",
        "Mirror around a pitch flips the line upside down; in time it plays backwards from the stroke's start; Both adds all three copies.",
    ),
    ("a7.axis", "The pitch the line is mirrored around."),
    (
        "a8",
        "Stamps a copy of the selected notes at the line's steps instead of single notes, moved to the line's pitch.",
    ),
    (
        "a8.every",
        "Stamps the motif on every Nth step of the line (1 = every step).",
    ),
    (
        "a9",
        "Moves each drawn note by a random pitch and time offset (seeded, so the same seed gives the same result).",
    ),
    (
        "a9.pitch",
        "The largest random pitch move up or down, in semitones.",
    ),
    (
        "a9.ticks",
        "The largest random time move earlier or later, in ticks.",
    ),
    (
        "a10",
        "Continues the line past the stroke's end with the step and pitch change of its last two notes, at 60 % of their velocity.",
    ),
    (
        "a10.count",
        "How many notes the continuation adds after the stroke.",
    ),
    (
        "n01",
        "Moves drawn notes that are not in the current chord to the nearest chord tone.",
    ),
    (
        "n01.follow",
        "Where the chords come from: the notes sounding in the active track under each drawn note, a reference track, or a typed chord list (one chord per bar).",
    ),
    (
        "n01.ref",
        "The track whose sounding notes give the chords, by its number in the track list (0 = the active track).",
    ),
    (
        "n01.chords",
        "Chord names such as C Am F G, one per bar from the stroke's first bar, repeated when the list runs out.",
    ),
    (
        "n01.tension",
        "The chance that a drawn note outside the chord keeps its pitch (0 % = every note moves to a chord tone).",
    ),
    (
        "n02",
        "Smooths the wobble of the stroke so the line follows the pointer more steadily.",
    ),
    (
        "n02.smooth",
        "How much each note leans on the previous one instead of the pointer (0 % = follows the pointer exactly).",
    ),
    (
        "n02.lag",
        "Reads the stroke this many ticks before each note, so the line trails behind the pointer.",
    ),
    (
        "n03",
        "Turns the line into an even staircase: pitches move in fixed steps of rows from the first note.",
    ),
    ("n03.rows", "How many rows (semitones) each stair is."),
    (
        "n03.mode",
        "Bresenham puts each note on the nearest stair; Hold Until Change also joins neighboring notes on the same stair into one.",
    ),
    (
        "n04",
        "Plays each drawn note as an arpeggio of a chord, splitting the note's length between the chord's notes.",
    ),
    (
        "n04.chord",
        "The chord's intervals in semitones above the drawn pitch (0 = the drawn pitch itself).",
    ),
    (
        "n04.pattern",
        "The arpeggio's direction: up, down, or alternating up and down from note to note.",
    ),
    (
        "n05",
        "Adds lines parallel to the drawn one, a set number of scale steps away in a key.",
    ),
    (
        "n05.lines",
        "How many scale steps each extra line sits from the drawn line (negative = below; -2 in a major scale is a third below).",
    ),
    ("n05.key", "The key of the scale the parallel lines follow."),
    ("n05.scale", "The scale the parallel lines follow."),
    (
        "n06",
        "Places notes on the note starts of a reference instead of on every step; each note lasts until the next start.",
    ),
    (
        "n06.ref",
        "Takes the rhythm from a track or from a subchannel of the active track.",
    ),
    (
        "n06.num",
        "The track number (0 = the active track) or the subchannel position whose note starts give the rhythm.",
    ),
    (
        "n07",
        "Recognizes a roughly drawn circle, line or zigzag and draws the perfect shape instead.",
    ),
    (
        "n07.shapes",
        "Which shape to look for; All takes whichever fits best, Off recognizes nothing.",
    ),
    (
        "n07.tol",
        "How far the stroke may stray from the perfect shape (average error, % of the stroke's height) and still be replaced by it.",
    ),
    (
        "n08",
        "Decides what happens to the notes already under the stroke.",
    ),
    (
        "n08.existing",
        "Keep leaves them; Replace Under the Stroke deletes the notes the drawing overlaps; Skip Occupied Steps leaves out drawn notes that would overlap them.",
    ),
    (
        "n08.scope",
        "Which notes count as already there: the active track's, or those of every visible track.",
    ),
    (
        "n09",
        "Gives the line a rhythm: each note spans a number of steps taken from a list in turn.",
    ),
    (
        "n09.rhythm",
        "How many steps each note spans, in turn and repeated (for example 2, 1, 1).",
    ),
    (
        "n09.accents",
        "The velocities of the notes in turn, repeated; leave it empty to keep the normal velocity.",
    ),
    (
        "n10",
        "Alternates the line with a second pitch for a trill or tremolo.",
    ),
    (
        "n10.interval",
        "The semitones every second note moves when the pattern is empty (2 = a whole-tone trill).",
    ),
    (
        "n10.pattern",
        "Semitone offsets added to the notes in turn, repeated; leave it empty to alternate between 0 and the interval.",
    ),
    (
        "n11",
        "Colors the line with a palette of subchannels from its start to its end.",
    ),
    (
        "n11.palette",
        "The subchannel positions used, from the first to the last (0 = the track's first subchannel).",
    ),
    (
        "n11.repeat",
        "Once spreads the palette over the whole stroke; Cycle gives each note the next subchannel in turn.",
    ),
];

fn draw_spec() -> ToolSpec {
    let mut p = vec![
        choice("step", "Note every", NOTE_VALUES, 10),
        ticks("step.ticks", "Custom step (ticks)", 1, 1 << 24, 240),
        float("gate", "Gate (of the step)", 5.0, 400.0, 100.0, "%"),
        int("vel", "Velocity", 1, 127, 100),
        int(
            "sub",
            "Subchannel (position in the track's list)",
            0,
            4095,
            0,
        ),
        seed("seed"),
        adv_switch(1, "Constraint line (straight from start to end)"),
        adv_switch(2, "Freehand melodic contour (repeated pitches join)"),
        adv_switch(3, "Pitch-scale magnet"),
        choice("a3.key", "Root", &KEY_NAMES, 0).adv(3),
        choice("a3.scale", "Scale", scale_names(), 0).adv(3),
        adv_switch(4, "Velocity-pressure brush (faster = louder)"),
        int("a4.lo", "Velocity when slow", 1, 127, 40).adv(4),
        int("a4.hi", "Velocity when fast", 1, 127, 127).adv(4),
        adv_switch(5, "Gate-pressure brush (faster = shorter)"),
        float("a5.slow", "Gate when slow", 5.0, 400.0, 100.0, "%").adv(5),
        float("a5.fast", "Gate when fast", 5.0, 400.0, 25.0, "%").adv(5),
        adv_switch(6, "Subchannel cycling brush"),
        list("a6.subs", "Subchannel positions", 0, 4095, 64, &[0, 1, 2]).adv(6),
        adv_switch(7, "Symmetry drawing"),
        choice("a7.mode", "Mirror", SYMMETRY, 0).adv(7),
        pitch("a7.axis", "Pitch axis", 60).adv(7),
        adv_switch(8, "Repeating motif brush (the selected notes)"),
        int("a8.every", "Stamp every (steps)", 1, 1024, 4).adv(8),
        adv_switch(9, "Pitch / time noise brush"),
        int("a9.pitch", "Pitch jitter (semitones)", 0, 24, 2).adv(9),
        ticks("a9.ticks", "Time jitter (ticks)", 0, 1 << 20, 60).adv(9),
        adv_switch(10, "Predictive continuation"),
        int("a10.count", "Continue with notes", 1, 64, 4).adv(10),
        // V09-S39 step 3: the functions declared by id
        // (docs/TRANSFORM_ENGINE.md "Draw").
        flag("n01", "Chord follow", false),
        choice("n01.follow", "Follow", DRAW_FOLLOW, 0),
        int(
            "n01.ref",
            "Reference track (number; 0 = the active track)",
            0,
            65535,
            0,
        ),
        text(
            "n01.chords",
            "Chords (one per bar, cycled)",
            512,
            "C Am F G",
        ),
        float(
            "n01.tension",
            "Tension (non-chord tones kept)",
            0.0,
            100.0,
            0.0,
            "%",
        ),
        flag("n02", "Stroke smoothing", false),
        float("n02.smooth", "Smoothing", 0.0, 100.0, 50.0, "%"),
        ticks("n02.lag", "Lag (ticks)", 0, 1 << 24, 0),
        flag("n03", "Staircase line", false),
        int("n03.rows", "Rows per step", 1, 24, 2),
        choice(
            "n03.mode",
            "Mode",
            &["Bresenham (even steps)", "Hold until change"],
            0,
        ),
        flag("n04", "Arpeggiated stroke", false),
        list("n04.chord", "Chord intervals", -48, 48, 16, &[0, 4, 7]),
        choice("n04.pattern", "Pattern", &["Up", "Down", "Up-down"], 0),
        flag("n05", "Parallel lines in key", false),
        list("n05.lines", "Lines (scale degrees)", -14, 14, 8, &[-2]),
        choice("n05.key", "Key", &KEY_NAMES, 0),
        choice("n05.scale", "Scale", scale_names(), 0),
        flag("n06", "Reference rhythm", false),
        choice("n06.ref", "Reference", &["A track", "A subchannel"], 0),
        int(
            "n06.num",
            "Reference number (track number, 0 = the active track; or subchannel position)",
            0,
            65535,
            0,
        ),
        flag("n07", "Shape recognition", false),
        choice("n07.shapes", "Shapes", DRAW_SHAPES, 4),
        float("n07.tol", "Tolerance", 1.0, 100.0, 15.0, "%"),
        flag("n08", "Overdraw policy", false),
        choice("n08.existing", "Existing notes", DRAW_OVERDRAW, 1),
        choice(
            "n08.scope",
            "Scope",
            &["The active track", "All visible tracks"],
            0,
        ),
        flag("n09", "Rhythm pattern", false),
        list(
            "n09.rhythm",
            "Rhythm (steps per note)",
            1,
            64,
            64,
            &[2, 1, 1],
        ),
        list(
            "n09.accents",
            "Accents (velocities, cycled; empty = keep)",
            1,
            127,
            64,
            &[120, 80, 100, 80],
        ),
        flag("n10", "Tremolo stroke", false),
        int("n10.interval", "Interval", -24, 24, 2),
        list(
            "n10.pattern",
            "Pattern (intervals, cycled; empty = alternate with the interval)",
            -24,
            24,
            64,
            &[],
        ),
        flag("n11", "Color gradient stroke", false),
        list(
            "n11.palette",
            "Palette (subchannel positions)",
            0,
            4095,
            64,
            &[0, 1, 2, 3],
        ),
        choice(
            "n11.repeat",
            "Repeat",
            &["Once (start to end)", "Cycle (one per note)"],
            0,
        ),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, DRAW_HELP);
    ToolSpec {
        id: "draw",
        name: "Draw",
        family: Family::Interactive,
        summary: "Drag in the Piano Roll to draw a melodic line: one note per step at the pitch under the pointer, on the active track. Preview shows the line; Apply adds it (one undo step).",
        params: p,
        advanced: vec![
            "Constraint line mode",
            "Freehand melodic contour",
            "Pitch-scale magnet",
            "Velocity-pressure brush",
            "Gate-pressure brush",
            "Subchannel cycling brush",
            "Symmetry drawing",
            "Repeating motif brush",
            "Pitch / time noise brush",
            "Predictive continuation guide",
        ],
        // Orders = the catalog's (§235) = where each runs in `Draw9`: the
        // clock (n06, n09), the path (n07, n02, n03), the pitch (n01, n10),
        // the subchannel (n11), the expansion (n04, n05), then n08 on the
        // finished drawing.
        functions: [
            ("n01", "Chord follow", 160),
            ("n02", "Stroke smoothing", 140),
            ("n03", "Staircase line", 150),
            ("n04", "Arpeggiated stroke", 190),
            ("n05", "Parallel lines in key", 200),
            ("n06", "Reference rhythm", 110),
            ("n07", "Shape recognition", 130),
            ("n08", "Overdraw policy", 600),
            ("n09", "Rhythm pattern", 120),
            ("n10", "Tremolo stroke", 170),
            ("n11", "Color gradient stroke", 180),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: false,
        context: false,
    }
}

fn draw_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    // V09-S39: any new function, or Control Sweeps (evaluated per drawn
    // position inside the kernel) — the v0.9 kernel; else the v0.8 one.
    let new = DrawNew::from(p)?;
    if new.any() || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty()) {
        return Draw9::build(p, b, new);
    }
    let step = note_value(p.choice("step"), p.ticks("step.ticks"), b.ctx).max(1);
    let gate = p.float("gate") / 100.0;
    let vel = p.int("vel").clamp(1, 127) as u8;
    let sub = p.int("sub").max(0) as usize;
    let seed = p.seed("seed");
    let line = p.adv(1);
    let contour = p.adv(2);
    let scale = p
        .adv(3)
        .then(|| Scale::new(p.choice("a3.key"), p.choice("a3.scale")));
    let top = i64::from(b.ctx.keys.max_pitch());
    let vpress = p
        .adv(4)
        .then(|| (p.int("a4.lo") as f64, p.int("a4.hi") as f64));
    let gpress = p
        .adv(5)
        .then(|| (p.float("a5.slow") / 100.0, p.float("a5.fast") / 100.0));
    let cycle = p.adv(6).then(|| p.list("a6.subs"));
    let sym = p.adv(7).then(|| (p.choice("a7.mode"), p.int("a7.axis")));
    let motif = p.adv(8).then(|| p.int("a8.every").max(1) as u64);
    let noise = p.adv(9).then(|| (p.int("a9.pitch"), p.ticks("a9.ticks")));
    let cont = p.adv(10).then(|| p.int("a10.count").max(1) as u64);
    let stroke = b.ctx.stroke.clone();
    let ppq = b.ctx.ppq;
    let steps = steps_of(b.ctx, step).map_or(0, |s| s.2);
    let per = 4 * (1 + cont.unwrap_or(0)) * motif.map_or(1, |_| b.stats.count.max(1) as u64);
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            for n in &src.notes {
                if !out.push(*n) {
                    return out.check();
                }
            }
            let Some(st) = stroke.as_ref().filter(|s| !s.points.is_empty()) else {
                return no_stroke(out);
            };
            let pts = &st.points;
            let (a, hi) = (
                pts.iter().map(|q| q.tick).min().unwrap_or(0) / step * step,
                pts.iter().map(|q| q.tick).max().unwrap_or(0),
            );
            // Time order of the endpoints (line mode).
            let first = pts
                .iter()
                .min_by_key(|q| q.tick)
                .copied()
                .expect("non-empty");
            let last = pts
                .iter()
                .max_by_key(|q| q.tick)
                .copied()
                .expect("non-empty");
            let max_speed = pts
                .windows(2)
                .map(|w| {
                    let dt = (w[1].t - w[0].t).max(1e-3);
                    ((w[1].tick as f64 - w[0].tick as f64).abs() / ppq.max(1) as f64
                        + (w[1].pitch - w[0].pitch).abs())
                        / dt
                })
                .fold(1e-9, f64::max);
            // (start, pitch, gate factor, velocity, sub steps)
            let mut line_notes: Vec<(u64, i64, f64, u8, i64)> = Vec::new();
            let mut t = a;
            let mut k = 0usize;
            while t <= hi {
                let pitch = if line {
                    let f = if last.tick == first.tick {
                        0.0
                    } else {
                        (t as f64 - first.tick as f64) / (last.tick as f64 - first.tick as f64)
                    };
                    Some(first.pitch + (last.pitch - first.pitch) * f.clamp(0.0, 1.0))
                } else {
                    pitch_at(pts, t.max(first.tick))
                };
                if let Some(pf) = pitch {
                    let mut pi = pf.floor() as i64;
                    if let Some(s) = &scale {
                        // Up to two octaves beyond the keyboard (its top key:
                        // 127, or 255 in 256-Key Mode) before the boundary.
                        pi = s.snap(pi.clamp(-24, top + 24), SnapMode::Nearest);
                    }
                    let sp = speed_at(pts, t, ppq) / max_speed;
                    let v = match vpress {
                        Some((lo, hi2)) => (lo + (hi2 - lo) * sp).round().clamp(1.0, 127.0) as u8,
                        None => vel,
                    };
                    let g = match gpress {
                        Some((slow, fast)) => slow + (fast - slow) * sp,
                        None => gate,
                    };
                    let sub_steps = cycle
                        .as_ref()
                        .filter(|c| !c.is_empty())
                        .map_or(0, |c| c[k % c.len()] - sub as i64);
                    line_notes.push((t, pi, g, v, sub_steps));
                    k += 1;
                }
                t += step;
            }
            // Predictive continuation from the last two notes.
            if let Some(c) = cont
                && line_notes.len() >= 2
            {
                let (t1, p1, g1, v1, s1) = line_notes[line_notes.len() - 1];
                let (t0, p0, ..) = line_notes[line_notes.len() - 2];
                let (dt, dp) = (t1 - t0, p1 - p0);
                for j in 1..=c {
                    line_notes.push((
                        t1 + dt * j,
                        p1 + dp * j as i64,
                        g1,
                        ((f64::from(v1) * 0.6).round() as u8).max(1),
                        s1,
                    ));
                }
            }
            // Contour: consecutive equal pitches join into one note.
            let mut notes: Vec<(u64, u64, i64, u8, i64)> = Vec::new();
            for &(t0, pi, g, v, s) in &line_notes {
                let dur = ((step as f64 * g).round() as u64).max(1);
                if contour
                    && let Some(l) = notes.last_mut()
                    && l.2 == pi
                    && l.4 == s
                    && t0 <= l.0 + l.1.max(step)
                {
                    l.1 = (t0 + dur).saturating_sub(l.0);
                    continue;
                }
                notes.push((t0, dur, pi, v, s));
            }
            let tpl = template(src, sub, vel);
            let base_motif: Vec<TNote> = src.notes.clone();
            let emit = |out: &mut Output,
                        s: u64,
                        d: u64,
                        pi: i64,
                        v: u8,
                        sub_steps: i64,
                        k: u64|
             -> bool {
                let (mut s2, mut p2) = (i128::from(s), pi);
                if let Some((pj, tj)) = noise {
                    let mut r = Rng::for_index(seed, salt::SHAPE, k);
                    p2 += r.range_i64(-pj, pj);
                    s2 += i128::from(r.range_i64(-(tj as i64), tj as i64));
                }
                let mut t = tpl;
                if sub_steps != 0
                    && let Some(tr) = src.tracks.first()
                {
                    let (id, ch) = tr.sub_offset(tpl.sub, sub_steps);
                    t.sub = id;
                    t.ch = ch;
                }
                let mut ok = out.emit(s2, i128::from(d), p2, i64::from(v), &t);
                if let Some((mode, axis)) = sym {
                    if mode != 1 {
                        ok &= out.emit(s2, i128::from(d), 2 * axis - p2, i64::from(v), &t);
                    }
                    if mode != 0 {
                        let mirror = 2 * i128::from(a) - s2 - i128::from(d);
                        ok &= out.emit(mirror, i128::from(d), p2, i64::from(v), &t);
                        if mode == 2 {
                            ok &= out.emit(mirror, i128::from(d), 2 * axis - p2, i64::from(v), &t);
                        }
                    }
                }
                ok
            };
            for (k, &(s, d, pi, v, sub_steps)) in notes.iter().enumerate() {
                // V09-0143: the driver dimension is the line's step (0 =
                // the stroke's first).
                out.set_dim(k as u32);
                match motif {
                    Some(every) if !base_motif.is_empty() => {
                        if !(k as u64).is_multiple_of(every) {
                            continue;
                        }
                        let m0 = base_motif[0];
                        for (j, m) in base_motif.iter().enumerate() {
                            let rs = s.saturating_add(m.start - m0.start);
                            let rp = pi + i64::from(m.pitch) - i64::from(m0.pitch);
                            if !emit(out, rs, m.dur, rp, m.vel, sub_steps, (k * 4096 + j) as u64) {
                                return out.check();
                            }
                        }
                    }
                    _ => {
                        if !emit(out, s, d, pi, v, sub_steps, k as u64) {
                            return out.check();
                        }
                    }
                }
                out.check()?;
            }
            out.message = Some(format!("{} drawn note(s).", notes.len()));
            Ok(())
        },
        growth: Growth::Add(steps.saturating_mul(per)),
        estimate: None,
    })
}

// ============================================================ Draw (v0.9)

const DRAW_FOLLOW: &[&str] = &[
    "The chord of the track",
    "A reference track",
    "A chord list",
];
const DRAW_SHAPES: &[&str] = &["Off", "Circle", "Line", "Zigzag", "All"];
const DRAW_OVERDRAW: &[&str] = &["Keep", "Replace under the stroke", "Skip occupied steps"];

/// Draw's prepared layers in [`Source::aux`] ([`draw_prepare`]): the notes
/// n01 reads chords from, the notes n06 takes onsets from, then one layer
/// per other visible track (n08).
const AUX_CHORDS: usize = 0;
const AUX_ONSETS: usize = 1;
const AUX_VISIBLE: usize = 2;

/// Salt of n01's tension dice (beside the v0.8 salts).
const SALT_FOLLOW: u64 = 0xD4A3_0001;

/// Draw's v0.8 settings (the v0.8 kernel's closure reads the same values;
/// [`Draw9`] re-reads them here, unchanged).
#[derive(Clone, Debug)]
struct Draw8 {
    step: u64,
    gate: f64,
    vel: u8,
    sub: usize,
    seed: u64,
    line: bool,
    contour: bool,
    scale: Option<Scale>,
    top: i64,
    vpress: Option<(f64, f64)>,
    gpress: Option<(f64, f64)>,
    cycle: Option<Vec<i64>>,
    sym: Option<(usize, i64)>,
    motif: Option<u64>,
    noise: Option<(i64, u64)>,
    cont: Option<u64>,
    ppq: u64,
}

impl Draw8 {
    fn from(p: &Params, b: &BuildCtx) -> Draw8 {
        Draw8 {
            step: note_value(p.choice("step"), p.ticks("step.ticks"), b.ctx).max(1),
            gate: p.float("gate") / 100.0,
            vel: p.int("vel").clamp(1, 127) as u8,
            sub: p.int("sub").max(0) as usize,
            seed: p.seed("seed"),
            line: p.adv(1),
            contour: p.adv(2),
            scale: p
                .adv(3)
                .then(|| Scale::new(p.choice("a3.key"), p.choice("a3.scale"))),
            top: i64::from(b.ctx.keys.max_pitch()),
            vpress: p
                .adv(4)
                .then(|| (p.int("a4.lo") as f64, p.int("a4.hi") as f64)),
            gpress: p
                .adv(5)
                .then(|| (p.float("a5.slow") / 100.0, p.float("a5.fast") / 100.0)),
            cycle: p.adv(6).then(|| p.list("a6.subs")),
            sym: p.adv(7).then(|| (p.choice("a7.mode"), p.int("a7.axis"))),
            motif: p.adv(8).then(|| p.int("a8.every").max(1) as u64),
            noise: p.adv(9).then(|| (p.int("a9.pitch"), p.ticks("a9.ticks"))),
            cont: p.adv(10).then(|| p.int("a10.count").max(1) as u64),
            ppq: b.ctx.ppq,
        }
    }
}

/// n01: where the chords come from.
#[derive(Clone, Debug)]
enum Follow {
    /// The notes sounding at each drawn note (0: the active track, 1: a
    /// reference track — [`AUX_CHORDS`] when prepared).
    Notes(usize, i64),
    /// One chord (pitch-class mask) per bar, cycled from the stroke's first
    /// bar.
    List(Vec<u16>),
}

/// Draw's settings of the functions declared by id (V09-S39 step 3).
#[derive(Clone, Debug, Default)]
struct DrawNew {
    /// n01: the chords, tension (% of non-chord tones kept).
    follow: Option<(Follow, f64)>,
    /// n02: smoothing %, lag (ticks).
    smooth: Option<(f64, u64)>,
    /// n03: rows per step, hold until change.
    stairs: Option<(i64, bool)>,
    /// n04: intervals, pattern (0 up, 1 down, 2 up-down).
    arp: Option<(Vec<i64>, usize)>,
    /// n05: scale degrees (zeros dropped), the key's lattice.
    lines: Option<(Vec<i64>, Lattice)>,
    /// n06: reference kind (0 track, 1 subchannel), number.
    onsets: Option<(usize, i64)>,
    /// n07: shapes (0 off, 1 circle, 2 line, 3 zigzag, 4 all), tolerance %.
    shapes: Option<(usize, f64)>,
    /// n08: existing notes (0 keep, 1 replace, 2 skip), scope (0 active
    /// track, 1 all visible tracks).
    overdraw: Option<(usize, usize)>,
    /// n09: steps per note (cycled), accent velocities (cycled; empty =
    /// keep).
    rhythm: Option<(Vec<u64>, Vec<i64>)>,
    /// n10: interval, pattern (cycled; empty = alternate with the interval).
    tremolo: Option<(i64, Vec<i64>)>,
    /// n11: palette (subchannel positions), cycle per note.
    gradient: Option<(Vec<i64>, bool)>,
}

impl DrawNew {
    fn from(p: &Params) -> Result<DrawNew, TransformError> {
        let follow = if p.flag("n01") {
            let f = match p.choice("n01.follow") {
                2 => {
                    let text = p.text("n01.chords");
                    let mut masks = Vec::new();
                    for tok in text
                        .split([' ', ',', ';', '|'])
                        .filter(|t| !t.trim().is_empty())
                    {
                        let c = parse_chord(tok.trim()).ok_or_else(|| {
                            TransformError::Invalid(format!(
                                "Chord follow: “{}” is not a chord name (for example C, Am, F#m7, Bbmaj7).",
                                tok.trim()
                            ))
                        })?;
                        masks.push(c.pitch_classes());
                    }
                    if masks.is_empty() {
                        return Err(TransformError::Invalid(
                            "Chord follow: type at least one chord (one per bar), for example “C Am F G”.".into(),
                        ));
                    }
                    Follow::List(masks)
                }
                m => Follow::Notes(m, p.int("n01.ref")),
            };
            Some((f, p.float("n01.tension")))
        } else {
            None
        };
        let lines = p.flag("n05").then(|| {
            let mut d: Vec<i64> = p
                .list("n05.lines")
                .into_iter()
                .filter(|&x| x != 0)
                .collect();
            d.dedup();
            let scale = Scale::new(p.choice("n05.key"), p.choice("n05.scale"));
            (d, Lattice::of_scale(&scale))
        });
        Ok(DrawNew {
            follow,
            smooth: p
                .flag("n02")
                .then(|| (p.float("n02.smooth"), p.ticks("n02.lag"))),
            stairs: p
                .flag("n03")
                .then(|| (p.int("n03.rows").max(1), p.choice("n03.mode") == 1)),
            arp: p.flag("n04").then(|| {
                let mut v = p.list("n04.chord");
                if v.is_empty() {
                    v.push(0);
                }
                (v, p.choice("n04.pattern"))
            }),
            lines,
            onsets: p
                .flag("n06")
                .then(|| (p.choice("n06.ref"), p.int("n06.num"))),
            shapes: p
                .flag("n07")
                .then(|| (p.choice("n07.shapes"), p.float("n07.tol"))),
            overdraw: p
                .flag("n08")
                .then(|| (p.choice("n08.existing"), p.choice("n08.scope"))),
            rhythm: p.flag("n09").then(|| {
                let mut r: Vec<u64> = p
                    .list("n09.rhythm")
                    .into_iter()
                    .map(|x| x.max(1) as u64)
                    .collect();
                if r.is_empty() {
                    r.push(1);
                }
                (r, p.list("n09.accents"))
            }),
            tremolo: p
                .flag("n10")
                .then(|| (p.int("n10.interval"), p.list("n10.pattern"))),
            gradient: p.flag("n11").then(|| {
                let mut v = p.list("n11.palette");
                if v.is_empty() {
                    v.push(0);
                }
                (v, p.choice("n11.repeat") == 1)
            }),
        })
    }

    fn any(&self) -> bool {
        self.follow.is_some()
            || self.smooth.is_some()
            || self.stairs.is_some()
            || self.arp.is_some()
            || self.lines.is_some()
            || self.onsets.is_some()
            || self.shapes.is_some()
            || self.overdraw.is_some()
            || self.rhythm.is_some()
            || self.tremolo.is_some()
            || self.gradient.is_some()
    }

    /// Most notes one drawn note becomes (n04 × n05).
    fn per_note(&self) -> u64 {
        let arp = self.arp.as_ref().map_or(1, |(v, _)| v.len().max(1) as u64);
        let lines = self.lines.as_ref().map_or(1, |(d, _)| 1 + d.len() as u64);
        arp * lines
    }

    /// Voices per position (2: a circle's lower arc).
    fn voices(&self) -> u64 {
        match self.shapes {
            Some((1 | 4, _)) => 2,
            _ => 1,
        }
    }
}

/// A recognized gesture (n07).
#[derive(Clone, Debug, PartialEq)]
enum ShapeKind {
    /// pitch = a + b · tick (least squares).
    Line(f64, f64),
    /// An axis-aligned ellipse in the stroke's box: centre (ticks, pitch),
    /// radii.
    Circle(f64, f64, f64, f64),
    /// A regular zigzag: vertices (tick, pitch) at even times, alternating
    /// between the mean peak and the mean trough.
    Zigzag(Vec<(f64, f64)>),
}

#[derive(Clone, Debug, PartialEq)]
struct Shape {
    kind: ShapeKind,
    /// The stroke's deviation from the perfect shape, % of its size.
    err: f64,
}

impl Shape {
    fn name(&self) -> &'static str {
        match self.kind {
            ShapeKind::Line(..) => "line",
            ShapeKind::Circle(..) => "circle",
            ShapeKind::Zigzag(..) => "zigzag",
        }
    }

    /// The perfect shape's pitch at `tick`: the main value and, for a circle,
    /// the lower arc (where the arcs are at least a row apart).
    fn at(&self, tick: u64) -> (f64, Option<f64>) {
        let x = tick as f64;
        match &self.kind {
            ShapeKind::Line(a, b) => (a + b * x, None),
            ShapeKind::Circle(cx, cy, rx, ry) => {
                let u = ((x - cx) / rx).clamp(-1.0, 1.0);
                let h = ry * (1.0 - u * u).max(0.0).sqrt();
                (cy + h, (2.0 * h >= 1.0).then_some(cy - h))
            }
            ShapeKind::Zigzag(v) => (polyline_at(v, x), None),
        }
    }
}

/// Linear interpolation along vertices with increasing x (clamped ends).
fn polyline_at(v: &[(f64, f64)], x: f64) -> f64 {
    match v {
        [] => 0.0,
        [only] => only.1,
        _ => {
            if x <= v[0].0 {
                return v[0].1;
            }
            let k = v.partition_point(|p| p.0 <= x);
            if k >= v.len() {
                return v[v.len() - 1].1;
            }
            let (a, b) = (v[k - 1], v[k]);
            if b.0 == a.0 {
                b.1
            } else {
                a.1 + (b.1 - a.1) * (x - a.0) / (b.0 - a.0)
            }
        }
    }
}

/// n07: the stroke's straight line (least squares), or `None` when it has
/// no extent in time. Error: the RMS residual in rows, % of the stroke's
/// height (at least an octave).
fn fit_line(pts: &[StrokePoint]) -> Option<Shape> {
    let n = pts.len() as f64;
    if pts.len() < 2 {
        return None;
    }
    let mx = pts.iter().map(|q| q.tick as f64).sum::<f64>() / n;
    let my = pts.iter().map(|q| q.pitch).sum::<f64>() / n;
    let sxx: f64 = pts.iter().map(|q| (q.tick as f64 - mx).powi(2)).sum();
    if sxx <= 0.0 {
        return None;
    }
    let sxy: f64 = pts
        .iter()
        .map(|q| (q.tick as f64 - mx) * (q.pitch - my))
        .sum();
    let b = sxy / sxx;
    let a = my - b * mx;
    let rms = (pts
        .iter()
        .map(|q| (q.pitch - (a + b * q.tick as f64)).powi(2))
        .sum::<f64>()
        / n)
        .sqrt();
    let (lo, hi) = pitch_span(pts);
    Some(Shape {
        kind: ShapeKind::Line(a, b),
        err: 100.0 * rms / (hi - lo).max(12.0),
    })
}

fn pitch_span(pts: &[StrokePoint]) -> (f64, f64) {
    pts.iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), q| {
            (lo.min(q.pitch), hi.max(q.pitch))
        })
}

/// n07: the ellipse of the stroke's box, when the stroke is closed (its
/// ends within a quarter of the box) and goes round (seven of the eight
/// octants around the centre). Error: the RMS of each sample's normalized
/// radius − 1, %.
fn fit_circle(pts: &[StrokePoint]) -> Option<Shape> {
    if pts.len() < 8 {
        return None;
    }
    let lo = pts.iter().map(|q| q.tick).min()? as f64;
    let hi = pts.iter().map(|q| q.tick).max()? as f64;
    let (plo, phi) = pitch_span(pts);
    let (rx, ry) = ((hi - lo) / 2.0, (phi - plo) / 2.0);
    if rx <= 0.0 || ry < 1.0 {
        return None;
    }
    let (cx, cy) = ((lo + hi) / 2.0, (plo + phi) / 2.0);
    let norm = |q: &StrokePoint| ((q.tick as f64 - cx) / rx, (q.pitch - cy) / ry);
    let (f, l) = (norm(&pts[0]), norm(&pts[pts.len() - 1]));
    if (f.0 - l.0).hypot(f.1 - l.1) > 0.5 {
        return None;
    }
    let mut octants = 0u8;
    let mut sum = 0.0;
    for q in pts {
        let (x, y) = norm(q);
        let r = x.hypot(y);
        sum += (r - 1.0).powi(2);
        if r > 0.0 {
            let o = ((y.atan2(x) + std::f64::consts::PI) / (std::f64::consts::PI / 4.0)) as u32;
            octants |= 1 << o.min(7);
        }
    }
    if octants.count_ones() < 7 {
        return None;
    }
    Some(Shape {
        kind: ShapeKind::Circle(cx, cy, rx, ry),
        err: 100.0 * (sum / pts.len() as f64).sqrt(),
    })
}

/// n07: a regular zigzag — the stroke's turning points (reversals of at
/// least a row and a fifth of its height), at least two, in time order;
/// the perfect zigzag puts them at even times between the stroke's ends,
/// at the mean peak / mean trough. Error: the RMS deviation in rows, % of
/// the peak-to-trough height.
fn fit_zigzag(pts: &[StrokePoint]) -> Option<Shape> {
    if pts.len() < 3 {
        return None;
    }
    let (plo, phi) = pitch_span(pts);
    let hyst = (0.2 * (phi - plo)).max(1.0);
    // (tick, pitch, is a peak)
    let mut ext: Vec<(u64, f64, bool)> = Vec::new();
    let mut cand = (pts[0].tick, pts[0].pitch);
    let mut dir = 0i8;
    for q in &pts[1..] {
        match dir {
            0 => {
                if q.pitch >= cand.1 + hyst {
                    dir = 1;
                    cand = (q.tick, q.pitch);
                } else if q.pitch <= cand.1 - hyst {
                    dir = -1;
                    cand = (q.tick, q.pitch);
                }
            }
            1 => {
                if q.pitch > cand.1 {
                    cand = (q.tick, q.pitch);
                } else if q.pitch <= cand.1 - hyst {
                    ext.push((cand.0, cand.1, true));
                    dir = -1;
                    cand = (q.tick, q.pitch);
                }
            }
            _ => {
                if q.pitch < cand.1 {
                    cand = (q.tick, q.pitch);
                } else if q.pitch >= cand.1 + hyst {
                    ext.push((cand.0, cand.1, false));
                    dir = 1;
                    cand = (q.tick, q.pitch);
                }
            }
        }
    }
    let (t0, t1) = (pts[0].tick, pts[pts.len() - 1].tick);
    if ext.len() < 2 || t1 <= t0 || ext.windows(2).any(|w| w[1].0 <= w[0].0) {
        return None;
    }
    let mean = |peak: bool| {
        let v: Vec<f64> = ext.iter().filter(|e| e.2 == peak).map(|e| e.1).collect();
        v.iter().sum::<f64>() / v.len().max(1) as f64
    };
    let (h, l) = (mean(true), mean(false));
    if h - l < 1.0 {
        return None;
    }
    let m = ext.len();
    let dt = (t1 - t0) as f64 / (m + 1) as f64;
    let level = |peak: bool| if peak { h } else { l };
    let mut v = Vec::with_capacity(m + 2);
    v.push((t0 as f64, level(!ext[0].2)));
    for (i, e) in ext.iter().enumerate() {
        v.push((t0 as f64 + dt * (i + 1) as f64, level(e.2)));
    }
    v.push((t1 as f64, level(!ext[m - 1].2)));
    let rms = (pts
        .iter()
        .map(|q| (q.pitch - polyline_at(&v, q.tick as f64)).powi(2))
        .sum::<f64>()
        / pts.len() as f64)
        .sqrt();
    Some(Shape {
        kind: ShapeKind::Zigzag(v),
        err: 100.0 * rms / (h - l),
    })
}

/// n07: the shape `which` asks for (4: the best of the three), if the
/// stroke has one.
fn recognize(pts: &[StrokePoint], which: usize) -> Option<Shape> {
    let all = [
        (which == 1 || which == 4)
            .then(|| fit_circle(pts))
            .flatten(),
        (which == 2 || which == 4).then(|| fit_line(pts)).flatten(),
        (which == 3 || which == 4)
            .then(|| fit_zigzag(pts))
            .flatten(),
    ];
    all.into_iter()
        .flatten()
        .filter(|s| s.err.is_finite())
        .min_by(|a, b| a.err.total_cmp(&b.err))
}

/// One key's spans `[start, end)` by start, and the running largest end.
type Spans = (Vec<(i128, i128)>, Vec<i128>);

/// Per pitch, spans `[start, end)` sorted by start with the running
/// largest end: "does any span on this key overlap `[s, e)`" in O(log n).
#[derive(Default)]
struct KeySpans {
    by: std::collections::HashMap<i64, Spans>,
}

impl KeySpans {
    fn of(spans: impl Iterator<Item = (i64, i128, i128)>) -> KeySpans {
        let mut by: std::collections::HashMap<i64, Spans> = Default::default();
        for (p, s, e) in spans {
            by.entry(p).or_default().0.push((s, e));
        }
        for (v, run) in by.values_mut() {
            v.sort_unstable();
            let mut m = i128::MIN;
            *run = v
                .iter()
                .map(|&(_, e)| {
                    m = m.max(e);
                    m
                })
                .collect();
        }
        KeySpans { by }
    }

    fn hits(&self, pitch: i64, s: i128, e: i128) -> bool {
        self.by.get(&pitch).is_some_and(|(v, run)| {
            let k = v.partition_point(|&(st, _)| st < e);
            k > 0 && run[k - 1] > s
        })
    }
}

/// A drawn note before emission (the v0.8 `emit` arguments).
#[derive(Clone, Copy, Debug)]
struct Drawn {
    start: i128,
    dur: i128,
    pitch: i64,
    vel: i64,
    tpl: TNote,
    /// V09-0143: the step of the line it belongs to (the driver dimension).
    dim: u32,
}

/// One drawn note of a voice: (start, slot length, pitch, gate factor,
/// velocity, subchannel steps, unit index).
type LineNote = (u64, u64, i64, f64, u8, i64, usize);

/// A note after the contour join: (start, length, pitch, velocity,
/// subchannel steps, unit index).
type JoinedNote = (u64, u64, i64, u8, i64, usize);

/// Swept values of one unit (a drawn position): each its parameter's own
/// value when no Control Sweep drives it.
#[derive(Clone, Copy, Debug)]
struct UnitVals {
    gate: f64,
    vel: u8,
    tension: f64,
    smooth: f64,
    rows: i64,
    tol: f64,
    interval: i64,
    jitter: (i64, u64),
    axis: i64,
}

/// The v0.9 Draw kernel (V09-S39 step 3): the v0.8 pipeline re-read from
/// the same parameters, with the functions declared by id at their stages
/// and the Control Sweeps evaluated per drawn position (V09-0143: the
/// output does not come from the selected notes).
struct Draw9 {
    v8: Draw8,
    new: DrawNew,
    stroke: Option<Arc<Stroke>>,
    meter: crate::view::grid::MeterMap,
    sp: Option<SweptParams>,
    /// Base values of the swept parameters.
    base: UnitVals,
    growth: u64,
    /// The stroke's first and last tick.
    span: Option<(u64, u64)>,
    /// (voices, continuation notes, notes per drawn note) of the bound.
    per_position: (u64, u64, u64),
}

impl Draw9 {
    fn build(p: &Params, b: &BuildCtx, new: DrawNew) -> Result<Box<dyn Kernel>, TransformError> {
        let v8 = Draw8::from(p, b);
        let stroke = b.ctx.stroke.clone();
        // Positions: the v0.8 steps, or (n06) at most one per tick of the
        // stroke's span (the source's onsets count them exactly:
        // `estimate_source`).
        let span = stroke.as_ref().map(|st| {
            let lo = st.points.iter().map(|q| q.tick).min().unwrap_or(0);
            let hi = st.points.iter().map(|q| q.tick).max().unwrap_or(0);
            (lo, hi)
        });
        let positions = span.map_or(0, |(lo, hi)| {
            if new.onsets.is_some() {
                hi - lo + 1
            } else {
                (hi - lo / v8.step * v8.step) / v8.step + 1
            }
        });
        // Each drawn note at most: its symmetry copies × the motif × n04's
        // intervals × (1 + n05's lines).
        let sym = match v8.sym {
            None => 1,
            Some((2, _)) => 4,
            Some(_) => 2,
        };
        let per = sym * v8.motif.map_or(1, |_| b.stats.count.max(1) as u64) * new.per_note();
        let bound = |positions: u64| {
            positions
                .saturating_mul(new.voices())
                .saturating_add(v8.cont.unwrap_or(0))
                .saturating_mul(per)
        };
        let growth = bound(positions);
        let per_position = (new.voices(), v8.cont.unwrap_or(0), per);
        let base = UnitVals {
            gate: v8.gate,
            vel: v8.vel,
            tension: p.float("n01.tension"),
            smooth: p.float("n02.smooth"),
            rows: p.int("n03.rows").max(1),
            tol: p.float("n07.tol"),
            interval: p.int("n10.interval"),
            jitter: v8.noise.unwrap_or((p.int("a9.pitch"), p.ticks("a9.ticks"))),
            axis: v8.sym.map_or(p.int("a7.axis"), |s| s.1),
        };
        Ok(Box::new(Draw9 {
            v8,
            new,
            stroke,
            meter: b.ctx.meter.clone(),
            sp: SweptParams::of(p, b),
            base,
            growth,
            span,
            per_position,
        }))
    }

    /// Each unit's swept values ([`SweptParams`]: measured at the unit —
    /// its start, its pitch on the raw stroke, its index — in the units'
    /// own frame).
    fn unit_vals(&self, units: &[TNote]) -> Vec<UnitVals> {
        let Some(sp) = &self.sp else {
            return vec![self.base; units.len()];
        };
        let sp = sp.in_frame(Frame::of_notes(units));
        let n = units.len();
        units
            .iter()
            .enumerate()
            .map(|(i, u)| {
                let pl = Place::unit(i, n);
                let b = self.base;
                let f = |k: &str, v: f64| if sp.drives(k) { sp.float(k, u, pl) } else { v };
                let g = |k: &str, v: i64| if sp.drives(k) { sp.int(k, u, pl) } else { v };
                UnitVals {
                    gate: if sp.drives("gate") {
                        sp.float("gate", u, pl) / 100.0
                    } else {
                        b.gate
                    },
                    vel: g("vel", i64::from(b.vel)).clamp(1, 127) as u8,
                    tension: f("n01.tension", b.tension),
                    smooth: f("n02.smooth", b.smooth),
                    rows: g("n03.rows", b.rows).max(1),
                    tol: f("n07.tol", b.tol),
                    interval: g("n10.interval", b.interval),
                    jitter: (
                        g("a9.pitch", b.jitter.0),
                        g("a9.ticks", b.jitter.1 as i64).max(0) as u64,
                    ),
                    axis: g("a7.axis", b.axis),
                }
            })
            .collect()
    }

    /// n06: the reference onsets in `[lo, hi]`, each with its slot (to the
    /// next onset; the last one step).
    fn onsets(&self, src: &Source, lo: u64, hi: u64) -> Vec<(u64, u64)> {
        let Some((kind, num)) = self.new.onsets else {
            return Vec::new();
        };
        let mut starts: Vec<u64> = if src.aux.len() > AUX_ONSETS {
            src.aux[AUX_ONSETS].iter().map(|n| n.start).collect()
        } else {
            // Not prepared (no document at hand): the source's own notes.
            let own = src.notes.iter().chain(src.context.iter());
            match kind {
                1 => {
                    let id = src
                        .tracks
                        .first()
                        .and_then(|t| t.subs.get(usize::try_from(num).unwrap_or(usize::MAX)))
                        .map(|s| s.0);
                    own.filter(|n| Some(n.sub) == id).map(|n| n.start).collect()
                }
                _ if num == 0 => own.map(|n| n.start).collect(),
                _ => Vec::new(),
            }
        };
        starts.retain(|&t| t >= lo && t <= hi);
        starts.sort_unstable();
        starts.dedup();
        let step = self.v8.step;
        (0..starts.len())
            .map(|i| {
                let slot = starts.get(i + 1).map_or(step, |&n| n - starts[i]);
                (starts[i], slot.max(1))
            })
            .collect()
    }

    /// n01: the pitch classes sounding at each of `times` (ascending), or the
    /// chord list's chord of each time's bar.
    fn chords(&self, src: &Source, times: &[u64]) -> Vec<u16> {
        let Some((follow, _)) = &self.new.follow else {
            return Vec::new();
        };
        match follow {
            Follow::List(masks) => {
                let first = times.first().map_or(0, |&t| bar_place(&self.meter, t).0);
                times
                    .iter()
                    .map(|&t| {
                        let bar = bar_place(&self.meter, t).0.saturating_sub(first);
                        masks[(bar % masks.len() as u64) as usize]
                    })
                    .collect()
            }
            Follow::Notes(mode, num) => {
                if src.aux.len() > AUX_CHORDS {
                    sounding_masks(src.aux[AUX_CHORDS].iter(), times)
                } else if *mode == 0 || *num == 0 {
                    // Not prepared (no document at hand): the source's notes.
                    sounding_masks(src.notes.iter().chain(src.context.iter()), times)
                } else {
                    vec![0; times.len()]
                }
            }
        }
    }

    fn run_stroke(
        &self,
        src: &Source,
        st: &Stroke,
        out: &mut Output,
    ) -> Result<(), TransformError> {
        let v = &self.v8;
        let nw = &self.new;
        let pts = &st.points;
        let step = v.step;
        let lo = pts.iter().map(|q| q.tick).min().unwrap_or(0);
        let hi = pts.iter().map(|q| q.tick).max().unwrap_or(0);
        let a = lo / step * step;
        let first = *pts.iter().min_by_key(|q| q.tick).expect("non-empty");
        let last = *pts.iter().max_by_key(|q| q.tick).expect("non-empty");
        let ppq = v.ppq;
        let max_speed = pts
            .windows(2)
            .map(|w| {
                let dt = (w[1].t - w[0].t).max(1e-3);
                ((w[1].tick as f64 - w[0].tick as f64).abs() / ppq.max(1) as f64
                    + (w[1].pitch - w[0].pitch).abs())
                    / dt
            })
            .fold(1e-9, f64::max);
        // ---- 1. The clock: v0.8's steps from the step before the stroke,
        // or (n06) the reference onsets; (n09) a note every r candidates.
        let cands: Vec<(u64, u64)> = if nw.onsets.is_some() {
            self.onsets(src, lo, hi)
        } else {
            (a..=hi).step_by(step as usize).map(|t| (t, step)).collect()
        };
        let slots: Vec<(u64, u64)> = match &nw.rhythm {
            Some((r, _)) => {
                let mut v = Vec::new();
                let (mut j, mut k) = (0usize, 0usize);
                while j < cands.len() {
                    let rk = r[k % r.len()] as usize;
                    let end = j.saturating_add(rk).min(cands.len());
                    let missing = (j.saturating_add(rk) - end) as u64;
                    let len: u64 = cands[j..end].iter().map(|c| c.1).sum::<u64>()
                        + missing * cands[cands.len() - 1].1;
                    v.push((cands[j].0, len));
                    j = end;
                    k += 1;
                }
                v
            }
            None => cands,
        };
        // ---- 2. The path: the stroke (v0.8), or the constraint line (a1).
        // The stroke's pitch at ascending ticks: v0.8's `pitch_at` (the
        // last-drawn segment covering the tick) found for all of them at
        // once ([`last_covering`]: O(ticks + samples), not their product).
        let raw_at = |ticks: &[u64]| -> Vec<Option<f64>> {
            if v.line {
                return ticks
                    .iter()
                    .map(|&t| {
                        let f = if last.tick == first.tick {
                            0.0
                        } else {
                            (t as f64 - first.tick as f64) / (last.tick as f64 - first.tick as f64)
                        };
                        Some(first.pitch + (last.pitch - first.pitch) * f.clamp(0.0, 1.0))
                    })
                    .collect();
            }
            let q: Vec<u64> = ticks.iter().map(|&t| t.max(first.tick)).collect();
            if let [only] = pts.as_slice() {
                return q
                    .iter()
                    .map(|&t| (only.tick == t).then_some(only.pitch))
                    .collect();
            }
            last_covering(pts, &q)
                .into_iter()
                .zip(&q)
                .map(|(w, &t)| w.map(|w| segment_pitch(pts, w, t)))
                .collect()
        };
        // Units: the slots the path covers.
        let slot_ticks: Vec<u64> = slots.iter().map(|s| s.0).collect();
        let mut units: Vec<(u64, u64, f64)> = Vec::with_capacity(slots.len());
        for (&(t, len), pf) in slots.iter().zip(raw_at(&slot_ticks)) {
            if let Some(pf) = pf {
                units.push((t, len, pf));
            }
        }
        // v0.8's `speed_at` at each unit (the same last covering segment,
        // at the tick itself).
        let unit_ticks: Vec<u64> = units.iter().map(|u| u.0).collect();
        let speeds: Vec<f64> = if v.vpress.is_some() || v.gpress.is_some() {
            last_covering(pts, &unit_ticks)
                .into_iter()
                .map(|w| w.map_or(0.0, |w| segment_speed(pts, w, ppq)))
                .collect()
        } else {
            vec![0.0; units.len()]
        };
        let reps: Vec<TNote> = units
            .iter()
            .map(|&(t, len, pf)| TNote {
                start: t,
                dur: len,
                pitch: pf.floor().clamp(0.0, 255.0) as u8,
                vel: v.vel,
                ..TNote::default()
            })
            .collect();
        let vals = self.unit_vals(&reps);
        // n07 (not with a1: the constraint line is already a perfect line).
        let shape = nw
            .shapes
            .filter(|_| !v.line)
            .and_then(|(which, _)| recognize(pts, which));
        let use_shape = |k: usize| shape.as_ref().is_some_and(|s| s.err <= vals[k].tol);
        // Per voice (voice 1: a circle's lower arc) the path value of each
        // unit, read `lag` ticks back with n02.
        let lag = nw.smooth.map_or(0, |s| s.1);
        let path = |t: u64| -> (f64, Option<f64>) { shape.as_ref().expect("checked").at(t) };
        // The lagged ticks (never before the stroke; where the stroke does
        // not reach one, the unit's own value).
        let lagged: Vec<u64> = units
            .iter()
            .map(|u| u.0.saturating_sub(lag).max(lo))
            .collect();
        let raw_lagged = if lag > 0 { raw_at(&lagged) } else { Vec::new() };
        let mut voices: [Vec<(usize, f64)>; 2] = [Vec::new(), Vec::new()];
        for (k, &(t, _, pf)) in units.iter().enumerate() {
            let at = if lag > 0 { lagged[k] } else { t };
            let (main, lower) = if use_shape(k) {
                path(at)
            } else if at != t {
                (raw_lagged[k].unwrap_or(pf), None)
            } else {
                (pf, None)
            };
            voices[0].push((k, main));
            if let Some(l) = lower {
                voices[1].push((k, l));
            }
        }
        // n02: each note moves (1 − 0.95 s) of the way from the previous
        // smoothed pitch toward the path.
        if nw.smooth.is_some() {
            for voice in &mut voices {
                let mut prev: Option<f64> = None;
                for (k, pf) in voice.iter_mut() {
                    let beta = 0.95 * (vals[*k].smooth / 100.0).clamp(0.0, 1.0);
                    let s = match prev {
                        Some(q) if beta > 0.0 => q + (1.0 - beta) * (*pf - q),
                        _ => *pf,
                    };
                    *pf = s;
                    prev = Some(s);
                }
            }
        }
        // ---- 3. Pitches: n03's staircase (rows from the voice's first
        // row) or v0.8's floor; n03 hold merges equal rows.
        let mut lines: [Vec<LineNote>; 2] = [Vec::new(), Vec::new()];
        let (ut0, ut1) = (
            units.first().map_or(0, |u| u.0),
            units.last().map_or(0, |u| u.0),
        );
        for (vi, voice) in voices.iter().enumerate() {
            let r0 = voice.first().map_or(0, |x| x.1.floor() as i64);
            // (unit, row, start, slot)
            let mut rows: Vec<(usize, i64, u64, u64)> = Vec::with_capacity(voice.len());
            for &(k, pf) in voice {
                let row = match nw.stairs {
                    Some(_) => {
                        let n = vals[k].rows.max(1);
                        r0 + n * ((pf - r0 as f64) / n as f64).round() as i64
                    }
                    None => pf.floor() as i64,
                };
                let (t, len, _) = units[k];
                if let Some((_, true)) = nw.stairs
                    && let Some(l) = rows.last_mut()
                    && l.1 == row
                {
                    l.3 = (t + len).saturating_sub(l.2);
                    continue;
                }
                rows.push((k, row, t, len));
            }
            let chords = self.chords(src, &rows.iter().map(|r| r.2).collect::<Vec<u64>>());
            for (m, &(k, row, t, len)) in rows.iter().enumerate() {
                let mut pi = row;
                if let Some(s) = &v.scale {
                    // Up to two octaves beyond the keyboard (its top key:
                    // 127, or 255 in 256-Key Mode) before the boundary.
                    pi = s.snap(pi.clamp(-24, v.top + 24), SnapMode::Nearest);
                }
                // n01: to the nearest chord tone, unless the tension keeps it.
                if let Some(&mask) = chords.get(m)
                    && mask != 0
                {
                    let lat = Lattice::from_mask(mask);
                    if !lat.contains(pi) {
                        let keep = vals[k].tension / 100.0;
                        let roll =
                            Rng::for_index(v.seed, SALT_FOLLOW, (k as u64) << 1 | vi as u64).unit();
                        if roll >= keep {
                            pi = lat.nearest(pi);
                        }
                    }
                }
                // n10: alternate with the interval, or the pattern in turn.
                if let Some((_, pat)) = &nw.tremolo {
                    pi += if pat.is_empty() {
                        if m % 2 == 1 { vals[k].interval } else { 0 }
                    } else {
                        pat[m % pat.len()]
                    };
                }
                let sp = speeds[k] / max_speed;
                let mut vel = match v.vpress {
                    Some((lo2, hi2)) => (lo2 + (hi2 - lo2) * sp).round().clamp(1.0, 127.0) as u8,
                    None => vals[k].vel,
                };
                if let Some((_, acc)) = &nw.rhythm
                    && !acc.is_empty()
                {
                    vel = acc[m % acc.len()].clamp(1, 127) as u8;
                }
                let g = match v.gpress {
                    Some((slow, fast)) => slow + (fast - slow) * sp,
                    None => vals[k].gate,
                };
                let mut sub_steps = v
                    .cycle
                    .as_ref()
                    .filter(|c| !c.is_empty())
                    .map_or(0, |c| c[m % c.len()] - v.sub as i64);
                // n11: the palette from the stroke's start to its end.
                if let Some((pal, cyc)) = &nw.gradient {
                    let i = if *cyc {
                        m % pal.len()
                    } else if ut1 > ut0 {
                        let f = (t - ut0) as f64 / (ut1 - ut0) as f64;
                        ((f * pal.len() as f64) as usize).min(pal.len() - 1)
                    } else {
                        0
                    };
                    sub_steps = pal[i] - v.sub as i64;
                }
                lines[vi].push((t, len, pi, g, vel, sub_steps, k));
            }
        }
        // ---- 4. v0.8: continuation (a10, the main voice) and the
        // contour (a2), per voice.
        if let Some(c) = v.cont
            && lines[0].len() >= 2
        {
            let ln = &lines[0];
            let (t1, l1, p1, g1, v1, s1, k1) = ln[ln.len() - 1];
            let (t0, _, p0, ..) = ln[ln.len() - 2];
            let (dt, dp) = (t1 - t0, p1 - p0);
            for j in 1..=c {
                lines[0].push((
                    t1 + dt * j,
                    l1,
                    p1 + dp * j as i64,
                    g1,
                    ((f64::from(v1) * 0.6).round() as u8).max(1),
                    s1,
                    k1,
                ));
            }
        }
        // (start, length, pitch, velocity, subchannel steps, unit)
        let mut notes: [Vec<JoinedNote>; 2] = [Vec::new(), Vec::new()];
        for (vi, ln) in lines.iter().enumerate() {
            let mut slot_of_last = step;
            for &(t0, len, pi, g, vel, s, k) in ln {
                let dur = ((len as f64 * g).round() as u64).max(1);
                if v.contour
                    && let Some(l) = notes[vi].last_mut()
                    && l.2 == pi
                    && l.4 == s
                    && t0 <= l.0 + l.1.max(slot_of_last)
                {
                    l.1 = (t0 + dur).saturating_sub(l.0);
                    slot_of_last = len;
                    continue;
                }
                notes[vi].push((t0, dur, pi, vel, s, k));
                slot_of_last = len;
            }
        }
        // ---- 5. Emission (v0.8's noise a9, symmetry a7, motif a8) after
        // n04's arpeggio and n05's lines, into a buffer for n08.
        let tpl = template(src, v.sub, v.vel);
        let base_motif: &[TNote] = &src.notes;
        let mut drawn: Vec<Drawn> = Vec::new();
        let push = |drawn: &mut Vec<Drawn>,
                    s: u64,
                    d: u64,
                    pi: i64,
                    vel: u8,
                    sub_steps: i64,
                    key: u64,
                    uv: &UnitVals,
                    dim: u32| {
            let (mut s2, mut p2) = (i128::from(s), pi);
            if v.noise.is_some() {
                let (pj, tj) = uv.jitter;
                let mut r = Rng::for_index(v.seed, salt::SHAPE, key);
                p2 += r.range_i64(-pj, pj);
                s2 += i128::from(r.range_i64(-(tj as i64), tj as i64));
            }
            let mut t = tpl;
            if sub_steps != 0
                && let Some(tr) = src.tracks.first()
            {
                let (id, ch) = tr.sub_offset(tpl.sub, sub_steps);
                t.sub = id;
                t.ch = ch;
            }
            let one = |start: i128, pitch: i64| Drawn {
                start,
                dur: i128::from(d),
                pitch,
                vel: i64::from(vel),
                tpl: t,
                dim,
            };
            drawn.push(one(s2, p2));
            if let Some((mode, _)) = v.sym {
                let axis = uv.axis;
                if mode != 1 {
                    drawn.push(one(s2, 2 * axis - p2));
                }
                if mode != 0 {
                    let mirror = 2 * i128::from(a) - s2 - i128::from(d);
                    drawn.push(one(mirror, p2));
                    if mode == 2 {
                        drawn.push(one(mirror, 2 * axis - p2));
                    }
                }
            }
        };
        let mut count = 0usize;
        for (vi, ns) in notes.iter().enumerate() {
            count += ns.len();
            for (k, &(s, d, pi, vel, sub_steps, unit)) in ns.iter().enumerate() {
                let uv = &vals[unit.min(vals.len().saturating_sub(1))];
                // n04: the note as an arpeggio of its chord's intervals.
                let arp: Vec<(i64, u64, u64)> = match &nw.arp {
                    Some((iv, pat)) => {
                        let mut iv = iv.clone();
                        iv.sort_unstable();
                        let down = match pat {
                            1 => true,
                            2 => k % 2 == 1,
                            _ => false,
                        };
                        if down {
                            iv.reverse();
                        }
                        let m = iv.len() as u64;
                        iv.iter()
                            .enumerate()
                            .map(|(j, &x)| {
                                let (j, m2) = (j as u64, m);
                                let a0 = d * j / m2;
                                let a1 = d * (j + 1) / m2;
                                (x, s + a0, (a1 - a0).max(1))
                            })
                            .collect()
                    }
                    None => vec![(0, s, d)],
                };
                for (j, &(off, s3, d3)) in arp.iter().enumerate() {
                    let p3 = pi + off;
                    // n05: the line itself, then each parallel line in key.
                    let mut ps = vec![p3];
                    if let Some((deg, lat)) = &nw.lines {
                        ps.extend(deg.iter().map(|&dg| lat.step(p3, dg)));
                    }
                    for (l, &p4) in ps.iter().enumerate() {
                        let key = if vi == 0 && j == 0 && l == 0 {
                            k as u64
                        } else {
                            hash(&[0xD4A9, vi as u64, k as u64, j as u64, l as u64])
                        };
                        match v.motif {
                            Some(every) if !base_motif.is_empty() => {
                                if !(k as u64).is_multiple_of(every) {
                                    continue;
                                }
                                let m0 = base_motif[0];
                                for (jm, mn) in base_motif.iter().enumerate() {
                                    let rs = s3.saturating_add(mn.start - m0.start);
                                    let rp = p4 + i64::from(mn.pitch) - i64::from(m0.pitch);
                                    let mkey = if key == k as u64 {
                                        (k * 4096 + jm) as u64
                                    } else {
                                        hash(&[key, jm as u64])
                                    };
                                    push(
                                        &mut drawn, rs, mn.dur, rp, mn.vel, sub_steps, mkey, uv,
                                        k as u32,
                                    );
                                }
                            }
                            _ => push(&mut drawn, s3, d3, p4, vel, sub_steps, key, uv, k as u32),
                        }
                    }
                }
            }
            out.check()?;
        }
        // ---- 6. n08: the drawing against the notes already there.
        let mut keep_src = vec![true; src.notes.len()];
        let mut note = String::new();
        if let Some((mode @ 1..=2, scope)) = nw.overdraw {
            let others: &[Vec<TNote>] = if scope == 1 && src.aux.len() > AUX_VISIBLE {
                &src.aux[AUX_VISIBLE..]
            } else {
                &[]
            };
            // The active track's notes: the selection on its slot and its
            // unselected notes; with scope 1 every visible track's.
            let mine = |n: &TNote| scope == 1 || n.track == 0;
            if mode == 2 {
                let spans = KeySpans::of(
                    src.notes
                        .iter()
                        .chain(src.context.iter())
                        .filter(|n| mine(n))
                        .chain(others.iter().flatten())
                        .map(|n| (i64::from(n.pitch), i128::from(n.start), i128::from(n.end()))),
                );
                let before = drawn.len();
                drawn.retain(|d| !spans.hits(d.pitch, d.start, d.start + d.dur));
                let skipped = before - drawn.len();
                if skipped > 0 {
                    note = format!(" {skipped} on occupied steps skipped.");
                }
            } else {
                let spans = KeySpans::of(drawn.iter().map(|d| (d.pitch, d.start, d.start + d.dur)));
                let hit = |n: &TNote| {
                    spans.hits(i64::from(n.pitch), i128::from(n.start), i128::from(n.end()))
                };
                let mut replaced = 0usize;
                for (i, n) in src.notes.iter().enumerate() {
                    if mine(n) && hit(n) {
                        keep_src[i] = false;
                        replaced += 1;
                    }
                }
                for n in src
                    .context
                    .iter()
                    .filter(|n| mine(n))
                    .chain(others.iter().flatten())
                {
                    if hit(n) {
                        out.erase.push(*n);
                        replaced += 1;
                    }
                }
                if replaced > 0 {
                    note = format!(" {replaced} note(s) under the stroke replaced.");
                }
            }
        }
        // ---- 7. Output: the selection (what n08 keeps), then the drawing.
        out.set_dim(0);
        for (n, keep) in src.notes.iter().zip(&keep_src) {
            if *keep && !out.push(*n) {
                return out.check();
            }
        }
        for d in &drawn {
            out.set_dim(d.dim);
            if !out.emit(d.start, d.dur, d.pitch, d.vel, &d.tpl) {
                return out.check();
            }
        }
        out.message = Some(format!("{count} drawn note(s).{note}"));
        if let Some(s) = shape.as_ref().filter(|_| (0..vals.len()).any(use_shape)) {
            let m = out.message.get_or_insert_with(String::new);
            m.push_str(&format!(" Recognized a {} ({:.1} % off).", s.name(), s.err));
        }
        Ok(())
    }
}

/// For ascending query ticks, the index of the last stroke segment
/// (`pts.windows(2)`) covering each — what `pitch_at` / `speed_at` find by
/// scanning every segment — in O((queries + segments) α): the segments in
/// reverse drawing order take the queries no later one took (a "next free
/// query" union-find).
fn last_covering(pts: &[StrokePoint], q: &[u64]) -> Vec<Option<usize>> {
    fn find(next: &mut [usize], mut i: usize) -> usize {
        let mut r = i;
        while next[r] != r {
            r = next[r];
        }
        while next[i] != r {
            let j = next[i];
            next[i] = r;
            i = j;
        }
        r
    }
    let n = q.len();
    let mut out = vec![None; n];
    let mut next: Vec<usize> = (0..=n).collect();
    for (wi, w) in pts.windows(2).enumerate().rev() {
        let (lo, hi) = (w[0].tick.min(w[1].tick), w[0].tick.max(w[1].tick));
        let mut i = find(&mut next, q.partition_point(|&t| t < lo));
        while i < n && q[i] <= hi {
            out[i] = Some(wi);
            next[i] = i + 1;
            i = find(&mut next, i + 1);
        }
    }
    out
}

/// `pitch_at`'s value on segment `w` (its arithmetic exactly).
fn segment_pitch(pts: &[StrokePoint], w: usize, tick: u64) -> f64 {
    let (a, b) = (pts[w], pts[w + 1]);
    if b.tick == a.tick {
        b.pitch
    } else {
        let t = (tick as f64 - a.tick as f64) / (b.tick as f64 - a.tick as f64);
        a.pitch + (b.pitch - a.pitch) * t
    }
}

/// `speed_at`'s value on segment `w` (its arithmetic exactly).
fn segment_speed(pts: &[StrokePoint], w: usize, ppq: u64) -> f64 {
    let (a, b) = (pts[w], pts[w + 1]);
    let dt = (b.t - a.t).max(1e-3);
    let beats = (b.tick as f64 - a.tick as f64).abs() / ppq.max(1) as f64;
    let rows = (b.pitch - a.pitch).abs();
    (beats + rows) / dt
}

/// The pitch-class mask of `notes` sounding at each of `times` (ascending):
/// a sweep over the notes' starts and ends. O((n + times) log n).
fn sounding_masks<'a>(notes: impl Iterator<Item = &'a TNote>, times: &[u64]) -> Vec<u16> {
    let mut starts: Vec<(u64, usize)> = Vec::new();
    let mut ends: Vec<(u64, usize)> = Vec::new();
    for n in notes {
        let pc = usize::from(n.pitch) % 12;
        starts.push((n.start, pc));
        ends.push((n.end(), pc));
    }
    starts.sort_unstable();
    ends.sort_unstable();
    let (mut i, mut j) = (0usize, 0usize);
    let mut count = [0i64; 12];
    times
        .iter()
        .map(|&t| {
            while i < starts.len() && starts[i].0 <= t {
                count[starts[i].1] += 1;
                i += 1;
            }
            while j < ends.len() && ends[j].0 <= t {
                count[ends[j].1] -= 1;
                j += 1;
            }
            count
                .iter()
                .enumerate()
                .filter(|(_, c)| **c > 0)
                .fold(0u16, |m, (pc, _)| m | 1 << pc)
        })
        .collect()
}

impl Kernel for Draw9 {
    fn growth(&self) -> Growth {
        Growth::Add(self.growth)
    }

    /// With n06 the positions are the reference onsets under the stroke,
    /// counted in one pass (the prepared layer is in time order; without
    /// it, the source's notes bound them).
    fn estimate_source(&self, src: &Source) -> super::super::Estimate {
        let (Some((kind, num)), Some((lo, hi))) = (self.new.onsets, self.span) else {
            return super::super::Estimate::from_growth(self.growth(), &src.stats);
        };
        let positions = if src.aux.len() > AUX_ONSETS {
            let mut last = None;
            let mut n = 0u64;
            for x in src.aux[AUX_ONSETS]
                .iter()
                .filter(|x| x.start >= lo && x.start <= hi)
            {
                if last != Some(x.start) {
                    n += 1;
                    last = Some(x.start);
                }
            }
            n
        } else {
            let id = src
                .tracks
                .first()
                .and_then(|t| t.subs.get(usize::try_from(num).unwrap_or(usize::MAX)))
                .map(|s| s.0);
            src.notes
                .iter()
                .chain(src.context.iter())
                .filter(|x| match kind {
                    1 => Some(x.sub) == id,
                    _ => num == 0,
                })
                .count() as u64
        };
        let (voices, cont, per) = self.per_position;
        let bound = positions
            .saturating_mul(voices)
            .saturating_add(cont)
            .saturating_mul(per);
        super::super::Estimate::from_growth(Growth::Add(bound), &src.stats)
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let Some(st) = self.stroke.as_ref().filter(|s| !s.points.is_empty()) else {
            for n in &src.notes {
                if !out.push(*n) {
                    return out.check();
                }
            }
            return no_stroke(out);
        };
        self.run_stroke(src, st, out)
    }
}

/// The tick window Draw's notes can reach for this stroke and these
/// settings (conservative): from the step before the stroke, its span ×
/// the longest rhythm × the longest gate (400 %) × (1 + continuation),
/// plus the motif's span and the time jitter; mirrored in time (a7) to
/// before the stroke.
fn draw_window(p: &Params, ctx: &Ctx, st: &Stroke, src: &Source) -> (u64, u64) {
    let step = note_value(p.choice("step"), p.ticks("step.ticks"), ctx).max(1);
    let lo = st.points.iter().map(|q| q.tick).min().unwrap_or(0);
    let hi = st.points.iter().map(|q| q.tick).max().unwrap_or(0);
    let a = lo / step * step;
    let r = if p.flag("n09") {
        p.list("n09.rhythm").into_iter().max().unwrap_or(1).max(1) as u64
    } else {
        1
    };
    let cont = if p.adv(10) {
        p.int("a10.count").max(1) as u64
    } else {
        0
    };
    let span = (hi - a).saturating_add(step);
    let motif = if p.adv(8) {
        src.stats.max_end.saturating_sub(src.stats.min_start)
    } else {
        0
    };
    let jitter = if p.adv(9) {
        super::super::sweep::swept_ticks_max(p, "draw", "a9.ticks")
    } else {
        0
    };
    let reach = span
        .saturating_mul(r)
        .saturating_mul(4)
        .saturating_mul(1 + cont)
        .saturating_add(motif)
        .saturating_add(jitter);
    let b = a.saturating_add(reach);
    let mirror = p.adv(7) && p.choice("a7.mode") != 0;
    let w0 = if mirror { a.saturating_sub(b - a) } else { a }.saturating_sub(jitter);
    (w0, b)
}

/// Draw's source preparation (V09-S39 step 3): the notes its functions
/// read around the pointer stroke — n01's chord notes (the active track's
/// or a reference track's, all of them, sounding in the window), n06's
/// reference notes (a track's, or the active track's on one subchannel,
/// starting under the stroke), n08's existing notes (the active track's
/// unselected notes in the window as context; with "all visible tracks"
/// each Onion Skin track's notes, its track a destination so the commit can
/// remove the replaced ones). Without a context (no stroke) nothing is
/// read.
pub fn draw_prepare(
    doc: &crate::model::PlumDocument,
    p: &Params,
    src: &mut Source,
    limit: usize,
    ctx: Option<&Ctx>,
) -> Result<(), TransformError> {
    let follow = p.flag("n01") && p.choice("n01.follow") < 2;
    let refs = p.flag("n06");
    let over = p.flag("n08") && p.choice("n08.existing") != 0;
    if !(follow || refs || over) {
        return Ok(());
    }
    let Some(ctx) = ctx else {
        return Ok(());
    };
    let Some(st) = ctx.stroke.clone().filter(|s| !s.points.is_empty()) else {
        return Ok(());
    };
    let Some(active) = src.tracks.first().cloned() else {
        return Ok(());
    };
    let (w0, w1) = draw_window(p, ctx, &st, src);
    let lo = st.points.iter().map(|q| q.tick).min().unwrap_or(0);
    let hi = st.points.iter().map(|q| q.tick).max().unwrap_or(0);
    let track_index = |num: i64, what: &str| -> Result<usize, TransformError> {
        if num == 0 {
            return Ok(active.index);
        }
        usize::try_from(num - 1)
            .ok()
            .filter(|&i| doc.track(i).is_some())
            .ok_or_else(|| {
                TransformError::Invalid(format!(
                    "{what}: track {num} does not exist. Choose the track by its number in the track list (0 = the active track)."
                ))
            })
    };
    // The notes of track `index` (the active one: its edited notes, an
    // opened clip's content; B2-F16: another track as the song shows it, on
    // the edited notes' ticks) passing `keep`, as `slot`.
    let read = |index: usize,
                slot: u16,
                t0: u64,
                t1: u64,
                keep: &dyn Fn(&crate::model::NoteRecord) -> bool|
     -> Vec<TNote> {
        let mut v = Vec::new();
        let mut take = |r: crate::model::NoteRecord| {
            if keep(&r) && v.len() < limit {
                v.push(TNote::from_record(&r, slot));
            }
        };
        if index == active.index {
            if let Ok(t) = crate::editor::note_view(doc, active.target()) {
                t.notes.query(t0, t1, |_, n| take(n.record()));
            }
        } else {
            super::super::source::placed_query(doc, index, ctx.shift, t0, t1, &mut take);
        }
        super::super::source::sort_time_major(&mut v);
        v
    };
    let mut aux: Vec<Vec<TNote>> = vec![Vec::new(), Vec::new()];
    if follow {
        let num = if p.choice("n01.follow") == 1 {
            p.int("n01.ref")
        } else {
            0
        };
        let i = track_index(num, "Chord follow")?;
        aux[AUX_CHORDS] = read(i, u16::MAX, w0, w1, &|_| true);
    }
    if refs {
        let num = p.int("n06.num");
        aux[AUX_ONSETS] = if p.choice("n06.ref") == 1 {
            let id = usize::try_from(num)
                .ok()
                .and_then(|i| active.subs.get(i))
                .map(|s| s.0)
                .ok_or_else(|| {
                    TransformError::Invalid(format!(
                        "Reference rhythm: the active track has no subchannel at position {num} (0 = its first)."
                    ))
                })?;
            read(active.index, u16::MAX, lo, hi, &|n| {
                n.start_tick >= lo && n.sub.0 == id
            })
        } else {
            let i = track_index(num, "Reference rhythm")?;
            read(i, u16::MAX, lo, hi, &|n| n.start_tick >= lo)
        };
    }
    if over {
        if src.context.is_empty() {
            let s = std::mem::replace(src, Source::from_notes(Vec::new(), Vec::new()));
            *src = s.with_context_sounding(doc, w0, w1);
        }
        if p.choice("n08.scope") == 1 {
            let ids: Vec<crate::model::TrackId> = doc.tracks().iter().map(|t| t.id()).collect();
            for i in doc.project().onion.sources(&ids, active.index) {
                if src.tracks.iter().any(|t| t.index == i) {
                    continue;
                }
                // B2-F16: each visible note keeps the slot of its own clip,
                // so a replacement erases it there.
                aux.push(src.placed_window_slots(doc, i, (w0, w1), limit, ctx.shift)?);
            }
        }
    }
    src.aux = Arc::new(aux);
    Ok(())
}

// ====================================================================== Paint

pub const PAINT: ToolDef = ToolDef {
    id: "paint",
    spec: paint_spec,
    build: paint_build,
};

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const PAINT_HELP: &[(&str, &str)] = &[
    (
        "step",
        "The width of one brush cell; every cell the pointer passes over (this wide, one key high) gets a note (Snap uses the Piano Roll's snap).",
    ),
    (
        "step.ticks",
        "The cell width in ticks, used when Cell Width is set to Custom Ticks.",
    ),
    (
        "gate",
        "Each painted note's length as a share of its cell (100 % fills the cell).",
    ),
    ("vel", "The velocity of every painted note."),
    (
        "sub",
        "The subchannel of the active track the painted notes go to, by its position in the track's list (0 = the first).",
    ),
    (
        "a1",
        "Paints several short notes into each cell instead of one.",
    ),
    (
        "a1.count",
        "How many notes share each cell, one after another.",
    ),
    (
        "a2",
        "Scatters each painted note randomly around the pointer (seeded).",
    ),
    ("a2.rows", "The largest random move up or down, in keys."),
    (
        "a2.cells",
        "The largest random move earlier or later, in cells.",
    ),
    (
        "a3",
        "Paints a chord in each cell instead of a single note.",
    ),
    (
        "a3.chord",
        "The chord's intervals in semitones above the painted key (0 = the key itself).",
    ),
    (
        "a4",
        "Paints a band several keys tall, centered on the pointer.",
    ),
    ("a4.width", "How many keys tall the band is."),
    (
        "a5",
        "Gives each painted cell a random subchannel from a list (seeded).",
    ),
    (
        "a5.subs",
        "The subchannel positions to choose from (0 = the track's first subchannel).",
    ),
    (
        "a6",
        "Stamps a copy of the selected notes into the cells instead of single notes.",
    ),
    (
        "a6.every",
        "Stamps on every Nth painted cell (1 = every cell).",
    ),
    (
        "a7",
        "Fades the velocity from one value to another along the stroke.",
    ),
    ("a7.from", "The velocity of the first painted cell."),
    ("a7.to", "The velocity of the last painted cell."),
    (
        "a8",
        "Changes the gate from one value to another along the stroke.",
    ),
    (
        "a8.from",
        "The gate (% of the cell) at the start of the stroke.",
    ),
    (
        "a8.to",
        "The gate (% of the cell) at the end of the stroke.",
    ),
    (
        "a9",
        "Skips cells along the stroke following a repeating on / off pattern.",
    ),
    (
        "a9.pattern",
        "One digit per painted cell in turn, repeated: 1 paints the cell, 0 skips it.",
    ),
    (
        "a10",
        "Adds a trail of softer, shorter particle notes behind each painted cell (seeded).",
    ),
    ("a10.count", "How many particle notes trail each cell."),
    (
        "n01",
        "Turns the brush into an eraser: it deletes the notes it passes over instead of painting.",
    ),
    (
        "n01.erase",
        "Whose notes are erased: every visible track's, the active track's, or only the active track's notes on the brush's subchannel.",
    ),
    (
        "n01.rows",
        "How many keys tall the eraser is, centered on the pointer.",
    ),
    (
        "n02",
        "Fills the empty area enclosed by the active track's notes that the stroke touches, like a paint bucket.",
    ),
    (
        "n02.gap",
        "Notes on a row this close together (in ticks) count as a closed wall; gaps wider than this let the fill leak out.",
    ),
    (
        "n02.most",
        "The largest area that is filled, in cells; a bigger or open area is not filled.",
    ),
    (
        "n02.fill",
        "Cells fills cell by cell; Whole Rows joins each row of the area into one long note.",
    ),
    (
        "n03",
        "Paints with a soft round brush: full in the middle and thinning out toward the edge (seeded).",
    ),
    (
        "n03.radius",
        "The brush's radius, in keys (and as many cells across).",
    ),
    (
        "n03.hard",
        "How much of the radius is painted solid before the brush starts to thin out.",
    ),
    (
        "n04",
        "Paints a repeating tile pattern into the cells the brush covers.",
    ),
    (
        "n04.pattern",
        "The tile's cells, row by row from the bottom: 1 paints the cell, 0 leaves it empty.",
    ),
    ("n04.width", "How many cells wide one row of the tile is."),
    (
        "n05",
        "Pushes the active track's notes near the stroke in the direction the pointer moved, like a smudge tool.",
    ),
    (
        "n05.strength",
        "How far the notes are pushed: at 100 % a note right on the stroke moves a whole radius; farther notes move less.",
    ),
    (
        "n05.radius",
        "How close to the stroke a note must be to be dragged, in keys.",
    ),
    (
        "n06",
        "Changes the subchannel (color) of the notes the brush passes over instead of painting.",
    ),
    (
        "n06.colour",
        "The subchannel the notes get, by its position in each track's list (0 = the first).",
    ),
    (
        "n06.only",
        "Which notes are recolored: those of every visible track, or only the active track's.",
    ),
    (
        "n07",
        "Turns and resizes each new stamp a little more than the previous one (with Shape Stamp).",
    ),
    (
        "n07.rot",
        "How many degrees each stamp turns more than the previous one (counterclockwise).",
    ),
    (
        "n07.scale",
        "How much bigger (or, negative, smaller) each stamp is than the previous one.",
    ),
    (
        "n08",
        "Paints only part of the cells in an even ordered-dither pattern, for a lighter fill.",
    ),
    ("n08.level", "The share of cells that are painted."),
    (
        "n08.matrix",
        "The size of the dither pattern: smaller is coarser, larger gives finer steps of the level.",
    ),
    ("n09", "Paints (or edits) only on the keys of a scale."),
    ("n09.key", "The key of the scale."),
    ("n09.scale", "The scale whose keys are used."),
    (
        "n10",
        "Changes the length of the notes the brush passes over so they end where the stroke ends.",
    ),
    (
        "n10.mode",
        "Extend lengthens notes that end before the stroke's end; Trim shortens notes that end after it.",
    ),
    (
        "n10.scope",
        "Which notes are changed: those of every visible track, or only the active track's.",
    ),
    (
        "n11",
        "Changes the velocity of the active track's notes the brush passes over instead of painting.",
    ),
    (
        "n11.vel",
        "The velocity to set, the amount to add, or the percentage to scale by, depending on the mode.",
    ),
    (
        "n11.mode",
        "Set gives every note this velocity; Add adds it (negative lowers); Scale changes it by this percentage.",
    ),
];

fn paint_spec() -> ToolSpec {
    let mut p = vec![
        choice("step", "Cell width", NOTE_VALUES, 10),
        ticks("step.ticks", "Custom cell (ticks)", 1, 1 << 24, 240),
        float("gate", "Gate (of the cell)", 5.0, 400.0, 100.0, "%"),
        int("vel", "Velocity", 1, 127, 100),
        int(
            "sub",
            "Subchannel (position in the track's list)",
            0,
            4095,
            0,
        ),
        seed("seed"),
        adv_switch(1, "Density brush"),
        int("a1.count", "Notes per cell", 2, 64, 4).adv(1),
        adv_switch(2, "Scatter brush"),
        int("a2.rows", "Scatter radius (rows)", 1, 48, 4).adv(2),
        int("a2.cells", "Scatter radius (cells)", 0, 64, 2).adv(2),
        adv_switch(3, "Chord brush"),
        list("a3.chord", "Chord intervals", -48, 48, 16, &[0, 4, 7]).adv(3),
        adv_switch(4, "Ribbon brush"),
        int("a4.width", "Width (rows)", 2, 64, 5).adv(4),
        adv_switch(5, "Multi-subchannel spray"),
        list(
            "a5.subs",
            "Subchannel positions",
            0,
            4095,
            64,
            &[0, 1, 2, 3],
        )
        .adv(5),
        adv_switch(6, "Shape stamp (the selected notes)"),
        int("a6.every", "Stamp every (cells)", 1, 1024, 4).adv(6),
        adv_switch(7, "Velocity gradient along the stroke"),
        int("a7.from", "Velocity at the start", 1, 127, 30).adv(7),
        int("a7.to", "Velocity at the end", 1, 127, 127).adv(7),
        adv_switch(8, "Gate gradient along the stroke"),
        float("a8.from", "Gate at the start", 5.0, 400.0, 100.0, "%").adv(8),
        float("a8.to", "Gate at the end", 5.0, 400.0, 20.0, "%").adv(8),
        adv_switch(9, "Patterned spacing"),
        list(
            "a9.pattern",
            "Cells on (1) / off (0)",
            0,
            1,
            64,
            &[1, 0, 1, 1, 0],
        )
        .adv(9),
        adv_switch(10, "Seeded particle trail"),
        int("a10.count", "Particles per cell", 1, 16, 3).adv(10),
        // V09-S39 step 4: the functions declared by id
        // (docs/TRANSFORM_ENGINE.md "Note Brush").
        flag("n01", "Eraser brush", false),
        choice("n01.erase", "Erase", PAINT_ERASE, 1),
        int("n01.rows", "Cell rows", 1, 24, 1),
        flag("n02", "Fill bucket", false),
        ticks("n02.gap", "Boundary gap (ticks)", 0, 1 << 24, 0),
        int("n02.most", "Most cells", 1, 10_000_000, 100_000),
        choice("n02.fill", "Fill", &["Cells", "Whole rows"], 0),
        flag("n03", "Soft brush", false),
        int("n03.radius", "Radius (rows)", 1, 48, 4),
        float("n03.hard", "Hardness", 0.0, 100.0, 50.0, "%"),
        flag("n04", "Pattern brush", false),
        list(
            "n04.pattern",
            "Pattern (1 = paint, row by row from the bottom)",
            0,
            1,
            1024,
            &[1, 0, 0, 1],
        ),
        int("n04.width", "Pattern width", 1, 32, 2),
        flag("n05", "Smudge", false),
        float("n05.strength", "Strength", 0.0, 100.0, 50.0, "%"),
        int("n05.radius", "Radius (rows)", 1, 48, 4),
        flag("n06", "Recolor brush", false),
        int(
            "n06.colour",
            "Color (subchannel position in the track's list)",
            0,
            4095,
            1,
        ),
        choice("n06.only", "Recolor", PAINT_SCOPE, 1),
        flag("n07", "Stamp transform", false),
        float("n07.rot", "Rotation step", -180.0, 180.0, 15.0, "°"),
        float("n07.scale", "Scale step", -50.0, 50.0, 0.0, "%"),
        flag("n08", "Dither brush", false),
        float("n08.level", "Level", 0.0, 100.0, 50.0, "%"),
        choice("n08.matrix", "Matrix", &["2×2", "4×4", "8×8"], 1),
        flag("n09", "Scale rows", false),
        choice("n09.key", "Key", &KEY_NAMES, 0),
        choice("n09.scale", "Scale", scale_names(), 0),
        flag("n10", "Length brush", false),
        choice("n10.mode", "Mode", &["Extend", "Trim"], 0),
        choice("n10.scope", "Scope", PAINT_SCOPE, 1),
        flag("n11", "Velocity brush", false),
        int("n11.vel", "Velocity", -127, 127, 100),
        choice("n11.mode", "Mode", &["Set", "Add", "Scale (+ %)"], 0),
    ];
    p.extend(boundary::specs(PitchPolicy::Discard));
    with_help(&mut p, PAINT_HELP);
    ToolSpec {
        id: "paint",
        // v0.8: "Paint" is the subchannel painter (`paint.rs`); this v0.7
        // brush keeps its id (presets, recipes) under a new name.
        name: "Note Brush",
        family: Family::Interactive,
        summary: "Drag in the Piano Roll to paint notes into every grid cell the pointer passes over (cells = width × semitone), on the active track. Preview shows the strokes; Apply adds them (one undo step).",
        params: p,
        advanced: vec![
            "Density brush",
            "Scatter brush",
            "Chord brush",
            "Ribbon brush",
            "Multi-subchannel spray",
            "Shape stamp brush",
            "Velocity gradient paint",
            "Gate gradient paint",
            "Patterned spacing brush",
            "Seeded particle trail brush",
        ],
        // Orders = the catalog's (§235) = where each runs in `Brush9`: the
        // footprint (n02 fill, n03 soft disc, n04 pattern tiles, n08
        // dither), n09's keys, then either the edits (n01, n05, n06, n10,
        // n11 — they paint nothing) or the painting with n07's stamps.
        functions: [
            ("n01", "Eraser brush", 160),
            ("n02", "Fill bucket", 110),
            ("n03", "Soft brush", 120),
            ("n04", "Pattern brush", 130),
            ("n05", "Smudge", 170),
            ("n06", "Recolor brush", 180),
            ("n07", "Stamp transform", 210),
            ("n08", "Dither brush", 140),
            ("n09", "Scale rows", 150),
            ("n10", "Length brush", 190),
            ("n11", "Velocity brush", 200),
        ]
        .into_iter()
        .map(|(id, name, order)| FunctionSpec { id, name, order })
        .collect(),
        needs_selection: false,
        context: false,
    }
}

/// Cells (column, row) the stroke passes over, in drawing order, each once.
fn painted_cells(pts: &[StrokePoint], step: u64) -> Vec<(u64, i64)> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut add = |c: u64, r: i64, out: &mut Vec<(u64, i64)>| {
        if seen.insert((c, r)) {
            out.push((c, r));
        }
    };
    let cell = |q: &StrokePoint| (q.tick / step, q.pitch.floor() as i64);
    if let Some(q) = pts.first() {
        let (c, r) = cell(q);
        add(c, r, &mut out);
    }
    for w in pts.windows(2) {
        let (c0, r0) = cell(&w[0]);
        let (c1, r1) = cell(&w[1]);
        // Walk the cells between the samples (enough sub-steps to visit each).
        let n = (c0.abs_diff(c1) + r0.abs_diff(r1)).clamp(1, 100_000);
        for k in 1..=n {
            let f = k as f64 / n as f64;
            let c = (c0 as f64 + (c1 as f64 - c0 as f64) * f).round() as u64;
            let r = (r0 as f64 + (r1 as f64 - r0 as f64) * f).round() as i64;
            add(c, r, &mut out);
        }
    }
    out
}

fn paint_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    // V09-S39 step 4: any new function, or Control Sweeps (evaluated per
    // painted cell inside the kernel) — the v0.9 kernel; else the v0.8 one.
    let new = BrushNew::from(p);
    if new.any() || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty()) {
        return Brush9::build(p, b, new);
    }
    let step = note_value(p.choice("step"), p.ticks("step.ticks"), b.ctx).max(1);
    let gate = p.float("gate") / 100.0;
    let vel = p.int("vel").clamp(1, 127) as u8;
    let sub = p.int("sub").max(0) as usize;
    let seed = p.seed("seed");
    let density = p.adv(1).then(|| p.int("a1.count").max(2) as u64);
    let scatter = p
        .adv(2)
        .then(|| (p.int("a2.rows").max(1), p.int("a2.cells").max(0)));
    let chord: Vec<i64> = if p.adv(3) {
        p.list("a3.chord")
    } else {
        vec![0]
    };
    let ribbon = p.adv(4).then(|| p.int("a4.width").max(2));
    let spray = p.adv(5).then(|| p.list("a5.subs"));
    let stamp = p.adv(6).then(|| p.int("a6.every").max(1) as usize);
    let vgrad = p
        .adv(7)
        .then(|| (p.int("a7.from") as f64, p.int("a7.to") as f64));
    let ggrad = p
        .adv(8)
        .then(|| (p.float("a8.from") / 100.0, p.float("a8.to") / 100.0));
    let pattern = p.adv(9).then(|| p.list("a9.pattern"));
    let trail = p.adv(10).then(|| p.int("a10.count").max(1) as u64);
    let stroke = b.ctx.stroke.clone();
    let cells_bound = stroke.as_ref().map_or(0, |s| {
        let pts = &s.points;
        pts.windows(2)
            .map(|w| {
                (w[0].tick / step).abs_diff(w[1].tick / step)
                    + (w[0].pitch as i64).abs_diff(w[1].pitch as i64)
                    + 1
            })
            .sum::<u64>()
            + 1
    });
    let per = density.unwrap_or(1)
        * chord.len().max(1) as u64
        * ribbon.map_or(1, |w| w as u64)
        * (1 + trail.unwrap_or(0))
        * stamp.map_or(1, |_| b.stats.count.max(1) as u64);
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            for n in &src.notes {
                if !out.push(*n) {
                    return out.check();
                }
            }
            let Some(st) = stroke.as_ref().filter(|s| !s.points.is_empty()) else {
                return no_stroke(out);
            };
            let cells = painted_cells(&st.points, step);
            let total = cells.len().max(1);
            let tpl0 = template(src, sub, vel);
            let motif: Vec<TNote> = src.notes.clone();
            let mut made = 0u64;
            for (k, &(c, r)) in cells.iter().enumerate() {
                // V09-0143: the driver dimension is the cell's place along
                // the stroke (0 = the first painted).
                out.set_dim(u32::try_from(k).unwrap_or(u32::MAX));
                if let Some(pat) = &pattern
                    && !pat.is_empty()
                    && pat[k % pat.len()] == 0
                {
                    continue;
                }
                if let Some(every) = stamp
                    && k % every != 0
                {
                    continue;
                }
                let f = k as f64 / total as f64;
                let v = vgrad.map_or(f64::from(vel), |(a, z)| a + (z - a) * f);
                let g = ggrad.map_or(gate, |(a, z)| a + (z - a) * f);
                let mut rng = Rng::for_index(seed, salt::SHAPE, k as u64);
                let mut tpl = tpl0;
                if let Some(list) = spray.as_ref().filter(|l| !l.is_empty())
                    && let Some(tr) = src.tracks.first()
                {
                    let pos = list[rng.range_i64(0, list.len() as i64 - 1) as usize].max(0);
                    let (id, ch) = tr.sub_offset(tpl0.sub, pos - sub as i64);
                    tpl.sub = id;
                    tpl.ch = ch;
                }
                let (mut cc, mut rr) = (c as i64, r);
                if let Some((rows, cols)) = scatter {
                    rr += rng.range_i64(-rows, rows);
                    cc = (cc + rng.range_i64(-cols, cols)).max(0);
                }
                let start = cc as u64 * step;
                let dur = ((step as f64 * g).round() as u64).max(1);
                let rows: Vec<i64> = match ribbon {
                    Some(w) => (-(w / 2)..=(w - 1 - w / 2)).map(|d| rr + d).collect(),
                    None => vec![rr],
                };
                // Each cell: stamp or (density × chord) notes per row.
                for row in rows {
                    if stamp.is_some() && !motif.is_empty() {
                        let m0 = motif[0];
                        for m in &motif {
                            if !out.emit(
                                i128::from(start) + i128::from(m.start - m0.start),
                                i128::from(m.dur),
                                row + i64::from(m.pitch) - i64::from(m0.pitch),
                                i64::from(m.vel),
                                &tpl,
                            ) {
                                return out.check();
                            }
                            made += 1;
                        }
                        continue;
                    }
                    let parts = density.unwrap_or(1);
                    for j in 0..parts {
                        let s = start + step * j / parts;
                        let d = (dur / parts).max(1);
                        for iv in &chord {
                            if !out.emit(
                                i128::from(s),
                                i128::from(d),
                                row + iv,
                                v.round() as i64,
                                &tpl,
                            ) {
                                return out.check();
                            }
                            made += 1;
                        }
                    }
                }
                // Particles trailing behind the stroke.
                if let Some(np) = trail {
                    for q in 0..np {
                        let back = rng.range_i64(1, 4) as u64 * step / 2;
                        let pr = rr + rng.range_i64(-2, 2);
                        let pv = (v * (0.6 - 0.15 * q as f64)).max(1.0);
                        if !out.emit(
                            i128::from(start) - i128::from(back),
                            i128::from((dur / 2).max(1)),
                            pr,
                            pv.round() as i64,
                            &tpl,
                        ) {
                            return out.check();
                        }
                        made += 1;
                    }
                }
                out.check()?;
            }
            out.message = Some(format!("{made} painted note(s)."));
            Ok(())
        },
        growth: Growth::Add(cells_bound.saturating_mul(per)),
        estimate: None,
    })
}

// ====================================================== Note Brush (v0.9)

const PAINT_ERASE: &[&str] = &[
    "Every visible track's notes",
    "The active track's notes",
    "The brush color's notes",
];
const PAINT_SCOPE: &[&str] = &["All visible tracks", "The active track"];

/// The Note Brush's prepared layers in [`Source::aux`] ([`paint_prepare`]):
/// one per other visible track (Onion Skin), its notes' `track` = its slot.
const PAINT_AUX_VISIBLE: usize = 0;

/// Salt of n03's per-cell dice (beside the v0.8 salts).
const SALT_SOFT: u64 = 0xBA17_0003;

/// The Note Brush's v0.8 settings (the v0.8 kernel's closure reads the same
/// values; [`Brush9`] re-reads them here, unchanged).
#[derive(Clone, Debug)]
struct Brush8 {
    step: u64,
    gate: f64,
    vel: u8,
    sub: usize,
    seed: u64,
    density: Option<u64>,
    scatter: Option<(i64, i64)>,
    chord: Vec<i64>,
    ribbon: Option<i64>,
    spray: Option<Vec<i64>>,
    stamp: Option<usize>,
    vgrad: Option<(f64, f64)>,
    ggrad: Option<(f64, f64)>,
    pattern: Option<Vec<i64>>,
    trail: Option<u64>,
}

impl Brush8 {
    fn from(p: &Params, b: &BuildCtx) -> Brush8 {
        Brush8 {
            step: note_value(p.choice("step"), p.ticks("step.ticks"), b.ctx).max(1),
            gate: p.float("gate") / 100.0,
            vel: p.int("vel").clamp(1, 127) as u8,
            sub: p.int("sub").max(0) as usize,
            seed: p.seed("seed"),
            density: p.adv(1).then(|| p.int("a1.count").max(2) as u64),
            scatter: p
                .adv(2)
                .then(|| (p.int("a2.rows").max(1), p.int("a2.cells").max(0))),
            chord: if p.adv(3) {
                p.list("a3.chord")
            } else {
                vec![0]
            },
            ribbon: p.adv(4).then(|| p.int("a4.width").max(2)),
            spray: p.adv(5).then(|| p.list("a5.subs")),
            stamp: p.adv(6).then(|| p.int("a6.every").max(1) as usize),
            vgrad: p
                .adv(7)
                .then(|| (p.int("a7.from") as f64, p.int("a7.to") as f64)),
            ggrad: p
                .adv(8)
                .then(|| (p.float("a8.from") / 100.0, p.float("a8.to") / 100.0)),
            pattern: p.adv(9).then(|| p.list("a9.pattern")),
            trail: p.adv(10).then(|| p.int("a10.count").max(1) as u64),
        }
    }

    /// v0.8's bound of the cells the stroke passes over.
    fn cells_bound(&self, st: Option<&Stroke>) -> u64 {
        let step = self.step;
        st.map_or(0, |s| {
            let pts = &s.points;
            pts.windows(2)
                .map(|w| {
                    (w[0].tick / step).abs_diff(w[1].tick / step)
                        + (w[0].pitch as i64).abs_diff(w[1].pitch as i64)
                        + 1
                })
                .sum::<u64>()
                + 1
        })
    }

    /// v0.8's most notes per painted cell (`motif`: the selection's size).
    fn per_cell(&self, motif: u64) -> u64 {
        self.density.unwrap_or(1)
            * self.chord.len().max(1) as u64
            * self.ribbon.map_or(1, |w| w as u64)
            * (1 + self.trail.unwrap_or(0))
            * self.stamp.map_or(1, |_| motif.max(1))
    }
}

/// The Note Brush's settings of the functions declared by id (V09-S39
/// step 4).
#[derive(Clone, Debug, Default)]
struct BrushNew {
    /// n01: whose notes (0 every visible track's, 1 the active track's, 2
    /// the brush colour's), cell rows.
    erase: Option<(usize, i64)>,
    /// n02: boundary gap (ticks), most cells, whole rows.
    fill: Option<(u64, u64, bool)>,
    /// n03: radius (rows = cells), hardness %.
    soft: Option<(i64, f64)>,
    /// n04: the tile (row by row from the bottom), its width.
    pattern: Option<(Vec<bool>, usize)>,
    /// n05: strength %, radius (rows = cells).
    smudge: Option<(f64, i64)>,
    /// n06: colour (subchannel position), all visible tracks.
    recolour: Option<(i64, bool)>,
    /// n07: rotation step (°), scale step (%).
    turn: Option<(f64, f64)>,
    /// n08: level %, matrix side (2, 4, 8).
    dither: Option<(f64, u64)>,
    /// n09: the rows of the key and scale.
    rows: Option<Lattice>,
    /// n10: trim (else extend), all visible tracks.
    length: Option<(bool, bool)>,
    /// n11: amount, mode (0 set, 1 add, 2 scale by + amount %).
    velocity: Option<(i64, usize)>,
}

impl BrushNew {
    fn from(p: &Params) -> BrushNew {
        BrushNew {
            erase: p
                .flag("n01")
                .then(|| (p.choice("n01.erase"), p.int("n01.rows").max(1))),
            fill: p.flag("n02").then(|| {
                (
                    p.ticks("n02.gap"),
                    p.int("n02.most").max(1) as u64,
                    p.choice("n02.fill") == 1,
                )
            }),
            soft: p
                .flag("n03")
                .then(|| (p.int("n03.radius").max(1), p.float("n03.hard"))),
            pattern: p.flag("n04").then(|| {
                (
                    p.list("n04.pattern").into_iter().map(|x| x != 0).collect(),
                    p.int("n04.width").clamp(1, 32) as usize,
                )
            }),
            smudge: p
                .flag("n05")
                .then(|| (p.float("n05.strength"), p.int("n05.radius").max(1))),
            recolour: p
                .flag("n06")
                .then(|| (p.int("n06.colour"), p.choice("n06.only") == 0)),
            turn: p
                .flag("n07")
                .then(|| (p.float("n07.rot"), p.float("n07.scale"))),
            dither: p
                .flag("n08")
                .then(|| (p.float("n08.level"), 2u64 << p.choice("n08.matrix").min(2))),
            rows: p.flag("n09").then(|| {
                Lattice::of_scale(&Scale::new(p.choice("n09.key"), p.choice("n09.scale")))
            }),
            length: p
                .flag("n10")
                .then(|| (p.choice("n10.mode") == 1, p.choice("n10.scope") == 0)),
            velocity: p
                .flag("n11")
                .then(|| (p.int("n11.vel"), p.choice("n11.mode"))),
        }
    }

    fn any(&self) -> bool {
        self.erase.is_some()
            || self.fill.is_some()
            || self.soft.is_some()
            || self.pattern.is_some()
            || self.smudge.is_some()
            || self.recolour.is_some()
            || self.turn.is_some()
            || self.dither.is_some()
            || self.rows.is_some()
            || self.length.is_some()
            || self.velocity.is_some()
    }

    /// An editing brush (n01, n05, n06, n10, n11): it changes the notes
    /// under the stroke and paints none.
    fn edits(&self) -> bool {
        self.erase.is_some()
            || self.smudge.is_some()
            || self.recolour.is_some()
            || self.length.is_some()
            || self.velocity.is_some()
    }
}

/// Swept values of one unit (a painted cell): each its parameter's own
/// value when no Control Sweep drives it.
#[derive(Clone, Copy, Debug)]
struct BrushVals {
    gate: f64,
    vel: u8,
    radius: i64,
    hard: f64,
    strength: f64,
    colour: i64,
    rot: f64,
    scale: f64,
    level: f64,
    amount: i64,
}

/// A cell the brush covers: its column (of `step` ticks), its row (key),
/// the painted cell (unit) it comes from and its width in columns (n02
/// whole rows).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cell {
    col: u64,
    row: i64,
    unit: usize,
    width: u64,
}

/// The v0.9 Note Brush kernel (V09-S39 step 4): the v0.8 brush re-read from
/// the same parameters; the footprint functions (n02, n03, n04, n08) shape
/// the cells it covers, n09 keeps the keys of a scale; with an editing
/// function on (n01, n05, n06, n10, n11) it changes the notes under the
/// stroke instead of painting; Control Sweeps are evaluated per painted
/// cell (V09-0143: the output does not come from the selected notes).
struct Brush9 {
    v8: Brush8,
    new: BrushNew,
    stroke: Option<Arc<Stroke>>,
    sp: Option<SweptParams>,
    /// Base values of the swept parameters.
    base: BrushVals,
    growth: u64,
}

impl Brush9 {
    fn build(p: &Params, b: &BuildCtx, new: BrushNew) -> Result<Box<dyn Kernel>, TransformError> {
        let v8 = Brush8::from(p, b);
        let stroke = b.ctx.stroke.clone();
        let sp = SweptParams::of(p, b);
        let base = BrushVals {
            gate: v8.gate,
            vel: v8.vel,
            radius: p.int("n03.radius").max(1),
            hard: p.float("n03.hard"),
            strength: p.float("n05.strength"),
            colour: p.int("n06.colour"),
            rot: p.float("n07.rot"),
            scale: p.float("n07.scale"),
            level: p.float("n08.level"),
            amount: p.int("n11.vel"),
        };
        // An editing brush writes at most the selection and an edited copy
        // of each other note it reads ([`Kernel::estimate_source`]); a
        // painting one the v0.8 bound over the footprint's cells.
        let growth = if new.edits() {
            0
        } else {
            let st = stroke.as_deref();
            let mut cells = v8.cells_bound(st);
            // The stroke's box (columns, rows), for the footprint's bound.
            let bx = st.filter(|s| !s.points.is_empty()).map(|s| {
                let c0 = s.points.iter().map(|q| q.tick).min().unwrap_or(0) / v8.step;
                let c1 = s.points.iter().map(|q| q.tick).max().unwrap_or(0) / v8.step;
                let (lo, hi) = pitch_span(&s.points);
                (c1 - c0 + 1, (hi.floor() - lo.floor()) as u64 + 1)
            });
            let boxed_in = new.fill.is_none();
            if let Some((_, most, _)) = new.fill {
                cells = cells.saturating_add(most);
            }
            let (mut gx, mut gy) = (0u64, 0u64);
            if new.soft.is_some() {
                let r = match &sp {
                    Some(s) if s.drives("n03.radius") => s.int_bounds("n03.radius").1,
                    _ => base.radius,
                }
                .clamp(1, 48) as u64;
                cells = cells.saturating_mul((2 * r + 1) * (2 * r + 1));
                (gx, gy) = (2 * r, 2 * r);
            }
            if let Some((pat, w)) = &new.pattern {
                let h = pat.len().div_ceil(*w).max(1) as u64;
                cells = cells.saturating_mul(*w as u64 * h);
                gx += 2 * *w as u64;
                gy += 2 * h;
            }
            if let Some((bc, br)) = bx
                && boxed_in
            {
                // The footprint stays within the stroke's box grown by the
                // discs and the tiles (each cell once).
                cells = cells.min(bc.saturating_add(gx).saturating_mul(br.saturating_add(gy)));
            }
            cells.saturating_mul(v8.per_cell(b.stats.count as u64))
        };
        Ok(Box::new(Brush9 {
            v8,
            new,
            stroke,
            sp,
            base,
            growth,
        }))
    }

    /// Each unit's swept values ([`SweptParams`]: measured at the painted
    /// cell — its start, its row, its index — among the painted cells).
    fn unit_vals(&self, units: &[TNote]) -> Vec<BrushVals> {
        let Some(sp) = &self.sp else {
            return vec![self.base; units.len()];
        };
        let sp = sp.in_frame(Frame::of_notes(units));
        let n = units.len();
        units
            .iter()
            .enumerate()
            .map(|(i, u)| {
                let pl = Place::unit(i, n);
                let b = self.base;
                let f = |k: &str, v: f64| if sp.drives(k) { sp.float(k, u, pl) } else { v };
                let g = |k: &str, v: i64| if sp.drives(k) { sp.int(k, u, pl) } else { v };
                BrushVals {
                    gate: if sp.drives("gate") {
                        sp.float("gate", u, pl) / 100.0
                    } else {
                        b.gate
                    },
                    vel: g("vel", i64::from(b.vel)).clamp(1, 127) as u8,
                    radius: g("n03.radius", b.radius).clamp(1, 48),
                    hard: f("n03.hard", b.hard),
                    strength: f("n05.strength", b.strength),
                    colour: g("n06.colour", b.colour),
                    rot: f("n07.rot", b.rot),
                    scale: f("n07.scale", b.scale),
                    level: f("n08.level", b.level),
                    amount: g("n11.vel", b.amount),
                }
            })
            .collect()
    }

    fn run_stroke(
        &self,
        src: &Source,
        st: &Stroke,
        out: &mut Output,
    ) -> Result<(), TransformError> {
        let step = self.v8.step;
        let (cells, seg) = painted_cells_by_segment(&st.points, step);
        let units: Vec<TNote> = cells
            .iter()
            .map(|&(c, r)| TNote {
                start: c * step,
                dur: step,
                pitch: r.clamp(0, 255) as u8,
                vel: self.v8.vel,
                ..TNote::default()
            })
            .collect();
        let vals = self.unit_vals(&units);
        let mut note = String::new();
        let foot = self.footprint(src, &cells, &vals, &mut note, out)?;
        if self.new.edits() {
            self.edit(src, st, &foot, &seg, &vals, out, note)
        } else {
            self.paint(src, &foot, &vals, out, note)
        }
    }

    /// The cells the brush covers, in order: the painted cells (v0.8), or
    /// (n02) the enclosed regions they lie in; (n03) each cell's soft disc;
    /// (n04) each cell's pattern tile; (n08) the cells the Bayer matrix
    /// keeps at the level; (n02 whole rows, painting) runs merged per row.
    fn footprint(
        &self,
        src: &Source,
        cells: &[(u64, i64)],
        vals: &[BrushVals],
        note: &mut String,
        out: &mut Output,
    ) -> Result<Vec<Cell>, TransformError> {
        let nw = &self.new;
        let mut foot: Vec<Cell> = cells
            .iter()
            .enumerate()
            .map(|(k, &(col, row))| Cell {
                col,
                row,
                unit: k,
                width: 1,
            })
            .collect();
        if let Some((gap, most, _)) = nw.fill {
            foot = self.fill(src, &foot, gap, most, note, out)?;
        }
        if nw.soft.is_some() {
            foot = self.soften(&foot, vals);
        }
        if let Some((pat, w)) = &nw.pattern {
            foot = tiles(&foot, pat, *w);
        }
        if let Some((_, m)) = nw.dither {
            let m2 = (m * m) as f64;
            foot.retain(|c| {
                let t = (bayer(m, c.col, c.row.rem_euclid(m as i64) as u64) as f64 + 0.5) / m2;
                t < vals[c.unit].level / 100.0
            });
        }
        if let Some((_, _, true)) = nw.fill
            && !nw.edits()
        {
            foot = whole_rows(foot);
        }
        Ok(foot)
    }

    /// n02: the regions of empty cells enclosed by the active track's notes
    /// (on a row, notes at most `gap` ticks apart close the gap) that the
    /// stroke's cells lie in, flooded four-connected in breadth-first order
    /// from them. A region reaching past the notes' box (or before tick 0)
    /// is open and one with more cells than are left of `most` is too
    /// large: neither is filled.
    fn fill(
        &self,
        src: &Source,
        seeds: &[Cell],
        gap: u64,
        most: u64,
        note: &mut String,
        out: &mut Output,
    ) -> Result<Vec<Cell>, TransformError> {
        let step = self.v8.step;
        // Per key (0 – 255) the notes' spans.
        let mut by_row: Vec<Vec<(u64, u64)>> = vec![Vec::new(); 256];
        for n in src
            .notes
            .iter()
            .chain(src.context.iter())
            .filter(|n| n.track == 0)
        {
            by_row[usize::from(n.pitch)].push((n.start, n.end().max(n.start + 1)));
        }
        if by_row.iter().all(Vec::is_empty) {
            note.push_str(" Fill bucket: no notes enclose a region here.");
            return Ok(Vec::new());
        }
        // Walls: per row the notes' spans merged across the gaps (sorted,
        // disjoint), and their box in cells.
        let (mut c_lo, mut c_hi, mut r_lo, mut r_hi) = (u64::MAX, 0u64, i64::MAX, i64::MIN);
        let mut walls: Vec<Vec<(u64, u64)>> = vec![Vec::new(); 256];
        for (row, mut v) in by_row.into_iter().enumerate() {
            if v.is_empty() {
                continue;
            }
            let row = row as i64;
            v.sort_unstable();
            let mut m: Vec<(u64, u64)> = Vec::with_capacity(v.len());
            for (s, e) in v {
                if let Some(l) = m.last_mut()
                    && s <= l.1.saturating_add(gap)
                {
                    l.1 = l.1.max(e);
                    continue;
                }
                m.push((s, e));
            }
            c_lo = c_lo.min(m[0].0 / step);
            c_hi = c_hi.max(m.iter().map(|x| (x.1 - 1) / step).max().unwrap_or(0));
            r_lo = r_lo.min(row);
            r_hi = r_hi.max(row);
            walls[row as usize] = m;
        }
        let wall = |c: u64, r: i64| -> bool {
            usize::try_from(r)
                .ok()
                .and_then(|r| walls.get(r))
                .is_some_and(|m| {
                    let (a, b) = (c.saturating_mul(step), (c + 1).saturating_mul(step));
                    let k = m.partition_point(|x| x.0 < b);
                    k > 0 && m[k - 1].1 > a
                })
        };
        let inside = |c: u64, r: i64| c >= c_lo && c <= c_hi && r >= r_lo && r <= r_hi;
        // Each explored cell's region; each region's state (0 filled, 1
        // open, 2 too large).
        let mut region_of: FastMap<(u64, i64), u32> = FastMap::default();
        let mut state: Vec<u8> = Vec::new();
        let mut filled: Vec<Cell> = Vec::new();
        let mut polls = 0u32;
        for s in seeds {
            let key = (s.col, s.row);
            if region_of.contains_key(&key) || wall(s.col, s.row) {
                continue;
            }
            let id = state.len() as u32;
            let left = most.saturating_sub(filled.len() as u64);
            let mut region = vec![key];
            region_of.insert(key, id);
            let mut st = if inside(s.col, s.row) { 0u8 } else { 1 };
            let mut i = 0;
            'flood: while st == 0 && i < region.len() {
                let (c, r) = region[i];
                i += 1;
                for (dc, dr) in [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)] {
                    let Some(nc) = c.checked_add_signed(dc) else {
                        st = 1;
                        break 'flood;
                    };
                    let nr = r + dr;
                    if !inside(nc, nr) {
                        st = 1;
                        break 'flood;
                    }
                    match region_of.get(&(nc, nr)) {
                        // Another seed's region stopped early: the same
                        // region, in the same state.
                        Some(&o) if o != id => {
                            st = state[o as usize].max(1);
                            break 'flood;
                        }
                        Some(_) => continue,
                        None => {}
                    }
                    if wall(nc, nr) {
                        continue;
                    }
                    region_of.insert((nc, nr), id);
                    region.push((nc, nr));
                }
                if region.len() as u64 > left {
                    st = 2;
                }
                polls += 1;
                if polls & 0xFFF == 0 {
                    out.poll()?;
                }
            }
            state.push(st);
            if st == 0 {
                filled.extend(region.into_iter().map(|(col, row)| Cell {
                    col,
                    row,
                    unit: s.unit,
                    width: 1,
                }));
            }
        }
        let count = |k: u8| state.iter().filter(|&&x| x == k).count();
        let (done, open, large) = (count(0), count(1), count(2));
        if done > 0 {
            note.push_str(&format!(
                " Filled {done} region(s), {} cell(s).",
                filled.len()
            ));
        }
        if open > 0 {
            note.push_str(&format!(
                " {open} region(s) under the stroke are not enclosed (close the outline or raise the boundary gap)."
            ));
        }
        if large > 0 {
            note.push_str(&format!(
                " {large} region(s) have more cells than Most cells allows and were not filled."
            ));
        }
        if state.is_empty() {
            note.push_str(" Fill bucket: the stroke lies on notes, not in an empty region.");
        }
        Ok(filled)
    }

    /// n03: every cell's disc (its radius in rows and in columns) with a
    /// density of 1 within hardness × radius of the centre, falling
    /// linearly to 0 at the radius. A cell is painted when its die (seeded
    /// by the cell, so overlapping discs agree) falls below the largest
    /// density of the discs covering it — one pass for the densities, one
    /// die per cell; each cell once, in the order the discs first reach it
    /// (its unit: that disc's).
    fn soften(&self, foot: &[Cell], vals: &[BrushVals]) -> Vec<Cell> {
        // (col, row, unit, best density) in first-reach order; cell → index.
        let mut order: Vec<(u64, i64, usize, f64)> = Vec::new();
        let mut at: FastMap<(u64, i64), u32> = FastMap::default();
        let mut discs: FastMap<i64, Vec<(i64, i64, f64)>> = FastMap::default();
        for c in foot {
            let r = vals[c.unit].radius.max(1);
            let rf = r as f64;
            let h = (vals[c.unit].hard / 100.0).clamp(0.0, 1.0);
            let disc = discs.entry(r).or_insert_with(|| {
                let mut v = Vec::new();
                for dy in -r..=r {
                    for dx in -r..=r {
                        let d = ((dx * dx + dy * dy) as f64).sqrt();
                        if d <= rf {
                            v.push((dx, dy, d));
                        }
                    }
                }
                v
            });
            for x in c.col..c.col + c.width {
                for &(dx, dy, d) in disc.iter() {
                    let Some(col) = x.checked_add_signed(dx) else {
                        continue;
                    };
                    let p = if d <= h * rf {
                        1.0
                    } else {
                        (rf - d) / (rf * (1.0 - h))
                    };
                    let row = c.row + dy;
                    match at.entry((col, row)) {
                        std::collections::hash_map::Entry::Occupied(e) => {
                            let o = &mut order[*e.get() as usize].3;
                            if p > *o {
                                *o = p;
                            }
                        }
                        std::collections::hash_map::Entry::Vacant(e) => {
                            e.insert(order.len() as u32);
                            order.push((col, row, c.unit, p));
                        }
                    }
                }
            }
        }
        order
            .into_iter()
            .filter(|&(col, row, _, p)| {
                p >= 1.0
                    || Rng::for_index(self.v8.seed, SALT_SOFT, hash(&[col, row as u64])).unit() < p
            })
            .map(|(col, row, unit, _)| Cell {
                col,
                row,
                unit,
                width: 1,
            })
            .collect()
    }

    fn paint(
        &self,
        src: &Source,
        foot: &[Cell],
        vals: &[BrushVals],
        out: &mut Output,
        note: String,
    ) -> Result<(), TransformError> {
        let v = &self.v8;
        for n in &src.notes {
            if !out.push(*n) {
                return out.check();
            }
        }
        let step = v.step;
        let total = foot.len().max(1);
        let tpl0 = template(src, v.sub, v.vel);
        let motif: &[TNote] = &src.notes;
        let keys = self.new.rows;
        let mut made = 0u64;
        // n09: notes off the scale's keys are not painted (nor counted).
        let mut emit = |out: &mut Output, s: i128, d: i128, p: i64, vel: i64, tpl: &TNote| {
            if let Some(l) = keys
                && !l.contains(p)
            {
                return true;
            }
            made += 1;
            out.emit(s, d, p, vel, tpl)
        };
        // n07: the stamps' turn and size so far.
        let (mut stamps, mut angle, mut size) = (0usize, 0.0f64, 1.0f64);
        for (k, c) in foot.iter().enumerate() {
            out.set_dim(u32::try_from(k).unwrap_or(u32::MAX));
            if let Some(pat) = &v.pattern
                && !pat.is_empty()
                && pat[k % pat.len()] == 0
            {
                continue;
            }
            if let Some(every) = v.stamp
                && k % every != 0
            {
                continue;
            }
            let uv = &vals[c.unit];
            let f = k as f64 / total as f64;
            let vel = v.vgrad.map_or(f64::from(uv.vel), |(a, z)| a + (z - a) * f);
            let g = v.ggrad.map_or(uv.gate, |(a, z)| a + (z - a) * f);
            let mut rng = Rng::for_index(v.seed, salt::SHAPE, k as u64);
            let mut tpl = tpl0;
            if let Some(list) = v.spray.as_ref().filter(|l| !l.is_empty())
                && let Some(tr) = src.tracks.first()
            {
                let pos = list[rng.range_i64(0, list.len() as i64 - 1) as usize].max(0);
                let (id, ch) = tr.sub_offset(tpl0.sub, pos - v.sub as i64);
                tpl.sub = id;
                tpl.ch = ch;
            }
            let (mut cc, mut rr) = (c.col as i64, c.row);
            if let Some((rows, cols)) = v.scatter {
                rr += rng.range_i64(-rows, rows);
                cc = (cc + rng.range_i64(-cols, cols)).max(0);
            }
            let start = cc as u64 * step;
            let span = step.saturating_mul(c.width);
            let dur = ((span as f64 * g).round() as u64).max(1);
            let rows: Vec<i64> = match v.ribbon {
                Some(w) => (-(w / 2)..=(w - 1 - w / 2)).map(|d| rr + d).collect(),
                None => vec![rr],
            };
            let stamping = v.stamp.is_some() && !motif.is_empty();
            if stamping {
                if self.new.turn.is_some() && stamps > 0 {
                    angle += uv.rot;
                    size = (size * (1.0 + uv.scale / 100.0)).clamp(1.0 / 64.0, 64.0);
                }
                stamps += 1;
            }
            // Each cell: stamp or (density × chord) notes per row.
            for row in rows {
                if stamping {
                    let m0 = motif[0];
                    for m in motif {
                        let (ds, dp, dd) = if angle != 0.0 || size != 1.0 {
                            turn(m, &m0, step, angle, size)
                        } else {
                            (
                                i128::from(m.start - m0.start),
                                i64::from(m.pitch) - i64::from(m0.pitch),
                                i128::from(m.dur),
                            )
                        };
                        if !emit(
                            out,
                            i128::from(start) + ds,
                            dd,
                            row + dp,
                            i64::from(m.vel),
                            &tpl,
                        ) {
                            return out.check();
                        }
                    }
                    continue;
                }
                let parts = v.density.unwrap_or(1);
                for j in 0..parts {
                    let s = start + span * j / parts;
                    let d = (dur / parts).max(1);
                    for iv in &v.chord {
                        if !emit(
                            out,
                            i128::from(s),
                            i128::from(d),
                            row + iv,
                            vel.round() as i64,
                            &tpl,
                        ) {
                            return out.check();
                        }
                    }
                }
            }
            // Particles trailing behind the stroke.
            if let Some(np) = v.trail {
                for q in 0..np {
                    let back = rng.range_i64(1, 4) as u64 * step / 2;
                    let pr = rr + rng.range_i64(-2, 2);
                    let pv = (vel * (0.6 - 0.15 * q as f64)).max(1.0);
                    if !emit(
                        out,
                        i128::from(start) - i128::from(back),
                        i128::from((dur / 2).max(1)),
                        pr,
                        pv.round() as i64,
                        &tpl,
                    ) {
                        return out.check();
                    }
                }
            }
            out.check()?;
        }
        out.message = Some(format!("{made} painted note(s).{note}"));
        Ok(())
    }

    /// The editing brush: the notes under the stroke (n09: on the scale's
    /// keys) — n01 removes them, n05 drags the active track's notes near
    /// the stroke, n06 recolours, n10 lengthens or trims and n11 sets their
    /// velocities. The selection is written edited (without the removed
    /// notes); every other note it changes is removed ([`Output::erase`])
    /// and written edited, so Apply is one undo step (the edited notes
    /// selected).
    #[allow(clippy::too_many_arguments)]
    fn edit(
        &self,
        src: &Source,
        st: &Stroke,
        foot: &[Cell],
        seg: &[(usize, usize)],
        vals: &[BrushVals],
        out: &mut Output,
        note: String,
    ) -> Result<(), TransformError> {
        let nw = &self.new;
        let step = self.v8.step;
        let cells = |rows: i64| {
            foot.iter().flat_map(move |c| {
                (0..c.width).flat_map(move |i| {
                    (-(rows / 2)..=(rows - 1 - rows / 2))
                        .map(move |d| (c.col + i, c.row + d, c.unit))
                })
            })
        };
        let cover = Cover::of(cells(1));
        let erase_cover = match nw.erase {
            Some((_, rows)) if rows > 1 => Some(Cover::of(cells(rows))),
            _ => None,
        };
        let brush_sub = template(src, self.v8.sub, self.v8.vel).sub;
        // n10: the end of the cell where the stroke ends.
        let end = st.points.last().map_or(0, |q| {
            (q.tick / step).saturating_add(1).saturating_mul(step)
        });
        let smudge = nw
            .smudge
            .map(|(_, radius)| Smudge::new(&st.points, step, radius));
        // Counts: removed, smudged, recoloured, lengths, velocities.
        let mut counts = [0usize; 5];
        let mut edit_one = |n: &TNote| -> Edited {
            let active = n.track == 0;
            if let Some(l) = nw.rows
                && !l.contains(i64::from(n.pitch))
            {
                return Edited::Same;
            }
            let covered = cover.unit(n, step);
            if let Some((who, _)) = nw.erase {
                let hit = erase_cover.as_ref().map_or(covered, |c| c.unit(n, step));
                let whose = match who {
                    0 => true,
                    1 => active,
                    _ => active && n.sub == brush_sub,
                };
                if hit.is_some() && whose {
                    counts[0] += 1;
                    return Edited::Gone;
                }
            }
            let mut e = (
                i128::from(n.start),
                i128::from(n.dur),
                i64::from(n.pitch),
                i64::from(n.vel),
                *n,
            );
            let mut changed = false;
            if active
                && let Some(sm) = &smudge
                && let Some((dt, dp)) = sm.shift(n, |u| {
                    let u = seg_unit(seg, u.0, u.1);
                    vals[u.min(vals.len().saturating_sub(1))].strength
                })
                && (dt != 0 || dp != 0)
            {
                e.0 += dt;
                e.2 += dp;
                changed = true;
                counts[1] += 1;
            }
            if let Some(u) = covered {
                let uv = &vals[u];
                if let Some((_, all)) = nw.recolour
                    && (all || active)
                    && let Some(tr) = src.tracks.get(usize::from(n.track))
                    && !tr.subs.is_empty()
                {
                    let pos = uv.colour.rem_euclid(tr.subs.len() as i64) as usize;
                    let id = tr.subs[pos].0;
                    if id != e.4.sub {
                        e.4.sub = id;
                        changed = true;
                        counts[2] += 1;
                    }
                }
                if let Some((trim, all)) = nw.length
                    && (all || active)
                {
                    let z = i128::from(end);
                    let fits = if trim { e.0 + e.1 > z } else { e.0 + e.1 < z };
                    if e.0 < z && fits {
                        e.1 = z - e.0;
                        changed = true;
                        counts[3] += 1;
                    }
                }
                if let Some((_, mode)) = nw.velocity
                    && active
                {
                    let a = uv.amount;
                    let nv = match mode {
                        0 => a,
                        1 => e.3 + a,
                        _ => (e.3 as f64 * (100.0 + a as f64) / 100.0).round() as i64,
                    }
                    .clamp(1, 127);
                    if nv != e.3 {
                        e.3 = nv;
                        changed = true;
                        counts[4] += 1;
                    }
                }
            }
            if changed {
                Edited::Changed(e.0, e.1, e.2, e.3, e.4)
            } else {
                Edited::Same
            }
        };
        for n in &src.notes {
            let ok = match edit_one(n) {
                Edited::Gone => true,
                Edited::Same => out.push(*n),
                Edited::Changed(s, d, p, v, t) => out.emit(s, d, p, v, &t),
            };
            if !ok {
                return out.check();
            }
        }
        let others: &[Vec<TNote>] = if src.aux.len() > PAINT_AUX_VISIBLE {
            &src.aux[PAINT_AUX_VISIBLE..]
        } else {
            &[]
        };
        for n in src.context.iter().chain(others.iter().flatten()) {
            match edit_one(n) {
                Edited::Same => {}
                Edited::Gone => out.erase.push(*n),
                Edited::Changed(s, d, p, v, t) => {
                    out.erase.push(*n);
                    if !out.emit(s, d, p, v, &t) {
                        return out.check();
                    }
                }
            }
            out.check()?;
        }
        let names = [
            "removed",
            "smudged",
            "recoloured",
            "lengths changed",
            "velocities changed",
        ];
        let parts: Vec<String> = counts
            .iter()
            .zip(names)
            .filter(|(c, _)| **c > 0)
            .map(|(c, n)| format!("{c} {n}"))
            .collect();
        out.message = Some(if parts.is_empty() {
            format!("No note under the stroke changed.{note}")
        } else {
            format!("Under the stroke: {}.{note}", parts.join(", "))
        });
        Ok(())
    }
}

/// What the editing brush does to one note: nothing, remove it, or write it
/// as (start, length, pitch, velocity, the note's other fields).
enum Edited {
    Same,
    Gone,
    Changed(i128, i128, i64, i64, TNote),
}

/// The unit of segment `wi`'s point at `t` (0 … 1 along it): the painted
/// cells the segment added, by `t`; a segment that added none, the latest
/// cell before it.
fn seg_unit(seg: &[(usize, usize)], wi: usize, t: f64) -> usize {
    match seg.get(wi) {
        Some(&(a, b)) if b > a => a + ((b - a - 1) as f64 * t.clamp(0.0, 1.0)).round() as usize,
        Some(&(_, b)) => b.saturating_sub(1),
        None => 0,
    }
}

impl Kernel for Brush9 {
    fn growth(&self) -> Growth {
        Growth::Add(self.growth)
    }

    /// An editing brush writes the selection and at most one edited copy of
    /// each other note it read (the active track's unselected notes and
    /// the other visible tracks' notes, [`paint_prepare`]).
    fn estimate_source(&self, src: &Source) -> super::super::Estimate {
        if !self.new.edits() {
            return super::super::Estimate::from_growth(self.growth(), &src.stats);
        }
        let others: u64 = src
            .aux
            .iter()
            .skip(PAINT_AUX_VISIBLE)
            .map(|l| l.len() as u64)
            .sum();
        super::super::Estimate::from_growth(
            Growth::Add((src.context.len() as u64).saturating_add(others)),
            &src.stats,
        )
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        let Some(st) = self.stroke.as_ref().filter(|s| !s.points.is_empty()) else {
            for n in &src.notes {
                if !out.push(*n) {
                    return out.check();
                }
            }
            return no_stroke(out);
        };
        self.run_stroke(src, st, out)
    }
}

/// Painted cells (column, row) and, per stroke segment, the cells it added
/// (cells before it, cells after it).
type CellsBySegment = (Vec<(u64, i64)>, Vec<(usize, usize)>);

/// [`painted_cells`] (the same cells in the same order) with, per stroke
/// segment, the cells it added: (cells before it, cells after it).
fn painted_cells_by_segment(pts: &[StrokePoint], step: u64) -> CellsBySegment {
    let mut seen: FastSet<(u64, i64)> = FastSet::default();
    let mut out = Vec::new();
    let mut seg = Vec::with_capacity(pts.len().saturating_sub(1));
    let mut add = |c: u64, r: i64, out: &mut Vec<(u64, i64)>| {
        if seen.insert((c, r)) {
            out.push((c, r));
        }
    };
    let cell = |q: &StrokePoint| (q.tick / step, q.pitch.floor() as i64);
    if let Some(q) = pts.first() {
        let (c, r) = cell(q);
        add(c, r, &mut out);
    }
    for w in pts.windows(2) {
        let before = out.len();
        let (c0, r0) = cell(&w[0]);
        let (c1, r1) = cell(&w[1]);
        let n = (c0.abs_diff(c1) + r0.abs_diff(r1)).clamp(1, 100_000);
        for k in 1..=n {
            let f = k as f64 / n as f64;
            let c = (c0 as f64 + (c1 as f64 - c0 as f64) * f).round() as u64;
            let r = (r0 as f64 + (r1 as f64 - r0 as f64) * f).round() as i64;
            add(c, r, &mut out);
        }
        seg.push((before, out.len()));
    }
    (out, seg)
}

/// n04: every cell's pattern tile (the width × the rows the list fills,
/// tiled from tick 0 and key 0, row by row from the bottom) — its painted
/// cells, each once.
fn tiles(foot: &[Cell], pat: &[bool], w: usize) -> Vec<Cell> {
    let h = pat.len().div_ceil(w).max(1) as i64;
    let w64 = w as u64;
    let mut done: FastSet<(u64, i64)> = FastSet::default();
    let mut seen: FastSet<(u64, i64)> = FastSet::default();
    let mut out = Vec::new();
    for c in foot {
        for x in c.col..c.col + c.width {
            let (ox, oy) = (x / w64 * w64, c.row - c.row.rem_euclid(h));
            if !done.insert((ox, oy)) {
                continue;
            }
            for ty in 0..h {
                for tx in 0..w {
                    if !pat.get(ty as usize * w + tx).copied().unwrap_or(false) {
                        continue;
                    }
                    let cell = (ox + tx as u64, oy + ty);
                    if seen.insert(cell) {
                        out.push(Cell {
                            col: cell.0,
                            row: cell.1,
                            unit: c.unit,
                            width: 1,
                        });
                    }
                }
            }
        }
    }
    out
}

/// The value of an `m` × `m` Bayer matrix (m a power of two) at column `x`,
/// row `y`: 0 … m² − 1 (`[[0, 2], [3, 1]]` for m = 2, row by row).
fn bayer(m: u64, x: u64, y: u64) -> u64 {
    const B2: [[u64; 2]; 2] = [[0, 2], [3, 1]];
    let (mut x, mut y, mut n, mut v) = (x % m, y % m, m, 0u64);
    while n > 1 {
        v = v * 4 + B2[(y & 1) as usize][(x & 1) as usize];
        x >>= 1;
        y >>= 1;
        n >>= 1;
    }
    v
}

/// n02 whole rows: each region's cells (one unit) merged into runs of
/// adjacent columns per row, by row then column.
fn whole_rows(mut foot: Vec<Cell>) -> Vec<Cell> {
    foot.sort_by_key(|c| (c.unit, c.row, c.col));
    let mut out: Vec<Cell> = Vec::with_capacity(foot.len());
    for c in foot {
        if let Some(l) = out.last_mut()
            && l.unit == c.unit
            && l.row == c.row
            && l.col + l.width == c.col
        {
            l.width += 1;
            continue;
        }
        out.push(c);
    }
    out
}

/// n07: a stamp note's offsets from the stamp's first note (ticks, rows)
/// and its length, turned by `deg` (counter-clockwise, a column as wide as
/// a row is high) and scaled by `size`.
fn turn(m: &TNote, m0: &TNote, step: u64, deg: f64, size: f64) -> (i128, i64, i128) {
    let x = (m.start as f64 - m0.start as f64) / step as f64;
    let y = f64::from(m.pitch) - f64::from(m0.pitch);
    let (s, c) = deg.to_radians().sin_cos();
    let (x2, y2) = (size * (x * c - y * s), size * (x * s + y * c));
    (
        (x2 * step as f64).round() as i128,
        y2.round() as i64,
        ((m.dur as f64 * size).round() as i128).max(1),
    )
}

/// The covered cells by row: (column, unit) by column, each cell once (the
/// first unit that covers it).
struct Cover {
    by: FastMap<i64, Vec<(u64, usize)>>,
}

impl Cover {
    fn of(cells: impl Iterator<Item = (u64, i64, usize)>) -> Cover {
        let mut by: FastMap<i64, Vec<(u64, usize)>> = FastMap::default();
        for (c, r, u) in cells {
            by.entry(r).or_default().push((c, u));
        }
        for v in by.values_mut() {
            v.sort_by_key(|x| x.0);
            v.dedup_by_key(|x| x.0);
        }
        Cover { by }
    }

    /// The unit of the first covered cell note `n` overlaps (on its key,
    /// in the columns from its start to its last tick).
    fn unit(&self, n: &TNote, step: u64) -> Option<usize> {
        let v = self.by.get(&i64::from(n.pitch))?;
        let (c0, c1) = (n.start / step, (n.end().max(n.start + 1) - 1) / step);
        let k = v.partition_point(|x| x.0 < c0);
        v.get(k).filter(|x| x.0 <= c1).map(|x| x.1)
    }
}

/// n05: the stroke in cell space (x = tick / step, y = pitch) with its
/// segments bucketed by the radius, to find each note's closest segment.
struct Smudge {
    xy: Vec<(f64, f64)>,
    buckets: FastMap<(i64, i64), Vec<usize>>,
    radius: f64,
    step: u64,
}

impl Smudge {
    fn new(pts: &[StrokePoint], step: u64, radius: i64) -> Smudge {
        let xy: Vec<(f64, f64)> = pts
            .iter()
            .map(|q| (q.tick as f64 / step as f64, q.pitch))
            .collect();
        let b = radius.max(1) as f64;
        let mut buckets: FastMap<(i64, i64), Vec<usize>> = FastMap::default();
        // Each segment in the buckets around its samples (half a bucket
        // apart): a point within the radius of a segment is within 1.25
        // buckets of a sample, so in one of the 5 × 5 around it.
        for (wi, w) in xy.windows(2).enumerate() {
            let (a, z) = (w[0], w[1]);
            let len = (z.0 - a.0).hypot(z.1 - a.1);
            let n = ((len / (b / 2.0)).ceil() as usize).clamp(1, 1 << 20);
            let mut last = None;
            for i in 0..=n {
                let t = i as f64 / n as f64;
                let key = (
                    ((a.0 + (z.0 - a.0) * t) / b).floor() as i64,
                    ((a.1 + (z.1 - a.1) * t) / b).floor() as i64,
                );
                if last == Some(key) {
                    continue;
                }
                last = Some(key);
                for dx in -2..=2 {
                    for dy in -2..=2 {
                        let v = buckets.entry((key.0 + dx, key.1 + dy)).or_default();
                        if v.last() != Some(&wi) {
                            v.push(wi);
                        }
                    }
                }
            }
        }
        for v in buckets.values_mut() {
            v.sort_unstable();
            v.dedup();
        }
        Smudge {
            xy,
            buckets,
            radius: b,
            step,
        }
    }

    /// How far note `n` is dragged (ticks, rows): along its closest
    /// segment's direction (the later segment on a tie), radius × strength
    /// × (1 − distance / radius) cells, `strength` (%) taken at (segment,
    /// place along it); None beyond the radius or without a direction.
    fn shift(&self, n: &TNote, strength: impl Fn((usize, f64)) -> f64) -> Option<(i128, i64)> {
        let b = self.radius;
        let p = (n.start as f64 / self.step as f64, f64::from(n.pitch) + 0.5);
        let cands = self
            .buckets
            .get(&((p.0 / b).floor() as i64, (p.1 / b).floor() as i64))?;
        let mut best: Option<(f64, usize, f64)> = None;
        for &wi in cands {
            let (a, z) = (self.xy[wi], self.xy[wi + 1]);
            let (dx, dy) = (z.0 - a.0, z.1 - a.1);
            let l2 = dx * dx + dy * dy;
            if l2 <= 0.0 {
                continue;
            }
            let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0);
            let d = (p.0 - (a.0 + dx * t)).hypot(p.1 - (a.1 + dy * t));
            if best.is_none_or(|x| d <= x.0) {
                best = Some((d, wi, t));
            }
        }
        let (d, wi, t) = best?;
        if d > b {
            return None;
        }
        let (a, z) = (self.xy[wi], self.xy[wi + 1]);
        let (dx, dy) = (z.0 - a.0, z.1 - a.1);
        let l = dx.hypot(dy);
        let k = b * (strength((wi, t)) / 100.0).clamp(0.0, 1.0) * (1.0 - d / b);
        Some((
            (dx / l * k * self.step as f64).round() as i128,
            (dy / l * k).round() as i64,
        ))
    }
}

/// The Note Brush's source preparation (V09-S39 step 4): with a fill (n02)
/// or an editing function on, the active track's unselected notes around
/// the stroke as context (as far as the footprint, the smudge and the fill
/// can reach); with "every visible track" (n01, n06, n10) each Onion Skin
/// track's notes there, its track a destination so the commit can remove
/// or rewrite them. Without a context (no stroke) nothing is read.
pub fn paint_prepare(
    doc: &crate::model::PlumDocument,
    p: &Params,
    src: &mut Source,
    limit: usize,
    ctx: Option<&Ctx>,
) -> Result<(), TransformError> {
    let new = BrushNew::from(p);
    if !(new.edits() || new.fill.is_some()) {
        return Ok(());
    }
    let Some(ctx) = ctx else {
        return Ok(());
    };
    let Some(st) = ctx.stroke.clone().filter(|s| !s.points.is_empty()) else {
        return Ok(());
    };
    let Some(active) = src.tracks.first().cloned() else {
        return Ok(());
    };
    let step = note_value(p.choice("step"), p.ticks("step.ticks"), ctx).max(1);
    let swept = p.flag(super::super::functions::SWEEP);
    // How many cells beyond the stroke the brush reaches.
    let mut reach: u64 = 0;
    if let Some((r, _)) = new.soft {
        reach += if swept { 48 } else { r as u64 };
    }
    if let Some((_, w)) = &new.pattern {
        reach += *w as u64;
    }
    if let Some((_, r)) = new.smudge {
        reach = reach.max(r as u64 + 1);
    }
    if let Some((_, most, _)) = new.fill {
        reach = reach.max(most.saturating_add(1));
    }
    let lo = st.points.iter().map(|q| q.tick).min().unwrap_or(0) / step;
    let hi = st.points.iter().map(|q| q.tick).max().unwrap_or(0) / step;
    let w0 = lo.saturating_sub(reach).saturating_mul(step);
    let w1 = hi
        .saturating_add(1)
        .saturating_add(reach)
        .saturating_mul(step);
    if src.context.is_empty() {
        let s = std::mem::replace(src, Source::from_notes(Vec::new(), Vec::new()));
        *src = s.with_context_sounding(doc, w0, w1);
    }
    let visible = matches!(new.erase, Some((0, _)))
        || matches!(new.recolour, Some((_, true)))
        || matches!(new.length, Some((_, true)));
    let mut aux: Vec<Vec<TNote>> = Vec::new();
    if visible {
        let ids: Vec<crate::model::TrackId> = doc.tracks().iter().map(|t| t.id()).collect();
        for i in doc.project().onion.sources(&ids, active.index) {
            if src.tracks.iter().any(|t| t.index == i) {
                continue;
            }
            // B2-F16: the visible track as the song shows it, each note in
            // the slot of its own clip (erased / recoloured there).
            aux.push(src.placed_window_slots(doc, i, (w0, w1), limit, ctx.shift)?);
        }
    }
    src.aux = Arc::new(aux);
    Ok(())
}

#[cfg(test)]
mod paint_tests {
    use super::*;

    fn pt(tick: u64, pitch: f64) -> StrokePoint {
        StrokePoint {
            tick,
            pitch,
            t: 0.0,
        }
    }

    #[test]
    fn painted_cells_by_segment_finds_the_v08_cells_and_the_segments_that_added_them() {
        let pts: Vec<StrokePoint> = (0..40u64)
            .map(|k| {
                let tick = if k < 20 {
                    k * 130
                } else {
                    2600 - (k - 20) * 70
                };
                pt(tick, 60.0 + 7.0 * (k as f64 / 5.0).sin())
            })
            .collect();
        for step in [1u64, 60, 240, 1000] {
            let (cells, seg) = painted_cells_by_segment(&pts, step);
            assert_eq!(cells, painted_cells(&pts, step));
            assert_eq!(seg.len(), pts.len() - 1);
            let mut prev = 1;
            for &(a, b) in &seg {
                assert_eq!(a, prev);
                assert!(b >= a);
                prev = b;
            }
            assert_eq!(prev, cells.len());
        }
    }

    #[test]
    fn bayer_matrices_hold_every_threshold_once() {
        assert_eq!(
            (0..2)
                .flat_map(|y| (0..2).map(move |x| bayer(2, x, y)))
                .collect::<Vec<_>>(),
            vec![0, 2, 3, 1]
        );
        for m in [2u64, 4, 8] {
            let mut v: Vec<u64> = (0..m)
                .flat_map(|y| (0..m).map(move |x| bayer(m, x, y)))
                .collect();
            v.sort_unstable();
            assert_eq!(v, (0..m * m).collect::<Vec<_>>());
        }
        // 4 × 4: 4 · B2(x mod 2, y mod 2) + B2(x div 2, y div 2).
        assert_eq!(bayer(4, 1, 0), 8);
        assert_eq!(bayer(4, 2, 0), 2);
        assert_eq!(bayer(4, 3, 3), 4 + 1);
    }

    #[test]
    fn a_turn_by_zero_at_size_one_changes_nothing_and_a_quarter_turn_swaps_the_axes() {
        let m0 = TNote {
            start: 960,
            pitch: 60,
            dur: 240,
            ..TNote::default()
        };
        let m = TNote {
            start: 1440,
            pitch: 63,
            dur: 120,
            ..TNote::default()
        };
        assert_eq!(turn(&m, &m0, 240, 0.0, 1.0), (480, 3, 120));
        // (2 cells, 3 rows) turned 90° → (−3 cells, 2 rows); twice as big.
        assert_eq!(turn(&m, &m0, 240, 90.0, 2.0), (-1440, 4, 240));
    }
}
