//! The tool kernels, grouped by family. Every tool is a [`ToolDef`]: a spec
//! function (declarative parameters, advanced-function names) and a build
//! function that turns parameter values into a [`Kernel`].
//!
//! Two kernel shapes cover every tool:
//! * [`Pointwise`] — a per-note rule (0..k outputs per note) with a bounded time
//!   reach; previews lazily over the viewport;
//! * [`Contextual`] — a rule over the whole source (chords, neighbours,
//!   statistics, generators); previews from a worker run.

pub mod brush;
pub mod claw;
pub mod displacement_map;
pub mod duplicate;
pub mod edit;
pub mod editors;
pub mod fl;
pub mod flip;
pub mod flow;
pub mod geometry;
pub mod gravity;
pub mod group;
pub mod harmony;
pub mod kaleido;
pub mod mask;
pub mod note_outline;
pub mod notes;
pub mod orbit;
pub mod paint;
pub mod particle;
pub mod paste_repeat;
pub mod plugins;
pub mod pulse;
pub mod riff;
pub mod ripple;
pub mod slide;
pub mod snap;
pub mod space3d;
pub mod stretch;
pub mod tiny_planet;
pub mod trails;
pub mod tumours;
pub mod vortex;
pub mod warp_transform;
pub mod xfield;
pub mod xgeom;
pub mod xgrid;

use super::registry::ToolDef;
use super::{Growth, Kernel, Output, PointKernel, Source, SourceStats, TNote, TransformError};

/// Kernels behind one-click commands (`registry::command_tool`): never
/// listed as tools.
pub static COMMANDS: &[ToolDef] = &[notes::MUTE_NOTES, edit::SLICE_NOTES, edit::DUPLICATE_OCTAVE];

/// Every registered tool, in menu order.
pub static ALL: &[ToolDef] = &[
    fl::ARPEGGIATE,
    fl::ARTICULATE,
    fl::CHOP,
    claw::CLAW,
    fl::QUANTIZE,
    fl::RANDOMIZE,
    fl::STRUM,
    fl::FLAM,
    fl::LIMIT,
    harmony::CHORD_PROGRESSION,
    harmony::CHORD_STAMP,
    riff::RIFF,
    edit::DELETE,
    edit::SELECT,
    brush::DRAW,
    brush::PAINT,
    edit::SLICE,
    edit::SNAP_SCALE,
    edit::FLIP,
    edit::DUPLICATE,
    edit::STRETCH,
    editors::HUMANIZE,
    editors::TRANSPOSE,
    editors::LEGATO,
    notes::NOTE_PROPS,
    notes::JOIN,
    notes::REPEAT,
    group::GROUP,
    slide::SLIDE,
    geometry::SKEW,
    geometry::DISTORT,
    geometry::PERSPECTIVE,
    geometry::WARP,
    geometry::SHEAR,
    geometry::TWIRL,
    geometry::WAVE,
    // V09-S44: Warp Transform (contract §124 – §131).
    warp_transform::WARP_TRANSFORM,
    // V09-S45: Trails (contract §132 – §135).
    trails::TRAILS,
    // V09-S46: Note Outline (contract §136 – §138).
    note_outline::NOTE_OUTLINE,
    // V09-S47: Displacement Map (contract §139 – §143).
    displacement_map::DISPLACEMENT_MAP,
    // V09-S48: Tiny Planet (contract §144 – §145).
    tiny_planet::TINY_PLANET,
    // V09-S49: Ripple (contract §146 – §148).
    ripple::RIPPLE,
    mask::CLIP_MASK,
    mask::PAINT_COLOR,
    paint::PAINT,
    xgeom::GRAVITY_WELL,
    xgeom::KALEIDOSCOPE,
    xgeom::ORBIT,
    xgeom::VORTEX_TRAILS,
    xfield::PULSE_FIELD,
    xfield::PARTICLE_BURST,
    xfield::FLOW_FIELD,
    // V09-S43: the 3D tool (contract §120 – §123).
    space3d::TOOL3D,
    // V09-S50: Tumours (contract §149, §150, §255).
    tumours::TUMOURS,
    plugins::HZ_CHOP,
    plugins::SAFOR,
    paste_repeat::PASTE_REPEAT_NOTES,
    // V09-S51: the Note Generators (Simple, then Advanced).
    super::generator::lines::LINES,
    super::generator::rect::RECT,
    super::generator::circles::CIRCLES,
    super::generator::arcs::ARCS,
    super::generator::polygons::POLYGONS,
    super::generator::custom::CUSTOM,
    super::generator::parametric::PARAMETRIC,
    super::generator::lsystem::LSYSTEM,
    super::generator::spirograph::SPIROGRAPH,
    super::generator::growth::GROWTH,
];

/// v1.0-beta2 B2-F16: whether tool `id`'s source preparation can read or
/// write tracks outside its scope (a mask, map, reference or destination
/// track, the visible tracks) — its result then depends on what those
/// tracks show, so a preview of it is keyed by
/// [`PlumDocument::analysis_revision`](crate::model::PlumDocument::analysis_revision)
/// (every placement and content) besides the scope's notes. Every arm of
/// [`prepare`] that reads another track is listed.
pub fn reads_other_tracks(id: &str) -> bool {
    matches!(
        id,
        "draw"
            | "paint"
            | "snap_scale"
            | "clip_mask"
            | "displacement_map"
            | "tumours"
            | "gen.growth"
            | "articulate"
            | "chop"
            | "quantize"
            | "randomize"
            | "warp"
            | "shear"
            | "wave"
            | "subpaint"
            | "duplicate"
    )
}

/// Tool-specific source preparation (auxiliary tracks, destinations), run by
/// [`super::run::collect_with`] after the selection is collected.
pub fn prepare(
    id: &str,
    doc: &crate::model::PlumDocument,
    scopes: &[super::TrackScope],
    p: &super::Params,
    src: &mut Source,
    limit: usize,
    ctx: Option<&super::Ctx>,
) -> Result<(), TransformError> {
    match id {
        "draw" => brush::draw_prepare(doc, p, src, limit, ctx),
        "paint" => brush::paint_prepare(doc, p, src, limit, ctx),
        "snap_scale" => snap::snap_prepare(doc, p, src, limit, ctx),
        "clip_mask" => mask::prepare(doc, scopes, p, src, limit, ctx),
        "displacement_map" => displacement_map::prepare(doc, scopes, p, src, limit, ctx),
        "tumours" => tumours::prepare(doc, scopes, p, src, limit, ctx),
        "pulse_field" => pulse::prepare(doc, p, src, limit),
        "particle_burst" => particle::prepare(doc, p, src, limit),
        "flow_field" => flow::prepare(doc, p, src, limit),
        "gen.growth" => super::generator::growth::prepare(doc, scopes, p, src, limit, ctx),
        "articulate" => fl::articulate_prepare(doc, p, src, limit, ctx),
        "chop" => fl::chop_prepare(doc, p, src, limit, ctx),
        "quantize" => fl::quantize_prepare(doc, p, src, limit, ctx),
        "randomize" => fl::randomize_prepare(doc, p, src, limit, ctx),
        "slide" => slide::slide_prepare(doc, p, src),
        "transpose" => editors::transpose_prepare(doc, p, src),
        "warp" => geometry::warp_prepare(doc, p, src, limit, ctx),
        "shear" => geometry::shear_prepare(doc, p, src, limit, ctx),
        "wave" => geometry::wave_prepare(doc, p, src, limit, ctx),
        "subpaint" => paint::subpaint_prepare(doc, p, src, limit, ctx),
        "delete" => edit::delete_prepare(doc, p, src, limit),
        "select" => edit::select_prepare(doc, p, src),
        "duplicate" => duplicate::duplicate_prepare(doc, p, src, limit, ctx),
        _ => Ok(()),
    }
}

/// A per-note kernel built from a closure.
pub struct Pointwise<F: Fn(&TNote, &mut Output) + Send + Sync> {
    pub f: F,
    /// Largest distance a result can start before its source note.
    pub ahead: u64,
    /// Largest distance a result can end after its source note's end.
    pub behind: u64,
    pub growth: Growth,
}

impl<F: Fn(&TNote, &mut Output) + Send + Sync> Kernel for Pointwise<F> {
    fn growth(&self) -> Growth {
        self.growth
    }

    fn as_point(&self) -> Option<&dyn PointKernel> {
        Some(self)
    }
}

impl<F: Fn(&TNote, &mut Output) + Send + Sync> PointKernel for Pointwise<F> {
    fn map(&self, n: &TNote, out: &mut Output) {
        (self.f)(n, out)
    }

    fn source_window(&self, t0: u64, t1: u64) -> (u64, u64) {
        (
            t0.saturating_sub(self.behind),
            t1.saturating_add(self.ahead),
        )
    }
}

/// A kernel over the whole source.
pub struct Contextual<F: Fn(&Source, &mut Output) -> Result<(), TransformError> + Send + Sync> {
    pub f: F,
    pub growth: Growth,
    /// Optional tighter estimate (e.g. generators computing their exact count).
    pub estimate: Option<u64>,
}

impl<F: Fn(&Source, &mut Output) -> Result<(), TransformError> + Send + Sync> Kernel
    for Contextual<F>
{
    fn growth(&self) -> Growth {
        self.growth
    }

    fn estimate(&self, stats: &SourceStats) -> super::Estimate {
        match self.estimate {
            Some(n) => super::Estimate::for_notes(stats.count as u64, n),
            None => super::Estimate::from_growth(self.growth, stats),
        }
    }

    fn run(&self, src: &Source, out: &mut Output) -> Result<(), TransformError> {
        (self.f)(src, out)?;
        out.check()
    }
}

pub fn boxed<K: Kernel + 'static>(k: K) -> Result<Box<dyn Kernel>, TransformError> {
    Ok(Box::new(k))
}

/// Position of a note in the selection's time span, 0–1.
#[inline]
pub fn pos(n: &TNote, s: &SourceStats) -> f64 {
    let span = s.max_start.saturating_sub(s.min_start);
    if span == 0 {
        0.0
    } else {
        (n.start.saturating_sub(s.min_start)) as f64 / span as f64
    }
}

/// Smooth deterministic 1-D noise in [-1, 1] with correlation length `period`
/// (value noise, cosine interpolation) — "random walk" values that depend only
/// on the position, so lazy previews agree with Apply.
pub fn smooth_noise(seed: u64, salt: u64, x: u64, period: u64) -> f64 {
    use super::rng::Rng;
    let period = period.max(1);
    let k = x / period;
    let f = (x % period) as f64 / period as f64;
    let a = Rng::for_index(seed, salt, k).unit() * 2.0 - 1.0;
    let b = Rng::for_index(seed, salt, k + 1).unit() * 2.0 - 1.0;
    let w = (1.0 - (f * std::f64::consts::PI).cos()) * 0.5;
    a + (b - a) * w
}

/// Musical note values offered for steps and units.
pub const NOTE_VALUES: &[&str] = &[
    "1 bar",
    "1/2",
    "1/4",
    "1/8",
    "1/16",
    "1/32",
    "1/64",
    "1/4 triplet",
    "1/8 triplet",
    "1/16 triplet",
    "Snap",
    "Custom ticks",
];

/// The note values without "Custom ticks" (grids of the v0.9 functions:
/// metric positions, swing units, accent grids).
pub fn art_grid_values() -> &'static [&'static str] {
    &NOTE_VALUES[..NOTE_VALUES.len() - 1]
}

/// Notes of one track slot, in time order (indices into `src.notes`).
pub fn per_track(src: &super::Source) -> Vec<Vec<usize>> {
    let mut v = vec![Vec::new(); src.tracks.len()];
    for (i, n) in src.notes.iter().enumerate() {
        v[usize::from(n.track)].push(i);
    }
    v
}

/// Ticks of note value `choice` (custom ticks for the last entry), ≥ 1.
pub fn note_value(choice: usize, custom: u64, ctx: &super::Ctx) -> u64 {
    let q = ctx.ppq;
    let v = match choice {
        0 => q * 4,
        1 => q * 2,
        2 => q,
        3 => q / 2,
        4 => q / 4,
        5 => q / 8,
        6 => q / 16,
        7 => q * 2 / 3,
        8 => q / 3,
        9 => q / 6,
        10 => ctx.snap,
        _ => custom,
    };
    v.max(1)
}

/// Time multiplication (FL: right = slower ×1–×4, left = faster ×2–×8).
pub const TIME_MULS: &[&str] = &[
    "×8 faster",
    "×4 faster",
    "×2 faster",
    "×1",
    "×2 slower",
    "×3 slower",
    "×4 slower",
];

/// (numerator, denominator) of a time multiplication choice.
pub fn time_mul(choice: usize) -> (u64, u64) {
    match choice {
        0 => (1, 8),
        1 => (1, 4),
        2 => (1, 2),
        4 => (2, 1),
        5 => (3, 1),
        6 => (4, 1),
        _ => (1, 1),
    }
}

/// For every note, the next strictly later start among the notes (and
/// `extra` context notes) with the same key. O(n log n).
pub fn next_onsets<K: Ord + Copy>(
    notes: &[TNote],
    extra: &[TNote],
    key: impl Fn(&TNote) -> K,
) -> Vec<Option<u64>> {
    let mut all: Vec<(K, u64, Option<usize>)> = notes
        .iter()
        .enumerate()
        .map(|(i, n)| (key(n), n.start, Some(i)))
        .chain(extra.iter().map(|n| (key(n), n.start, None)))
        .collect();
    all.sort_by_key(|a| (a.0, a.1));
    let mut out = vec![None; notes.len()];
    let mut i = 0;
    while i < all.len() {
        let k = all[i].0;
        let mut j = i;
        while j < all.len() && all[j].0 == k {
            j += 1;
        }
        let group = &all[i..j];
        for g in group {
            if let Some(ix) = g.2 {
                let p = group.partition_point(|x| x.1 <= g.1);
                out[ix] = group.get(p).map(|x| x.1);
            }
        }
        i = j;
    }
    out
}

/// Exponential time warp of [0,1] (Claw's curve): a = 0 is identity.
#[inline]
pub fn warp(u: f64, a: f64) -> f64 {
    if a.abs() < 1e-9 {
        u
    } else {
        ((a * u).exp() - 1.0) / (a.exp() - 1.0)
    }
}

#[cfg(test)]
mod helper_tests {
    use super::*;

    fn n(start: u64, pitch: u8) -> TNote {
        TNote {
            start,
            dur: 1,
            pitch,
            vel: 1,
            ..TNote::default()
        }
    }

    #[test]
    fn next_onsets_skip_equal_starts_and_respect_keys() {
        let v = vec![n(0, 60), n(0, 64), n(10, 60), n(20, 64), n(20, 60)];
        let all = next_onsets(&v, &[], |_| 0u8);
        assert_eq!(all, vec![Some(10), Some(10), Some(20), None, None]);
        let by_pitch = next_onsets(&v, &[], |x| x.pitch);
        assert_eq!(by_pitch, vec![Some(10), Some(20), Some(20), None, None]);
        let with_ctx = next_onsets(&v, &[n(15, 0)], |_| 0u8);
        assert_eq!(with_ctx[2], Some(15));
        assert!((warp(0.25, 3.93) - 0.0335).abs() < 1e-3);
        assert_eq!(warp(0.4, 0.0), 0.4);
    }
}
