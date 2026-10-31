//! Experimental spatial tools: Gravity Well, Kaleidoscope, Orbit and Vortex Trails.
//!
//! Every tool is a deterministic map of the selection's unit box (u = time,
//! v = pitch rows; see `transform/geom.rs`) or a bounded set of transformed
//! copies, rasterized by the shared geometry engine. Randomness comes only
//! from the seed.

use super::super::boundary::{self, PitchPolicy};
use super::super::geom::{Fill, Geom, geom_specs};
use super::super::param::*;
use super::super::registry::{BuildCtx, Family, FunctionSpec, ToolDef, ToolSpec};
use super::super::rng::{Rng, hash, salt};
use super::super::theory::{KEY_NAMES, scale_names};
use super::super::{Growth, Kernel, Output, Source, SourceTrack, TNote, TransformError};
use super::geometry::{permille, smoothstep, sub_pos};
use super::{Contextual, boxed};
use std::f64::consts::PI;

/// Spec of an experimental tool (shared geometry controls when `geom`).
pub(super) fn xspec(
    id: &'static str,
    name: &'static str,
    summary: &'static str,
    mut p: Vec<ParamSpec>,
    advanced: [&'static str; 10],
    geom: bool,
) -> ToolSpec {
    if geom {
        p.extend(geom_specs());
    }
    p.extend(boundary::specs(PitchPolicy::Discard));
    ToolSpec {
        id,
        name,
        family: Family::Experimental,
        summary,
        params: p,
        advanced: advanced.to_vec(),
        functions: Vec::new(),
        needs_selection: true,
        context: false,
    }
}

/// The note's middle in unit coordinates.
#[inline]
pub(super) fn mid_unit(g: &Geom, n: &TNote) -> (f64, f64) {
    g.bbox.to_unit(
        (n.start as f64 + n.end() as f64) / 2.0,
        f64::from(n.pitch) + 0.5,
    )
}

/// `n` with velocity × `f` (1–127).
#[inline]
pub(super) fn vel_scaled(n: &TNote, f: f64) -> u8 {
    (f64::from(n.vel) * f).round().clamp(1.0, 127.0) as u8
}

/// `n` moved `steps` positions along its track's subchannel table.
#[inline]
pub(super) fn sub_stepped(tracks: &[SourceTrack], n: &TNote, steps: i64) -> TNote {
    let mut t = *n;
    if steps != 0
        && let Some(tr) = tracks.get(usize::from(n.track))
    {
        let (id, ch) = tr.sub_offset(n.sub, steps);
        t.sub = id;
        t.ch = ch;
    }
    t
}

/// Smooth 2-D value noise in [-1, 1] (deterministic from the seed).
pub(super) fn noise2(seed: u64, salt: u64, x: f64, y: f64) -> f64 {
    let (xi, yi) = (x.floor(), y.floor());
    let (fx, fy) = (smoothstep(x - xi), smoothstep(y - yi));
    let h = |a: f64, b: f64| {
        Rng::new(hash(&[seed, salt, a as i64 as u64, b as i64 as u64])).unit() * 2.0 - 1.0
    };
    let a = h(xi, yi) + (h(xi + 1.0, yi) - h(xi, yi)) * fx;
    let b = h(xi, yi + 1.0) + (h(xi + 1.0, yi + 1.0) - h(xi, yi + 1.0)) * fx;
    a + (b - a) * fy
}

/// Rotation of (x, y) by `a` radians.
#[inline]
pub(super) fn rot(x: f64, y: f64, a: f64) -> (f64, f64) {
    let (s, c) = a.sin_cos();
    (x * c - y * s, x * s + y * c)
}

/// Rasterizes `copies` transformed copies of every note: `tpl(n, k)` is copy
/// `k`'s template (None skips it), `map(n, k, u, v)` its unit-space map,
/// `dim(k)` its driver dimension (V09-0143: the sector, orbit copy or trail).
pub(super) fn copies_kernel<T, M, D>(
    g: Geom,
    tracks: Vec<SourceTrack>,
    copies: usize,
    tpl: T,
    map: M,
    dim: D,
) -> Result<Box<dyn Kernel>, TransformError>
where
    T: Fn(&TNote, usize) -> Option<TNote> + Send + Sync + 'static,
    M: Fn(&TNote, usize, f64, f64) -> (f64, f64) + Send + Sync + 'static,
    D: Fn(usize) -> u32 + Send + Sync + 'static,
{
    let per = g.per_note().saturating_mul(copies.max(1) as u64);
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            for n in &src.notes {
                for k in 0..copies {
                    let Some(t) = tpl(n, k) else { continue };
                    out.set_dim(dim(k));
                    if !g.rasterize(&t, &|u, v| map(n, k, u, v), &tracks, out) {
                        return out.check();
                    }
                }
                out.check()?;
            }
            Ok(())
        },
        growth: if per > 1 {
            Growth::Expand(per)
        } else {
            Growth::Map
        },
        estimate: None,
    })
}

// =============================================================== Gravity Well

pub const GRAVITY_WELL: ToolDef = ToolDef {
    id: "gravity_well",
    spec: gravity_spec,
    build: gravity_build,
};

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const GRAVITY_WELL_HELP: &[(&str, &str)] = &[
    (
        "x",
        "Where the well lies in time, as a share of the selection's box (50 % = its middle).",
    ),
    (
        "y",
        "Where the well lies in pitch, as a share of the selection's box.",
    ),
    ("strength", "How strongly notes are pulled toward the well."),
    (
        "radius",
        "How far the well's pull reaches, as a share of the box; notes nearer move most.",
    ),
    ("a1", "Adds more wells that pull at the same time."),
    (
        "a1.wells",
        "The extra wells as x, y pairs in thousandths of the box.",
    ),
    ("a2", "Lets wells push notes away instead of pulling them."),
    (
        "a2.signs",
        "For each well in turn: 1 pulls, −1 pushes (repeated over the wells).",
    ),
    ("a3", "Changes how fast the pull fades with distance."),
    (
        "a3.exp",
        "The falloff's exponent: higher values keep the pull close to the well.",
    ),
    (
        "a4",
        "Makes notes near a well swirl around it instead of falling straight in.",
    ),
    (
        "a4.radius",
        "How close to a well notes must be to be caught in its swirl, as a share of the box.",
    ),
    (
        "a4.angle",
        "How far notes at the very center are turned, in degrees.",
    ),
    (
        "a5",
        "Removes notes that fall too close to a well, like a black hole.",
    ),
    (
        "a6",
        "Pulls only in pitch, toward a horizontal line through the well.",
    ),
    (
        "a7",
        "Pulls only in time, toward a vertical line through the well.",
    ),
    (
        "a8",
        "Lets each subchannel feel the pull more or less strongly.",
    ),
    (
        "a8.mass",
        "How strongly each subchannel position is pulled, in percent (0 = not at all).",
    ),
    (
        "a9",
        "Fills the gaps that the pull stretches open, so stretched areas keep their density.",
    ),
    (
        "a10",
        "Moves the well across the selection from its start to its end.",
    ),
    (
        "a10.x",
        "Where the well lies in time at the selection's end, as a share of the box.",
    ),
    (
        "a10.y",
        "Where the well lies in pitch at the selection's end, as a share of the box.",
    ),
    (
        "n01",
        "Lets the notes also pull on each other, in several small steps.",
    ),
    (
        "n01.steps",
        "How many small steps the notes move toward each other.",
    ),
    ("n01.coupling", "How strongly the notes pull on each other."),
    (
        "n01.soft",
        "Softens the pull between notes that are very close, as a share of the box.",
    ),
    (
        "n02",
        "Bends the notes like a gravitational lens, pushing them out toward a ring around the well.",
    ),
    ("n02.radius", "The ring's radius, as a share of the box."),
    (
        "n02.ring",
        "The velocity of notes on the ring, as a share of their own (they brighten there).",
    ),
    (
        "n03",
        "Stretches notes toward the well and squeezes them sideways, like tides.",
    ),
    (
        "n03.tidal",
        "How strongly notes are stretched toward the well.",
    ),
    ("n03.length", "How much longer notes near the well become."),
    (
        "n04",
        "Spirals notes into the well, like matter around a black hole.",
    ),
    ("n04.turns", "How many times the spiral goes around."),
    ("n04.infall", "How far the notes fall in along the spiral."),
    (
        "n05",
        "Lets loud notes escape the pull and fly straight off instead.",
    ),
    ("n05.vel", "The velocity a note needs to escape."),
    (
        "n05.flight",
        "How far escaping notes fly, as a share of the box.",
    ),
    (
        "n06",
        "Sets each note's velocity by how deep in the well's pull it ends up.",
    ),
    (
        "n06.gain",
        "How much louder notes at the core get (negative = quieter).",
    ),
    (
        "n06.reach",
        "How far from the well the loudness change reaches, as a share of the box.",
    ),
    ("n07", "Places wells where the notes are densest."),
    (
        "n07.wells",
        "How many wells are placed at the densest spots.",
    ),
    (
        "n07.window",
        "The length of time over which the density is measured, in ticks.",
    ),
    ("n08", "Places wells at musical points."),
    (
        "n08.at",
        "Where the wells go: at every bar, every marker, or every chord root.",
    ),
    ("n08.pitch", "The pitch of the wells at bars and markers."),
    (
        "n09",
        "Uses two wells that orbit each other over the selection.",
    ),
    (
        "n09.sep",
        "How far apart the two wells are, as a share of the box.",
    ),
    (
        "n09.turns",
        "How many times the two wells orbit each other over the selection.",
    ),
    (
        "n10",
        "Shapes the force by distance with a curve, so wells can pull at some distances and push at others.",
    ),
    (
        "n10.curve",
        "The force from the well (left) out to the reach (right): above the middle pulls, below pushes.",
    ),
    (
        "n10.reach",
        "How far the force profile reaches, as a share of the box.",
    ),
    (
        "n11",
        "Slows time near the wells: notes near them are stretched out in time.",
    ),
    ("n11.dilation", "How strongly time is slowed at the well."),
    (
        "n11.reach",
        "How far the slowing reaches, as a share of the box.",
    ),
];

fn gravity_spec() -> ToolSpec {
    let mut s = xspec(
        "gravity_well",
        "Gravity Well",
        "Bends the selection toward an attractor in time × pitch: points near the well move most (softened inverse-square pull), far points barely move.",
        vec![
            float(
                "x",
                "Well X (of the selection box)",
                -50.0,
                150.0,
                50.0,
                "%",
            ),
            float("y", "Well Y", -50.0, 150.0, 50.0, "%"),
            float("strength", "Strength", 0.0, 100.0, 50.0, "%"),
            float("radius", "Radius", 1.0, 200.0, 40.0, "%"),
            seed("seed"),
            adv_switch(1, "Multiple wells"),
            list(
                "a1.wells",
                "More wells ‰ (x, y, …)",
                -500,
                1500,
                32,
                &[200, 200, 800, 800],
            )
            .adv(1),
            adv_switch(2, "Repulsive wells"),
            list(
                "a2.signs",
                "Sign per well (−1 repels, 1 attracts; cycles)",
                -1,
                1,
                16,
                &[-1],
            )
            .adv(2),
            adv_switch(3, "Falloff exponent"),
            float("a3.exp", "Exponent", 0.25, 8.0, 4.0, "").adv(3),
            adv_switch(4, "Orbit capture"),
            float("a4.radius", "Capture radius", 1.0, 100.0, 25.0, "%").adv(4),
            float(
                "a4.angle",
                "Orbit angle at the core",
                -720.0,
                720.0,
                120.0,
                "°",
            )
            .adv(4),
            adv_switch(5, "Event horizon"),
            float(
                "a5.radius",
                "Horizon radius (notes inside vanish)",
                0.5,
                100.0,
                10.0,
                "%",
            )
            .adv(5)
            .help("How close to a well notes may come before they vanish, as a share of the box."),
            adv_switch(6, "Pitch-only gravity (horizontal attractor line)"),
            adv_switch(7, "Time-only gravity (vertical attractor line)"),
            adv_switch(8, "Subchannel mass weighting"),
            list(
                "a8.mass",
                "Mass per subchannel position (%)",
                0,
                400,
                4096,
                &[100, 30, 250, 0],
            )
            .adv(8),
            adv_switch(9, "Density-preserving fill"),
            adv_switch(10, "Animated attractor path"),
            float(
                "a10.x",
                "Well X at the end of the selection",
                -50.0,
                150.0,
                85.0,
                "%",
            )
            .adv(10),
            float("a10.y", "Well Y at the end", -50.0, 150.0, 15.0, "%").adv(10),
            // V09-S40 step 1: functions declared by stable id (TOOL_CATALOG
            // n01 … n11; the v0.9 kernel is `gravity.rs`).
            flag("n01", "N-body steps", false),
            int("n01.steps", "Steps", 1, 64, 8),
            float("n01.coupling", "Coupling", 0.0, 100.0, 50.0, "%"),
            float("n01.soft", "Softening", 1.0, 100.0, 10.0, "%"),
            flag("n02", "Lensing", false),
            float("n02.radius", "Einstein radius", 1.0, 100.0, 15.0, "%"),
            float("n02.ring", "Ring velocity", 0.0, 200.0, 140.0, "%"),
            flag("n03", "Tidal stretch", false),
            float("n03.tidal", "Tidal strength", 0.0, 100.0, 60.0, "%"),
            float("n03.length", "Length stretch", 0.0, 400.0, 100.0, "%"),
            flag("n04", "Accretion spiral", false),
            float("n04.turns", "Turns", 0.0, 32.0, 1.0, ""),
            float("n04.infall", "Infall", 0.0, 100.0, 50.0, "%"),
            flag("n05", "Escape trajectories", false),
            int("n05.vel", "Escape velocity", 1, 127, 100),
            float("n05.flight", "Flight length", 0.0, 200.0, 50.0, "%"),
            flag("n06", "Potential loudness", false),
            float("n06.gain", "Gain at the core", -100.0, 200.0, 60.0, "%"),
            float("n06.reach", "Reach", 1.0, 200.0, 40.0, "%"),
            flag("n07", "Density wells", false),
            int("n07.wells", "Wells", 1, 8, 2),
            ticks("n07.window", "Window", 1, 1 << 40, 3840),
            flag("n08", "Musical wells", false),
            choice("n08.at", "At", super::gravity::MUSICAL_AT, 0),
            pitch("n08.pitch", "Pitch", 60),
            flag("n09", "Binary orbit", false),
            float("n09.sep", "Separation", 1.0, 100.0, 30.0, "%"),
            float("n09.turns", "Orbit turns", 0.0, 16.0, 2.0, ""),
            flag("n10", "Force profile", false),
            curve(
                "n10.curve",
                "Force by distance (0.5 = none, above pulls, below pushes)",
                super::gravity::default_profile(),
            )
            .overshoot(),
            float("n10.reach", "Reach", 1.0, 200.0, 60.0, "%"),
            flag("n11", "Time dilation", false),
            float("n11.dilation", "Dilation", 0.0, 100.0, 50.0, "%"),
            float("n11.reach", "Reach", 1.0, 200.0, 40.0, "%"),
        ],
        [
            "Multiple wells",
            "Repulsive wells",
            "Falloff exponent",
            "Orbit capture",
            "Event horizon",
            "Pitch-only gravity",
            "Time-only gravity",
            "Subchannel mass weighting",
            "Density-preserving fill",
            "Animated attractor path",
        ],
        true,
    );
    s.functions = [
        ("n01", "N-body steps", 110),
        ("n02", "Lensing", 120),
        ("n03", "Tidal stretch", 130),
        ("n04", "Accretion spiral", 140),
        ("n05", "Escape trajectories", 150),
        ("n06", "Potential loudness", 510),
        ("n07", "Density wells", 160),
        ("n08", "Musical wells", 170),
        ("n09", "Binary orbit", 180),
        ("n10", "Force profile", 190),
        ("n11", "Time dilation", 200),
    ]
    .into_iter()
    .map(|(id, name, order)| FunctionSpec { id, name, order })
    .collect();
    with_help(&mut s.params, GRAVITY_WELL_HELP);
    s
}

struct Gravity {
    wells: Vec<(f64, f64, f64)>,
    strength: f64,
    radius: f64,
    exp: f64,
    capture: Option<(f64, f64)>,
    horizon: Option<f64>,
    pitch_only: bool,
    time_only: bool,
    mass: Vec<f64>,
    path: Option<(f64, f64)>,
    tracks: Vec<SourceTrack>,
}

impl Gravity {
    fn wells_at(&self, nu: f64) -> impl Iterator<Item = (f64, f64, f64)> + '_ {
        let (ox, oy) = self.path.map_or((0.0, 0.0), |(dx, dy)| (dx * nu, dy * nu));
        self.wells.iter().map(move |&(x, y, s)| (x + ox, y + oy, s))
    }

    fn swallowed(&self, nu: f64, nv: f64) -> bool {
        self.horizon.is_some_and(|h| {
            self.wells_at(nu)
                .any(|(x, y, _)| ((nu - x).powi(2) + (nv - y).powi(2)).sqrt() < h)
        })
    }

    fn map(&self, n: &TNote, u: f64, v: f64, nu: f64) -> (f64, f64) {
        let m = if self.mass.is_empty() {
            1.0
        } else {
            self.mass[sub_pos(&self.tracks, n) % self.mass.len()]
        };
        let (mut du, mut dv) = (0.0, 0.0);
        for (wx, wy, sign) in self.wells_at(nu) {
            let pull = |dist: f64| -> f64 {
                let f = 1.0 / (1.0 + (dist / self.radius).powf(self.exp));
                let k = self.strength * sign * m * f;
                k.clamp(-3.0, 1.0)
            };
            if let Some((cr, ang)) = self.capture {
                let d = ((u - wx).powi(2) + (v - wy).powi(2)).sqrt();
                if d < cr {
                    let (x, y) = rot(u - wx, v - wy, ang * (1.0 - d / cr) * sign);
                    du += wx + x - u;
                    dv += wy + y - v;
                    continue;
                }
            }
            if !self.pitch_only && !self.time_only {
                let (dx, dy) = (wx - u, wy - v);
                let k = pull((dx * dx + dy * dy).sqrt());
                du += dx * k;
                dv += dy * k;
            }
            if self.pitch_only {
                dv += (wy - v) * pull((wy - v).abs());
            }
            if self.time_only {
                du += (wx - u) * pull((wx - u).abs());
            }
        }
        (u + du, v + dv)
    }
}

/// |det J| of `f` at (u, v) by finite differences (area scale of the map).
pub(super) fn area_scale(f: &dyn Fn(f64, f64) -> (f64, f64), u: f64, v: f64) -> f64 {
    let h = 1e-3;
    let (u0, v0) = f(u, v);
    let (u1, v1) = f(u + h, v);
    let (u2, v2) = f(u, v + h);
    (((u1 - u0) * (v2 - v0) - (u2 - u0) * (v1 - v0)) / (h * h)).abs()
}

fn gravity_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    // V09-S40 step 1: the new functions and Control Sweeps (per note) run in
    // the v0.9 kernel; with every one off this v0.8 kernel runs unchanged.
    if super::gravity::NEW.iter().any(|k| p.flag(k))
        || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty())
    {
        return super::gravity::grav9_build(p, b);
    }
    let g = Geom::from_params(p, b.stats);
    let mut pts = vec![(p.float("x") / 100.0, p.float("y") / 100.0)];
    if p.adv(1) {
        pts.extend(
            permille(&p.list("a1.wells"))
                .chunks_exact(2)
                .map(|c| (c[0], c[1])),
        );
    }
    let signs: Vec<f64> = if p.adv(2) {
        p.list("a2.signs")
            .iter()
            .map(|&s| if s < 0 { -1.0 } else { 1.0 })
            .collect()
    } else {
        Vec::new()
    };
    let wells: Vec<(f64, f64, f64)> = pts
        .iter()
        .enumerate()
        .map(|(i, &(x, y))| {
            let s = if signs.is_empty() {
                1.0
            } else {
                signs[i % signs.len()]
            };
            (x, y, s)
        })
        .collect();
    let path = p.adv(10).then(|| {
        (
            p.float("a10.x") / 100.0 - pts[0].0,
            p.float("a10.y") / 100.0 - pts[0].1,
        )
    });
    let gr = Gravity {
        wells,
        strength: p.float("strength") / 100.0,
        radius: (p.float("radius") / 100.0).max(1e-3),
        exp: if p.adv(3) { p.float("a3.exp") } else { 2.0 },
        capture: p.adv(4).then(|| {
            (
                (p.float("a4.radius") / 100.0).max(1e-3),
                p.float("a4.angle") * PI / 180.0,
            )
        }),
        horizon: p.adv(5).then(|| p.float("a5.radius") / 100.0),
        pitch_only: p.adv(6),
        time_only: p.adv(7),
        mass: if p.adv(8) {
            p.list("a8.mass")
                .iter()
                .map(|&m| m as f64 / 100.0)
                .collect()
        } else {
            Vec::new()
        },
        path,
        tracks: b.tracks.to_vec(),
    };
    let density = p.adv(9);
    let seed = p.seed("seed");
    let mut gf = g;
    gf.fill = Some(Fill::default());
    let per = if density { gf.per_note() } else { g.per_note() };
    boxed(Contextual {
        f: move |src: &Source, out: &mut Output| {
            for n in &src.notes {
                let (nu, nv) = mid_unit(&g, n);
                if gr.swallowed(nu, nv) {
                    continue;
                }
                let map = |u: f64, v: f64| gr.map(n, u, v, nu);
                let mut geo = &g;
                if density {
                    let j = area_scale(&map, nu, nv);
                    if j < 1.0 {
                        if !Rng::for_note(seed, salt::KEEP, n).chance(j) {
                            continue;
                        }
                    } else if j > 1.05 {
                        geo = &gf;
                    }
                }
                if !geo.rasterize(n, &map, &gr.tracks, out) {
                    return out.check();
                }
                out.check()?;
            }
            Ok(())
        },
        growth: if per > 1 {
            Growth::Expand(per)
        } else {
            Growth::Map
        },
        estimate: None,
    })
}

// =============================================================== Kaleidoscope

pub const KALEIDOSCOPE: ToolDef = ToolDef {
    id: "kaleidoscope",
    spec: kaleido_spec,
    build: kaleido_build,
};

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const KALEIDOSCOPE_HELP: &[(&str, &str)] = &[
    (
        "a1",
        "Sets how many sectors the selection is repeated in around the center instead of two.",
    ),
    (
        "a1.n",
        "How many sectors go around the center, the original included.",
    ),
    (
        "a2",
        "Reflects every other sector, like a real kaleidoscope's mirrors.",
    ),
    (
        "a3",
        "Turns each sector a little further, for a pinwheel twist.",
    ),
    (
        "a3.twist",
        "How much further each sector turns than the previous one, in degrees.",
    ),
    (
        "a4",
        "Also adds the pitch mirror image of every copy (upside down).",
    ),
    (
        "a5",
        "Also adds the time mirror image of every copy (backward).",
    ),
    ("a6", "Moves each sector to another subchannel (color)."),
    (
        "a6.step",
        "How many subchannel positions each sector moves from the previous one.",
    ),
    ("a7", "Moves the center the sectors turn around."),
    (
        "a7.x",
        "Where the center lies in time, as a share of the selection's box.",
    ),
    (
        "a7.y",
        "Where the center lies in pitch, as a share of the selection's box.",
    ),
    (
        "a8",
        "Makes each sector smaller (or larger) than the previous one.",
    ),
    (
        "a8.scale",
        "Each sector's size as a share of the previous sector's.",
    ),
    (
        "a9",
        "Adds smaller kaleidoscopes inside the first, level by level.",
    ),
    ("a9.levels", "How many levels of kaleidoscopes are drawn."),
    (
        "a9.scale",
        "Each level's size as a share of the level before.",
    ),
    (
        "a10",
        "Uses only the notes in the first sector's wedge, so the copies fit together without overlapping.",
    ),
    (
        "n01",
        "Repeats the selection in one of the 17 wallpaper patterns instead of around a center.",
    ),
    (
        "n01.group",
        "The wallpaper group, in crystallographic notation (p1 only repeats; others add turns, mirrors and glides).",
    ),
    ("n01.width", "How wide each pattern cell is, in ticks."),
    ("n01.height", "How many rows tall each pattern cell is."),
    ("n01.rt", "How many cells the pattern repeats in time."),
    ("n01.rp", "How many cells the pattern repeats in pitch."),
    (
        "n02",
        "Spins the whole kaleidoscope as time goes on, like a turning mandala.",
    ),
    (
        "n02.turns",
        "How many turns it spins over the selection (negative = the other way).",
    ),
    (
        "n03",
        "Delays each sector a little more in time than the previous one.",
    ),
    (
        "n03.delay",
        "How many ticks later each sector starts than the previous one.",
    ),
    (
        "n04",
        "Thins the copies so the result has about as many notes as the original.",
    ),
    (
        "n04.mode",
        "Interleave takes notes from the sectors in turn; Outline keeps the outer notes; None keeps them all.",
    ),
    ("n05", "Transposes each sector by its own amount."),
    (
        "n05.semis",
        "The semitones each sector is transposed by, in turn (repeated).",
    ),
    (
        "n06",
        "Arranges the sectors in several rings around the center, each with its own sector count.",
    ),
    ("n06.rings", "How many rings are drawn."),
    (
        "n06.sectors",
        "How many sectors each ring has, ring by ring from the center out.",
    ),
    (
        "n06.spacing",
        "How far apart the rings are, as a share of the box.",
    ),
    (
        "n07",
        "Mirrors in musical units: the center sits on a bar line or beat and every copy stays in a key.",
    ),
    ("n07.key", "The key every copy's pitch stays in."),
    ("n07.scale", "The scale every copy's pitch stays in."),
    (
        "n07.at",
        "Whether the mirror's center sits on bar lines or on beats.",
    ),
    (
        "n08",
        "Breaks the symmetry by moving a few sectors a little at random (seeded).",
    ),
    ("n08.count", "How many sectors are moved."),
    ("n08.amount", "How far the broken sectors are moved."),
    (
        "n09",
        "Merges the doubled notes along the mirror seams into one.",
    ),
    (
        "n09.width",
        "How close together doubled notes may be to be merged, in ticks.",
    ),
    (
        "n09.keep",
        "What is left of doubled notes: one of them, or one with their velocities blended.",
    ),
    (
        "n10",
        "Fills only some sectors, following a repeating pattern.",
    ),
    (
        "n10.pattern",
        "One digit per sector in turn, repeated: 1 fills the sector, 0 leaves it empty.",
    ),
    (
        "n11",
        "Builds a small kaleidoscope around each chord, bar or group instead of one for the whole selection.",
    ),
    (
        "n11.unit",
        "The unit each small kaleidoscope is built around: a chord, a bar or a note group.",
    ),
    (
        "n11.sectors",
        "How many sectors each small kaleidoscope has.",
    ),
];

fn kaleido_spec() -> ToolSpec {
    let mut s = xspec(
        "kaleidoscope",
        "Kaleidoscope",
        "Builds a symmetric structure: the selection is repeated in sectors around the center of its box (two sectors = point symmetry).",
        vec![
            adv_switch(1, "Sector count"),
            int("a1.n", "Sectors", 2, 24, 6).adv(1),
            adv_switch(2, "Mirror sectors (alternate sectors reflected)"),
            adv_switch(3, "Rotational sectors (pinwheel twist)"),
            float("a3.twist", "Extra turn per sector", -90.0, 90.0, 15.0, "°").adv(3),
            adv_switch(4, "Pitch-only symmetry (add pitch mirror images)"),
            adv_switch(5, "Time-only symmetry (add time mirror images)"),
            adv_switch(6, "Alternating subchannels"),
            int("a6.step", "Subchannel positions per sector", -16, 16, 1).adv(6),
            adv_switch(7, "Center offset"),
            float("a7.x", "Center X", -50.0, 150.0, 30.0, "%").adv(7),
            float("a7.y", "Center Y", -50.0, 150.0, 60.0, "%").adv(7),
            adv_switch(8, "Radial scaling per sector"),
            float("a8.scale", "Scale per sector", 10.0, 200.0, 85.0, "%").adv(8),
            adv_switch(9, "Nested kaleidoscopes"),
            int("a9.levels", "Levels", 2, 4, 2).adv(9),
            float("a9.scale", "Scale per level", 10.0, 95.0, 50.0, "%").adv(9),
            adv_switch(10, "Wedge masking (only the first sector's notes)"),
            // V09-S40 step 4: functions declared by stable id (TOOL_CATALOG
            // n01 … n11; the v0.9 kernel is `kaleido.rs`).
            flag("n01", "Wallpaper groups", false),
            choice("n01.group", "Group", super::kaleido::GROUPS, 1),
            ticks("n01.width", "Cell width", 1, 1 << 30, 1920),
            int("n01.height", "Cell height (rows)", 1, 64, 12),
            int("n01.rt", "Repeats in time", 1, 64, 4),
            int("n01.rp", "Repeats in pitch", 1, 16, 1),
            flag("n02", "Spinning mandala", false),
            float("n02.turns", "Turns", -16.0, 16.0, 1.0, ""),
            flag("n03", "Sector delay", false),
            ticks("n03.delay", "Delay per sector", 0, 1 << 30, 240),
            flag("n04", "Sector thinning", false),
            choice("n04.mode", "Mode", super::kaleido::THIN, 0),
            flag("n05", "Sector transposition", false),
            list(
                "n05.semis",
                "Transposition per sector",
                -48,
                48,
                64,
                &[0, 4, 7],
            ),
            flag("n06", "Mandala rings", false),
            int("n06.rings", "Rings", 1, 8, 2),
            list("n06.sectors", "Sectors per ring", 1, 64, 8, &[6, 12]),
            float("n06.spacing", "Ring spacing", 1.0, 100.0, 20.0, "%"),
            flag("n07", "Diatonic symmetry", false),
            choice("n07.key", "Key", &KEY_NAMES, 0),
            choice("n07.scale", "Scale", scale_names(), 0),
            choice("n07.at", "Mirror at", super::kaleido::MIRROR_AT, 0),
            flag("n08", "Symmetry breaking", false),
            int("n08.count", "Broken sectors", 0, 64, 2),
            float("n08.amount", "Perturbation", 0.0, 100.0, 30.0, "%"),
            seed("n08.seed"),
            flag("n09", "Seam cleanup", false),
            ticks("n09.width", "Seam width", 0, 1 << 20, 30),
            choice("n09.keep", "Keep", super::kaleido::SEAM_KEEP, 0),
            flag("n10", "Sector pattern", false),
            list("n10.pattern", "Fill pattern", 0, 1, 64, &[1, 0]),
            flag("n11", "Local mandalas", false),
            choice("n11.unit", "Unit", super::kaleido::UNITS, 0),
            int("n11.sectors", "Sectors", 2, 32, 4),
        ],
        [
            "Sector count",
            "Mirror sectors",
            "Rotational sectors",
            "Pitch-only symmetry",
            "Time-only symmetry",
            "Alternating subchannels",
            "Center offset",
            "Radial scaling",
            "Nested kaleidoscopes",
            "Wedge masking",
        ],
        true,
    );
    s.functions = [
        ("n01", "Wallpaper groups", 110),
        ("n02", "Spinning mandala", 120),
        ("n03", "Sector delay", 130),
        ("n04", "Sector thinning", 700),
        ("n05", "Sector transposition", 140),
        ("n06", "Mandala rings", 150),
        ("n07", "Diatonic symmetry", 160),
        ("n08", "Symmetry breaking", 170),
        ("n09", "Seam cleanup", 710),
        ("n10", "Sector pattern", 180),
        ("n11", "Local mandalas", 190),
    ]
    .into_iter()
    .map(|(id, name, order)| FunctionSpec { id, name, order })
    .collect();
    with_help(&mut s.params, KALEIDOSCOPE_HELP);
    s
}

fn kaleido_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    // V09-S40 step 4: the new functions and Control Sweeps (per note) run in
    // the v0.9 kernel; with every one off this v0.8 kernel runs unchanged.
    if super::kaleido::NEW.iter().any(|k| p.flag(k))
        || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty())
    {
        return super::kaleido::kal9_build(p, b);
    }
    let g = Geom::from_params(p, b.stats);
    let n_sec = if p.adv(1) {
        p.int("a1.n").clamp(2, 24) as usize
    } else {
        2
    };
    let mirror = p.adv(2);
    let twist = if p.adv(3) {
        p.float("a3.twist") * PI / 180.0
    } else {
        0.0
    };
    let pm = if p.adv(4) { 2 } else { 1 };
    let tm = if p.adv(5) { 2 } else { 1 };
    let sub_step = if p.adv(6) { p.int("a6.step") } else { 0 };
    let (cx, cy) = if p.adv(7) {
        (p.float("a7.x") / 100.0, p.float("a7.y") / 100.0)
    } else {
        (0.5, 0.5)
    };
    let rscale = if p.adv(8) {
        p.float("a8.scale") / 100.0
    } else {
        1.0
    };
    let (levels, lscale) = if p.adv(9) {
        (
            p.int("a9.levels").clamp(2, 4) as usize,
            p.float("a9.scale") / 100.0,
        )
    } else {
        (1, 1.0)
    };
    let wedge = p.adv(10);
    let copies = n_sec * pm * tm * levels;
    let tracks = b.tracks.to_vec();
    let decode = move |k: usize| -> (usize, bool, bool, usize) {
        let sec = k % n_sec;
        let r = k / n_sec;
        (sec, r % pm == 1, (r / pm) % tm == 1, r / (pm * tm))
    };
    let tr = tracks.clone();
    copies_kernel(
        g,
        tracks,
        copies,
        move |n, k| {
            if wedge {
                let (u, v) = mid_unit(&g, n);
                let a = (v - cy).atan2(u - cx).rem_euclid(2.0 * PI);
                if a >= 2.0 * PI / n_sec as f64 {
                    return None;
                }
            }
            let (sec, _, _, _) = decode(k);
            Some(sub_stepped(&tr, n, sub_step * sec as i64))
        },
        move |_, k, u, v| {
            let (sec, pmir, tmir, level) = decode(k);
            let (mut x, mut y) = (u - cx, v - cy);
            if mirror && sec % 2 == 1 {
                y = -y;
            }
            let ang = 2.0 * PI * sec as f64 / n_sec as f64 + twist * sec as f64;
            (x, y) = rot(x, y, ang);
            let s = rscale.powi(sec as i32) * lscale.powi(level as i32);
            x *= s;
            y *= s;
            if pmir {
                y = -y;
            }
            if tmir {
                x = -x;
            }
            (cx + x, cy + y)
        },
        // V09-0143: the sector (0 = the original's).
        move |k| (k % n_sec) as u32,
    )
}

// ====================================================================== Orbit

pub const ORBIT: ToolDef = ToolDef {
    id: "orbit",
    spec: orbit_spec,
    build: orbit_build,
};

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const ORBIT_HELP: &[(&str, &str)] = &[
    (
        "copies",
        "How many copies of the motif are placed around the path.",
    ),
    (
        "cx",
        "The center of the orbit in time, as a share of the selection's box (50 % = its middle).",
    ),
    (
        "cy",
        "The center of the orbit in pitch, as a share of the selection's box (50 % = its middle).",
    ),
    (
        "rx",
        "How far the path reaches in time from the center, as a share of the box.",
    ),
    (
        "ry",
        "How far the path reaches in pitch from the center, as a share of the box.",
    ),
    (
        "size",
        "How big each copy of the motif is, as a share of the original.",
    ),
    ("keep", "Also keeps the original selection where it is."),
    (
        "a1",
        "Changes the path's shape between a diamond, an ellipse and a rounded square (a superellipse).",
    ),
    (
        "a1.exp",
        "The shape's exponent: 1 gives a diamond, 2 an ellipse, higher values a rounder square.",
    ),
    (
        "a2",
        "Places the copies on part of the path, from one angle to another, instead of all the way around.",
    ),
    (
        "a2.from",
        "The angle of the first copy, in degrees (0 = to the right of the center).",
    ),
    ("a2.to", "The angle of the last copy, in degrees."),
    (
        "a3",
        "Adds more rings of copies inside or outside the first one.",
    ),
    ("a3.rings", "How many rings are placed."),
    (
        "a3.scale",
        "Each ring's radius as a share of the previous ring's.",
    ),
    (
        "a4",
        "Makes the radius grow or shrink from copy to copy, for a spiral.",
    ),
    (
        "a4.end",
        "The last copy's radius as a share of the first's.",
    ),
    ("a5", "Turns each copy to follow the direction of the path."),
    ("a6", "Changes the velocity of the copies around the path."),
    (
        "a6.curve",
        "The velocity factor of the copies from the first to the last (1 = unchanged).",
    ),
    (
        "a7",
        "Changes the length of the copies' notes around the path.",
    ),
    (
        "a7.curve",
        "The length factor of the copies from the first to the last (0.5 = unchanged, 1 = double).",
    ),
    (
        "a8",
        "Divides the path into sectors and gives each sector the next subchannel.",
    ),
    ("a8.sectors", "How many sectors the path is divided into."),
    (
        "a9",
        "Changes the path's height from copy to copy, making it more or less flat.",
    ),
    (
        "a9.curve",
        "The pitch radius factor from the first copy to the last (0.5 = unchanged, 1 = double).",
    ),
    (
        "a10",
        "Uses a Lissajous figure as the path: time and pitch run around at different speeds.",
    ),
    (
        "a10.p",
        "How many times the path swings back and forth in time per turn.",
    ),
    (
        "a10.q",
        "How many times the path swings up and down in pitch per turn.",
    ),
    (
        "n01",
        "Places the copies along a path you draw instead of an ellipse.",
    ),
    (
        "n01.x",
        "The path's position in time from its start to its end (0.5 = the center).",
    ),
    (
        "n01.y",
        "The path's position in pitch from its start to its end (0.5 = the center).",
    ),
    (
        "n02",
        "Tilts the orbit away from you, so copies on the far side are smaller and quieter.",
    ),
    ("n02.tilt", "How far the orbit tilts, in degrees."),
    (
        "n02.depth",
        "How much smaller and quieter the far side gets.",
    ),
    (
        "n03",
        "Spaces the copies like a planet's orbit: closer and faster near one end of an oval path, slower and farther apart at the other.",
    ),
    ("n03.e", "How oval the orbit is (0 % = a circle)."),
    (
        "n03.peri",
        "The direction, in degrees, of the orbit's closest point to the center.",
    ),
    (
        "n04",
        "Spins each copy a little more than the previous one.",
    ),
    (
        "n04.spin",
        "How many degrees each copy turns more than the previous one.",
    ),
    (
        "n05",
        "Adds copies at the orbit's Lagrange points: the balance points of each copy and the center.",
    ),
    (
        "n05.points",
        "Which Lagrange points get copies: 1 – 3 on the line through the center, 4 and 5 at 60 degrees ahead and behind.",
    ),
    (
        "n05.mass",
        "How heavy each copy is compared with the center, which moves points 1 – 3.",
    ),
    (
        "n06",
        "Spaces the copies evenly along the path's length instead of placing a fixed number.",
    ),
    (
        "n06.spacing",
        "The distance between copies along the path, as a share of the box.",
    ),
    ("n07", "Varies the motif from copy to copy."),
    (
        "n07.mode",
        "Rotate Pitches shifts the motif's pitches one step per copy; Every Other Note alternates which notes each copy plays; Alternate Inversion turns every other copy upside down.",
    ),
    (
        "n08",
        "Hides the copies behind a planet in the middle of the orbit.",
    ),
    ("n08.radius", "The planet's radius, as a share of the box."),
    (
        "n09",
        "Turns each ring (or the path over time) a little further, so the orbit drifts around.",
    ),
    (
        "n09.angle",
        "How far each ring (or the whole path) turns, in degrees.",
    ),
    (
        "n09.along",
        "Rings turns each ring more than the previous one; Time turns the path gradually along its length.",
    ),
    ("n10", "Adds small moons circling each copy."),
    ("n10.moons", "How many moons each copy gets."),
    (
        "n10.radius",
        "How far the moons are from their copy, as a share of the box.",
    ),
    ("n10.size", "How big each moon is compared with its copy."),
    (
        "n11",
        "Also draws the orbit's path itself as a trail of notes.",
    ),
    ("n11.density", "How many trail notes draw one ring."),
    ("n11.vel", "The velocity of the trail notes."),
];

fn orbit_spec() -> ToolSpec {
    let mut s = xspec(
        "orbit",
        "Orbit",
        "Places copies of the selected motif (scaled to Motif size) on an ellipse around a center in time × pitch.",
        vec![
            int("copies", "Copies on the path", 1, 64, 8),
            float("cx", "Center X", -50.0, 150.0, 50.0, "%"),
            float("cy", "Center Y", -50.0, 150.0, 50.0, "%"),
            float("rx", "Radius X", 0.0, 200.0, 40.0, "%"),
            float("ry", "Radius Y", 0.0, 200.0, 40.0, "%"),
            float("size", "Motif size", 1.0, 200.0, 35.0, "%"),
            flag("keep", "Keep the original", false),
            adv_switch(1, "Circle / ellipse family (superellipse)"),
            float(
                "a1.exp",
                "Shape exponent (2 = ellipse, 1 = diamond)",
                0.3,
                10.0,
                4.0,
                "",
            )
            .adv(1),
            adv_switch(2, "Start / end angle"),
            float("a2.from", "Start angle", -360.0, 360.0, 0.0, "°").adv(2),
            float("a2.to", "End angle", -360.0, 360.0, 180.0, "°").adv(2),
            adv_switch(3, "Multiple rings"),
            int("a3.rings", "Rings", 2, 8, 3).adv(3),
            float("a3.scale", "Radius per ring", 10.0, 200.0, 60.0, "%").adv(3),
            adv_switch(4, "Spiral radius"),
            float("a4.end", "Radius at the last copy", 0.0, 400.0, 30.0, "%").adv(4),
            adv_switch(5, "Tangent alignment (copies turn with the path)"),
            adv_switch(6, "Velocity by angle"),
            curve(
                "a6.curve",
                "Velocity around the path",
                Curve::line(1.0, 0.3),
            )
            .overshoot()
            .adv(6),
            adv_switch(7, "Gate by angle"),
            curve(
                "a7.curve",
                "Length around the path (0.5 = unchanged)",
                Curve::line(0.5, 0.15),
            )
            .overshoot()
            .adv(7),
            adv_switch(8, "Subchannel by sector"),
            int("a8.sectors", "Sectors", 1, 64, 4).adv(8),
            adv_switch(9, "Eccentricity automation"),
            curve(
                "a9.curve",
                "Radius Y factor along the path (0.5 = unchanged)",
                Curve::line(0.5, 1.0),
            )
            .overshoot()
            .adv(9),
            adv_switch(10, "Orbital resonance (Lissajous path)"),
            int("a10.p", "Time frequency", 1, 12, 3).adv(10),
            int("a10.q", "Pitch frequency", 1, 12, 2).adv(10),
            // V09-S40 step 5: functions declared by stable id (TOOL_CATALOG
            // n01 … n11; the v0.9 kernel is `orbit.rs`).
            flag("n01", "Custom path", false),
            curve("n01.x", "Path X", Curve::line(0.0, 1.0)),
            curve(
                "n01.y",
                "Path Y",
                Curve::new(vec![
                    (0.0, 0.5),
                    (0.25, 1.0),
                    (0.5, 0.5),
                    (0.75, 0.0),
                    (1.0, 0.5),
                ]),
            ),
            flag("n02", "Tilted orbit", false),
            float("n02.tilt", "Tilt", -89.0, 89.0, 60.0, "°"),
            float("n02.depth", "Depth scale", 0.0, 100.0, 50.0, "%"),
            flag("n03", "Kepler spacing", false),
            float("n03.e", "Eccentricity", 0.0, 95.0, 50.0, "%"),
            float("n03.peri", "Periapsis angle", -180.0, 180.0, 0.0, "°"),
            flag("n04", "Self spin", false),
            float("n04.spin", "Spin per copy", -180.0, 180.0, 45.0, "°"),
            flag("n05", "Lagrange points", false),
            list("n05.points", "Points (1 – 5)", 1, 5, 5, &[4, 5]),
            float("n05.mass", "Mass ratio", 1.0, 100.0, 10.0, "%"),
            flag("n06", "Arc spacing", false),
            float("n06.spacing", "Spacing", 1.0, 100.0, 20.0, "%"),
            flag("n07", "Copy variation", false),
            choice("n07.mode", "Variation", super::orbit::VARIATIONS, 0),
            flag("n08", "Planet occlusion", false),
            float("n08.radius", "Body radius", 0.0, 100.0, 20.0, "%"),
            flag("n09", "Precession", false),
            float("n09.angle", "Precession per ring", -180.0, 180.0, 30.0, "°"),
            choice("n09.along", "Along", super::orbit::ALONG, 0),
            flag("n10", "Moons", false),
            int("n10.moons", "Moons", 1, 8, 2),
            float("n10.radius", "Moon radius", 1.0, 100.0, 10.0, "%"),
            float("n10.size", "Moon size", 1.0, 100.0, 40.0, "%"),
            flag("n11", "Orbit line", false),
            int("n11.density", "Trail density (notes per ring)", 1, 256, 32),
            int("n11.vel", "Trail velocity", 1, 127, 40),
        ],
        [
            "Circle / ellipse family",
            "Start / end angle",
            "Multiple rings",
            "Spiral radius",
            "Tangent alignment",
            "Velocity by angle",
            "Gate by angle",
            "Subchannel by sector",
            "Eccentricity automation",
            "Orbital resonance ratios",
        ],
        true,
    );
    s.functions = [
        ("n01", "Custom path", 110),
        ("n02", "Tilted orbit", 120),
        ("n03", "Kepler spacing", 130),
        ("n04", "Self spin", 140),
        ("n05", "Lagrange points", 150),
        ("n06", "Arc spacing", 160),
        ("n07", "Copy variation", 170),
        ("n08", "Planet occlusion", 180),
        ("n09", "Precession", 190),
        ("n10", "Moons", 200),
        ("n11", "Orbit line", 210),
    ]
    .into_iter()
    .map(|(id, name, order)| FunctionSpec { id, name, order })
    .collect();
    with_help(&mut s.params, ORBIT_HELP);
    s
}

/// One placed copy: centre, rotation, scale, velocity / gate factors, subchannel steps.
#[derive(Clone, Copy, Debug)]
struct Placed {
    cx: f64,
    cy: f64,
    rot: f64,
    scale: f64,
    vel: f64,
    gate: f64,
    sub: i64,
}

fn orbit_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    // V09-S40 step 5: the new functions and Control Sweeps (per note) run in
    // the v0.9 kernel; with every one off this v0.8 kernel runs unchanged.
    if super::orbit::NEW.iter().any(|k| p.flag(k))
        || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty())
    {
        return super::orbit::orb9_build(p, b);
    }
    let g = Geom::from_params(p, b.stats);
    let k = p.int("copies").clamp(1, 64) as usize;
    let (cx, cy) = (p.float("cx") / 100.0, p.float("cy") / 100.0);
    let (rx, ry) = (p.float("rx") / 100.0, p.float("ry") / 100.0);
    let size = p.float("size") / 100.0;
    let sexp = p.adv(1).then(|| p.float("a1.exp").max(0.05));
    let (a0, a1, full) = if p.adv(2) {
        (
            p.float("a2.from") * PI / 180.0,
            p.float("a2.to") * PI / 180.0,
            false,
        )
    } else {
        (0.0, 2.0 * PI, true)
    };
    let (rings, rscale) = if p.adv(3) {
        (
            p.int("a3.rings").clamp(2, 8) as usize,
            p.float("a3.scale") / 100.0,
        )
    } else {
        (1, 1.0)
    };
    let spiral = p.adv(4).then(|| p.float("a4.end") / 100.0);
    let tangent = p.adv(5);
    let vcurve = p.adv(6).then(|| p.curve("a6.curve"));
    let gcurve = p.adv(7).then(|| p.curve("a7.curve"));
    let sectors = p.adv(8).then(|| p.int("a8.sectors").max(1) as usize);
    let ecc = p.adv(9).then(|| p.curve("a9.curve"));
    let lissa = p
        .adv(10)
        .then(|| (p.int("a10.p").max(1) as f64, p.int("a10.q").max(1) as f64));
    let mut placed: Vec<Placed> = Vec::new();
    if p.flag("keep") {
        placed.push(Placed {
            cx: 0.5,
            cy: 0.5,
            rot: 0.0,
            scale: 1.0,
            vel: 1.0,
            gate: 1.0,
            sub: 0,
        });
    }
    for ring in 0..rings {
        let rf = rscale.powi(ring as i32);
        for i in 0..k {
            let f = if full {
                i as f64 / k as f64
            } else if k == 1 {
                0.0
            } else {
                i as f64 / (k - 1) as f64
            };
            let th = a0 + (a1 - a0) * f;
            let mut r = rf;
            if let Some(end) = spiral {
                r *= 1.0 + (end - 1.0) * f;
            }
            let ryk = ecc.as_ref().map_or(ry, |c| ry * c.eval(f) * 2.0);
            let (c, s) = match lissa {
                Some((pp, qq)) => ((pp * th).cos(), (qq * th).sin()),
                None => (th.cos(), th.sin()),
            };
            let (c, s) = match sexp {
                Some(e) => (
                    c.signum() * c.abs().powf(2.0 / e),
                    s.signum() * s.abs().powf(2.0 / e),
                ),
                None => (c, s),
            };
            placed.push(Placed {
                cx: cx + rx * r * c,
                cy: cy + ryk * r * s,
                rot: if tangent { th + PI / 2.0 } else { 0.0 },
                scale: size,
                vel: vcurve.as_ref().map_or(1.0, |cv| cv.eval(f)),
                gate: gcurve.as_ref().map_or(1.0, |cv| cv.eval(f) * 2.0),
                sub: sectors.map_or(0, |n| ((f * n as f64) as i64).min(n as i64 - 1)),
            });
        }
    }
    let tracks = b.tracks.to_vec();
    let tr = tracks.clone();
    let pl = placed.clone();
    copies_kernel(
        g,
        tracks,
        placed.len(),
        move |n, i| {
            let c = pl[i];
            let mut t = sub_stepped(&tr, n, c.sub);
            t.vel = vel_scaled(n, c.vel);
            t.dur = ((n.dur as f64 * c.gate).round() as u64).max(1);
            Some(t)
        },
        move |_, i, u, v| {
            let c = placed[i];
            let (x, y) = rot((u - 0.5) * c.scale, (v - 0.5) * c.scale, c.rot);
            (c.cx + x, c.cy + y)
        },
        // V09-0143: the orbit copy (ring by ring).
        |i| u32::try_from(i).unwrap_or(u32::MAX),
    )
}

// ============================================================== Vortex Trails

pub const VORTEX_TRAILS: ToolDef = ToolDef {
    id: "vortex_trails",
    spec: vortex_spec,
    build: vortex_build,
};

/// B1-0066: the tooltips of this tool's controls (each checked against its kernel).
const VORTEX_TRAILS_HELP: &[(&str, &str)] = &[
    (
        "cx",
        "Where the vortex's center lies in time, as a share of the selection's box.",
    ),
    (
        "cy",
        "Where the vortex's center lies in pitch, as a share of the selection's box.",
    ),
    (
        "rx",
        "How far the vortex reaches in time from its center, as a share of the box.",
    ),
    (
        "ry",
        "How far the vortex reaches in pitch from its center, as a share of the box.",
    ),
    (
        "spacing",
        "How much further each trail is turned than the previous one, in degrees (most near the center).",
    ),
    (
        "a1",
        "Sets how many trails follow the original instead of three.",
    ),
    ("a1.count", "How many trailing copies are made."),
    (
        "a2",
        "Thins each trail more than the previous one, so the trails fade out.",
    ),
    (
        "a2.keep",
        "The share of notes each trail keeps from the previous one.",
    ),
    (
        "a3",
        "Spaces the trails' turns by a curve instead of evenly.",
    ),
    (
        "a3.curve",
        "How far each trail turns, from the first (left) to the last (right), as a share of the last trail's turn.",
    ),
    ("a3.total", "How far the last trail is turned, in degrees."),
    (
        "a4",
        "Makes each trail larger or smaller around the vortex than the previous one.",
    ),
    (
        "a4.step",
        "How much each trail's size changes, as a share of the box.",
    ),
    (
        "a5",
        "Makes each trail's notes shorter (or longer) than the previous trail's.",
    ),
    (
        "a5.decay",
        "Each trail's note length as a share of the previous trail's.",
    ),
    (
        "a6",
        "Makes each trail quieter (or louder) than the previous one.",
    ),
    (
        "a6.decay",
        "Each trail's velocity as a share of the previous trail's.",
    ),
    ("a7", "Moves each trail to another subchannel (color)."),
    (
        "a7.step",
        "How many subchannel positions each trail moves from the previous one.",
    ),
    (
        "a8",
        "Adds trails turned the other way too, so they spread in both directions.",
    ),
    (
        "a9",
        "Spaces the trails along a logarithmic spiral instead of a circle.",
    ),
    (
        "a9.k",
        "How much the spiral grows per radian of turn (negative = it shrinks).",
    ),
    (
        "a10",
        "Adds a second vortex that twirls the trails as well.",
    ),
    (
        "a10.x",
        "Where the second vortex lies in time, as a share of the box.",
    ),
    (
        "a10.y",
        "Where the second vortex lies in pitch, as a share of the box.",
    ),
    (
        "n01",
        "Delays each trail a little more in time than the previous one.",
    ),
    (
        "n01.delay",
        "How many ticks later each trail starts than the previous one.",
    ),
    (
        "n02",
        "Transposes each trail a little more than the previous one.",
    ),
    (
        "n02.semis",
        "How many semitones each trail moves from the previous one.",
    ),
    ("n03", "Fills the vortex's core with a cluster of notes."),
    ("n03.radius", "The core's radius, as a share of the box."),
    ("n03.density", "How many notes per beat fill the core."),
    (
        "n04",
        "Uses two vortices that move together and merge into one along the trails.",
    ),
    ("n04.at", "How far along the trails the two vortices merge."),
    (
        "n04.final",
        "The merged vortex's strength, as a share of one vortex's.",
    ),
    ("n05", "Places the trails on a rhythm grid."),
    ("n05.grid", "The grid the trails are placed on."),
    ("n05.per_beat", "How many trails start per beat."),
    (
        "n06",
        "Tilts each trail a little more than the previous one, as if it turns in depth.",
    ),
    (
        "n06.tilt",
        "How many degrees each trail tilts more than the previous one.",
    ),
    (
        "n07",
        "Makes each trail play only a moving window of the motif, like a comet's tail.",
    ),
    ("n07.window", "How much of the motif each trail plays."),
    (
        "n07.overlap",
        "How much each trail's window overlaps the previous one's.",
    ),
    (
        "n08",
        "Makes each trail recede toward the vortex's center, as if it moves into the distance.",
    ),
    (
        "n08.depth",
        "How far each trail recedes compared with the previous one.",
    ),
    ("n08.fade", "How much quieter the receding trails get."),
    (
        "n09",
        "Places trails before the motif in time, or on both sides of it.",
    ),
    (
        "n09.side",
        "Where the trails go: after the motif, before it, or both.",
    ),
    ("n10", "Keeps only every Nth trail, like a strobe."),
    ("n10.every", "Keeps one trail out of this many."),
    (
        "n10.offset",
        "Which trail of each group is kept (0 = the first).",
    ),
    (
        "n11",
        "Changes the motif gradually toward its inversion or retrograde along the trails.",
    ),
    (
        "n11.target",
        "What the motif changes toward: its upside-down inversion, its backward retrograde, or both.",
    ),
    (
        "n11.progress",
        "How far each trail moves toward the target.",
    ),
];

fn vortex_spec() -> ToolSpec {
    let mut s = xspec(
        "vortex_trails",
        "Vortex Trails",
        "Twirls copies of the selection around a vortex: each trail is turned further (most near the center), leaving three trailing copies behind the original.",
        vec![
            float("cx", "Vortex X", -50.0, 150.0, 50.0, "%"),
            float("cy", "Vortex Y", -50.0, 150.0, 50.0, "%"),
            float("rx", "Radius X", 5.0, 200.0, 60.0, "%"),
            float("ry", "Radius Y", 5.0, 200.0, 60.0, "%"),
            float("spacing", "Turn per trail", -180.0, 180.0, 30.0, "°"),
            seed("seed"),
            adv_switch(1, "Trail count"),
            int("a1.count", "Trails", 1, 32, 6).adv(1),
            adv_switch(2, "Trail decay (thinning)"),
            float("a2.keep", "Notes kept per trail", 5.0, 100.0, 70.0, "%").adv(2),
            adv_switch(3, "Angular spacing curve"),
            curve(
                "a3.curve",
                "Turn of each trail (fraction of all trails' turn)",
                Curve::line(0.0, 1.0),
            )
            .overshoot()
            .adv(3),
            float(
                "a3.total",
                "Turn of the last trail",
                -720.0,
                720.0,
                270.0,
                "°",
            )
            .adv(3),
            adv_switch(4, "Radial spacing"),
            float("a4.step", "Radius change per trail", -50.0, 50.0, 8.0, "%").adv(4),
            adv_switch(5, "Gate decay"),
            float("a5.decay", "Length per trail", 5.0, 150.0, 80.0, "%").adv(5),
            adv_switch(6, "Velocity decay"),
            float("a6.decay", "Velocity per trail", 5.0, 150.0, 80.0, "%").adv(6),
            adv_switch(7, "Subchannel trail cycling"),
            int("a7.step", "Subchannel positions per trail", -16, 16, 1).adv(7),
            adv_switch(8, "Reverse trails (both directions)"),
            adv_switch(9, "Logarithmic trails"),
            float("a9.k", "Growth per radian", -100.0, 100.0, 25.0, "%").adv(9),
            adv_switch(10, "Dual-vortex trails"),
            float("a10.x", "Second vortex X", -50.0, 150.0, 25.0, "%").adv(10),
            float("a10.y", "Second vortex Y", -50.0, 150.0, 75.0, "%").adv(10),
            // V09-S40 step 6: functions declared by stable id (TOOL_CATALOG
            // n01 … n11; the v0.9 kernel is `vortex.rs`).
            flag("n01", "Trail delay", false),
            ticks("n01.delay", "Delay per trail", 0, 1 << 30, 240),
            flag("n02", "Trail transpose", false),
            int("n02.semis", "Semitones per trail", -24, 24, 2),
            flag("n03", "Core fill", false),
            float("n03.radius", "Core radius", 1.0, 100.0, 25.0, "%"),
            float(
                "n03.density",
                "Density (notes per beat)",
                0.1,
                64.0,
                8.0,
                "",
            ),
            flag("n04", "Vortex merger", false),
            float("n04.at", "Merge at", 0.0, 100.0, 50.0, "%"),
            float("n04.final", "Final strength", 0.0, 200.0, 150.0, "%"),
            flag("n05", "Trail rhythm", false),
            choice("n05.grid", "Grid", super::art_grid_values(), 2),
            int("n05.per_beat", "Trails per beat", 1, 16, 4),
            flag("n06", "Trail tilt", false),
            float("n06.tilt", "Tilt per trail", -90.0, 90.0, 30.0, "°"),
            flag("n07", "Comet segments", false),
            float("n07.window", "Window per trail", 1.0, 100.0, 30.0, "%"),
            float("n07.overlap", "Overlap", 0.0, 100.0, 50.0, "%"),
            flag("n08", "Trail depth", false),
            float("n08.depth", "Depth per trail", 0.0, 50.0, 20.0, "%"),
            float("n08.fade", "Velocity fade", 0.0, 100.0, 50.0, "%"),
            flag("n09", "Pre-trails", false),
            choice("n09.side", "Direction", super::vortex::SIDES, 1),
            flag("n10", "Trail strobe", false),
            int("n10.every", "Keep every", 1, 16, 2),
            int("n10.offset", "Offset", 0, 15, 0),
            flag("n11", "Trail morph", false),
            choice("n11.target", "Target", super::vortex::TARGETS, 0),
            float("n11.progress", "Progress per trail", 0.0, 100.0, 25.0, "%"),
        ],
        [
            "Trail count",
            "Trail decay",
            "Angular spacing",
            "Radial spacing",
            "Gate decay",
            "Velocity decay",
            "Subchannel trail cycling",
            "Reverse trails",
            "Logarithmic trails",
            "Dual-vortex trails",
        ],
        true,
    );
    s.functions = [
        ("n01", "Trail delay", 110),
        ("n02", "Trail transpose", 120),
        ("n03", "Core fill", 130),
        ("n04", "Vortex merger", 140),
        ("n05", "Trail rhythm", 150),
        ("n06", "Trail tilt", 160),
        ("n07", "Comet segments", 170),
        ("n08", "Trail depth", 180),
        ("n09", "Pre-trails", 190),
        ("n10", "Trail strobe", 200),
        ("n11", "Trail morph", 210),
    ]
    .into_iter()
    .map(|(id, name, order)| FunctionSpec { id, name, order })
    .collect();
    with_help(&mut s.params, VORTEX_TRAILS_HELP);
    s
}

fn vortex_build(p: &Params, b: &BuildCtx) -> Result<Box<dyn Kernel>, TransformError> {
    // V09-S40 step 6: the new functions and Control Sweeps (per note) run in
    // the v0.9 kernel; with every one off this v0.8 kernel runs unchanged.
    if super::vortex::NEW.iter().any(|k| p.flag(k))
        || b.ctx.sweeps.as_ref().is_some_and(|s| !s.is_empty())
    {
        return super::vortex::vor9_build(p, b);
    }
    let g = Geom::from_params(p, b.stats);
    let c1 = (p.float("cx") / 100.0, p.float("cy") / 100.0);
    let (rx, ry) = (
        (p.float("rx") / 100.0).max(1e-3),
        (p.float("ry") / 100.0).max(1e-3),
    );
    let trails = if p.adv(1) {
        p.int("a1.count").clamp(1, 32) as usize
    } else {
        3
    };
    let spacing = p.float("spacing") * PI / 180.0;
    let keep = p.adv(2).then(|| p.float("a2.keep") / 100.0);
    let curve_spacing = p
        .adv(3)
        .then(|| (p.curve("a3.curve"), p.float("a3.total") * PI / 180.0));
    let dr = if p.adv(4) {
        p.float("a4.step") / 100.0
    } else {
        0.0
    };
    let gd = if p.adv(5) {
        p.float("a5.decay") / 100.0
    } else {
        1.0
    };
    let vd = if p.adv(6) {
        p.float("a6.decay") / 100.0
    } else {
        1.0
    };
    let sub_step = if p.adv(7) { p.int("a7.step") } else { 0 };
    let both = p.adv(8);
    let logk = p.adv(9).then(|| p.float("a9.k") / 100.0);
    let c2 = p
        .adv(10)
        .then(|| (p.float("a10.x") / 100.0, p.float("a10.y") / 100.0));
    let seed = p.seed("seed");
    // Copy 0 = the original; then trails 1..=T (and -1..=-T when reversed).
    let copies = 1 + trails * if both { 2 } else { 1 };
    let trail_of = move |k: usize| -> (usize, f64) {
        if k == 0 {
            (0, 1.0)
        } else if k <= trails {
            (k, 1.0)
        } else {
            (k - trails, -1.0)
        }
    };
    let angle_of = move |j: usize| -> f64 {
        match &curve_spacing {
            Some((c, total)) => total * c.eval(j as f64 / trails as f64),
            None => spacing * j as f64,
        }
    };
    let tracks = b.tracks.to_vec();
    let tr = tracks.clone();
    copies_kernel(
        g,
        tracks,
        copies,
        move |n, k| {
            let (j, _) = trail_of(k);
            if j > 0
                && let Some(kp) = keep
                && !Rng::for_note(seed, salt::KEEP ^ j as u64, n).chance(kp.powi(j as i32))
            {
                return None;
            }
            let mut t = sub_stepped(&tr, n, sub_step * j as i64);
            t.vel = vel_scaled(n, vd.powi(j as i32));
            t.dur = ((n.dur as f64 * gd.powi(j as i32)).round() as u64).max(1);
            Some(t)
        },
        move |_, k, u, v| {
            let (j, dir) = trail_of(k);
            if j == 0 {
                return (u, v);
            }
            let (centre, sign) = match c2 {
                Some(c) if j % 2 == 0 => (c, -1.0),
                _ => (c1, 1.0),
            };
            let (x, y) = ((u - centre.0) / rx, (v - centre.1) / ry);
            let r = (x * x + y * y).sqrt();
            let w = (1.0 - r).max(0.0);
            let a = angle_of(j) * dir * sign * w;
            let (mut x2, mut y2) = rot(x, y, a);
            let mut s = 1.0 + dr * j as f64;
            if let Some(kk) = logk {
                s *= (kk * a.abs()).exp();
            }
            x2 *= s;
            y2 *= s;
            (centre.0 + x2 * rx, centre.1 + y2 * ry)
        },
        // V09-0143: the trail (0 = the original; reversed trails count
        // from the note too).
        move |k| trail_of(k).0 as u32,
    )
}
