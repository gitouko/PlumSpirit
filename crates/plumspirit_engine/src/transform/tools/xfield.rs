//! Experimental fields: Pulse Field, Particle Burst and Flow Field.
//! Deterministic maps of the selection through the shared geometry engine.

use super::super::geom::{Geom, GeomBox};
use super::super::param::*;
use super::super::registry::{BuildCtx, FunctionSpec, ToolDef, ToolSpec};
use super::super::rng::{Rng, salt};
use super::super::theory::{KEY_NAMES, scale_names};
use super::super::{Growth, Kernel, Output, Source, SourceTrack, TNote, TransformError};
use super::geometry::sub_pos;
use super::xgeom::{copies_kernel, mid_unit, noise2, rot, sub_stepped, vel_scaled, xspec};
use super::{Contextual, NOTE_VALUES, boxed};
use std::f64::consts::PI;

// ================================================================ Pulse Field

pub const PULSE_FIELD: ToolDef = ToolDef {
    id: "pulse_field",
    spec: pulse_spec,
    build: pulse_build,
};

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const PULSE_FIELD_HELP: &[(&str, &str)] = &[
    (
        "amp",
        "How far the pulse pushes the notes out from (or pulls them in toward) the box's center.",
    ),
    ("cycles", "How many pulses happen across the selection."),
    (
        "a1",
        "Pulses in rings from the center outward instead of along time.",
    ),
    (
        "a2",
        "Pulses only in time: notes move earlier and later, never in pitch.",
    ),
    (
        "a3",
        "Pulses only in pitch: notes move up and down, never in time.",
    ),
    (
        "a4",
        "Uses a smooth sine wave for the pulse instead of a triangle.",
    ),
    (
        "a5",
        "Uses a square wave for the pulse: notes jump between fully out and fully in.",
    ),
    ("a6", "Uses a pulse shape you draw."),
    (
        "a6.curve",
        "One period of the pulse; 0.5 is rest, higher pushes out and lower pulls in.",
    ),
    (
        "a7",
        "Mixes several pulse layers with different cycle counts, like a polyrhythm.",
    ),
    (
        "a7.cycles",
        "How many cycles each layer has across the selection.",
    ),
    (
        "a8",
        "Gives each subchannel its own pulse phase, so the colors pulse in turn.",
    ),
    (
        "a8.phases",
        "The pulse phase of each subchannel position, in degrees.",
    ),
    (
        "a9",
        "Changes the pulse's strength across the selection with a curve.",
    ),
    (
        "a9.curve",
        "The pulse's strength over the selection's time (1 = the full amplitude).",
    ),
    (
        "a10",
        "Adds echo copies, each pulsing a little later and softer.",
    ),
    ("a10.count", "How many echo copies are added."),
    (
        "a10.phase",
        "How much later in the pulse each echo is, in degrees.",
    ),
    (
        "a10.decay",
        "Each echo's velocity as a share of the previous one's.",
    ),
    (
        "n01",
        "Ties the pulses to the song's beats or bars instead of the selection's length.",
    ),
    (
        "n01.unit",
        "Whether the cycles are counted per beat or per bar.",
    ),
    ("n01.cycles", "How many pulses happen per beat or bar."),
    (
        "n02",
        "Fires a pulse at each note start of a reference instead of pulsing evenly.",
    ),
    (
        "n02.ref",
        "Takes the note starts from a track or from a subchannel of the active track.",
    ),
    (
        "n02.num",
        "The track number (0 = the active track) or the subchannel position whose note starts fire the pulses.",
    ),
    (
        "n02.envelope",
        "How each pulse dies away over its decay (1 = fully pulsed).",
    ),
    ("n02.decay", "How long each fired pulse lasts, in ticks."),
    (
        "n03",
        "Lets the velocity or note length follow the pulse as well.",
    ),
    (
        "n03.target",
        "What follows the pulse: the velocity or the gate.",
    ),
    (
        "n03.depth",
        "How strongly the velocity or gate follows the pulse.",
    ),
    ("n05", "Rounds the pitch movement to whole steps."),
    (
        "n05.step",
        "The step the pitch movement is rounded to: a semitone, an octave or a degree of the scale.",
    ),
    ("n05.key", "The key of the scale, used with Scale Degree."),
    ("n05.scale", "The scale, used with Scale Degree."),
    ("n06", "Thins out notes while the pulse is pulled in."),
    (
        "n06.keep",
        "The share of notes kept when the pulse is pulled in furthest.",
    ),
    (
        "n07",
        "Gives time and pitch their own pulse sizes and lets pitch pulse out of step with time.",
    ),
    ("n07.x", "How far the pulse moves notes in time."),
    ("n07.y", "How far the pulse moves notes in pitch."),
    (
        "n07.offset",
        "How far the pitch pulse is out of step with the time pulse, in degrees.",
    ),
    (
        "n08",
        "Makes every pulse a little bigger (or smaller) than the last.",
    ),
    (
        "n08.growth",
        "How much each pulse grows over the previous one (negative = shrinks).",
    ),
    (
        "n09",
        "Makes the pulse a square wave whose on-time changes across the selection.",
    ),
    (
        "n09.start",
        "The share of each cycle the pulse is out, at the selection's start.",
    ),
    (
        "n09.end",
        "The share of each cycle the pulse is out, at the selection's end.",
    ),
    (
        "n10",
        "Keeps only the notes that fall in the pulse's on-time, like a strobe.",
    ),
    ("n10.duty", "The share of each cycle when notes are kept."),
    (
        "n10.phase",
        "Shifts where in the cycle the kept part starts.",
    ),
    ("n11", "Colors notes by their place in the pulse."),
    (
        "n11.bands",
        "How many color bands one pulse cycle is divided into.",
    ),
    (
        "n11.palette",
        "The subchannel position of each band (empty = band 1 gets the first subchannel, and so on).",
    ),
    (
        "n12",
        "Pulses the inside of a ring against the outside, so they move in opposite directions.",
    ),
    (
        "n12.split",
        "The ring's radius that divides inside from outside, as a share of the box.",
    ),
    (
        "n12.phase",
        "How far the inside's pulse is out of step with the outside's, in degrees (180 = opposite).",
    ),
];

fn pulse_spec() -> ToolSpec {
    let mut s = xspec(
        "pulse_field",
        "Pulse Field",
        "Expands and contracts the art periodically around the center of its box: each note is pushed out or pulled in by a triangle pulse that cycles across the selection.",
        vec![
            float("amp", "Amplitude", 0.0, 200.0, 25.0, "%"),
            float("cycles", "Cycles across the selection", 0.25, 64.0, 4.0, ""),
            adv_switch(1, "Radial pulse (rings from the center)"),
            adv_switch(2, "X pulse (time only)"),
            adv_switch(3, "Y pulse (pitch only)"),
            adv_switch(4, "Sine pulse"),
            adv_switch(5, "Square pulse"),
            adv_switch(6, "Custom pulse curve"),
            curve(
                "a6.curve",
                "One period (0.5 = rest)",
                Curve::smooth(vec![(0.0, 0.5), (0.1, 1.0), (0.3, 0.4), (1.0, 0.5)]),
            )
            .overshoot()
            .adv(6),
            adv_switch(7, "Polyrhythmic pulse layers"),
            list("a7.cycles", "Cycles of each layer", 1, 256, 8, &[3, 4]).adv(7),
            adv_switch(8, "Phase by subchannel"),
            list(
                "a8.phases",
                "Phase per subchannel position (°)",
                -360,
                360,
                4096,
                &[0, 120, 240],
            )
            .adv(8),
            adv_switch(9, "Amplitude envelope"),
            curve(
                "a9.curve",
                "Amplitude across the selection",
                Curve::line(0.0, 1.0),
            )
            .overshoot()
            .adv(9),
            adv_switch(10, "Pulse echoes"),
            int("a10.count", "Echoes", 1, 8, 2).adv(10),
            float("a10.phase", "Phase step per echo", -180.0, 180.0, 90.0, "°").adv(10),
            float("a10.decay", "Velocity per echo", 5.0, 100.0, 60.0, "%").adv(10),
            // V09-S41: functions declared by stable id (TOOL_CATALOG n01 …
            // n12, n04 retired; the v0.9 kernel is `pulse.rs`).
            flag("n01", "Tempo-locked cycles", false),
            choice("n01.unit", "Unit", super::pulse::UNITS, 0),
            float("n01.cycles", "Cycles per unit", 0.125, 16.0, 1.0, ""),
            flag("n02", "Onset trigger", false),
            choice("n02.ref", "Reference", super::pulse::REFERENCES, 0),
            int(
                "n02.num",
                "Reference number (track number, 0 = the active track; or subchannel position)",
                0,
                65535,
                0,
            ),
            curve(
                "n02.envelope",
                "Envelope over one decay (1 = fully pulsed)",
                Curve::line(1.0, 0.0),
            )
            .overshoot(),
            ticks("n02.decay", "Decay", 1, 1 << 24, 480),
            flag("n03", "Pulse dynamics", false),
            choice("n03.target", "Target", super::pulse::TARGETS, 0),
            float("n03.depth", "Depth", 0.0, 100.0, 50.0, "%"),
            flag("n05", "Quantized pulse", false),
            choice("n05.step", "Step", super::pulse::STEPS, 0),
            choice("n05.key", "Key", &KEY_NAMES, 0),
            choice("n05.scale", "Scale", scale_names(), 0),
            flag("n06", "Pulse thinning", false),
            float("n06.keep", "Keep at the smallest", 0.0, 100.0, 30.0, "%"),
            flag("n07", "Elliptic pulse", false),
            float("n07.x", "Time amplitude", 0.0, 200.0, 25.0, "%"),
            float("n07.y", "Pitch amplitude", 0.0, 200.0, 50.0, "%"),
            float("n07.offset", "Pitch phase offset", 0.0, 360.0, 90.0, "°"),
            flag("n08", "Ratchet growth", false),
            float("n08.growth", "Growth per cycle", -50.0, 50.0, 10.0, "%"),
            flag("n09", "Pulse width", false),
            float("n09.start", "Duty at the start", 1.0, 99.0, 20.0, "%"),
            float("n09.end", "Duty at the end", 1.0, 99.0, 80.0, "%"),
            flag("n10", "Strobe gate", false),
            float("n10.duty", "Duty", 1.0, 99.0, 50.0, "%"),
            float("n10.phase", "Phase", 0.0, 100.0, 0.0, "%"),
            flag("n11", "Phase colors", false),
            int("n11.bands", "Bands", 2, 64, 4),
            list(
                "n11.palette",
                "Palette (subchannel positions; empty = the band's)",
                0,
                4095,
                64,
                &[],
            ),
            flag("n12", "Counter-phase rings", false),
            float("n12.split", "Split radius", 1.0, 100.0, 40.0, "%"),
            float("n12.phase", "Counter phase", 0.0, 360.0, 180.0, "°"),
        ],
        [
            "Radial pulse",
            "X pulse",
            "Y pulse",
            "Sine pulse",
            "Square pulse",
            "Custom pulse curve",
            "Polyrhythmic layers",
            "Phase by subchannel",
            "Amplitude envelope",
            "Pulse echoes",
        ],
        true,
    );
    s.functions = [
        ("n01", "Tempo-locked cycles", 110),
        ("n02", "Onset trigger", 120),
        ("n03", "Pulse dynamics", 130),
        ("n12", "Counter-phase rings", 140),
        ("n05", "Quantized pulse", 150),
        ("n06", "Pulse thinning", 160),
        ("n07", "Elliptic pulse", 170),
        ("n08", "Ratchet growth", 180),
        ("n09", "Pulse width", 190),
        ("n10", "Strobe gate", 200),
        ("n11", "Phase colors", 520),
    ]
    .into_iter()
    .map(|(id, name, order)| FunctionSpec { id, name, order })
    .collect();
    with_help(&mut s.params, PULSE_FIELD_HELP);
    s
}

fn pulse_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    // V09-S41: the new functions and Control Sweeps (per note) run in the
    // v0.9 kernel; with every one off this v0.8 kernel runs unchanged.
    if super::pulse::NEW.iter().any(|k| p.flag(k))
        || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty())
    {
        return super::pulse::pulse9_build(p, b);
    }
    let g = Geom::from_params(p, b.stats);
    let amp = p.float("amp") / 100.0;
    let radial = p.adv(1);
    let (xo, yo) = (p.adv(2), p.adv(3));
    let (ax, ay) = if xo == yo { (true, true) } else { (xo, yo) };
    let shape = if p.adv(6) {
        3
    } else if p.adv(5) {
        2
    } else if p.adv(4) {
        1
    } else {
        0
    };
    let custom = p.curve("a6.curve");
    let layers: Vec<f64> = if p.adv(7) {
        p.list("a7.cycles").iter().map(|&c| c as f64).collect()
    } else {
        vec![p.float("cycles")]
    };
    let phases: Vec<f64> = if p.adv(8) {
        p.list("a8.phases")
            .iter()
            .map(|&d| d as f64 * PI / 180.0)
            .collect()
    } else {
        Vec::new()
    };
    let env = p.adv(9).then(|| p.curve("a9.curve"));
    let echoes = p.adv(10).then(|| {
        (
            p.int("a10.count").clamp(1, 8) as usize,
            p.float("a10.phase") * PI / 180.0,
            p.float("a10.decay") / 100.0,
        )
    });
    let wave = move |x: f64| -> f64 {
        let t = crate::transform::geom::rem1(x / (2.0 * PI));
        match shape {
            1 => x.sin(),
            2 => {
                if t < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            3 => (custom.eval(t) - 0.5) * 2.0,
            _ => {
                if t < 0.25 {
                    4.0 * t
                } else if t < 0.75 {
                    2.0 - 4.0 * t
                } else {
                    4.0 * t - 4.0
                }
            }
        }
    };
    let copies = 1 + echoes.map_or(0, |e| e.0);
    let tracks = b.tracks.to_vec();
    let tr = tracks.clone();
    copies_kernel(
        g,
        tracks,
        copies,
        move |n, k| {
            let mut t = *n;
            if let Some((_, _, d)) = echoes {
                t.vel = vel_scaled(n, d.powi(k as i32));
            }
            Some(t)
        },
        move |n, k, u, v| {
            let (mu, mv) = mid_unit(&g, n);
            let pos = if radial {
                ((mu - 0.5).powi(2) + (mv - 0.5).powi(2)).sqrt() / std::f64::consts::FRAC_1_SQRT_2
            } else {
                mu
            };
            let mut ph = phases
                .get(sub_pos(&tr, n) % phases.len().max(1))
                .copied()
                .filter(|_| !phases.is_empty())
                .unwrap_or(0.0);
            if let Some((_, step, _)) = echoes {
                ph += step * k as f64;
            }
            let w: f64 = layers
                .iter()
                .map(|c| wave(2.0 * PI * c * pos + ph))
                .sum::<f64>()
                / layers.len().max(1) as f64;
            let e = env.as_ref().map_or(1.0, |c| c.eval(mu.clamp(0.0, 1.0)));
            let s = 1.0 + amp * w * e;
            let x = if ax { 0.5 + (u - 0.5) * s } else { u };
            let y = if ay { 0.5 + (v - 0.5) * s } else { v };
            (x, y)
        },
        // V09-0143: the echo (0 = the note itself).
        |k| u32::try_from(k).unwrap_or(u32::MAX),
    )
}

// ============================================================= Particle Burst

pub const PARTICLE_BURST: ToolDef = ToolDef {
    id: "particle_burst",
    spec: particle_spec,
    build: particle_build,
};

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const PARTICLE_BURST_HELP: &[(&str, &str)] = &[
    (
        "dist",
        "How far the fragments fly from their note, as a share of the selection's box.",
    ),
    ("frag", "How long each fragment note is, in ticks."),
    ("keep", "Keeps the original notes the fragments burst from."),
    (
        "a1",
        "Sends the fragments out in all directions instead of a forward cone.",
    ),
    (
        "a2",
        "Sends the fragments out in a cone pointing in a direction you choose.",
    ),
    (
        "a2.dir",
        "The cone's direction in degrees (0 = later in time, 90 = up in pitch).",
    ),
    ("a2.width", "How wide the cone is, in degrees."),
    (
        "a3",
        "Lets loud notes burst into more fragments than soft ones.",
    ),
    (
        "a3.max",
        "How many fragments a note at velocity 127 bursts into.",
    ),
    ("a4", "Makes the fragments quieter the farther they fly."),
    (
        "a4.decay",
        "The velocity of the farthest fragments, as a share of the note's.",
    ),
    ("a5", "Makes the fragments shorter the farther they fly."),
    (
        "a5.decay",
        "The length of the farthest fragments, as a share of the fragment length.",
    ),
    (
        "a6",
        "Makes the fragments fall (or rise) as they fly, like gravity.",
    ),
    (
        "a6.g",
        "How far the fragments fall, as a share of the box (negative = they rise).",
    ),
    (
        "a7",
        "Slows the fragments with air drag, so they land closer.",
    ),
    ("a7.k", "How strong the drag is."),
    (
        "a8",
        "Scatters the fragments' directions and speeds at random (seeded).",
    ),
    (
        "a8.angle",
        "The largest random change of a fragment's direction, in degrees.",
    ),
    (
        "a9",
        "Gives each fragment of a burst the next subchannel (color).",
    ),
    (
        "a10",
        "Lets every landed fragment burst again into smaller fragments.",
    ),
    (
        "a10.count",
        "How many smaller fragments each landed fragment bursts into.",
    ),
    ("n01", "Lets only the notes on chosen grid positions burst."),
    (
        "n01.grid",
        "The grid whose positions decide which notes burst.",
    ),
    (
        "n01.ticks",
        "The grid length in ticks, used when Grid is set to Custom Ticks.",
    ),
    (
        "n01.pattern",
        "One digit per grid position in turn, repeated: 1 lets the notes there burst, 0 does not.",
    ),
    (
        "n02",
        "Keeps each note bursting for a while, like a fountain, instead of once.",
    ),
    (
        "n02.duration",
        "How long each note keeps bursting, in ticks.",
    ),
    ("n02.rate", "How many bursts a note makes per beat."),
    (
        "n03",
        "Bounces the fragments off a floor key instead of letting them fall through.",
    ),
    ("n03.floor", "The key the fragments bounce off."),
    (
        "n03.restitution",
        "How high a fragment bounces, as a share of its fall.",
    ),
    ("n03.bounces", "How many times a fragment may bounce."),
    ("n04", "Leaves a trail of notes behind every fragment."),
    ("n04.length", "How long each trail is, in ticks."),
    ("n04.density", "How many notes make up each trail."),
    (
        "n05",
        "Pulls the fragments' landings toward the notes of a track or subchannel.",
    ),
    (
        "n05.target",
        "Whose notes attract the fragments: a track or a subchannel of the active track.",
    ),
    (
        "n05.num",
        "The track number (0 = the active track) or the subchannel position whose notes attract the fragments.",
    ),
    (
        "n05.strength",
        "How strongly the landings are pulled toward the nearest target note.",
    ),
    ("n06", "Blows the fragments sideways with a wind."),
    (
        "n06.time",
        "How far the wind pushes the fragments in time, as a share of the box (negative = earlier).",
    ),
    (
        "n06.pitch",
        "How far the wind pushes the fragments in pitch, as a share of the box (negative = down).",
    ),
    (
        "n07",
        "Colors the fragments by their age, speed or direction.",
    ),
    (
        "n07.palette",
        "The subchannel positions used, in order (empty = four bands of the track's first subchannels).",
    ),
    (
        "n07.by",
        "What picks a fragment's color: how far it flew (age), how fast it flew, or its direction.",
    ),
    ("n08", "Moves every fragment onto the notes of a scale."),
    ("n08.key", "The key of the scale the fragments land on."),
    ("n08.scale", "The scale the fragments land on."),
    (
        "n09",
        "Reverses the burst: fragments start on a ring and fly in to meet the note.",
    ),
    (
        "n09.radius",
        "How far out the ring the fragments start from is, as a share of the box.",
    ),
    (
        "n09.arrival",
        "Whether the fragments arrive at the note's start or just before it.",
    ),
    (
        "n10",
        "Arranges the burst's fragments in a shape around the note.",
    ),
    (
        "n10.shape",
        "The shape the fragments form: a ring, a star, a heart or a spiral.",
    ),
    ("n10.size", "How big the shape is, as a share of the box."),
    ("n11", "Adds swirling turbulence to the fragments' flight."),
    (
        "n11.amount",
        "How strongly the turbulence moves the fragments.",
    ),
    (
        "n11.scale",
        "How big the turbulence's swirls are, as a share of the box.",
    ),
];

fn particle_spec() -> ToolSpec {
    let mut s = xspec(
        "particle_burst",
        "Particle Burst",
        "Every selected note emits six short fragments that fly forward in a 60° cone; each lands Distance away (of the selection box).",
        vec![
            float("dist", "Distance", 0.0, 100.0, 12.0, "%"),
            ticks("frag", "Fragment length (ticks)", 1, 1 << 24, 60),
            flag("keep", "Keep the anchors", true),
            seed("seed"),
            adv_switch(1, "Radial emission (all directions)"),
            adv_switch(2, "Directional cone"),
            float("a2.dir", "Direction", -180.0, 180.0, 90.0, "°").adv(2),
            float("a2.width", "Cone width", 1.0, 360.0, 30.0, "°").adv(2),
            adv_switch(3, "Particle count from velocity"),
            int("a3.max", "Particles at velocity 127", 1, 256, 16).adv(3),
            adv_switch(4, "Velocity decay with distance"),
            float("a4.decay", "Velocity at the end", 1.0, 100.0, 30.0, "%").adv(4),
            adv_switch(5, "Gate decay with distance"),
            float("a5.decay", "Length at the end", 1.0, 100.0, 30.0, "%").adv(5),
            adv_switch(6, "Gravity"),
            float("a6.g", "Fall (of the box)", -100.0, 100.0, 30.0, "%").adv(6),
            adv_switch(7, "Drag"),
            float("a7.k", "Drag", 0.1, 10.0, 2.0, "").adv(7),
            adv_switch(8, "Seeded scatter"),
            float("a8.angle", "Angle jitter", 0.0, 180.0, 25.0, "°").adv(8),
            adv_switch(9, "Subchannel cycling"),
            adv_switch(10, "Recursive secondary bursts"),
            int("a10.count", "Sub-particles", 1, 16, 3).adv(10),
            // V09-S41: functions declared by stable id (TOOL_CATALOG n01 …
            // n11; the v0.9 kernel is `particle.rs`).
            flag("n01", "Metric emitters", false),
            choice("n01.grid", "Grid", NOTE_VALUES, 2),
            ticks("n01.ticks", "Custom grid (ticks)", 1, 1 << 24, 240),
            list(
                "n01.pattern",
                "Emit at position (1 / 0)",
                0,
                1,
                64,
                &[1, 0, 0, 0],
            ),
            flag("n02", "Emission duration", false),
            ticks("n02.duration", "Emission duration", 0, 1 << 24, 480),
            float("n02.rate", "Rate (per beat)", 0.25, 256.0, 4.0, ""),
            flag("n03", "Floor bounce", false),
            pitch("n03.floor", "Floor", 48),
            float("n03.restitution", "Restitution", 0.0, 100.0, 50.0, "%"),
            int("n03.bounces", "Bounces", 0, 8, 2),
            flag("n04", "Particle trails", false),
            ticks("n04.length", "Trail length", 0, 1 << 24, 240),
            int("n04.density", "Trail density", 1, 64, 4),
            flag("n05", "Target attraction", false),
            choice("n05.target", "Target", super::particle::TARGETS, 0),
            int(
                "n05.num",
                "Target number (track number, 0 = the active track; or subchannel position)",
                0,
                65535,
                0,
            ),
            float("n05.strength", "Strength", 0.0, 100.0, 50.0, "%"),
            flag("n06", "Wind", false),
            float("n06.time", "Wind in time", -100.0, 100.0, 20.0, "%"),
            float("n06.pitch", "Wind in pitch", -100.0, 100.0, 0.0, "%"),
            flag("n07", "Age colors", false),
            list(
                "n07.palette",
                "Palette (subchannel positions; empty = four bands)",
                0,
                4095,
                64,
                &[0, 1, 2, 3],
            ),
            choice("n07.by", "By", super::particle::COLOUR_BY, 0),
            flag("n08", "Scale landing", false),
            choice("n08.key", "Key", &KEY_NAMES, 0),
            choice("n08.scale", "Scale", scale_names(), 0),
            flag("n09", "Implosion", false),
            float("n09.radius", "Ring radius", 1.0, 100.0, 12.0, "%"),
            choice("n09.arrival", "Arrival", super::particle::ARRIVAL, 0),
            flag("n10", "Shape burst", false),
            choice("n10.shape", "Shape", super::particle::SHAPES, 0),
            float("n10.size", "Size", 1.0, 100.0, 12.0, "%"),
            flag("n11", "Turbulence", false),
            float("n11.amount", "Turbulence", 0.0, 100.0, 40.0, "%"),
            float("n11.scale", "Scale", 1.0, 100.0, 20.0, "%"),
        ],
        [
            "Radial emission",
            "Directional cone",
            "Particle count",
            "Velocity decay",
            "Gate decay",
            "Gravity",
            "Drag",
            "Seeded scatter",
            "Subchannel cycling",
            "Recursive secondary bursts",
        ],
        false,
    );
    s.functions = [
        ("n01", "Metric emitters", 110),
        ("n02", "Emission duration", 120),
        ("n03", "Floor bounce", 130),
        ("n04", "Particle trails", 140),
        ("n05", "Target attraction", 150),
        ("n06", "Wind", 160),
        ("n07", "Age colors", 170),
        ("n08", "Scale landing", 180),
        ("n09", "Implosion", 190),
        ("n10", "Shape burst", 200),
        ("n11", "Turbulence", 210),
    ]
    .into_iter()
    .map(|(id, name, order)| FunctionSpec { id, name, order })
    .collect();
    with_help(&mut s.params, PARTICLE_BURST_HELP);
    s
}

fn particle_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    // V09-S41: the new functions and Control Sweeps (per note) run in the
    // v0.9 kernel; with every one off this v0.8 kernel runs unchanged.
    if super::particle::NEW.iter().any(|k| p.flag(k))
        || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty())
    {
        return super::particle::part9_build(p, b);
    }
    let dist = p.float("dist") / 100.0;
    let frag = p.ticks("frag").max(1) as f64;
    let keep = p.flag("keep");
    let seed = p.seed("seed");
    let (dir, width) = if p.adv(1) {
        (0.0, 2.0 * PI)
    } else if p.adv(2) {
        (
            p.float("a2.dir") * PI / 180.0,
            p.float("a2.width") * PI / 180.0,
        )
    } else {
        (0.0, PI / 3.0)
    };
    let full = p.adv(1);
    let count_max = if p.adv(3) {
        p.int("a3.max").clamp(1, 256) as usize
    } else {
        6
    };
    let by_vel = p.adv(3);
    let vdec = p.adv(4).then(|| p.float("a4.decay") / 100.0);
    let gdec = p.adv(5).then(|| p.float("a5.decay") / 100.0);
    let grav = p.adv(6).then(|| p.float("a6.g") / 100.0);
    let drag = p.adv(7).then(|| p.float("a7.k"));
    let jitter = p.adv(8).then(|| p.float("a8.angle") * PI / 180.0);
    let cycle = p.adv(9);
    let second = p.adv(10).then(|| p.int("a10.count").clamp(1, 16) as usize);
    let bx = GeomBox::of(b.stats);
    let tracks = b.tracks.to_vec();
    let per = 1 + count_max as u64 * (1 + second.unwrap_or(0) as u64);
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            // One particle from (u, v): angle index j of k, scale s → landing point.
            let fly = |n: &TNote, u: f64, v: f64, j: usize, k: usize, s: f64, salt2: u64| {
                let mut r = Rng::for_note(seed, salt::SHAPE ^ salt2 ^ (j as u64) << 8, n);
                let frac = if full {
                    j as f64 / k as f64
                } else if k == 1 {
                    0.5
                } else {
                    j as f64 / (k - 1) as f64
                };
                let mut a = dir - width / 2.0 + width * frac;
                let mut speed = 1.0;
                if let Some(jt) = jitter {
                    a += r.range_f64(-jt, jt);
                    speed = r.range_f64(0.5, 1.0);
                }
                let mut d = dist * speed * s;
                if let Some(kd) = drag {
                    d *= (1.0 - (-kd).exp()) / kd;
                }
                let (mut x, mut y) = (u + a.cos() * d, v + a.sin() * d);
                if let Some(gv) = grav {
                    y -= gv * speed * speed * s;
                }
                x = x.max(-1.0);
                y = y.clamp(-1.0, 2.0);
                (x, y, d / dist.max(1e-9))
            };
            let emit = |out: &mut Output, n: &TNote, x: f64, y: f64, t: f64, j: usize| -> bool {
                let (ax, ay) = bx.from_unit(x, y);
                let mut tpl = if cycle {
                    sub_stepped(&tracks, n, j as i64 + 1)
                } else {
                    *n
                };
                tpl.vel = vel_scaled(n, vdec.map_or(1.0, |e| 1.0 - (1.0 - e) * t.min(1.0)));
                let len = frag * gdec.map_or(1.0, |e| 1.0 - (1.0 - e) * t.min(1.0));
                out.emit(
                    ax.round() as i128,
                    len.round().max(1.0) as i128,
                    ay.floor() as i64,
                    i64::from(tpl.vel),
                    &tpl,
                )
            };
            for n in &src.notes {
                out.set_dim(0);
                if keep && !out.push(*n) {
                    return out.check();
                }
                let (u, v) = bx.to_unit(n.start as f64, f64::from(n.pitch) + 0.5);
                let k = if by_vel {
                    ((count_max as f64 * f64::from(n.vel) / 127.0).round() as usize).max(1)
                } else {
                    count_max
                };
                for j in 0..k {
                    // V09-0143: the particle's number in its burst.
                    out.set_dim(u32::try_from(j).unwrap_or(u32::MAX));
                    let (x, y, t) = fly(n, u, v, j, k, 1.0, 0);
                    if !emit(out, n, x, y, t, j) {
                        return out.check();
                    }
                    if let Some(m) = second {
                        for q in 0..m {
                            let (x2, y2, t2) = fly(n, x, y, q, m, 0.5, 0x5EC0 + j as u64);
                            if !emit(out, n, x2, y2, t + t2, j + q + 1) {
                                return out.check();
                            }
                        }
                    }
                }
                out.check()?;
            }
            Ok(())
        },
        growth: Growth::Expand(per),
        estimate: None,
    })
}

// ================================================================= Flow Field

pub const FLOW_FIELD: ToolDef = ToolDef {
    id: "flow_field",
    spec: flow_spec,
    build: flow_build,
};

const BLEND_FIELDS: &[&str] = &[
    "Uniform upwards",
    "Vortex",
    "Shear backwards",
    "Radial outwards",
];

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const FLOW_FIELD_HELP: &[(&str, &str)] = &[
    (
        "strength",
        "How far the flow carries the notes, as a share of the selection's box.",
    ),
    ("a1", "Adds a straight flow in one direction."),
    (
        "a1.angle",
        "The flow's direction in degrees (0 = later in time, 90 = up in pitch).",
    ),
    (
        "a2",
        "Adds a whirlpool flow that turns notes around a center.",
    ),
    (
        "a2.x",
        "Where the whirlpool's center lies in time, as a share of the box.",
    ),
    (
        "a2.y",
        "Where the whirlpool's center lies in pitch, as a share of the box.",
    ),
    (
        "a3",
        "Adds a smooth random flow (seeded) that swirls the notes.",
    ),
    ("a3.scale", "How big the swirls are, as a share of the box."),
    ("a4", "Adds a flow you paint as a grid of directions."),
    (
        "a4.angles",
        "The flow's direction in each grid cell in degrees, row by row from the bottom (a square grid).",
    ),
    (
        "a5",
        "Changes the flow's strength across the selection with a curve.",
    ),
    (
        "a5.curve",
        "The flow's strength over the selection's time (1 = full).",
    ),
    (
        "a6",
        "Moves the notes in several smaller steps, so they follow curved flows more closely.",
    ),
    ("a6.steps", "How many steps each note moves in."),
    (
        "a7",
        "Leaves a trail of notes along the path each note took.",
    ),
    ("a7.count", "How many notes make up each trail."),
    (
        "a8",
        "Brings notes that flow out of the box back in at the other side.",
    ),
    (
        "a9",
        "Turns the flow differently for each subchannel, so the colors flow apart.",
    ),
    (
        "a9.angles",
        "How far the flow is turned for each subchannel position, in degrees.",
    ),
    ("a10", "Blends a second kind of flow into the first."),
    (
        "a10.field",
        "The second flow: straight up, a whirlpool, a backward shear, or outward from the center.",
    ),
    (
        "n01",
        "Makes the flow follow the rises and falls of a reference melody.",
    ),
    (
        "n01.ref",
        "Takes the melody from a track or from a subchannel of the active track.",
    ),
    (
        "n01.num",
        "The track number (0 = the active track) or the subchannel position of the reference melody.",
    ),
    (
        "n01.smoothing",
        "How much the melody is smoothed before it shapes the flow, in ticks.",
    ),
    ("n02", "Makes the flow turn and drift as time goes on."),
    (
        "n02.turns",
        "How many times the flow turns around over the selection (negative = the other way).",
    ),
    (
        "n02.drift",
        "How far the flow pattern drifts over the selection.",
    ),
    ("n03", "Colors each note by where the flow brought it from."),
    (
        "n03.palette",
        "The subchannel positions used for the colors.",
    ),
    (
        "n03.steps",
        "How many steps the flow is traced back to find a note's origin.",
    ),
    (
        "n04",
        "Draws streamlines of the flow from a grid of starting points.",
    ),
    (
        "n04.spacing",
        "How far apart the streamlines start, as a share of the box.",
    ),
    (
        "n04.length",
        "How long each streamline is, as a share of the box.",
    ),
    ("n04.notes", "How many notes make up each streamline."),
    (
        "n05",
        "Makes the flow go around the notes of a track or subchannel, like water around stones.",
    ),
    (
        "n05.ref",
        "Takes the obstacles from a track or from a subchannel of the active track.",
    ),
    (
        "n05.num",
        "The track number (0 = the active track) or the subchannel position of the obstacles.",
    ),
    (
        "n05.radius",
        "How far around each obstacle note the flow is pushed aside, in rows.",
    ),
    (
        "n06",
        "Stretches notes along the flow, so fast flow makes long streaks.",
    ),
    (
        "n06.streak",
        "How much the notes are stretched along the flow.",
    ),
    ("n07", "Makes the flow symmetric."),
    (
        "n07.symmetry",
        "The symmetry: none, mirrored in time, mirrored in pitch, or rotational around the center.",
    ),
    (
        "n07.order",
        "How many times the flow repeats around the center, used with Rotational.",
    ),
    (
        "n08",
        "Adds layers of flow moving in opposite directions, with curls between them.",
    ),
    ("n08.layers", "How many layers of flow there are."),
    (
        "n08.curl",
        "How strongly the flow curls where the layers meet.",
    ),
    ("n09", "Fades the flow out away from the center."),
    (
        "n09.radius",
        "How far from the center the fade reaches, as a share of the box.",
    ),
    (
        "n09.fade",
        "The flow's strength from the center (left) to the fade radius (right).",
    ),
    ("n10", "Adds a flow along a path you draw."),
    (
        "n10.path",
        "The path's points as x, y pairs in thousandths of the box.",
    ),
    (
        "n10.width",
        "How wide the path's flow is, as a share of the box.",
    ),
    ("n11", "Thins notes where the flow piles them up."),
    ("n11.most", "The most notes one cell may keep."),
    ("n11.cell", "How many rows high each cell is."),
];

fn flow_spec() -> ToolSpec {
    let mut s = xspec(
        "flow_field",
        "Flow Field",
        "Moves the notes through a 2-D vector field (a meandering shear by default: rows above the middle drift later, rows below earlier, and the flow rises and falls along time).",
        vec![
            float("strength", "Strength (of the box)", 0.0, 200.0, 20.0, "%"),
            seed("seed"),
            adv_switch(1, "Uniform flow"),
            float("a1.angle", "Direction", -180.0, 180.0, 30.0, "°").adv(1),
            adv_switch(2, "Vortex flow"),
            float("a2.x", "Vortex X", -50.0, 150.0, 50.0, "%").adv(2),
            float("a2.y", "Vortex Y", -50.0, 150.0, 50.0, "%").adv(2),
            adv_switch(3, "Noise flow"),
            float("a3.scale", "Noise scale", 1.0, 100.0, 20.0, "%").adv(3),
            adv_switch(4, "Custom painted field"),
            list(
                "a4.angles",
                "Directions ° (rows of a square grid, bottom row first)",
                -360,
                360,
                64,
                &[0, 45, 90, 0, 0, 0, -45, -90, 180],
            )
            .adv(4),
            adv_switch(5, "Field strength envelope"),
            curve(
                "a5.curve",
                "Strength across the selection",
                Curve::line(0.0, 1.0),
            )
            .overshoot()
            .adv(5),
            adv_switch(6, "Integration steps"),
            int("a6.steps", "Steps", 2, 64, 8).adv(6),
            adv_switch(7, "Streamline trails"),
            int("a7.count", "Trail notes", 2, 32, 6).adv(7),
            adv_switch(8, "Boundary wrap"),
            adv_switch(9, "Subchannel-dependent fields"),
            list(
                "a9.angles",
                "Field turn per subchannel position (°)",
                -360,
                360,
                4096,
                &[0, 90, 180, 270],
            )
            .adv(9),
            adv_switch(10, "Blend two fields"),
            choice("a10.field", "Second field", BLEND_FIELDS, 1).adv(10),
            // V09-S41: functions declared by stable id (TOOL_CATALOG n01 …
            // n11; the v0.9 kernel is `flow.rs`).
            flag("n01", "Contour field", false),
            choice("n01.ref", "Reference", super::flow::REFERENCES, 0),
            int(
                "n01.num",
                "Reference number (track number, 0 = the active track; or subchannel position)",
                0,
                65535,
                0,
            ),
            ticks("n01.smoothing", "Smoothing", 0, 1 << 24, 960),
            flag("n02", "Evolving field", false),
            float("n02.turns", "Turns over the selection", -8.0, 8.0, 1.0, ""),
            float("n02.drift", "Drift", 0.0, 100.0, 20.0, "%"),
            flag("n03", "Color advection", false),
            list(
                "n03.palette",
                "Palette (subchannel positions)",
                0,
                4095,
                64,
                &[0, 1, 2, 3],
            ),
            int("n03.steps", "Steps", 1, 64, 8),
            flag("n04", "Grid streamlines", false),
            float("n04.spacing", "Seed spacing", 1.0, 100.0, 20.0, "%"),
            float("n04.length", "Line length", 1.0, 100.0, 30.0, "%"),
            int("n04.notes", "Notes per line", 2, 256, 12),
            flag("n05", "Obstacles", false),
            choice("n05.ref", "Obstacles", super::flow::REFERENCES, 0),
            int(
                "n05.num",
                "Reference number (track number, 0 = the active track; or subchannel position)",
                0,
                65535,
                0,
            ),
            int("n05.radius", "Radius (rows)", 1, 16, 4),
            flag("n06", "Streaks", false),
            float("n06.streak", "Streak", 0.0, 400.0, 100.0, "%"),
            flag("n07", "Field symmetry", false),
            choice("n07.symmetry", "Symmetry", super::flow::SYMMETRY, 1),
            int("n07.order", "Order", 2, 12, 4),
            flag("n08", "Shear layers", false),
            int("n08.layers", "Layers", 2, 16, 4),
            float("n08.curl", "Curl", 0.0, 100.0, 50.0, "%"),
            flag("n09", "Radial fade", false),
            float("n09.radius", "Fade radius", 1.0, 200.0, 70.0, "%"),
            curve("n09.fade", "Fade (center → radius)", Curve::line(1.0, 0.0)).overshoot(),
            flag("n10", "Path field", false),
            list(
                "n10.path",
                "Path ‰ (x, y per point)",
                0,
                1000,
                64,
                &[100, 200, 400, 700, 700, 300, 900, 800],
            ),
            float("n10.width", "Width", 1.0, 100.0, 15.0, "%"),
            flag("n11", "Convergence thinning", false),
            int("n11.most", "Most notes per cell", 1, 1000, 2),
            int("n11.cell", "Cell (rows)", 1, 16, 2),
        ],
        [
            "Uniform flow",
            "Vortex flow",
            "Noise flow",
            "Custom painted field",
            "Field strength",
            "Integration steps",
            "Streamline trails",
            "Boundary wrap",
            "Subchannel-dependent fields",
            "Blend two fields",
        ],
        true,
    );
    s.functions = [
        ("n01", "Contour field", 110),
        ("n02", "Evolving field", 120),
        ("n03", "Color advection", 130),
        ("n04", "Grid streamlines", 140),
        ("n05", "Obstacles", 150),
        ("n06", "Streaks", 160),
        ("n07", "Field symmetry", 170),
        ("n08", "Shear layers", 180),
        ("n09", "Radial fade", 190),
        ("n10", "Path field", 200),
        ("n11", "Convergence thinning", 700),
    ]
    .into_iter()
    .map(|(id, name, order)| FunctionSpec { id, name, order })
    .collect();
    with_help(&mut s.params, FLOW_FIELD_HELP);
    s
}

/// v0.8's field (shared with the v0.9 kernel `flow.rs`, V09-S41).
#[derive(Clone, Debug)]
pub(super) struct Flow {
    strength: f64,
    uniform: Option<f64>,
    vortex: Option<(f64, f64)>,
    noise: Option<(f64, u64)>,
    painted: Option<(usize, Vec<f64>)>,
    env: Option<Curve>,
    pub(super) steps: usize,
    pub(super) wrap: bool,
    turn: Vec<f64>,
    blend: Option<usize>,
    tracks: Vec<SourceTrack>,
}

impl Flow {
    fn preset(k: usize, u: f64, v: f64) -> (f64, f64) {
        match k {
            0 => (0.0, 1.0),
            1 => (-(v - 0.5) * 2.0, (u - 0.5) * 2.0),
            2 => (-(v - 0.5) * 2.0, 0.0),
            _ => (u - 0.5, v - 0.5),
        }
    }

    fn field(&self, u: f64, v: f64, t: f64, turn: f64) -> (f64, f64) {
        self.finish(self.sum(u, v), u, v, t, turn)
    }

    /// a01 – a04's fields summed at (u, v), and whether any is on.
    pub(super) fn sum(&self, u: f64, v: f64) -> (f64, f64, bool) {
        let (mut fx, mut fy) = (0.0, 0.0);
        let mut any = false;
        if let Some(a) = self.uniform {
            fx += a.cos();
            fy += a.sin();
            any = true;
        }
        if let Some((cx, cy)) = self.vortex {
            fx += -(v - cy) * 2.0;
            fy += (u - cx) * 2.0;
            any = true;
        }
        if let Some((sc, seed)) = self.noise {
            let a = PI * 2.0 * noise2(seed, 7, u / sc, v / sc);
            fx += a.cos();
            fy += a.sin();
            any = true;
        }
        if let Some((side, ang)) = &self.painted {
            let s = *side as f64;
            let (x, y) = (
                (u.clamp(0.0, 1.0) * (s - 1.0)),
                (v.clamp(0.0, 1.0) * (s - 1.0)),
            );
            let (i, j) = (
                (x.floor() as usize).min(side - 1),
                (y.floor() as usize).min(side - 1),
            );
            let (i2, j2) = ((i + 1).min(side - 1), (j + 1).min(side - 1));
            let (fx1, fy1) = (x - i as f64, y - j as f64);
            let at = |c: usize, r: usize| ang.get(r * side + c).copied().unwrap_or(0.0);
            let mut vx = 0.0;
            let mut vy = 0.0;
            for (c, r, w) in [
                (i, j, (1.0 - fx1) * (1.0 - fy1)),
                (i2, j, fx1 * (1.0 - fy1)),
                (i, j2, (1.0 - fx1) * fy1),
                (i2, j2, fx1 * fy1),
            ] {
                let a = at(c, r);
                vx += a.cos() * w;
                vy += a.sin() * w;
            }
            fx += vx;
            fy += vy;
            any = true;
        }
        (fx, fy, any)
    }

    /// The rest of v0.8's field: the meander when no field is on, a10's
    /// blend, a05's envelope at the note's time `t`, the turn, the strength.
    pub(super) fn finish(
        &self,
        (fx, fy, any): (f64, f64, bool),
        u: f64,
        v: f64,
        t: f64,
        turn: f64,
    ) -> (f64, f64) {
        let (mut fx, mut fy) = (fx, fy);
        if !any {
            // Meandering shear: rows drift in time by their height, and the
            // flow rises and falls along time.
            fx = (v - 0.5) * 2.0;
            fy = (2.0 * PI * u).sin() * 0.5;
        }
        if let Some(k) = self.blend {
            let (px, py) = Flow::preset(k, u, v);
            fx = fx * (1.0 - t) + px * t;
            fy = fy * (1.0 - t) + py * t;
        }
        let e = self.env.as_ref().map_or(1.0, |c| c.eval(t));
        let (fx, fy) = rot(fx, fy, turn);
        (fx * e * self.strength, fy * e * self.strength)
    }

    /// Integrates (u, v) for `frac` of the flow time (midpoint steps).
    /// a09's turn of note `n` (its subchannel's).
    pub(super) fn turn_of(&self, n: &TNote) -> f64 {
        if self.turn.is_empty() {
            0.0
        } else {
            self.turn[sub_pos(&self.tracks, n) % self.turn.len()]
        }
    }

    fn advect(&self, n: &TNote, u: f64, v: f64, t: f64, frac: f64) -> (f64, f64) {
        let turn = self.turn_of(n);
        let steps = ((self.steps as f64 * frac).round() as usize).max(1);
        let dt = frac / steps as f64;
        let (mut x, mut y) = (u, v);
        for _ in 0..steps {
            if self.steps == 1 {
                let (fx, fy) = self.field(x, y, t, turn);
                x += fx * dt;
                y += fy * dt;
            } else {
                let (fx, fy) = self.field(x, y, t, turn);
                let (mx, my) = (x + fx * dt / 2.0, y + fy * dt / 2.0);
                let (gx, gy) = self.field(mx, my, t, turn);
                x += gx * dt;
                y += gy * dt;
            }
            if self.wrap {
                x = crate::transform::geom::rem1(x);
                y = crate::transform::geom::rem1(y);
            }
        }
        (x, y)
    }
}

/// v0.8's field from the parameters (shared with `flow.rs`).
pub(super) fn flow_of(p: &Params, b: &BuildCtx) -> Flow {
    Flow {
        strength: p.float("strength") / 100.0,
        uniform: p.adv(1).then(|| p.float("a1.angle") * PI / 180.0),
        vortex: p
            .adv(2)
            .then(|| (p.float("a2.x") / 100.0, p.float("a2.y") / 100.0)),
        noise: p
            .adv(3)
            .then(|| ((p.float("a3.scale") / 100.0).max(0.01), p.seed("seed"))),
        painted: p.adv(4).then(|| {
            let v = p.list("a4.angles");
            let side = ((v.len() as f64).sqrt().floor() as usize).max(1);
            (
                side,
                v.iter()
                    .take(side * side)
                    .map(|&d| d as f64 * PI / 180.0)
                    .collect(),
            )
        }),
        env: p.adv(5).then(|| p.curve("a5.curve")),
        steps: if p.adv(6) {
            p.int("a6.steps").clamp(2, 64) as usize
        } else {
            1
        },
        wrap: p.adv(8),
        turn: if p.adv(9) {
            p.list("a9.angles")
                .iter()
                .map(|&d| d as f64 * PI / 180.0)
                .collect()
        } else {
            Vec::new()
        },
        blend: p.adv(10).then(|| p.choice("a10.field")),
        tracks: b.tracks.to_vec(),
    }
}

fn flow_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    // V09-S41: the new functions and Control Sweeps (per note) run in the
    // v0.9 kernel; with every one off this v0.8 kernel runs unchanged.
    if super::flow::NEW.iter().any(|k| p.flag(k))
        || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty())
    {
        return super::flow::flow9_build(p, b);
    }
    let g = Geom::from_params(p, b.stats);
    let fl = flow_of(p, b);
    let trails = p.adv(7).then(|| p.int("a7.count").clamp(2, 32) as usize);
    let copies = trails.unwrap_or(1);
    let tracks = b.tracks.to_vec();
    copies_kernel(
        g,
        tracks,
        copies,
        |n, _| Some(*n),
        move |n, k, u, v| {
            let (mu, _) = mid_unit(&g, n);
            let frac = (k + 1) as f64 / copies as f64;
            fl.advect(n, u, v, mu.clamp(0.0, 1.0), frac)
        },
        // V09-0143: the trail copy (0 = the least advected; the last is
        // the note's whole flow).
        |k| u32::try_from(k).unwrap_or(u32::MAX),
    )
}
