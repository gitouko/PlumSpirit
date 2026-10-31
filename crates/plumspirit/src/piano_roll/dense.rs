//! Dense-note visualization (v0.8 contract §106–§114, §123, §207–§209, §214):
//! one subsystem for the active track and Onion Skin.
//!
//! Level-of-detail ladder (never blank — §107 / §108):
//! * **L0 / L1** — exact rectangles in one batched mesh (`render::draw_exact`,
//!   up to [`ENTER_DENSE`] visible notes; hysteresis back at [`LEAVE_DENSE`]).
//! * **L2 / L3** — colour-aware tiles (`plumspirit_engine::view::tiles`): a
//!   texel shows the note the exact renderer draws on top there, in the exact
//!   renderer's colour ([`SourceColors`]), at the opacity of the notes
//!   covering it (one note: exactly its own), outlined like exact notes
//!   (v0.9 §026–§029, §175).
//! * **L4** — a neutral loading placeholder over the chunk spans holding notes
//!   (O(chunks)), drawn for any tile that is not ready and has no other
//!   stand-in; never a note colour (§025).
//!
//! Tiles are built on a background worker (§209) in priority order — visible
//! tiles from the centre out, then one tile of prefetch on each side — and
//! cached by (content identity, zoom level, index), so scrolling and
//! autoscroll reuse every still-visible tile (§111). While a tile is missing
//! the renderer draws, in order: the same content at another zoom level, the
//! previous content of the same view (the family before, then the last family
//! drawn complete: last valid frame, §109), or L4 — tick range by tick range,
//! so whatever a stand-in does not reach gets the next one (B1-0151). A
//! content identity ("family") changes with the notes' revision, colours and
//! options; the worker drops builds of families no longer shown (generation
//! check, §110). At most [`UPLOADS_PER_FRAME`] tiles are uploaded per frame
//! (§208); the cache is bounded by [`BUDGET_BYTES`] with least-recently-used
//! eviction (§214).

use egui::{Color32, Painter, Pos2, Rect, TextureHandle, TextureOptions};
use plumspirit_engine::model::{NoteStore, NoteView};
use plumspirit_engine::view::tiles::{
    self, Reading, Scratch, SourcePlace, TileColors, TileOptions, TileOut, Winner,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};

use super::Geometry;

/// Visible notes from which the dense levels are used …
pub const ENTER_DENSE: usize = plumspirit_engine::view::lod::EXACT_RENDER_LIMIT;
/// … and below which exact rectangles return (no flicker at the threshold).
pub const LEAVE_DENSE: usize = ENTER_DENSE * 3 / 4;
/// Tile textures kept (GPU memory), least recently used evicted first.
pub const BUDGET_BYTES: usize = 48 << 20;
/// v0.8 §258: the tile cache on PCs with at most [`LOW_RAM_BYTES`] of RAM
/// (rebuilding a tile is cheap; RAM is not).
pub const LOW_END_BUDGET_BYTES: usize = 24 << 20;
pub const LOW_RAM_BYTES: u64 = 3 << 30;

/// The tile cache budget for a PC with `total_ram` bytes (unknown: normal).
pub fn budget_for(total_ram: Option<u64>) -> usize {
    match total_ram {
        Some(r) if r <= LOW_RAM_BYTES => LOW_END_BUDGET_BYTES,
        _ => BUDGET_BYTES,
    }
}
/// Tiles uploaded per frame at most.
pub const UPLOADS_PER_FRAME: usize = 8;

/// B1-0151: a view that has not moved (zoom level, Onion order range) for
/// this long is at rest. While the zoom moves, tiles are built at an anchor
/// level that follows the shown level only after it rests this long …
pub const VIEW_REST: std::time::Duration = std::time::Duration::from_millis(150);
/// … or strays further than this many levels (one octave of scale) from it.
pub const ANCHOR_LEVELS: i32 = tiles::LEVELS_PER_OCTAVE as i32;

/// The level a role builds while its shown level moves (B1-0151).
#[derive(Clone, Copy, Debug)]
struct Anchor {
    /// The level built.
    level: i32,
    /// The level shown last frame and since when.
    shown: i32,
    since: std::time::Instant,
}

/// B1-0151: tile workers for a PC with `cores` hardware threads — half of
/// them, one to four: a dense view entering new ticks (pans, zooms, the
/// Onion re-ranked) builds its tiles in parallel while the UI thread, the
/// synthesizer and the playback scheduler keep the other half.
/// `PLUMSPIRIT_TILE_WORKERS` overrides it (measurements).
pub fn tile_workers(cores: usize) -> usize {
    std::env::var("PLUMSPIRIT_TILE_WORKERS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(cores / 2)
        .clamp(1, 4)
}
/// B1-0059 (Settings → Performance → Rendering Quality): visible notes up
/// to which exact rectangles are drawn — Fast / Balanced (the default, the
/// engine's [`ENTER_DENSE`]) / Detailed. Leaving the dense levels happens at
/// three quarters of it (hysteresis).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RenderQuality {
    Fast,
    #[default]
    Balanced,
    Detailed,
}

impl RenderQuality {
    pub const ALL: [RenderQuality; 3] = [
        RenderQuality::Fast,
        RenderQuality::Balanced,
        RenderQuality::Detailed,
    ];

    /// Visible notes drawn as exact rectangles at most.
    pub fn exact_limit(self) -> usize {
        match self {
            RenderQuality::Fast => ENTER_DENSE / 2,
            RenderQuality::Balanced => ENTER_DENSE,
            RenderQuality::Detailed => ENTER_DENSE * 4,
        }
    }

    /// The `settings.json` name.
    pub fn key(self) -> &'static str {
        match self {
            RenderQuality::Fast => "fast",
            RenderQuality::Balanced => "balanced",
            RenderQuality::Detailed => "detailed",
        }
    }

    pub fn from_key(k: &str) -> Option<RenderQuality> {
        RenderQuality::ALL.into_iter().find(|q| q.key() == k)
    }
}

/// B1-0059: tiles built beyond the view by default — this many before it and
/// one more after it (the direction playback and reading go).
pub const DEFAULT_PREFETCH_TILES: u8 = 1;
/// L-G01: the whole song of the Piano Roll's and the Onion's family is
/// built in the background in at most this many tiles of a coarse level
/// (2 MB at 128 keys), after the visible and prefetch tiles: a stand-in for
/// any place a pan or a jump reaches before its own tiles are ready, so the
/// L4 band shows only until the overview exists.
pub const OVERVIEW_TILES: f64 = 16.0;
/// An overview only for views at least this many levels finer (an octave).
const OVERVIEW_MIN_LEVELS: i32 = tiles::LEVELS_PER_OCTAVE as i32;

/// L-G01: a newly shown Onion family whose tiles have no stand-in keeps
/// the last valid frame (no ghosts there) this long before the L4 band
/// shows that they are still being built — most finish sooner, so the
/// Onion appears whole instead of as translucent slabs first.
pub const ONION_GRACE: std::time::Duration = std::time::Duration::from_millis(300);

/// The most prefetch tiles a setting may ask for.
pub const MAX_PREFETCH_TILES: u8 = 8;
/// Tile cache budgets a setting may ask for, in MB (0 = automatic).
pub const MIN_CACHE_MB: u32 = 8;
pub const MAX_CACHE_MB: u32 = 2048;
/// The L4 placeholder: a neutral translucent band over the time spans whose
/// tiles are still being built. It is a loading indication — deliberately
/// not a note colour, so it can never pass for notes at a reduced opacity
/// (v0.9 §025: ghost / track opacity is only ever the chosen one).
const PLACEHOLDER: Color32 = Color32::from_rgba_premultiplied(35, 35, 35, 70);
/// Most chunk spans merged into placeholder bands per tile.
const PLACEHOLDER_SPANS: usize = 4096;

/// The users of the subsystem: the Piano Roll's active track and Onion
/// Skin (one family each at a time), the Track List's clips (v0.9
/// V09-S09: any number of families — one per content window shown) and the
/// MIDI Player (v0.9 V09-S17: one family — the exported file's notes,
/// `docs/PLAYER.md` P6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Track = 0,
    Onion = 1,
    Clip = 2,
    Player = 3,
}

/// Roles (array sizes).
const ROLES: usize = 4;

/// Where ticks `[a, b)` of a family's time axis land on screen over `rows`
/// texel rows (the roll: its pitch rows; a clip: its body).
type Place<'a> = &'a dyn Fn(f64, f64, usize) -> Rect;

/// The MIDI Player's rainfall (v0.9 V09-S17, `docs/PLAYER.md` P6): time runs
/// up the screen and every key is its own column. Each tile is one mesh of
/// one quad per key column (per segment between bends): the tile's U axis
/// along screen y, V fixed at the centre of the key's texel row, so
/// neighbouring rows never bleed into a column.
pub struct Columns<'a> {
    /// (texel row, x0, x1) of every column in drawing order — white keys,
    /// then the narrower black keys over them — insets applied.
    pub cols: &'a [(usize, f32, f32)],
    /// Screen y of tick `t` (later ticks higher: smaller y).
    pub y: &'a dyn Fn(f64) -> f32,
    /// Ticks where `y` bends (tempo changes, ramp pieces), ascending: quads
    /// are split there, so `y` is exact at every bend and linear between.
    pub bends: &'a [f64],
    /// The rainfall's area (L4 bands span its width).
    pub clip: Rect,
    /// v1.0-beta1 (B1-0143, Player only): each column shades from its full
    /// colour on the left to `1 − shade` of it on the right (PFA's note
    /// body, primary → dark); 0 = flat. The Piano Roll never draws columns.
    pub shade: f32,
}

/// How a role's dense tiles are drawn row by row in the roll (v1.0-beta2
/// B2-D02): the exact renderer's row geometry — `gap` points left empty under
/// each row, `inset` above it (opaque Onion ghosts), and, with `borders`, a
/// 1-point top and bottom border on every note from the tile's border
/// colours ([`tiles::TileOut::borders`]), between every two rows whatever
/// their track or subchannel. The default (all off) draws a tile as one
/// image (the clips, short rows).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RowStyle {
    pub gap: f32,
    pub inset: f32,
    pub borders: bool,
    /// The roll's pitch grid: each row's edges where the exact renderer puts
    /// them (`hit_test::row_span`, on the physical pixel grid), so at any UI
    /// scale both renderers cover the same pixels. `None`: rows evenly
    /// through the tile's rectangle.
    pub grid: Option<RowGrid>,
}

/// The note area's top, pitch axis and physical pixels per point
/// ([`RowStyle::grid`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowGrid {
    pub notes_top: f32,
    pub axis: plumspirit_engine::view::PitchAxis,
    pub ppp: f32,
}

impl RowStyle {
    /// The exact renderer's rules for rows `row_height` points tall: the gap
    /// from 6 pt, the inset (`inset_from`: the row height from which it
    /// applies, `None`: never), borders on notes at least `min_note` points
    /// tall (4 for the active track, 3 for the Onion; `None`: no borders).
    pub fn for_rows(row_height: f64, inset_from: Option<f64>, min_note: Option<f64>) -> RowStyle {
        let gap = if row_height >= 6.0 { 1.0 } else { 0.0 };
        let inset = match inset_from {
            Some(h) if row_height >= h => 1.0,
            _ => 0.0,
        };
        RowStyle {
            gap,
            inset,
            borders: min_note.is_some_and(|m| row_height - f64::from(gap + inset) >= m),
            grid: None,
        }
    }

    /// The same rows on the roll's pitch grid of `g`.
    pub fn on(self, g: &Geometry) -> RowStyle {
        RowStyle {
            grid: Some(RowGrid {
                notes_top: g.notes.top(),
                axis: g.view.pitch,
                ppp: g.ppp,
            }),
            ..self
        }
    }

    fn is_plain(self) -> bool {
        self.gap == 0.0 && self.inset == 0.0 && !self.borders
    }
}

/// Where a role's tiles land on screen.
#[derive(Clone, Copy)]
enum Layout<'a> {
    /// Time along x, pitch rows up the rectangle (the roll, the clips), drawn
    /// with the role's [`RowStyle`].
    Rect(Place<'a>, RowStyle),
    /// The Player's key columns.
    Columns(&'a Columns<'a>),
}

/// What one clip asks of the tile service in a frame (v0.9 V09-S09,
/// `docs/TRACK_LIST.md` "Mini notes").
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipTiles {
    pub family: FamilyId,
    /// The family this clip showed before (its content changed): its tiles
    /// stand in while the new ones are built.
    pub previous: Option<FamilyId>,
    /// Tile level (`tiles::level_for` of the physical pixels per tick).
    pub level: i32,
    /// Content ticks shown, `[c0, c1)`.
    pub range: (u64, u64),
    /// Screen x of content tick `t`: `x0 + (t − origin) × ppt` (points).
    pub origin: f64,
    pub x0: f32,
    pub ppt: f64,
    /// The clip body: the tiles' rows fill its height.
    pub body: Rect,
}

/// Outline darkness of [`Tint::Played`] notes: the border is the fill ×
/// 0.2, the value `docs/references/v0_9/KIVA.md` measured (`docs/PLAYER.md`
/// P7).
pub const PLAYED_OUTLINE_DARKNESS: u8 = 90;

/// How one source's notes are coloured.
#[derive(Clone, Debug, PartialEq)]
pub enum Tint {
    /// The active track: subchannel colours of the note palette
    /// (`theme::NotePalette::entry`).
    Subchannels {
        dark: bool,
        palette: Arc<crate::ui::theme::NotePalette>,
    },
    /// A ghost track: its tint per subchannel (`theme::ghost_tints`, turned
    /// by the palette's hue offsets) at an opacity.
    Ghost {
        base: [u8; 3],
        alpha: u8,
        palette: Arc<crate::ui::theme::NotePalette>,
    },
    /// Everything one colour (the dimmed context view).
    Flat([u8; 4]),
    /// The MIDI Player (v0.9 V09-S17, `docs/PLAYER.md` P7): every source's
    /// notes in the identity colour of their (exported track, subchannel),
    /// with the tiles' own border (`outline_shade` at the outline
    /// darkness). One entry serves every source of a family.
    Played(Arc<PlayedColors>),
}

/// The MIDI Player's note colours (`docs/PLAYER.md` P7): track `t`'s
/// subchannel `s` takes palette id `base(t) + s mod count(t)` — every track
/// its own colours, as Kiva colours track × channel, while the palette
/// (Random by Seed / Manual) stays the one resolver — at full saturation:
/// the palette's velocity-127 fill, opaque, never shaded by velocity.
/// Colours are computed on first use and cached by palette id (at most
/// [`PlayedColors::TABLE`] ids; beyond, computed when met).
pub struct PlayedColors {
    palette: Arc<crate::ui::theme::NotePalette>,
    dark: bool,
    /// (palette id of subchannel 0, subchannel count) of each source's
    /// track.
    bases: Vec<(u32, u32)>,
    table: Box<[std::sync::OnceLock<[u8; 3]>]>,
}

impl PlayedColors {
    /// Palette ids cached at most.
    pub const TABLE: u32 = 1 << 16;

    pub fn new(
        palette: Arc<crate::ui::theme::NotePalette>,
        dark: bool,
        bases: Vec<(u32, u32)>,
    ) -> PlayedColors {
        let ids = bases
            .iter()
            .map(|&(b, n)| b.saturating_add(n))
            .max()
            .unwrap_or(0)
            .min(Self::TABLE);
        PlayedColors {
            palette,
            dark,
            bases,
            table: (0..ids).map(|_| std::sync::OnceLock::new()).collect(),
        }
    }

    /// The palette id of subchannel `sub` of source `source`'s track.
    pub fn id(&self, source: usize, sub: u32) -> u32 {
        let (b, n) = self.bases.get(source).copied().unwrap_or((0, 1));
        b.saturating_add(sub % n.max(1))
    }

    fn compute(&self, id: u32) -> [u8; 3] {
        let f = self.palette.entry(id, 127, self.dark).fill;
        [f.r(), f.g(), f.b()]
    }

    /// The opaque fill of a note of subchannel `sub` from source `source`.
    pub fn fill(&self, source: usize, sub: u32) -> [u8; 4] {
        let id = self.id(source, sub);
        let [r, g, b] = match self.table.get(id as usize) {
            Some(c) => *c.get_or_init(|| self.compute(id)),
            None => self.compute(id),
        };
        [r, g, b, 255]
    }
}

impl std::fmt::Debug for PlayedColors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlayedColors")
            .field("palette", &self.palette.key())
            .field("dark", &self.dark)
            .field("sources", &self.bases.len())
            .finish()
    }
}

/// Equal when every note gets the same colour (the cache aside).
impl PartialEq for PlayedColors {
    fn eq(&self, other: &Self) -> bool {
        self.palette.key() == other.palette.key()
            && self.dark == other.dark
            && self.bases == other.bases
    }
}

/// One note colour level: the exact renderer's fill, selected fill and
/// border (unmultiplied RGBA / RGB).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Swatch {
    pub fill: [u8; 4],
    pub selected: [u8; 4],
    pub border: [u8; 3],
    /// v0.9 §119: a selected note's border and muted fill at the Selection
    /// Opacity (the exact renderer's `selected_border` / `selected_muted_fill`).
    pub selected_border: [u8; 3],
    pub selected_muted: [u8; 4],
}

/// Colours of one source, by subchannel and velocity — the same colours the
/// exact renderer uses (v0.9 §029: crossing the dense threshold keeps every
/// note's colour). A subchannel's levels (128 velocities for the active
/// track, 16 velocity bands for ghosts) are computed on first use and cached
/// for subchannels below the table size (v0.8 §256: nothing is computed up
/// front for 2,000 Onion sources); others are computed when met.
/// One subchannel's colour levels per slot, filled on first use.
type SwatchTable = Box<[std::sync::OnceLock<Box<[Swatch]>>]>;

#[derive(Debug)]
pub struct SourceColors {
    tint: Tint,
    /// Allocated on the first note coloured (L-G05: 15,000 Onion sources
    /// allocated 64 slots each up front — 23 MB and 13 ms at activation —
    /// though most draw few subchannels and many none in view).
    table: std::sync::OnceLock<SwatchTable>,
    slots: usize,
}

impl Clone for SourceColors {
    fn clone(&self) -> Self {
        SourceColors::new(self.tint.clone())
    }
}

impl SourceColors {
    pub fn new(tint: Tint) -> SourceColors {
        // The active track has one source and may use many subchannels;
        // ghosts are many sources with few subchannels each.
        let slots = match tint {
            Tint::Subchannels { .. } => 4096,
            // Played colours keep their own table (by palette id).
            Tint::Played(_) => 0,
            _ => 64,
        };
        SourceColors {
            tint,
            table: std::sync::OnceLock::new(),
            slots,
        }
    }

    /// Colour levels per subchannel: velocities, or velocity bands.
    fn levels(&self) -> usize {
        match self.tint {
            Tint::Subchannels { .. } => 128,
            Tint::Ghost { .. } => 16,
            Tint::Flat(_) | Tint::Played(_) => 1,
        }
    }

    fn level_of(&self, velocity: u8) -> usize {
        let v = usize::from(velocity.min(127));
        match self.levels() {
            128 => v,
            16 => v >> 3,
            _ => 0,
        }
    }

    /// The canonical colours (`theme::note_entry`, `theme::ghost_tint`: the
    /// exact renderer's) of `sub` at `level`.
    fn compute(tint: &Tint, sub: u32, level: usize) -> Swatch {
        use crate::ui::theme::ghost_tint;
        let rgba = |c: Color32| c.to_srgba_unmultiplied();
        match tint {
            Tint::Subchannels { dark, palette } => {
                let e = palette.entry(sub, level as u8, *dark);
                Swatch {
                    fill: rgba(e.fill),
                    selected: rgba(e.selected_fill),
                    border: [e.border.r(), e.border.g(), e.border.b()],
                    selected_border: [
                        e.selected_border.r(),
                        e.selected_border.g(),
                        e.selected_border.b(),
                    ],
                    selected_muted: rgba(e.selected_muted_fill),
                }
            }
            Tint::Ghost {
                base,
                alpha,
                palette,
            } => {
                let c = ghost_tint(
                    palette,
                    Color32::from_rgb(base[0], base[1], base[2]),
                    sub,
                    level,
                );
                let fill = [c.r(), c.g(), c.b(), *alpha];
                Swatch {
                    fill,
                    selected: fill,
                    border: [c.r(), c.g(), c.b()],
                    selected_border: [c.r(), c.g(), c.b()],
                    selected_muted: fill,
                }
            }
            Tint::Flat(c) => Swatch {
                fill: *c,
                selected: *c,
                border: [c[0], c[1], c[2]],
                selected_border: [c[0], c[1], c[2]],
                selected_muted: *c,
            },
            Tint::Played(pc) => {
                let fill = pc.fill(0, sub);
                let border =
                    tiles::outline_shade([fill[0], fill[1], fill[2]], PLAYED_OUTLINE_DARKNESS);
                Swatch {
                    fill,
                    selected: fill,
                    border,
                    selected_border: border,
                    selected_muted: fill,
                }
            }
        }
    }

    /// The colours of a note of subchannel `sub` at `velocity`.
    pub fn get(&self, sub: u32, velocity: u8) -> Swatch {
        let level = self.level_of(velocity);
        if sub as usize >= self.slots {
            return Self::compute(&self.tint, sub, level);
        }
        let table = self.table.get_or_init(|| {
            (0..self.slots)
                .map(|_| std::sync::OnceLock::new())
                .collect()
        });
        match table.get(sub as usize) {
            Some(row) => row.get_or_init(|| {
                (0..self.levels())
                    .map(|l| Self::compute(&self.tint, sub, l))
                    .collect()
            })[level],
            None => Self::compute(&self.tint, sub, level),
        }
    }
}

/// The colours of a family (Send + Sync for the worker).
#[derive(Clone, Debug)]
pub struct Colors {
    pub sources: Vec<SourceColors>,
    /// Selected notes are drawn selected (active track); `false`: like the
    /// others.
    pub selection: bool,
    /// Fill of muted notes (the exact renderer's hollow ghost fill); `None`:
    /// their own colour with the opacity factor `muted`.
    pub muted_fill: Option<[u8; 4]>,
    /// Opacity factor of muted notes (0–255) when `muted_fill` is `None`.
    pub muted: u8,
    /// Sources are layers, back to front (Onion): the later source wins
    /// shared texels (§116). `false`: one layer (the active track).
    pub ranked: bool,
}

impl Colors {
    fn source(&self, source: usize) -> Option<&SourceColors> {
        self.sources.get(source).or(self.sources.last())
    }
}

impl TileColors for Colors {
    fn color(&self, source: usize, note: &NoteView) -> ([u8; 4], bool) {
        let Some(s) = self.source(source) else {
            return ([255; 4], false);
        };
        if let Tint::Played(pc) = &s.tint {
            // A played reading has no selection and no muted notes.
            return (pc.fill(source, note.sub.0), false);
        }
        let sw = s.get(note.sub.0, note.attrs.velocity());
        let muted = note.attrs.muted();
        if note.selected && self.selection {
            // The exact renderer's selected (muted) fill (`exact_paint`).
            let c = if muted {
                sw.selected_muted
            } else {
                sw.selected
            };
            return (c, true);
        }
        if muted {
            if let Some(c) = self.muted_fill {
                return (c, false);
            }
            let mut c = sw.fill;
            c[3] = (u16::from(c[3]) * u16::from(self.muted) / 255) as u8;
            return (c, false);
        }
        (sw.fill, false)
    }

    fn rank(&self, source: usize) -> u16 {
        if self.ranked {
            source.min(usize::from(u16::MAX)) as u16
        } else {
            0
        }
    }

    fn border(&self, w: &Winner) -> Option<[u8; 3]> {
        let s = self.source(w.source)?;
        matches!(s.tint, Tint::Subchannels { .. }).then(|| {
            let sw = s.get(w.sub, w.velocity);
            // `w.selected` only when this family draws the selection.
            if w.selected {
                sw.selected_border
            } else {
                sw.border
            }
        })
    }
}

/// What a family of tiles is built from.
pub struct Family {
    pub stores: Vec<Arc<NoteStore>>,
    /// v0.9 V09-S11: where each store is drawn (the Onion's clip
    /// placements); empty — or a missing entry — draws a store where it is.
    pub places: Vec<SourcePlace>,
    /// v0.9 V09-S17: how each store is read (the MIDI Player:
    /// `Reading::Played` with the track's correction, `docs/PLAYER.md` P3);
    /// empty — or a missing entry — draws what is stored. The family id
    /// must cover the readings (corrections by value).
    pub readings: Vec<Reading>,
    pub colors: Colors,
    pub opt: TileOptions,
}

impl Family {
    /// Where store `i` is drawn.
    fn place(&self, i: usize) -> SourcePlace {
        self.places.get(i).copied().unwrap_or(SourcePlace::IDENTITY)
    }

    /// The last tick any of its notes reaches where it is drawn (O(stores)).
    fn end(&self) -> u64 {
        (0..self.stores.len())
            .filter_map(|i| {
                let e = self.stores[i].max_end()?;
                let pl = self.place(i);
                Some(if pl.is_identity() {
                    e
                } else {
                    tiles::shifted(e, pl.shift)
                })
            })
            .max()
            .unwrap_or(0)
    }
}

/// L-S49: store clones a new family can share with the families of its role
/// still held ([`DenseRenderer::store_pool`]), by (content revision,
/// length): a content revision names one state of one store's notes, so a
/// clone with the same one holds the same notes. The Onion of 1,431 tracks
/// cloned every store (each chunk's selection bits allocated) whenever its
/// opacity, outlines, colours or one source changed: 35-59 ms on the
/// owner's PC at 562M notes.
#[derive(Default)]
pub struct StorePool(HashMap<(u64, usize), Arc<NoteStore>>);

impl StorePool {
    /// `store`'s notes as a family source: a held clone of the same content,
    /// else a new clone.
    pub fn share(&self, store: &NoteStore) -> Arc<NoteStore> {
        match self.0.get(&(store.content_revision(), store.len())) {
            Some(s) => Arc::clone(s),
            None => Arc::new(store.clone()),
        }
    }
}

/// [`StorePool::share`] with an optional pool (a new clone without one).
pub fn share_store(pool: Option<&StorePool>, store: &NoteStore) -> Arc<NoteStore> {
    match pool {
        Some(p) => p.share(store),
        None => Arc::new(store.clone()),
    }
}

/// Content identity of a family (store identities and revisions, colours,
/// options) — computed by the caller.
pub type FamilyId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub family: FamilyId,
    pub level: i32,
    pub index: i64,
}

struct Job {
    key: TileKey,
    family: Arc<Family>,
    role: Role,
    /// L-G01: an overview tile — built only when nothing else waits, and
    /// given up (rebuilt later) when a tile someone waits for comes in.
    low: bool,
}

struct Done {
    key: TileKey,
    out: Option<TileOut>,
    micros: u64,
    /// `TileOut::hash` of `out`, computed off the UI thread.
    hash: u64,
}

struct Shared {
    queue: Mutex<VecDeque<Job>>,
    cv: Condvar,
    quit: AtomicBool,
    /// Family shown per role (builds of other families are dropped).
    wanted: [AtomicU64; ROLES],
    /// The clip families shown by the last Track List frame (clip builds of
    /// other families are dropped).
    clips: Mutex<HashSet<FamilyId>>,
    /// L-G05: the UI the workers wake when a tile is done — the UI no longer
    /// redraws every frame while tiles build (with 15,000 Onion tracks each
    /// of those frames cost 37 – 100 ms: the "freeze"); it redraws when
    /// there is something new to show.
    wake: Mutex<Option<egui::Context>>,
    /// Queued jobs that are not overview tiles (L-G01: an overview build
    /// stops for them).
    urgent: std::sync::atomic::AtomicUsize,
}

struct Entry {
    tex: TextureHandle,
    /// B2-D02: the border colours of the texels (roll tiles with outlines).
    borders: Option<TextureHandle>,
    last_used: u64,
    occupied: bool,
    /// Content hash of the pixels (determinism checks, diagnostics).
    hash: u64,
    /// Pitch rows (128, or 256 in 256-Key Mode).
    rows: usize,
}

impl Entry {
    fn bytes(&self) -> usize {
        tiles::TILE_COLS * self.rows * 4 * (1 + usize::from(self.borders.is_some()))
    }
}

/// Diagnostics of one role (developer overlay, §207).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DenseStats {
    /// 0 / 1 exact, 2 / 3 tiles (3: overlapping notes blended), 4 coarse.
    pub lod: u8,
    pub level: i32,
    pub visible_tiles: usize,
    pub ready: usize,
    pub stand_ins: usize,
    pub coarse: usize,
    pub pending: usize,
    pub cached: usize,
    pub cache_bytes: usize,
    pub generation: u64,
    pub last_build_micros: u64,
    /// Identity of the drawn picture: the content hashes of the ready tiles
    /// in drawing order (0 while anything stands in). Constant while paused
    /// (v0.9 §031); shown in the developer overlay.
    pub picture: u64,
    /// B1-0151: share of the visible ticks under the placeholder band (no
    /// tile or stand-in there yet), 0 – 1.
    pub placeholder: f32,
    /// B2-D02: pitch rows drawn with 1-point top / bottom borders this frame.
    pub bordered_rows: usize,
    /// L-G01: visible tiles of a newly shown Onion still within
    /// [`ONION_GRACE`]: nothing is drawn there yet (the last valid frame).
    pub held: usize,
    /// L-S48: this frame's parts (µs): a new family's sources made and the
    /// families released, finished tiles taken (textures), the visible tiles
    /// and stand-ins drawn, jobs queued and the cache trimmed.
    pub make_micros: u64,
    pub receive_micros: u64,
    pub draw_micros: u64,
    pub request_micros: u64,
}

/// The dense-note renderer (one per Piano Roll).
pub struct DenseRenderer {
    tiles: HashMap<TileKey, Entry>,
    max_count: HashMap<TileKey, u32>,
    pending: HashSet<TileKey>,
    shared: Arc<Shared>,
    rx: Receiver<Done>,
    tx: Sender<Done>,
    workers: Vec<std::thread::JoinHandle<()>>,
    /// Build inline instead of on the worker (tests).
    sync: bool,
    scratch: Scratch,
    frame: u64,
    generation: u64,
    current: [Option<FamilyId>; ROLES],
    previous: [Option<FamilyId>; ROLES],
    /// B1-0151: the last family each role drew complete (every visible
    /// tile ready) — the last valid frame, which stands in even after the
    /// family changed twice before its successor's tiles were ready.
    complete: [Option<FamilyId>; ROLES],
    anchor: [Option<Anchor>; ROLES],
    /// L-G01: each role's overview level this frame (`None`: none).
    overview: [Option<i32>; ROLES],
    /// L-G01: overviews are built (tests turn them off to compare).
    overview_on: bool,
    /// When each role's family last changed ([`ONION_GRACE`]).
    changed_at: [Option<std::time::Instant>; ROLES],
    /// The last tick of each family's notes (where its overview ends).
    ends: HashMap<FamilyId, u64>,
    families: HashMap<FamilyId, Arc<Family>>,
    dense: [bool; ROLES],
    /// B2-D02: each role's row geometry this frame ([`Self::set_rows`]).
    rows: [RowStyle; ROLES],
    /// B2-D02: rows given borders by `draw_tile` since the role's draw began.
    bordered_rows: usize,
    pub stats: [DenseStats; ROLES],
    /// Tile cache budget in bytes (§214, §258: smaller on low-RAM PCs).
    budget: usize,
    /// The automatic budget for this PC ([`budget_for`]).
    auto_budget: usize,
    /// B1-0059: prefetch tiles before the view (one more after it).
    prefetch: usize,
    /// B1-0059: visible notes above which the dense levels are used.
    enter: usize,
    /// Clip families drawn in the current / last Track List frame.
    clip_live: HashSet<FamilyId>,
    /// Tiles the clips of this frame want, visible first.
    clip_wanted: Vec<(TileKey, Arc<Family>)>,
    /// Tiles stored since the renderer was made (tests, diagnostics).
    pub built: u64,
    /// The worker may start (tests keep it off to look at the queue).
    worker_allowed: bool,
}

impl Default for DenseRenderer {
    fn default() -> Self {
        Self::new(false)
    }
}

impl Drop for DenseRenderer {
    fn drop(&mut self) {
        self.shared.quit.store(true, Ordering::SeqCst);
        self.shared.cv.notify_all();
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

fn worker(shared: Arc<Shared>, tx: Sender<Done>) {
    let mut scratch = Scratch::default();
    loop {
        let job = {
            let Ok(mut q) = shared.queue.lock() else {
                return;
            };
            loop {
                if shared.quit.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(j) = q.pop_front() {
                    shared
                        .urgent
                        .store(q.iter().filter(|j| !j.low).count(), Ordering::SeqCst);
                    break j;
                }
                q = match shared.cv.wait(q) {
                    Ok(g) => g,
                    Err(_) => return,
                };
            }
        };
        let started = std::time::Instant::now();
        let stores: Vec<&NoteStore> = job.family.stores.iter().map(|s| s.as_ref()).collect();
        let role = job.role as usize;
        let fam = job.key.family;
        let low = job.low;
        let cancelled = || {
            shared.quit.load(Ordering::Relaxed)
                || (low && shared.urgent.load(Ordering::Relaxed) > 0)
                || if job.role == Role::Clip {
                    shared.clips.lock().is_ok_and(|c| !c.contains(&fam))
                } else {
                    shared.wanted[role].load(Ordering::Relaxed) != fam
                }
        };
        let out = if cancelled() {
            None
        } else {
            tiles::build_tile_read(
                &stores,
                &job.family.places,
                &job.family.readings,
                &job.family.colors,
                job.key.level,
                job.key.index,
                job.family.opt,
                &mut scratch,
                &cancelled,
            )
        };
        let micros = started.elapsed().as_micros() as u64;
        let hash = out.as_ref().map_or(0, TileOut::hash);
        let done = Done {
            key: job.key,
            out,
            micros,
            hash,
        };
        if tx.send(done).is_err() {
            return;
        }
        if let Some(c) = shared.wake.lock().ok().and_then(|w| w.clone()) {
            c.request_repaint();
        }
    }
}

impl DenseRenderer {
    /// B1-0170: at exit — tells the workers to stop and leaves the tiles to
    /// the system instead of freeing them (no join: a worker finishing a
    /// tile never holds the window open).
    pub fn abandon(self) {
        self.shared.quit.store(true, Ordering::SeqCst);
        self.shared.cv.notify_all();
        std::mem::forget(self);
    }

    /// `sync`: build tiles inline (deterministic tests, no thread).
    pub fn new(sync: bool) -> DenseRenderer {
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
            quit: AtomicBool::new(false),
            wanted: std::array::from_fn(|_| AtomicU64::new(0)),
            clips: Mutex::new(HashSet::new()),
            wake: Mutex::new(None),
            urgent: std::sync::atomic::AtomicUsize::new(0),
        });
        let (tx, rx) = channel();
        DenseRenderer {
            tiles: HashMap::new(),
            max_count: HashMap::new(),
            pending: HashSet::new(),
            shared,
            rx,
            tx,
            workers: Vec::new(),
            sync,
            scratch: Scratch::default(),
            frame: 0,
            generation: 0,
            current: [None; ROLES],
            previous: [None; ROLES],
            complete: [None; ROLES],
            anchor: [None; ROLES],
            overview: [None; ROLES],
            overview_on: true,
            changed_at: [None; ROLES],
            ends: HashMap::new(),
            families: HashMap::new(),
            dense: [false; ROLES],
            rows: [RowStyle::default(); ROLES],
            bordered_rows: 0,
            stats: [DenseStats::default(); ROLES],
            // Tests (sync) use the fixed normal budget.
            budget: if sync {
                BUDGET_BYTES
            } else {
                budget_for(crate::platform::total_memory_bytes())
            },
            auto_budget: if sync {
                BUDGET_BYTES
            } else {
                budget_for(crate::platform::total_memory_bytes())
            },
            prefetch: usize::from(DEFAULT_PREFETCH_TILES),
            enter: ENTER_DENSE,
            clip_live: HashSet::new(),
            clip_wanted: Vec::new(),
            built: 0,
            worker_allowed: true,
        }
    }

    fn ensure_worker(&mut self) {
        if !self.workers.is_empty() || self.sync || !self.worker_allowed {
            return;
        }
        let cores = std::thread::available_parallelism().map_or(1, usize::from);
        for n in 0..tile_workers(cores) {
            let shared = Arc::clone(&self.shared);
            let tx = self.tx.clone();
            match std::thread::Builder::new()
                .name(format!("plumspirit-tiles-{n}"))
                .spawn(move || worker(shared, tx))
            {
                Ok(h) => self.workers.push(h),
                Err(e) => {
                    log::warn!("tile worker {n} could not start ({e})");
                    break;
                }
            }
        }
        if self.workers.is_empty() {
            log::warn!("no tile worker could start; building tiles inline");
            self.sync = true;
        }
    }

    /// Whether `role` should draw dense levels for `estimate` visible notes
    /// (with hysteresis).
    /// B2-D02: how `role`'s tiles are drawn row by row from now on (the roll
    /// sets it every frame from its row height; tiles are not rebuilt).
    pub fn set_rows(&mut self, role: Role, style: RowStyle) {
        self.rows[role as usize] = style;
    }

    pub fn wants_dense(&mut self, role: Role, estimate: usize) -> bool {
        let d = &mut self.dense[role as usize];
        *d = if *d {
            // The same ratio as LEAVE_DENSE / ENTER_DENSE (three quarters).
            estimate > self.enter * LEAVE_DENSE / ENTER_DENSE
        } else {
            estimate > self.enter
        };
        *d
    }

    /// Starts a frame (call once per Piano Roll frame).
    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }

    /// L-S49: the store clones of `role`'s families still held (shown,
    /// shown before, last complete), by note content, for a new `family` of
    /// that role to share instead of cloning every store again — `None`
    /// when `family` is not new (its sources are not made). Only for roles
    /// whose tiles draw no selection: a clone keeps the selection bits it
    /// was made with (the Onion).
    pub fn store_pool(&self, role: Role, family: FamilyId) -> Option<StorePool> {
        let r = role as usize;
        if self.current[r] == Some(family) || self.families.contains_key(&family) {
            return None;
        }
        let mut pool = StorePool::default();
        for f in [self.current[r], self.previous[r], self.complete[r]]
            .into_iter()
            .flatten()
        {
            if let Some(fam) = self.families.get(&f) {
                for s in &fam.stores {
                    pool.0
                        .entry((s.content_revision(), s.len()))
                        .or_insert_with(|| Arc::clone(s));
                }
            }
        }
        Some(pool)
    }

    /// The sources of the family `role` shows (tests).
    #[cfg(test)]
    pub(crate) fn current_stores(&self, role: Role) -> Vec<Arc<NoteStore>> {
        self.current[role as usize]
            .and_then(|f| self.families.get(&f))
            .map(|f| f.stores.clone())
            .unwrap_or_default()
    }

    /// Draws `role`'s notes as tiles for the current view. `family` is the
    /// content identity; `make` builds its sources only when it is new.
    /// Returns true while tiles are still being built (the workers wake the UI
    /// when one is done; L-G05).
    pub fn draw(
        &mut self,
        p: &Painter,
        ctx: &egui::Context,
        g: &Geometry,
        role: Role,
        family: FamilyId,
        make: impl FnOnce() -> Family,
    ) -> bool {
        let place: Place<'_> = &|a, b, rows| Self::rect_of(g, a, b, rows);
        let ppp = f64::from(g.ppp.max(0.1));
        let view = g.view.time;
        let level = tiles::level_for(view.pixels_per_tick * ppp);
        let (t0, t1) = view.visible_range(f64::from(g.notes.width()));
        self.draw_view(
            p,
            ctx,
            role,
            family,
            make,
            level,
            (t0, t1),
            Layout::Rect(place, self.rows[role as usize].on(g)),
            (place, g.notes),
        )
    }

    /// Draws the MIDI Player's rainfall (v0.9 V09-S17, `docs/PLAYER.md` P6)
    /// of `family` over ticks `[t0, t1]` at tile `level`, in `cols`'
    /// columns — the roll's machinery: ready tiles, else a stand-in (another
    /// level, the previous family), else the neutral L4 band; never blank.
    /// Returns true while tiles are still being built (the workers wake the UI
    /// when one is done; L-G05).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_rainfall(
        &mut self,
        p: &Painter,
        ctx: &egui::Context,
        family: FamilyId,
        make: impl FnOnce() -> Family,
        cols: &Columns<'_>,
        (t0, t1): (u64, u64),
        level: i32,
    ) -> bool {
        let y = cols.y;
        let clip = cols.clip;
        let place: Place<'_> =
            &|a, b, _rows| Rect::from_x_y_ranges(clip.x_range(), y(b).min(y(a))..=y(a).max(y(b)));
        self.draw_view(
            p,
            ctx,
            Role::Player,
            family,
            make,
            level,
            (t0, t1),
            Layout::Columns(cols),
            (place, clip),
        )
    }

    /// One role's tiles of `family` at `level` over ticks `[t0, t1]` (the
    /// roll and the Player): stand-ins first, ready tiles over them, then
    /// the missing tiles requested centre-out with one tile of prefetch on
    /// each side. `coarse`: where the L4 band goes and its clip.
    #[allow(clippy::too_many_arguments)]
    fn draw_view(
        &mut self,
        p: &Painter,
        ctx: &egui::Context,
        role: Role,
        family: FamilyId,
        make: impl FnOnce() -> Family,
        level: i32,
        (t0, t1): (u64, u64),
        layout: Layout<'_>,
        coarse: (Place<'_>, Rect),
    ) -> bool {
        self.ensure_worker();
        let r = role as usize;
        let t_make = std::time::Instant::now();
        if self.current[r] != Some(family) {
            self.families
                .entry(family)
                .or_insert_with(|| Arc::new(make()));
            if self.current[r].is_some() {
                self.previous[r] = self.current[r];
            }
            self.current[r] = Some(family);
            self.changed_at[r] = Some(std::time::Instant::now());
            self.generation += 1;
            self.shared.wanted[r].store(family, Ordering::SeqCst);
            // Families no role shows any more are released (their tiles age
            // out of the cache).
            self.retain_families();
        }
        let make_micros = t_make.elapsed().as_micros() as u64;
        let t_receive = std::time::Instant::now();
        self.receive(ctx);
        let receive_micros = t_receive.elapsed().as_micros() as u64;
        let t_draw = std::time::Instant::now();
        self.bordered_rows = 0;
        let fam = Arc::clone(&self.families[&family]);
        // L-G01: the whole song at a coarse level, built in the background
        // (the roll's and the Onion's views; tests building inline skip it).
        let overview = if !self.sync
            && self.overview_on
            && matches!(role, Role::Track | Role::Onion)
            && matches!(layout, Layout::Rect(..))
        {
            let end = *self.ends.entry(family).or_insert_with(|| fam.end());
            (end > 0)
                .then(|| tiles::level_for(tiles::TILE_COLS as f64 * OVERVIEW_TILES / end as f64))
                .filter(|&l| l <= level - OVERVIEW_MIN_LEVELS)
                .map(|l| (l, end))
        } else {
            None
        };
        self.overview[r] = overview.map(|o| o.0);
        let (i0, i1) = tiles::tile_range(level, t0, t1);
        let mut st = DenseStats {
            level,
            lod: 2,
            generation: self.generation,
            make_micros,
            receive_micros,
            ..Default::default()
        };
        let span = tiles::tile_span(level);
        // The Player draws only the window (the playhead up); the roll draws
        // whole tiles.
        let part = match layout {
            Layout::Rect(..) => None,
            Layout::Columns(_) => Some((t0 as f64, t1 as f64)),
        };
        let mut wanted: Vec<TileKey> = Vec::new();
        let mut dense_texels = false;
        // Draw back to front: stand-ins first, then ready tiles over them.
        let mut ready_draws: Vec<TileKey> = Vec::new();
        let mut placeholder = 0.0f64;
        // L-G05: the L4 band is drawn once for the frame's gaps (the chunk
        // spans of every store looked up once, not once per missing tile).
        let mut coarse_gaps: Vec<(f64, f64)> = Vec::new();
        let grace = (role == Role::Onion && !self.sync)
            .then(|| self.changed_at[r].map(|t| t.elapsed()))
            .flatten()
            .and_then(|e| ONION_GRACE.checked_sub(e))
            .filter(|left| !left.is_zero());
        for i in i0..=i1 {
            st.visible_tiles += 1;
            let key = TileKey {
                family,
                level,
                index: i,
            };
            if self.tiles.contains_key(&key) {
                ready_draws.push(key);
                st.ready += 1;
                if self.max_count.get(&key).is_some_and(|&m| m > 1) {
                    dense_texels = true;
                }
                continue;
            }
            let s = tiles::tile_start(level, i);
            let (a, b) = part.map_or((s, s + span), |(a, b)| (s.max(a), (s + span).min(b)));
            let fams = [Some(family), self.previous[r], self.complete[r]];
            let extra = self.overview[r];
            let (drew, gaps) = self.draw_stand_in(Some((p, layout)), fams, level, extra, a, b);
            st.stand_ins += usize::from(drew);
            // B1-0151: where no stand-in reaches, the placeholder band.
            if grace.is_some() {
                st.held += usize::from(!gaps.is_empty());
                continue;
            }
            for &(ga, gb) in &gaps {
                placeholder += (gb.min(t1 as f64) - ga.max(t0 as f64)).max(0.0);
            }
            st.coarse += usize::from(!gaps.is_empty());
            coarse_gaps.extend(gaps);
        }
        if let Some(left) = grace
            && st.held > 0
        {
            // The band shows then if the tiles are still missing.
            ctx.request_repaint_after(left);
        }
        Self::draw_coarse(p, coarse.0, coarse.1, &fam, &coarse_gaps);
        let mut picture = 0xcbf2_9ce4_8422_2325u64;
        for key in &ready_draws {
            if let Some(e) = self.tiles.get(key) {
                picture = (picture ^ e.hash ^ key.index as u64).wrapping_mul(0x0000_0100_0000_01b3);
            }
            self.draw_tile(p, layout, key, key.level, part);
        }
        if st.stand_ins == 0 && st.coarse == 0 && st.held == 0 {
            st.picture = picture;
            self.complete[r] = Some(family);
        }
        st.placeholder = (placeholder / (t1.saturating_sub(t0).max(1)) as f64).min(1.0) as f32;
        // B1-0151: a smooth zoom changes the shown level every few frames;
        // building each level in turn rescans the same notes and finishes
        // none (on Hypernova most of the view stayed under the placeholder
        // while zooming out). Tiles are built at an anchor level that
        // follows the shown one only when the zoom rests or strays an
        // octave; until then its tiles stand in, scaled. Inline builds
        // (tests) never lag, so they build the shown level.
        let now = std::time::Instant::now();
        let a = self.anchor[r].get_or_insert(Anchor {
            level,
            shown: level,
            since: now,
        });
        if a.shown != level {
            a.shown = level;
            a.since = now;
        }
        let resting = now.duration_since(a.since) >= VIEW_REST;
        if self.sync || resting || (level - a.level).abs() > ANCHOR_LEVELS {
            a.level = level;
        }
        let build = a.level;
        // While the zoom moves, only ticks no tile of this content reaches
        // at any nearby level are built (the newly uncovered edges); the
        // rest is rebuilt at the shown level once the zoom rests.
        let moving = !self.sync && !resting;
        let anchored = build != level;
        let (b0, b1) = if anchored {
            tiles::tile_range(build, t0, t1)
        } else {
            (i0, i1)
        };
        let mid = (b0 + b1) / 2;
        let mut order: Vec<i64> = (b0..=b1).collect();
        order.sort_by_key(|&i| ((i - mid).abs(), i));
        // Prefetch: `prefetch` tiles before the view, one more after it.
        let p = self.prefetch as i64;
        let after = if p == 0 { 0 } else { p + 1 };
        let ahead = (b0 - p..b0).chain(b1 + 1..=b1 + after).filter(|&i| i >= 0);
        for (n, i) in order.iter().copied().chain(ahead).enumerate() {
            let key = TileKey {
                family,
                level: build,
                index: i,
            };
            if moving && !self.tiles.contains_key(&key) {
                let s = tiles::tile_start(build, i);
                let (mut a, mut b) = (s, s + tiles::tile_span(build));
                if n < order.len() {
                    // A visible tile: only its visible part must be covered.
                    (a, b) = (a.max(t0 as f64), b.min(t1 as f64));
                }
                let fams = [Some(family), None, None];
                let extra = self.overview[r];
                if b <= a
                    || self
                        .draw_stand_in(None, fams, build, extra, a, b)
                        .1
                        .is_empty()
                {
                    continue;
                }
            }
            wanted.push(key);
        }
        // L-G01: the overview last (low priority), its ready tiles kept.
        let low_from = wanted.len();
        if let Some((ov, end)) = overview {
            let n = (end as f64 / tiles::tile_span(ov)).floor() as i64;
            for i in 0..=n {
                let key = TileKey {
                    family,
                    level: ov,
                    index: i,
                };
                match self.tiles.get_mut(&key) {
                    Some(e) => e.last_used = self.frame,
                    None => wanted.push(key),
                }
            }
        }
        st.draw_micros = t_draw.elapsed().as_micros() as u64;
        let t_request = std::time::Instant::now();
        let wanted: Vec<(TileKey, Arc<Family>)> =
            wanted.into_iter().map(|k| (k, Arc::clone(&fam))).collect();
        self.request(&wanted, low_from, role, ctx);
        self.evict();
        st.request_micros = t_request.elapsed().as_micros() as u64;
        if st.coarse > 0 {
            st.lod = 4;
        } else if dense_texels {
            st.lod = 3;
        }
        st.pending = self.pending.len();
        st.cached = self.tiles.len();
        st.cache_bytes = self.tiles.values().map(Entry::bytes).sum();
        st.last_build_micros = self.stats[r].last_build_micros;
        st.bordered_rows = self.bordered_rows;
        self.stats[r] = st;
        // A moving zoom: draw again once it rests, when the shown level's
        // own tiles are requested (nothing else may repaint: the tiles it
        // wanted can all be covered by other levels).
        if moving {
            ctx.request_repaint_after(VIEW_REST);
        }
        !self.pending.is_empty()
    }

    /// Families still shown: every role's current and previous family and
    /// the clip families of the last Track List frame. The others are
    /// released (their tiles age out of the cache).
    fn retain_families(&mut self) {
        let live: HashSet<FamilyId> = self
            .current
            .iter()
            .chain(self.previous.iter())
            .chain(self.complete.iter())
            .flatten()
            .copied()
            .chain(self.clip_live.iter().copied())
            .collect();
        // L-S48: a family no role shows may hold the last references to
        // notes the document no longer has (an Import Into undone and then
        // replaced: 224M notes, ~4 GB; the frame that dropped it here took
        // 694 ms on the owner's PC). Large ones are freed on a thread of
        // their own.
        let gone: Vec<Arc<Family>> = self
            .families
            .extract_if(|k, _| !live.contains(k))
            .map(|(_, f)| f)
            .collect();
        self.ends.retain(|k, _| live.contains(k));
        release_families(gone);
    }

    /// Starts a Track List frame (v0.9 V09-S09): takes finished tiles and
    /// forgets which clip families were shown.
    pub fn begin_clips(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        self.ensure_worker();
        self.receive(ctx);
        self.clip_live.clear();
        self.clip_wanted.clear();
        self.stats[Role::Clip as usize] = DenseStats {
            generation: self.generation,
            last_build_micros: self.stats[Role::Clip as usize].last_build_micros,
            ..Default::default()
        };
    }

    /// Draws one clip's mini-note tiles (`c.family`, built by `make` when it
    /// is new) over its visible content range: ready tiles, else a stand-in
    /// (the family at another level, or `c.previous`), else the neutral
    /// chunk-span band — never blank. The tiles it lacks are requested at
    /// [`DenseRenderer::end_clips`]. `p` should be clipped to the clip.
    /// Returns true when every visible tile was ready.
    pub fn draw_clip(&mut self, p: &Painter, c: &ClipTiles, make: impl FnOnce() -> Family) -> bool {
        let fam = Arc::clone(
            self.families
                .entry(c.family)
                .or_insert_with(|| Arc::new(make())),
        );
        self.clip_live.insert(c.family);
        let (c0, c1) = c.range;
        if c1 <= c0 {
            return true;
        }
        let x = |t: f64| c.x0 + ((t - c.origin) * c.ppt) as f32;
        let body = c.body;
        let place: Place<'_> = &|a, b, _rows| {
            Rect::from_min_max(Pos2::new(x(a), body.top()), Pos2::new(x(b), body.bottom()))
        };
        let level = c.level;
        let span = tiles::tile_span(level);
        let (i0, i1) = tiles::tile_range(level, c0, c1.saturating_sub(1));
        let (a_all, b_all) = (c0 as f64, c1 as f64);
        let mut complete = true;
        let mut ready: Vec<TileKey> = Vec::new();
        let st = &mut self.stats[Role::Clip as usize];
        for i in i0..=i1 {
            st.visible_tiles += 1;
            let key = TileKey {
                family: c.family,
                level,
                index: i,
            };
            if self.tiles.contains_key(&key) {
                ready.push(key);
                continue;
            }
            complete = false;
            self.clip_wanted.push((key, Arc::clone(&fam)));
        }
        // Back to front: stand-ins, then ready tiles.
        for i in i0..=i1 {
            let key = TileKey {
                family: c.family,
                level,
                index: i,
            };
            if ready.contains(&key) {
                continue;
            }
            let s = tiles::tile_start(level, i);
            let (a, b) = (s.max(a_all), (s + span).min(b_all));
            let fams = [Some(c.family), c.previous, None];
            let (drew, gaps) = self.draw_stand_in(
                Some((p, Layout::Rect(place, RowStyle::default()))),
                fams,
                level,
                None,
                a,
                b,
            );
            Self::draw_coarse(p, place, body, &fam, &gaps);
            let st = &mut self.stats[Role::Clip as usize];
            st.stand_ins += usize::from(drew);
            st.coarse += usize::from(!gaps.is_empty());
        }
        for key in &ready {
            self.draw_tile(
                p,
                Layout::Rect(place, RowStyle::default()),
                key,
                level,
                Some((a_all, b_all)),
            );
            self.stats[Role::Clip as usize].ready += 1;
        }
        complete
    }

    /// Ends a Track List frame: requests the missing tiles of the clips drawn
    /// (in drawing order, after the queued roll jobs of other roles), drops
    /// queued clip builds no clip wants any more, releases the families no
    /// longer shown and keeps the cache within its budget. Returns true while
    /// tiles are being built (the workers wake the UI when one is done).
    pub fn end_clips(&mut self, ctx: &egui::Context) -> bool {
        if let Ok(mut c) = self.shared.clips.lock() {
            c.clone_from(&self.clip_live);
        }
        let wanted = std::mem::take(&mut self.clip_wanted);
        self.request(&wanted, wanted.len(), Role::Clip, ctx);
        self.retain_families();
        self.evict();
        let st = &mut self.stats[Role::Clip as usize];
        st.pending = self.pending.len();
        st.cached = self.tiles.len();
        st.cache_bytes = self.tiles.values().map(Entry::bytes).sum();
        !self.pending.is_empty()
    }

    /// Screen rectangle of ticks `[a, b)` over `rows` pitch rows from pitch 0
    /// up (128, or 256 in 256-Key Mode).
    fn rect_of(g: &Geometry, a: f64, b: f64, rows: usize) -> Rect {
        let v = g.view.time;
        let x = |t: f64| g.notes.left() + ((t - v.origin_tick as f64) * v.pixels_per_tick) as f32;
        let rh = g.view.pitch.row_height;
        let bottom = g.notes.top() + (g.view.pitch.pitch_top_y(0) + rh) as f32;
        let top = bottom - (rows as f64 * rh) as f32;
        Rect::from_min_max(Pos2::new(x(a), top), Pos2::new(x(b), bottom))
    }

    /// Draws tile `key` (of `level`), optionally only its part over ticks
    /// `[a, b)`.
    fn draw_tile(
        &mut self,
        p: &Painter,
        layout: Layout<'_>,
        key: &TileKey,
        level: i32,
        part: Option<(f64, f64)>,
    ) {
        let Some(e) = self.tiles.get_mut(key) else {
            return;
        };
        e.last_used = self.frame;
        if !e.occupied {
            return;
        }
        let s = tiles::tile_start(level, key.index);
        let span = tiles::tile_span(level);
        let (a, b) = part.map_or((s, s + span), |(a, b)| (a.max(s), b.min(s + span)));
        if b <= a {
            return;
        }
        let (place, style) = match layout {
            Layout::Rect(place, style) => (place, style),
            Layout::Columns(c) => {
                Self::paint_columns(p, e.tex.id(), e.rows, (s, span), (a, b), c);
                return;
            }
        };
        let rect = place(a, b, e.rows);
        if !rect.width().is_finite() || rect.width() <= 0.0 {
            return;
        }
        let u0 = ((a - s) / span) as f32;
        let u1 = ((b - s) / span) as f32;
        if style.is_plain() {
            p.image(
                e.tex.id(),
                rect,
                Rect::from_min_max(Pos2::new(u0, 0.0), Pos2::new(u1, 1.0)),
                Color32::WHITE,
            );
            return;
        }
        let borders = e.borders.as_ref().map(TextureHandle::id);
        for (k, m) in row_meshes(
            e.tex.id(),
            borders,
            e.rows,
            rect,
            (u0, u1),
            style,
            p.clip_rect(),
        )
        .into_iter()
        .enumerate()
        {
            if k == 1 {
                self.bordered_rows += m.vertices.len() / 8;
            }
            p.add(egui::Shape::mesh(m));
        }
    }

    /// One tile's part over ticks `[a, b)` in the Player's key columns: one
    /// mesh, one quad per column and segment between bends; a quad samples
    /// its key's texel row at the row's centre along the tile's time axis
    /// (`s`, `span`: the tile's start and span in ticks).
    fn paint_columns(
        p: &Painter,
        tex: egui::TextureId,
        rows: usize,
        (s, span): (f64, f64),
        (a, b): (f64, f64),
        c: &Columns<'_>,
    ) {
        let lo = c.bends.partition_point(|&t| t <= a);
        let hi = c.bends.partition_point(|&t| t < b);
        let mut cuts: Vec<f64> = Vec::with_capacity(hi.saturating_sub(lo) + 2);
        cuts.push(a);
        cuts.extend_from_slice(&c.bends[lo..hi.max(lo)]);
        cuts.push(b);
        let (top, bottom) = (c.clip.top(), c.clip.bottom());
        // (y of the earlier tick, y of the later, their U).
        let segs: Vec<(f32, f32, f32, f32)> = cuts
            .windows(2)
            .filter(|w| w[1] > w[0])
            .map(|w| {
                let (ya, yb) = ((c.y)(w[0]), (c.y)(w[1]));
                (
                    ya,
                    yb,
                    ((w[0] - s) / span) as f32,
                    ((w[1] - s) / span) as f32,
                )
            })
            .filter(|&(ya, yb, ..)| ya.is_finite() && yb.is_finite() && ya >= top && yb <= bottom)
            .collect();
        if segs.is_empty() {
            return;
        }
        let mut mesh = egui::Mesh::with_texture(tex);
        mesh.vertices.reserve(c.cols.len() * segs.len() * 4);
        mesh.indices.reserve(c.cols.len() * segs.len() * 6);
        let vertex = |x: f32, y: f32, u: f32, v: f32, color: Color32| egui::epaint::Vertex {
            pos: Pos2::new(x, y),
            uv: Pos2::new(u, v),
            color,
        };
        // The right edge's tint (vertex colours multiply the texture).
        let dark = {
            let g = (255.0 * (1.0 - c.shade.clamp(0.0, 1.0))).round() as u8;
            Color32::from_rgb(g, g, g)
        };
        let (l, r) = (Color32::WHITE, dark);
        for &(row, x0, x1) in c.cols {
            if row >= rows || x1 <= x0 {
                continue;
            }
            let v = (row as f32 + 0.5) / rows as f32;
            for &(ya, yb, ua, ub) in &segs {
                let i = mesh.vertices.len() as u32;
                mesh.vertices.push(vertex(x0, yb, ub, v, l));
                mesh.vertices.push(vertex(x1, yb, ub, v, r));
                mesh.vertices.push(vertex(x1, ya, ua, v, r));
                mesh.vertices.push(vertex(x0, ya, ua, v, l));
                mesh.indices
                    .extend_from_slice(&[i, i + 1, i + 2, i, i + 2, i + 3]);
            }
        }
        p.add(egui::Shape::mesh(mesh));
    }

    /// Stand-ins for ticks `[a, b)` while their tile builds: `fams[0]` (the
    /// family shown) at nearby levels, then the earlier families (the one
    /// before it, the last one drawn complete: the last valid frame) at the
    /// same and nearby levels — each tile drawn only over the ticks still
    /// uncovered. Returns whether anything was drawn and the ticks no
    /// stand-in reached (B1-0151: a finer level reaches only part of a
    /// zoomed-out tile; the caller covers the rest with the placeholder band
    /// instead of leaving it empty).
    /// With `paint` `None` nothing is drawn: the gaps say whether stand-ins
    /// would cover the range.
    fn draw_stand_in(
        &mut self,
        paint: Option<(&Painter, Layout<'_>)>,
        fams: [Option<FamilyId>; 3],
        level: i32,
        overview: Option<i32>,
        a: f64,
        b: f64,
    ) -> (bool, Vec<(f64, f64)>) {
        // Nearest levels first; coarser before finer at the same distance —
        // up to four octaves either way, so the tiles of where a smooth zoom
        // started stand in for where it is (B1-0151).
        const NEAR: [i32; 32] = [
            -1, 1, -2, 2, -3, 3, -4, 4, -5, 5, -6, 6, -7, 7, -8, 8, -9, 9, -10, 10, -11, 11, -12,
            12, -13, 13, -14, 14, -15, 15, -16, 16,
        ];
        // Slivers below a millionth of the tile are float noise, not gaps.
        let eps = tiles::tile_span(level) * 1e-6;
        let mut gaps = vec![(a, b)];
        let mut drew = false;
        for (n, f) in fams.iter().enumerate() {
            let Some(f) = *f else {
                continue;
            };
            if fams[..n].contains(&Some(f)) {
                continue;
            }
            // The family shown lacks this level's tile; an earlier family
            // may have it (content changed, view the same).
            let same = (n > 0).then_some(0);
            // L-G01: last, the overview (often more than 16 levels away).
            let far = overview.map(|l| l - level).filter(|d| !NEAR.contains(d));
            for d in same.into_iter().chain(NEAR).chain(far) {
                if gaps.is_empty() {
                    return (drew, gaps);
                }
                let l = level + d;
                let ts = tiles::tile_span(l);
                let mut left = Vec::with_capacity(gaps.len());
                for &(ga, gb) in &gaps {
                    let (j0, j1) = ((ga / ts).floor() as i64, ((gb - eps) / ts).floor() as i64);
                    if j1 - j0 > 16 {
                        left.push((ga, gb));
                        continue;
                    }
                    let mut at = ga;
                    for j in j0..=j1 {
                        let k = TileKey {
                            family: f,
                            level: l,
                            index: j,
                        };
                        let s0 = tiles::tile_start(l, j);
                        let (s0, s1) = (s0.max(ga), (s0 + ts).min(gb));
                        if s1 <= s0 || !self.tiles.contains_key(&k) {
                            continue;
                        }
                        if let Some((p, layout)) = paint {
                            self.draw_tile(p, layout, &k, l, Some((s0, s1)));
                        }
                        drew = true;
                        if s0 - at > eps {
                            left.push((at, s0));
                        }
                        at = s1;
                    }
                    if gb - at > eps {
                        left.push((at, gb));
                    }
                }
                gaps = left;
            }
        }
        (drew, gaps)
    }

    /// L4: the placeholder over the time spans in each gap `[a, b)` (in
    /// ascending order) where any store's chunks hold notes (merged across
    /// stores, bounded; looked up once for all the gaps).
    fn draw_coarse(p: &Painter, place: Place<'_>, clip: Rect, fam: &Family, gaps: &[(f64, f64)]) {
        let (Some(a), Some(b)) = (
            gaps.iter().map(|g| g.0).reduce(f64::min),
            gaps.iter().map(|g| g.1).reduce(f64::max),
        ) else {
            return;
        };
        let (ta, tb) = (a.max(0.0) as u64, b.max(0.0).ceil() as u64);
        let mut spans: Vec<(u64, u64)> = Vec::new();
        for (i, s) in fam.stores.iter().enumerate() {
            let room = PLACEHOLDER_SPANS - spans.len();
            let pl = fam.place(i);
            if pl.is_identity() {
                spans.extend(s.chunk_spans(ta, tb).take(room).map(|(s0, s1, _)| (s0, s1)));
            } else if let Some((ca, cb)) = pl.content_range(ta, tb) {
                // A placed store's chunks where the placement shows them,
                // within the song span of its window.
                let to = |c: u64| tiles::shifted(c, pl.shift);
                let (w0, w1) = (to(pl.window.0), to(pl.window.1));
                spans.extend(
                    s.chunk_spans(ca, cb)
                        .map(|(s0, s1, _)| (to(s0).max(w0), to(s1).min(w1)))
                        .filter(|(s0, s1)| s1 > s0)
                        .take(room),
                );
            }
            if spans.len() >= PLACEHOLDER_SPANS {
                break;
            }
        }
        spans.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::new();
        for (s0, s1) in spans {
            match merged.last_mut() {
                Some(m) if s0 <= m.1 => m.1 = m.1.max(s1),
                _ => merged.push((s0, s1)),
            }
        }
        for &(a, b) in gaps {
            let from = merged.partition_point(|m| (m.1 as f64) < a);
            for &(s0, s1) in merged[from..].iter().take_while(|m| (m.0 as f64) < b) {
                let r = place(
                    (s0 as f64).max(a),
                    (s1 as f64).min(b).max((s0 as f64).max(a) + 1.0),
                    fam.opt.keys.keys(),
                );
                if r.width().is_finite() {
                    p.rect_filled(r.intersect(clip), 0.0, PLACEHOLDER);
                }
            }
        }
    }

    /// Queues the wanted tiles that are neither ready nor pending (visible
    /// first; from `low_from` on overview tiles, behind every other role's
    /// tiles too — L-G01); drops queued jobs no longer wanted.
    fn request(
        &mut self,
        wanted: &[(TileKey, Arc<Family>)],
        low_from: usize,
        role: Role,
        ctx: &egui::Context,
    ) {
        let low: HashSet<TileKey> = wanted[low_from.min(wanted.len())..]
            .iter()
            .map(|(k, _)| *k)
            .collect();
        let mut seen: HashSet<TileKey> = HashSet::new();
        let missing: Vec<(TileKey, Arc<Family>)> = wanted
            .iter()
            .filter(|(k, _)| !self.tiles.contains_key(k) && seen.insert(*k))
            .cloned()
            .collect();
        if self.sync {
            for (k, fam) in missing {
                let stores: Vec<&NoteStore> = fam.stores.iter().map(|s| s.as_ref()).collect();
                let started = std::time::Instant::now();
                let out = tiles::build_tile_read(
                    &stores,
                    &fam.places,
                    &fam.readings,
                    &fam.colors,
                    k.level,
                    k.index,
                    fam.opt,
                    &mut self.scratch,
                    &|| false,
                );
                let micros = started.elapsed().as_micros() as u64;
                let hash = out.as_ref().map_or(0, TileOut::hash);
                self.store(
                    ctx,
                    Done {
                        key: k,
                        out,
                        micros,
                        hash,
                    },
                );
            }
            return;
        }
        let Ok(mut q) = self.shared.queue.lock() else {
            return;
        };
        let keep: HashSet<TileKey> = missing.iter().map(|(k, _)| *k).collect();
        // This role's jobs for tiles no longer wanted leave the queue
        // (in-flight ones finish or cancel themselves). The other role's
        // jobs stay: both roles request every frame, and dropping each
        // other's work starved one of them into endless stand-ins (v0.9
        // §182).
        q.retain(|j| {
            let k = j.role != role || keep.contains(&j.key);
            if !k {
                self.pending.remove(&j.key);
            }
            k
        });
        let queued: HashSet<TileKey> = q.iter().map(|j| j.key).collect();
        let mut fresh: VecDeque<Job> = VecDeque::new();
        for (k, fam) in &missing {
            if queued.contains(k) {
                continue;
            }
            if self.pending.insert(*k) {
                fresh.push_back(Job {
                    key: *k,
                    family: Arc::clone(fam),
                    role,
                    low: low.contains(k),
                });
            }
        }
        // Wanted order first: rebuild the queue as [missing order].
        let mut all: Vec<Job> = q.drain(..).chain(fresh).collect();
        let pos: HashMap<TileKey, usize> = missing
            .iter()
            .enumerate()
            .map(|(i, (k, _))| (*k, i))
            .collect();
        all.sort_by_key(|j| (j.low, pos.get(&j.key).copied().unwrap_or(usize::MAX)));
        q.extend(all);
        self.shared
            .urgent
            .store(q.iter().filter(|j| !j.low).count(), Ordering::SeqCst);
        drop(q);
        self.shared.cv.notify_all();
    }

    /// Takes finished tiles from the worker (at most [`UPLOADS_PER_FRAME`];
    /// more waiting: another frame is asked for — the workers wake the UI
    /// for each tile they finish).
    fn receive(&mut self, ctx: &egui::Context) {
        if let Ok(mut w) = self.shared.wake.lock()
            && w.as_ref() != Some(ctx)
        {
            *w = Some(ctx.clone());
        }
        for n in 0..UPLOADS_PER_FRAME {
            let Ok(d) = self.rx.try_recv() else {
                break;
            };
            self.pending.remove(&d.key);
            if self.families.contains_key(&d.key.family) {
                self.store(ctx, d);
            }
            if n + 1 == UPLOADS_PER_FRAME {
                ctx.request_repaint();
            }
        }
    }

    fn store(&mut self, ctx: &egui::Context, d: Done) {
        let Some(out) = d.out else {
            return;
        };
        self.built += 1;
        let role = if Some(d.key.family) == self.current[Role::Onion as usize] {
            Role::Onion as usize
        } else if Some(d.key.family) == self.current[Role::Player as usize] {
            Role::Player as usize
        } else if self.clip_live.contains(&d.key.family) {
            Role::Clip as usize
        } else {
            Role::Track as usize
        };
        self.stats[role].last_build_micros = d.micros;
        let occupied = out.occupied > 0;
        let bytes: Vec<u8> = out.pixels.iter().flatten().copied().collect();
        let image = egui::ColorImage::from_rgba_unmultiplied([tiles::TILE_COLS, out.rows], &bytes);
        let opts = TextureOptions {
            magnification: egui::TextureFilter::Nearest,
            minification: egui::TextureFilter::Linear,
            ..TextureOptions::NEAREST
        };
        let tex = ctx.load_texture(
            format!("tile-{:x}-{}-{}", d.key.family, d.key.level, d.key.index),
            image,
            opts,
        );
        let borders = (!out.borders.is_empty()).then(|| {
            let bytes: Vec<u8> = out.borders.iter().flatten().copied().collect();
            ctx.load_texture(
                format!("tile-b-{:x}-{}-{}", d.key.family, d.key.level, d.key.index),
                egui::ColorImage::from_rgba_unmultiplied([tiles::TILE_COLS, out.rows], &bytes),
                opts,
            )
        });
        self.max_count.insert(d.key, out.max_count);
        self.tiles.insert(
            d.key,
            Entry {
                tex,
                borders,
                last_used: self.frame,
                occupied,
                hash: d.hash,
                rows: out.rows,
            },
        );
    }

    /// Least recently used tiles leave beyond the budget (never one drawn
    /// this frame).
    fn evict(&mut self) {
        let mut total: usize = self.tiles.values().map(Entry::bytes).sum();
        if total <= self.budget {
            return;
        }
        let mut by_age: Vec<(u64, TileKey)> = self
            .tiles
            .iter()
            .filter(|(_, e)| e.last_used < self.frame)
            .map(|(k, e)| (e.last_used, *k))
            .collect();
        by_age.sort_unstable_by_key(|x| (x.0, x.1.level, x.1.index, x.1.family));
        for (_, k) in by_age {
            if total <= self.budget {
                break;
            }
            if let Some(e) = self.tiles.remove(&k) {
                total -= e.bytes();
            }
            self.max_count.remove(&k);
        }
    }

    /// B1-0059 (Settings → Performance): the tile cache budget in MB
    /// (0 = automatic for this PC) and the prefetch distance in tiles. A
    /// smaller budget takes effect at the next draw (least recently used
    /// tiles leave first). Cheap: called every frame.
    pub fn configure(&mut self, cache_mb: u32, prefetch: u8) {
        self.budget = if cache_mb == 0 {
            self.auto_budget
        } else {
            (cache_mb.clamp(MIN_CACHE_MB, MAX_CACHE_MB) as usize) << 20
        };
        self.prefetch = usize::from(prefetch.min(MAX_PREFETCH_TILES));
    }

    /// B1-0059: the exact-rectangle limit of the Rendering Quality setting.
    pub fn set_quality(&mut self, q: RenderQuality) {
        self.enter = q.exact_limit();
    }

    /// Visible notes above which a view turns dense (see [`Self::wants_dense`]).
    pub fn enter_threshold(&self) -> usize {
        self.enter
    }

    /// The tile cache budget in bytes now in force (tests).
    #[cfg(test)]
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// The built tiles of `family`: (level, index, content hash), sorted
    /// (tests).
    #[cfg(test)]
    pub fn tile_hashes(&self, family: FamilyId) -> Vec<(i32, i64, u64)> {
        let mut v: Vec<(i32, i64, u64)> = self
            .tiles
            .iter()
            .filter(|(k, _)| k.family == family)
            .map(|(k, e)| (k.level, k.index, e.hash))
            .collect();
        v.sort_unstable();
        v
    }

    /// Waits until no tile is pending (tests).
    #[cfg(test)]
    pub fn settle(&mut self, ctx: &egui::Context) {
        for _ in 0..4000 {
            self.receive(ctx);
            if self.pending.is_empty() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

/// B2-D02: a tile's visible rows as the exact renderer draws notes: one fill
/// quad per pitch row (its texel row; `style.gap` below and `style.inset`
/// above left empty) and, with `style.borders` and a border texture, a
/// 1-point line at the top and the bottom of each row sampled from the
/// border colours (transparent where no note is bordered). Only rows meeting
/// `clip` are emitted (with `style.grid`, each row's edges are the exact
/// renderer's). Returns (fills, borders) meshes.
pub fn row_meshes(
    tex: egui::TextureId,
    borders: Option<egui::TextureId>,
    rows: usize,
    rect: Rect,
    (u0, u1): (f32, f32),
    style: RowStyle,
    clip: Rect,
) -> Vec<egui::Mesh> {
    let mut fill = egui::Mesh::with_texture(tex);
    let mut edge = borders
        .filter(|_| style.borders)
        .map(egui::Mesh::with_texture);
    if rows > 0 && rect.height() > 0.0 {
        let row_h = rect.height() / rows as f32;
        let lo = ((clip.top() - rect.top()) / row_h).floor().max(0.0) as usize;
        let hi = (((clip.bottom() - rect.top()) / row_h).ceil().max(0.0) as usize).min(rows);
        let n = rows as f32;
        // Row `r` is pitch `rows - 1 - r` (the tile's top row is its top key).
        let grid = style.grid.filter(|g| usize::from(g.axis.top) + 1 == rows);
        for r in lo..hi {
            let (top, bottom) = match grid {
                Some(g) => {
                    super::hit_test::row_span(g.notes_top, &g.axis, g.ppp, (rows - 1 - r) as u8)
                }
                None => {
                    let top = rect.top() + r as f32 * row_h;
                    (top, top + row_h)
                }
            };
            let (y0, y1) = (top + style.inset, bottom - style.gap);
            if y1 <= y0 {
                continue;
            }
            let (v0, v1) = (r as f32 / n, (r + 1) as f32 / n);
            fill.add_rect_with_uv(
                Rect::from_x_y_ranges(rect.x_range(), y0..=y1),
                Rect::from_min_max(Pos2::new(u0, v0), Pos2::new(u1, v1)),
                Color32::WHITE,
            );
            if let Some(m) = edge.as_mut()
                && y1 - y0 >= 2.0
            {
                // The row's centre: nearest sampling never reads a neighbour.
                let vc = (r as f32 + 0.5) / n;
                let uv = Rect::from_min_max(Pos2::new(u0, vc), Pos2::new(u1, vc));
                m.add_rect_with_uv(
                    Rect::from_x_y_ranges(rect.x_range(), y0..=y0 + 1.0),
                    uv,
                    Color32::WHITE,
                );
                m.add_rect_with_uv(
                    Rect::from_x_y_ranges(rect.x_range(), y1 - 1.0..=y1),
                    uv,
                    Color32::WHITE,
                );
            }
        }
    }
    let mut out = vec![fill];
    out.extend(edge);
    out
}

/// L-S48: families holding at least this many notes are freed on a thread of
/// their own when no role shows them any more.
const RELEASE_FAMILY_NOTES: usize = 4 << 20;

/// Frees released families: large ones (their stores may be the last owners
/// of millions of notes) on a short-lived thread, never in the frame.
fn release_families(gone: Vec<Arc<Family>>) {
    let notes: usize = gone
        .iter()
        .flat_map(|f| f.stores.iter())
        .map(|s| s.len())
        .sum();
    if notes < RELEASE_FAMILY_NOTES {
        return;
    }
    // If no thread can start, the closure (and the families) is dropped here.
    let _ = std::thread::Builder::new()
        .name("plumspirit-tiles-free".into())
        .spawn(move || drop(gone));
}

/// FNV-1a helper for family identities.
pub fn hash_words(words: impl IntoIterator<Item = u64>) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for w in words {
        h ^= w;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// B2-D02 (S05): on the roll's grid every dense row has exactly the
    /// exact renderer's edges at 100 – 200 % and any row height or scroll —
    /// `hit_test::note_rect` minus the gap, plus the Onion inset, with
    /// 1-point lines inside them — so both renderers cover the same pixels
    /// (unsnapped rows put the 150 % lines one pixel off).
    #[test]
    fn grid_rows_share_the_exact_renderers_pixel_edges_at_every_scale() {
        use super::super::viewport::RollView;
        use plumspirit_engine::view::{PitchAxis, TimeAxis};
        let (tex, btex) = (egui::TextureId::Managed(7), egui::TextureId::Managed(8));
        // 128 keys and 256-Key Mode.
        for (keys_top, rows) in [
            (plumspirit_engine::model::note::MAX_PITCH, 128usize),
            (255, 256),
        ] {
            for ppp in [1.0f32, 1.25, 1.5, 1.75, 2.0] {
                for (rh, scroll) in [(12.0, 27.0), (12.0, 27.37), (7.3, 40.5), (5.0, 3.21)] {
                    let view = RollView {
                        time: TimeAxis::new(0, 1.0),
                        pitch: PitchAxis {
                            scroll_rows: scroll,
                            row_height: rh,
                            top: keys_top,
                        },
                    };
                    let g = Geometry::new(
                        Rect::from_min_size(egui::pos2(13.0, 7.0), egui::vec2(1280.0, 700.0)),
                        view,
                        ppp,
                    );
                    for (inset_from, min_note) in [(None, Some(4.0)), (Some(5.0), Some(3.0))] {
                        let style = RowStyle::for_rows(rh, inset_from, min_note).on(&g);
                        let rect = DenseRenderer::rect_of(&g, 0.0, 256.0, rows);
                        let m = row_meshes(tex, Some(btex), rows, rect, (0.0, 1.0), style, g.notes);
                        let fills: Vec<(f32, f32)> = m[0]
                            .vertices
                            .chunks(4)
                            .map(|v| (v[0].pos.y, v[3].pos.y))
                            .collect();
                        let first = ((g.notes.top() - rect.top()) / rect.height() * rows as f32)
                            .floor()
                            .max(0.0) as usize;
                        for (i, &(y0, y1)) in fills.iter().enumerate() {
                            let pitch = (rows - 1 - first - i) as u8;
                            let r = super::super::hit_test::note_rect(&g, 0, 100, pitch);
                            let want = (r.min.y + style.inset, r.max.y - style.gap);
                            assert_eq!(
                                (y0, y1),
                                want,
                                "{ppp} ppp, {rh} pt, scroll {scroll}, pitch {pitch}"
                            );
                        }
                        if style.borders {
                            let lines: Vec<(f32, f32)> = m[1]
                                .vertices
                                .chunks(4)
                                .map(|v| (v[0].pos.y, v[3].pos.y))
                                .collect();
                            for (k, &(y0, y1)) in fills.iter().enumerate() {
                                assert_eq!(lines[2 * k], (y0, y0 + 1.0));
                                assert_eq!(lines[2 * k + 1], (y1 - 1.0, y1));
                            }
                        }
                    }
                }
            }
        }
    }

    /// B2-D02: a dense tile's rows have the exact renderer's geometry — a
    /// 1-point gap under rows from 6 pt, the opaque Onion's inset, 1-point
    /// top / bottom borders sampled at the row's own texel row — and only the
    /// rows meeting the clip are emitted; short rows stay one plain image.
    #[test]
    fn row_meshes_draw_the_exact_renderers_row_geometry() {
        use egui::{Rect, pos2, vec2};
        let (tex, btex) = (egui::TextureId::Managed(7), egui::TextureId::Managed(8));
        let rect = Rect::from_min_size(pos2(100.0, 50.0), vec2(256.0, 128.0 * 8.0));
        // Rows 10 – 14 visible (8-pt rows).
        let clip = Rect::from_min_size(pos2(0.0, 50.0 + 80.0 + 0.5), vec2(1000.0, 39.0));
        let quads = |m: &egui::Mesh| -> Vec<(Rect, Rect)> {
            m.vertices
                .chunks(4)
                .map(|v| {
                    (
                        Rect::from_two_pos(v[0].pos, v[3].pos),
                        Rect::from_two_pos(v[0].uv, v[3].uv),
                    )
                })
                .collect()
        };
        let style = RowStyle::for_rows(8.0, None, Some(4.0));
        assert_eq!(
            style,
            RowStyle {
                gap: 1.0,
                inset: 0.0,
                borders: true,
                grid: None,
            }
        );
        let m = row_meshes(tex, Some(btex), 128, rect, (0.25, 0.75), style, clip);
        assert_eq!((m.len(), m[0].texture_id, m[1].texture_id), (2, tex, btex));
        let (fills, edges) = (quads(&m[0]), quads(&m[1]));
        assert_eq!((fills.len(), edges.len()), (5, 10));
        for (i, (f, uv)) in fills.iter().enumerate() {
            let r = 10 + i;
            let top = 50.0 + 8.0 * r as f32;
            assert_eq!(
                (f.top(), f.bottom(), f.left(), f.right()),
                (top, top + 7.0, 100.0, 356.0)
            );
            assert_eq!(
                (uv.top(), uv.bottom()),
                (r as f32 / 128.0, (r + 1) as f32 / 128.0)
            );
            assert_eq!((uv.left(), uv.right()), (0.25, 0.75));
            let (t, tuv) = edges[2 * i];
            let (b, buv) = edges[2 * i + 1];
            assert_eq!(
                (t.top(), t.height(), b.bottom(), b.height()),
                (top, 1.0, top + 7.0, 1.0)
            );
            let vc = (r as f32 + 0.5) / 128.0;
            assert_eq!(
                (tuv.top(), tuv.bottom(), buv.top()),
                (vc, vc, vc),
                "the row's own texels"
            );
        }
        // 5-pt rows: borders without a gap; opaque Onion at 6 pt: inset too.
        let s5 = RowStyle::for_rows(5.0, None, Some(4.0));
        let rect5 = Rect::from_min_size(pos2(0.0, 0.0), vec2(10.0, 128.0 * 5.0));
        let m = row_meshes(tex, Some(btex), 128, rect5, (0.0, 1.0), s5, rect5);
        let f = quads(&m[0]);
        assert_eq!((f.len(), f[3].0.top(), f[3].0.bottom()), (128, 15.0, 20.0));
        assert_eq!(quads(&m[1]).len(), 256);
        let o6 = RowStyle::for_rows(6.0, Some(5.0), Some(3.0));
        assert_eq!(
            o6,
            RowStyle {
                gap: 1.0,
                inset: 1.0,
                borders: true,
                grid: None,
            }
        );
        let rect6 = Rect::from_min_size(pos2(0.0, 0.0), vec2(10.0, 128.0 * 6.0));
        let f = quads(&row_meshes(tex, Some(btex), 128, rect6, (0.0, 1.0), o6, rect6)[0]);
        assert_eq!((f[2].0.top(), f[2].0.bottom()), (13.0, 17.0));
        // No border texture (or borders off): fills only.
        assert_eq!(
            row_meshes(tex, None, 128, rect, (0.0, 1.0), style, clip).len(),
            1
        );
        // 3-pt track rows: no borders, no gap — the plain image path.
        assert!(RowStyle::for_rows(3.0, None, Some(4.0)).is_plain());
    }
    use crate::piano_roll::viewport::RollView;
    use plumspirit_engine::model::{NoteAttrs, NoteOffKind, NoteRecord, SubchannelId};
    use plumspirit_engine::view::{PitchAxis, TimeAxis};

    fn store(n: usize, span: u64, salt: u64) -> NoteStore {
        let mut s = 0x9E37_79B9_7F4A_7C15u64 ^ salt;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let mut v: Vec<NoteRecord> = (0..n)
            .map(|_| NoteRecord {
                start_tick: next() % span,
                duration_ticks: 30 + next() % 300,
                attrs: NoteAttrs::new((next() % 128) as u8, 100, 0, 0, NoteOffKind::NoteOff)
                    .unwrap(),
                sub: SubchannelId((next() % 4) as u32),
                group: 0,
            })
            .collect();
        v.sort_by_key(|r| r.start_tick);
        NoteStore::from_sorted_records(&v, false).unwrap()
    }

    fn geometry(span: u64) -> Geometry {
        let full = Rect::from_min_size(Pos2::ZERO, egui::vec2(1200.0, 700.0));
        let view = RollView {
            time: TimeAxis::new(0, 1100.0 / span as f64),
            pitch: PitchAxis {
                scroll_rows: 0.0,
                row_height: 5.0,
                top: plumspirit_engine::model::note::MAX_PITCH,
            },
        };
        Geometry::new(full, view, 1.0)
    }

    fn family_of(s: &NoteStore) -> Family {
        Family {
            stores: vec![Arc::new(s.clone())],
            places: Vec::new(),
            readings: Vec::new(),
            colors: Colors {
                sources: vec![SourceColors::new(Tint::Subchannels {
                    dark: true,
                    palette: Default::default(),
                })],
                selection: true,
                muted_fill: None,
                muted: 90,
                ranked: false,
            },
            opt: TileOptions::default(),
        }
    }

    /// One frame: draws `s` as family `fam`; returns (shapes painted, stats,
    /// textures drawn).
    fn frame(
        ctx: &egui::Context,
        d: &mut DenseRenderer,
        g: &Geometry,
        s: &NoteStore,
        fam: FamilyId,
    ) -> (usize, DenseStats, Vec<egui::TextureId>) {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let p = ui.ctx().layer_painter(egui::LayerId::background());
            d.begin_frame();
            d.draw(&p, ui.ctx(), g, Role::Track, fam, || family_of(s));
        });
        out.textures_delta.clear();
        let mut tex = Vec::new();
        let mut n = 0;
        for c in &out.shapes {
            match &c.shape {
                egui::Shape::Mesh(m) => {
                    n += 1;
                    tex.push(m.texture_id);
                }
                egui::Shape::Rect(_) => n += 1,
                _ => {}
            }
        }
        (n, d.stats[0], tex)
    }

    #[test]
    fn the_loading_placeholder_is_neutral_and_merged_never_a_faded_note_colour() {
        // §025: L4 must not show notes at a hidden reduced opacity. Many
        // sources: one band per merged span, all in the neutral placeholder.
        let span = 960 * 4 * 64;
        let stores: Vec<Arc<NoteStore>> =
            (0..50).map(|k| Arc::new(store(20_000, span, k))).collect();
        let fam = Family {
            stores,
            places: Vec::new(),
            readings: Vec::new(),
            colors: Colors {
                sources: (0..50)
                    .map(|_| {
                        SourceColors::new(Tint::Ghost {
                            base: [200, 40, 40],
                            alpha: 255,
                            palette: Default::default(),
                        })
                    })
                    .collect(),
                selection: false,
                muted_fill: None,
                muted: 255,
                ranked: true,
            },
            opt: TileOptions::default(),
        };
        let g = geometry(span);
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let p = ui.ctx().layer_painter(egui::LayerId::background());
            let place = |a, b, rows| DenseRenderer::rect_of(&g, a, b, rows);
            DenseRenderer::draw_coarse(&p, &place, g.notes, &fam, &[(0.0, span as f64)]);
        });
        out.textures_delta.clear();
        let rects: Vec<_> = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Rect(r) => Some(r.fill),
                _ => None,
            })
            .collect();
        assert!(!rects.is_empty(), "never blank");
        assert!(rects.len() <= 8, "merged across stores: {}", rects.len());
        assert!(rects.iter().all(|&c| c == PLACEHOLDER));
    }

    #[test]
    fn dense_views_are_never_blank_and_idle_frames_are_stable() {
        // §226 (10k – 1M here; 10M in the measurement below) and §224.
        for n in [10_000, 100_000, 1_000_000] {
            let span = 960 * 4 * 64;
            let s = store(n, span, n as u64);
            let g = geometry(span);
            let ctx = egui::Context::default();
            let mut d = DenseRenderer::new(true);
            // First frame: nothing built yet — coarse occupancy, not blank.
            let (shapes, st, _) = frame(&ctx, &mut d, &g, &s, 7);
            assert!(shapes > 0 && st.coarse > 0, "{n}: {st:?}");
            assert_eq!(st.lod, 4);
            // Then the tiles, drawing every visible tile.
            let (shapes, st, tex) = frame(&ctx, &mut d, &g, &s, 7);
            assert!(shapes > 0, "{n}");
            assert_eq!(st.ready, st.visible_tiles, "{n}: {st:?}");
            assert_eq!(st.coarse, 0);
            assert!(st.lod >= 2);
            // Idle frames: same tiles, nothing rebuilt or re-queued.
            for _ in 0..5 {
                let (_, st2, tex2) = frame(&ctx, &mut d, &g, &s, 7);
                assert_eq!(tex2, tex, "{n}: idle frames draw the same textures");
                assert_eq!(st2.pending, 0);
                assert_eq!(st2.ready, st.ready);
            }
        }
    }

    /// A view of `ticks` ticks from `origin` (the notes area of [`geometry`]).
    fn geometry_at(origin: u64, ticks: f64) -> Geometry {
        let mut g = geometry(1);
        g.view.time = TimeAxis::new(origin, 1100.0 / ticks);
        Geometry::new(
            Rect::from_min_size(Pos2::ZERO, egui::vec2(1200.0, 700.0)),
            g.view,
            1.0,
        )
    }

    /// One frame of `s` as family `fam`, its whole output.
    fn frame_output(
        ctx: &egui::Context,
        d: &mut DenseRenderer,
        g: &Geometry,
        s: &NoteStore,
        fam: FamilyId,
    ) -> egui::FullOutput {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let p = ui.ctx().layer_painter(egui::LayerId::background());
            d.begin_frame();
            d.draw(&p, ui.ctx(), g, Role::Track, fam, || family_of(s));
        });
        out.textures_delta.clear();
        out
    }

    /// B1-0151: points of the notes area's width that no shape of the frame
    /// reaches (tiles, stand-ins, the placeholder band) — an empty column.
    fn uncovered(out: &egui::FullOutput, area: Rect) -> f32 {
        let mut spans: Vec<(f32, f32)> = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Mesh(m) if !m.vertices.is_empty() => {
                    Some(m.vertices.iter().fold((f32::MAX, f32::MIN), |(a, b), v| {
                        (a.min(v.pos.x), b.max(v.pos.x))
                    }))
                }
                egui::Shape::Rect(r) => Some((r.rect.min.x, r.rect.max.x)),
                _ => None,
            })
            .collect();
        spans.sort_by(|x, y| x.0.total_cmp(&y.0));
        let (mut at, mut gap) = (area.left(), 0.0);
        for (a, b) in spans {
            gap += (a.min(area.right()) - at).max(0.0);
            at = at.max(b);
        }
        gap + (area.right() - at).max(0.0)
    }

    /// B1-0151 (owner, Hypernova: regions stayed blank on very dense notes
    /// while zooming / scrolling): while a zoomed-out level's tiles build,
    /// the old level's tiles stand in where they reach and the placeholder
    /// band covers the rest of each tile — no column of a dense view is
    /// left empty.
    #[test]
    fn a_zoomed_out_dense_view_is_covered_end_to_end_while_its_tiles_build() {
        let span = 960 * 4 * 64;
        let s = store(400_000, span, 151);
        let ctx = egui::Context::default();
        let mut d = DenseRenderer::new(true);
        let wide = span as f64 / 8.0;
        let mid = span as f64 / 2.0;
        let g = geometry_at((mid - wide / 2.0) as u64, wide);
        frame(&ctx, &mut d, &g, &s, 7);
        let st = frame(&ctx, &mut d, &g, &s, 7).1;
        assert_eq!(st.ready, st.visible_tiles);
        assert!(uncovered(&frame_output(&ctx, &mut d, &g, &s, 7), g.notes) < 0.5);
        // Builds stop; zoom out around the same centre, then pan.
        d.sync = false;
        d.worker_allowed = false;
        for (factor, pan) in [(1.2, 0.0), (1.7, 0.0), (3.0, 0.0), (1.2, 0.3), (0.8, -0.4)] {
            let w = wide * factor;
            let g = geometry_at((mid + pan * w - w / 2.0) as u64, w);
            let out = frame_output(&ctx, &mut d, &g, &s, 7);
            let st = d.stats[0];
            assert!(st.ready < st.visible_tiles, "{st:?}");
            let gap = uncovered(&out, g.notes);
            assert!(
                gap < 0.5,
                "zoom ×{factor}, pan {pan}: {gap} pt of the view left empty ({st:?})"
            );
        }
    }

    /// B1-0151: a family replaced again before its tiles were ready (the
    /// Onion re-ranked by Dense First while panning) — the last family drawn
    /// complete still stands in, not only the half-built one in between.
    #[test]
    fn the_last_complete_family_stands_in_after_quick_family_changes() {
        let span = 960 * 4 * 64;
        let s = store(200_000, span, 152);
        let g = geometry(span);
        let ctx = egui::Context::default();
        let mut d = DenseRenderer::new(true);
        frame(&ctx, &mut d, &g, &s, 7);
        let st = frame(&ctx, &mut d, &g, &s, 7).1;
        assert_eq!(st.ready, st.visible_tiles);
        d.sync = false;
        d.worker_allowed = false;
        for fam in [8, 9, 10, 9] {
            let st = frame(&ctx, &mut d, &g, &s, fam).1;
            assert_eq!(
                (st.stand_ins, st.coarse),
                (st.visible_tiles, 0),
                "family {fam}: {st:?}"
            );
        }
    }

    /// B1-0151: a moving zoom builds at its anchor level (the shown level
    /// changes again a few frames later, its builds would be wasted); once
    /// the zoom rests for [`VIEW_REST`] — or strays beyond an octave — the
    /// shown level's own tiles are built.
    #[test]
    fn a_moving_zoom_builds_its_anchor_level_until_it_rests() {
        let span = 960 * 4 * 64;
        let s = store(100_000, span, 153);
        let ctx = egui::Context::default();
        let mut d = DenseRenderer::new(false);
        d.worker_allowed = false;
        // (The view's own tiles: the overview — L-G01 — queues behind them.)
        let queued = |d: &DenseRenderer| -> HashSet<i32> {
            d.shared
                .queue
                .lock()
                .unwrap()
                .iter()
                .filter(|j| !j.low)
                .map(|j| j.key.level)
                .collect()
        };
        let wide = span as f64 / 8.0;
        frame(&ctx, &mut d, &geometry_at(span / 4, wide), &s, 7);
        let start = d.stats[0].level;
        assert_eq!(queued(&d), HashSet::from([start]));
        for k in 1..=3 {
            let g = geometry_at(span / 4, wide * 1.2f64.powi(k));
            frame(&ctx, &mut d, &g, &s, 7);
            assert_ne!(d.stats[0].level, start, "step {k}");
            assert_eq!(queued(&d), HashSet::from([start]), "step {k}");
        }
        std::thread::sleep(VIEW_REST + std::time::Duration::from_millis(30));
        frame(
            &ctx,
            &mut d,
            &geometry_at(span / 4, wide * 1.2f64.powi(3)),
            &s,
            7,
        );
        assert_eq!(queued(&d), HashSet::from([d.stats[0].level]));
        // More than an octave away: the anchor follows at once.
        frame(&ctx, &mut d, &geometry_at(span / 4, wide * 6.0), &s, 7);
        assert_eq!(queued(&d), HashSet::from([d.stats[0].level]));
    }

    /// L-G05: while tiles build nothing redraws every frame (with 15,000
    /// Onion tracks each such frame cost 37 – 100 ms: the freeze); a worker
    /// that finishes a tile wakes the UI, and once every tile is in, nothing
    /// asks for a frame.
    #[test]
    fn a_finished_tile_wakes_the_ui_and_nothing_redraws_while_it_builds() {
        let span = 960 * 4 * 64;
        let s = store(200_000, span, 155);
        let ctx = egui::Context::default();
        let g = geometry(span);
        let mut d = DenseRenderer::new(false);
        // (egui's own first frames ask for more.)
        for _ in 0..3 {
            let mut out = ctx.run_ui(egui::RawInput::default(), |_| {});
            out.textures_delta.clear();
        }
        // No worker yet: the jobs wait in the queue.
        d.worker_allowed = false;
        frame(&ctx, &mut d, &g, &s, 9);
        // (A new view asks for one frame once it rests: B1-0151.)
        std::thread::sleep(VIEW_REST + std::time::Duration::from_millis(30));
        let (_, st, _) = frame(&ctx, &mut d, &g, &s, 9);
        assert!(st.ready < st.visible_tiles, "{st:?}");
        assert!(!d.shared.queue.lock().unwrap().is_empty());
        assert!(
            !ctx.has_requested_repaint(),
            "no frame is asked for while tiles build"
        );
        // A worker: its first finished tile asks for a frame by itself.
        d.worker_allowed = true;
        frame(&ctx, &mut d, &g, &s, 9);
        let t = std::time::Instant::now();
        while !ctx.has_requested_repaint() {
            assert!(
                t.elapsed() < std::time::Duration::from_secs(20),
                "never woken"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // Frames only when woken, until every tile (prefetch included) is in.
        let t = std::time::Instant::now();
        loop {
            if ctx.has_requested_repaint() {
                frame(&ctx, &mut d, &g, &s, 9);
            }
            let idle = d.pending.is_empty() && d.shared.queue.lock().unwrap().is_empty();
            if idle && d.stats[0].ready == d.stats[0].visible_tiles {
                break;
            }
            assert!(
                t.elapsed() < std::time::Duration::from_secs(60),
                "{:?}",
                d.stats[0]
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        // (A worker's wake can land just after its tile was taken.)
        std::thread::sleep(std::time::Duration::from_millis(50));
        frame(&ctx, &mut d, &g, &s, 9);
        frame(&ctx, &mut d, &g, &s, 9);
        assert!(!ctx.has_requested_repaint(), "idle: no frame is asked for");
    }

    /// One frame drawing `s` as `role`'s family `fam`; returns its stats.
    fn role_frame(
        ctx: &egui::Context,
        d: &mut DenseRenderer,
        g: &Geometry,
        s: &NoteStore,
        role: Role,
        fam: FamilyId,
    ) -> DenseStats {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let p = ui.ctx().layer_painter(egui::LayerId::background());
            d.begin_frame();
            d.draw(&p, ui.ctx(), g, role, fam, || family_of(s));
        });
        out.textures_delta.clear();
        d.stats[role as usize]
    }

    /// L-G01 ("lazily build the whole onion and roll in the background"):
    /// once the visible and prefetch tiles are in, the whole song is built
    /// at a coarse level (at most [`OVERVIEW_TILES`] tiles, kept); a jump
    /// to a place never shown then draws that overview, scaled, while the
    /// place's own tiles build — never the L4 band. The overview queues
    /// behind every role's own tiles.
    #[test]
    fn a_jump_after_the_overview_is_built_shows_it_instead_of_the_loading_band() {
        let span = 960 * 4 * 256;
        let s = store(300_000, span, 156);
        let ctx = egui::Context::default();
        let mut d = DenseRenderer::new(false);
        let view = span as f64 / 64.0;
        let g = geometry_at(span / 8, view);
        // Every tile of the view, the prefetch and the overview.
        let t = std::time::Instant::now();
        loop {
            let st = role_frame(&ctx, &mut d, &g, &s, Role::Track, 5);
            if st.ready == st.visible_tiles && d.pending.is_empty() {
                break;
            }
            assert!(t.elapsed() < std::time::Duration::from_secs(60), "{st:?}");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let ov = d.overview[Role::Track as usize].expect("an overview level");
        let n = (d.ends[&5] as f64 / tiles::tile_span(ov)).floor() as i64 + 1;
        assert!(n as f64 <= OVERVIEW_TILES + 1.0, "{n}");
        for i in 0..n {
            assert!(
                d.tiles.contains_key(&TileKey {
                    family: 5,
                    level: ov,
                    index: i
                }),
                "{i}"
            );
        }
        // A jump to where nothing was built, no worker to build it yet.
        for at in [span * 3 / 4, span / 2, span - (view as u64)] {
            let g = geometry_at(at, view);
            // The workers may take a job between the frame and the check:
            // what matters is the picture of the frame.
            let st = role_frame(&ctx, &mut d, &g, &s, Role::Track, 5);
            assert_eq!(st.coarse, 0, "{at}: {st:?}");
            assert_eq!(st.stand_ins + st.ready, st.visible_tiles, "{at}: {st:?}");
            // The overview stays (used every frame).
            for i in 0..n {
                assert!(d.tiles.contains_key(&TileKey {
                    family: 5,
                    level: ov,
                    index: i
                }));
            }
        }
        // Behind other tiles: a new family's view tiles queue first.
        let mut d = DenseRenderer::new(false);
        d.worker_allowed = false;
        role_frame(&ctx, &mut d, &g, &s, Role::Onion, 6);
        role_frame(&ctx, &mut d, &g, &s, Role::Track, 7);
        let q = d.shared.queue.lock().unwrap();
        let first_low = q.iter().position(|j| j.low).expect("overview jobs");
        assert!(q.iter().skip(first_low).all(|j| j.low), "overview last");
        assert!(q.iter().take(first_low).any(|j| j.key.family == 7));
        assert_eq!(d.shared.urgent.load(Ordering::SeqCst), first_low);
    }

    /// L-G01: a newly shown Onion keeps the last valid frame (nothing there
    /// yet) for [`ONION_GRACE`] where no stand-in exists, then shows the L4
    /// band — the truthful "still building" — if its tiles are not in.
    #[test]
    fn a_new_onion_keeps_the_last_valid_frame_briefly_then_shows_it_is_loading() {
        let span = 960 * 4 * 64;
        let s = store(100_000, span, 157);
        let ctx = egui::Context::default();
        let mut d = DenseRenderer::new(false);
        d.worker_allowed = false;
        let g = geometry(span);
        let st = role_frame(&ctx, &mut d, &g, &s, Role::Onion, 3);
        assert!(
            st.held > 0 && st.coarse == 0 && st.placeholder == 0.0,
            "{st:?}"
        );
        assert!(ctx.has_requested_repaint(), "a frame when the grace ends");
        std::thread::sleep(ONION_GRACE + std::time::Duration::from_millis(20));
        let st = role_frame(&ctx, &mut d, &g, &s, Role::Onion, 3);
        assert!(
            st.held == 0 && st.coarse > 0 && st.placeholder > 0.0,
            "{st:?}"
        );
        // The active track is never held (never blank, §107).
        let st = role_frame(&ctx, &mut d, &g, &s, Role::Track, 4);
        assert!(st.held == 0 && st.coarse > 0, "{st:?}");
    }

    /// B1-0151: while the zoom moves, ticks that tiles of another level
    /// already cover are not rebuilt (only newly uncovered ones are); the
    /// renderer asks for a frame after [`VIEW_REST`], which builds the shown
    /// level everywhere.
    #[test]
    fn a_moving_zoom_rebuilds_covered_ticks_only_once_it_rests() {
        let span = 960 * 4 * 64;
        let s = store(100_000, span, 154);
        let ctx = egui::Context::default();
        let mut d = DenseRenderer::new(true);
        let wide = span as f64 / 4.0;
        frame(&ctx, &mut d, &geometry_at(0, wide), &s, 7);
        let st = frame(&ctx, &mut d, &geometry_at(0, wide), &s, 7).1;
        assert_eq!(st.ready, st.visible_tiles);
        d.sync = false;
        d.worker_allowed = false;
        // Zoomed in five levels at once: the coarse tiles cover the view.
        frame(&ctx, &mut d, &geometry_at(0, wide / 2.5), &s, 7);
        let st = d.stats[0];
        assert_eq!((st.stand_ins, st.coarse), (st.visible_tiles, 0), "{st:?}");
        // (The view's own tiles: the overview — L-G01 — queues behind them.)
        let own = |d: &DenseRenderer| {
            d.shared
                .queue
                .lock()
                .unwrap()
                .iter()
                .filter(|j| !j.low)
                .count()
        };
        assert_eq!(own(&d), 0);
        assert!(ctx.has_requested_repaint(), "a frame once the zoom rests");
        std::thread::sleep(VIEW_REST + std::time::Duration::from_millis(30));
        frame(&ctx, &mut d, &geometry_at(0, wide / 2.5), &s, 7);
        let st = d.stats[0];
        assert_eq!(own(&d), st.visible_tiles + 2);
    }

    /// One frame's logical picture (v0.9 §031 / §176): every shape in paint
    /// order — tiles by their content key (texture ids differ between
    /// renderers), with vertex positions, uvs and colours; rects with their
    /// fill — hashed.
    fn logical_frame(d: &DenseRenderer, out: &egui::FullOutput) -> u64 {
        use std::hash::{Hash, Hasher};
        let by_tex: HashMap<egui::TextureId, (TileKey, u64)> = d
            .tiles
            .iter()
            .map(|(k, e)| (e.tex.id(), (*k, e.hash)))
            .collect();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for c in &out.shapes {
            match &c.shape {
                egui::Shape::Mesh(m) => {
                    by_tex.get(&m.texture_id).hash(&mut h);
                    for v in &m.vertices {
                        for f in [v.pos.x, v.pos.y, v.uv.x, v.uv.y] {
                            f.to_bits().hash(&mut h);
                        }
                        v.color.to_array().hash(&mut h);
                    }
                }
                egui::Shape::Rect(r) => {
                    for f in [r.rect.min.x, r.rect.min.y, r.rect.max.x, r.rect.max.y] {
                        f.to_bits().hash(&mut h);
                    }
                    r.fill.to_array().hash(&mut h);
                }
                other => format!("{other:?}").hash(&mut h),
            }
        }
        h.finish()
    }

    /// A Track + Onion dense scene drawn as the Piano Roll does (Onion
    /// behind): the frame's logical hash and both roles' stats.
    fn scene_frame(
        ctx: &egui::Context,
        d: &mut DenseRenderer,
        g: &Geometry,
        track: &NoteStore,
        ghosts: &[NoteStore],
    ) -> (u64, [DenseStats; 2]) {
        let onion = || Family {
            stores: ghosts.iter().map(|s| Arc::new(s.clone())).collect(),
            places: Vec::new(),
            readings: Vec::new(),
            colors: Colors {
                sources: (0..ghosts.len())
                    .map(|k| {
                        SourceColors::new(Tint::Ghost {
                            base: [(37 * k) as u8, 120, 220],
                            alpha: 200,
                            palette: Default::default(),
                        })
                    })
                    .collect(),
                selection: false,
                muted_fill: None,
                muted: 255,
                ranked: true,
            },
            opt: TileOptions {
                outline: Some(tiles::Outline::new(255, 80)),
                ..TileOptions::default()
            },
        };
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let p = ui.ctx().layer_painter(egui::LayerId::background());
            d.begin_frame();
            d.draw(&p, ui.ctx(), g, Role::Onion, 31, onion);
            d.draw(&p, ui.ctx(), g, Role::Track, 32, || family_of(track));
        });
        let hash = logical_frame(d, &out);
        out.textures_delta.clear();
        // The roll's two roles (the Clip role is the Track List's).
        (hash, [d.stats[0], d.stats[1]])
    }

    fn settle_scene(
        ctx: &egui::Context,
        d: &mut DenseRenderer,
        g: &Geometry,
        track: &NoteStore,
        ghosts: &[NoteStore],
    ) -> u64 {
        for _ in 0..5000 {
            let (h, st) = scene_frame(ctx, d, g, track, ghosts);
            if st
                .iter()
                .all(|s| s.ready == s.visible_tiles && s.pending == 0)
            {
                return h;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the scene never settled: {:?}", d.stats);
    }

    /// `n` notes in ticks `[from, to)` (a Hypernova-like burst) plus `sparse`
    /// notes over `[0, span)`.
    fn burst(n: usize, (from, to): (u64, u64), sparse: usize, span: u64, salt: u64) -> NoteStore {
        let mut s = 0x2545_F491_4F6C_DD1Du64 ^ salt;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let mut v: Vec<NoteRecord> = (0..n + sparse)
            .map(|k| NoteRecord {
                start_tick: if k < n {
                    from + next() % (to - from)
                } else {
                    next() % span
                },
                duration_ticks: 1 + next() % 120,
                attrs: NoteAttrs::new((next() % 128) as u8, 100, 0, 0, NoteOffKind::NoteOff)
                    .unwrap(),
                sub: SubchannelId((next() % 4) as u32),
                group: 0,
            })
            .collect();
        v.sort_by_key(|r| r.start_tick);
        NoteStore::from_sorted_records(&v, false).unwrap()
    }

    /// v1.0-beta1 B1-0151 (PLAYTEST_ISSUES row 6): a dense burst entering
    /// the view while panning a bar at a time renders at every step — every
    /// visible tile of both roles becomes ready, with the pixels of the
    /// absolute tick window (the inline build), notes entering from earlier
    /// ticks included.
    #[test]
    fn a_dense_burst_entering_the_view_renders_at_every_pan_step() {
        let bar = 960 * 4;
        let span = bar * 64;
        let track = burst(400_000, (20 * bar, 24 * bar), 20_000, span, 1);
        let ghosts: Vec<NoteStore> = (0..6)
            .map(|k| burst(150_000, (20 * bar, 24 * bar), 10_000, span, 10 + k))
            .collect();
        let ctx = egui::Context::default();
        let mut g = geometry(span);
        // Two bars across the 1,100 px roll.
        g.view.time = TimeAxis::new(0, 1100.0 / (2 * bar) as f64);
        let mut worker = DenseRenderer::new(false);
        let mut sync = DenseRenderer::new(true);
        for step in 0..48u64 {
            g.view.time.origin_tick = 17 * bar + step * bar / 6;
            let t = std::time::Instant::now();
            settle_scene(&ctx, &mut worker, &g, &track, &ghosts);
            let waited = t.elapsed();
            settle_scene(&ctx, &mut sync, &g, &track, &ghosts);
            for fam in [31, 32] {
                let want: HashMap<(i32, i64), u64> = sync
                    .tile_hashes(fam)
                    .into_iter()
                    .map(|(l, i, h)| ((l, i), h))
                    .collect();
                for (l, i, h) in worker.tile_hashes(fam) {
                    if let Some(&w) = want.get(&(l, i)) {
                        assert_eq!(h, w, "step {step}: family {fam} tile {l}/{i}");
                    }
                }
            }
            assert!(
                waited < std::time::Duration::from_secs(30),
                "step {step}: {waited:?}"
            );
        }
    }

    /// v0.9 §031 / §176 / §262: a fixed scene gives one logical picture —
    /// the same over 1,000 paused frames, the same whether tiles were built
    /// inline or on the worker (any arrival order), and again after panning
    /// away and back.
    #[test]
    fn a_fixed_scene_has_one_picture_whatever_the_tile_arrival_order() {
        let span = 960 * 4 * 64;
        let track = store(250_000, span, 7);
        let ghosts: Vec<NoteStore> = (0..12).map(|k| store(40_000, span, 300 + k)).collect();
        let g = geometry(span);
        let ctx = egui::Context::default();
        let mut sync = DenseRenderer::new(true);
        let a = settle_scene(&ctx, &mut sync, &g, &track, &ghosts);
        let mut worker = DenseRenderer::new(false);
        let b = settle_scene(&ctx, &mut worker, &g, &track, &ghosts);
        assert_eq!(a, b, "inline and worker builds give the same picture");
        let roll = |d: &DenseRenderer| [d.stats[0].picture, d.stats[1].picture];
        let pictures = roll(&sync);
        assert!(pictures.iter().all(|&p| p != 0));
        assert_eq!(roll(&worker), pictures);
        for _ in 0..1000 {
            let (h, st) = scene_frame(&ctx, &mut worker, &g, &track, &ghosts);
            assert_eq!(h, a, "paused frames never change");
            assert!(st.iter().all(|s| s.pending == 0));
            assert_eq!(st.map(|s| s.picture), pictures);
        }
        // Pan away (new tiles build) and back: the same picture again.
        let mut g2 = g;
        g2.view.time.origin_tick += span / 3;
        settle_scene(&ctx, &mut worker, &g2, &track, &ghosts);
        assert_eq!(settle_scene(&ctx, &mut worker, &g, &track, &ghosts), a);
    }

    /// L-G06 (L-S28, after Wave G): at a zoom where the background overview
    /// and the Onion grace apply, a fixed scene still has one picture — the
    /// worker's (overview on) equals the inline build's, paused frames never
    /// change — and a jump to an unbuilt place shows the overview (never the
    /// L4 band) before settling on the same picture the inline build gives.
    #[test]
    fn the_overview_keeps_one_picture_and_jumps_never_show_the_loading_band() {
        let span = 960 * 4 * 256;
        let track = store(300_000, span, 11);
        let ghosts: Vec<NoteStore> = (0..12).map(|k| store(40_000, span, 400 + k)).collect();
        let view = span as f64 / 64.0;
        let g = geometry_at(span / 3, view);
        let ctx = egui::Context::default();
        let mut sync = DenseRenderer::new(true);
        let a = settle_scene(&ctx, &mut sync, &g, &track, &ghosts);
        let mut worker = DenseRenderer::new(false);
        let b = settle_scene(&ctx, &mut worker, &g, &track, &ghosts);
        assert_eq!(a, b, "inline and worker builds give the same picture");
        assert!(worker.overview[0].is_some() && worker.overview[1].is_some());
        for _ in 0..300 {
            let (h, st) = scene_frame(&ctx, &mut worker, &g, &track, &ghosts);
            assert_eq!(h, a, "paused frames never change");
            assert!(st.iter().all(|s| s.pending == 0 && s.stand_ins == 0));
        }
        let far = geometry_at(span * 3 / 4, view);
        let (_, st) = scene_frame(&ctx, &mut worker, &far, &track, &ghosts);
        assert!(
            st.iter().all(|s| s.coarse == 0 && s.held == 0),
            "the overview stands in: {st:?}"
        );
        assert!(st.iter().any(|s| s.stand_ins > 0), "{st:?}");
        let settled = settle_scene(&ctx, &mut worker, &far, &track, &ghosts);
        assert_eq!(
            settled,
            settle_scene(&ctx, &mut sync, &far, &track, &ghosts)
        );
    }

    /// v0.9 §182 / §031: with the active track and the Onion both dense,
    /// each role's per-frame request must not drop the other role's queued
    /// tiles — or one of them never finishes and keeps flickering through
    /// stand-ins while paused. Both settle, then every frame is identical.
    #[test]
    fn track_and_onion_tiles_both_finish_and_paused_frames_are_identical() {
        let span = 960 * 4 * 64;
        let track = store(300_000, span, 1);
        let ghosts: Vec<NoteStore> = (0..8).map(|k| store(60_000, span, 100 + k)).collect();
        let g = geometry(span);
        let ctx = egui::Context::default();
        let mut d = DenseRenderer::new(false);
        let onion_family = || Family {
            stores: ghosts.iter().map(|s| Arc::new(s.clone())).collect(),
            places: Vec::new(),
            readings: Vec::new(),
            colors: Colors {
                sources: (0..8)
                    .map(|k| {
                        SourceColors::new(Tint::Ghost {
                            base: [(40 * k) as u8, 90, 200],
                            alpha: 255,
                            palette: Default::default(),
                        })
                    })
                    .collect(),
                selection: false,
                muted_fill: None,
                muted: 255,
                ranked: true,
            },
            opt: TileOptions::default(),
        };
        // One frame as the Piano Roll draws it: Onion behind, track in front.
        let run = |d: &mut DenseRenderer| {
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                let p = ui.ctx().layer_painter(egui::LayerId::background());
                d.begin_frame();
                d.draw(&p, ui.ctx(), &g, Role::Onion, 21, onion_family);
                d.draw(&p, ui.ctx(), &g, Role::Track, 22, || family_of(&track));
            });
            out.textures_delta.clear();
            let shapes: Vec<String> = out
                .shapes
                .iter()
                .map(|c| format!("{:?}", c.shape))
                .collect();
            (d.stats, shapes)
        };
        let mut frames = None;
        for f in 0..3000 {
            let (st, _) = run(&mut d);
            if st
                .iter()
                .all(|s| s.ready == s.visible_tiles && s.pending == 0)
            {
                frames = Some(f);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        eprintln!("settled after {frames:?} frames");
        assert!(frames.is_some(), "both roles' tiles finish: {:?}", d.stats);
        let (_, first) = run(&mut d);
        for _ in 0..200 {
            let (st, shapes) = run(&mut d);
            assert!(
                st.iter()
                    .all(|s| s.ready == s.visible_tiles && s.pending == 0)
            );
            assert_eq!(shapes, first, "paused frames are identical");
        }
    }

    #[test]
    fn the_worker_builds_in_the_background_and_old_content_stands_in() {
        // §109 / §110: while a new family builds, the previous one is shown.
        let span = 960 * 4 * 32;
        let a = store(200_000, span, 1);
        let b = store(200_000, span, 2);
        let g = geometry(span);
        let ctx = egui::Context::default();
        let mut d = DenseRenderer::new(false);
        let (_, st, _) = frame(&ctx, &mut d, &g, &a, 11);
        assert!(st.coarse > 0, "first frame: coarse occupancy");
        d.settle(&ctx);
        let (_, st, _) = frame(&ctx, &mut d, &g, &a, 11);
        assert_eq!(st.ready, st.visible_tiles);
        // New content (an edit): the old tiles stand in until the new ones
        // arrive — never coarse, never blank.
        let (shapes, st, _) = frame(&ctx, &mut d, &g, &b, 12);
        assert!(shapes > 0);
        assert_eq!(st.coarse, 0, "{st:?}");
        assert_eq!(st.stand_ins + st.ready, st.visible_tiles, "{st:?}");
        d.settle(&ctx);
        let (_, st, _) = frame(&ctx, &mut d, &g, &b, 12);
        assert_eq!(st.ready, st.visible_tiles);
        // Scrolling by half a screen reuses the tiles still visible.
        let mut g2 = g;
        g2.view.time.origin_tick += span / 2;
        let (_, st2, _) = frame(&ctx, &mut d, &g2, &b, 12);
        assert!(
            st2.ready > 0 && st2.ready < st2.visible_tiles + 1,
            "{st2:?}"
        );
    }

    /// v1.0-beta1 B1-0152 (contract §022-A): after the view settles, the
    /// first frame after a one-pixel scroll, a zoom within the level, a zoom
    /// by one level in or out, a window widened by up to the prefetched tiles
    /// or an edit shows real tiles only — ready or a stand-in of another level
    /// / the previous content, never the L4 placeholder — so no change
    /// flickers through missing notes.
    #[test]
    fn view_changes_publish_coherent_frames_without_placeholders() {
        let span = 960 * 4 * 32;
        let a = store(200_000, span, 3);
        let b = store(200_000, span, 4);
        let ctx = egui::Context::default();
        let at = |width: f32, ppt: f64, origin: u64| {
            let full = Rect::from_min_size(Pos2::ZERO, egui::vec2(width, 700.0));
            let view = RollView {
                time: TimeAxis::new(origin, ppt),
                pitch: PitchAxis {
                    scroll_rows: 0.0,
                    row_height: 5.0,
                    top: plumspirit_engine::model::note::MAX_PITCH,
                },
            };
            Geometry::new(full, view, 1.0)
        };
        // Mid-way between two levels (quarter octaves).
        let ppt = tiles::level_ppt(tiles::level_for(4.0 * 1100.0 / span as f64)) * 2f64.powf(0.125);
        let origin = span / 3;
        let base = at(1200.0, ppt, origin);
        let mut d = DenseRenderer::new(false);
        let settle = |d: &mut DenseRenderer, g: &Geometry, s: &NoteStore, fam: FamilyId| {
            for _ in 0..20_000 {
                let (_, st, _) = frame(&ctx, d, g, s, fam);
                if st.ready == st.visible_tiles && st.pending == 0 {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            panic!("never settled");
        };
        settle(&mut d, &base, &a, 21);
        // A tile is 256 texels: at this zoom the prefetch (one tile left,
        // two right) covers a window up to two tiles wider.
        let tile_px =
            (tiles::TILE_COLS as f64 / tiles::level_ppt(tiles::level_for(ppt)) * ppt) as f32;
        let level = |f: f64| tiles::level_for(ppt * f) - tiles::level_for(ppt);
        let cases: Vec<(&str, Geometry)> = vec![
            (
                "scroll one pixel",
                at(1200.0, ppt, origin + (1.0 / ppt).ceil() as u64),
            ),
            ("zoom within the level", at(1200.0, ppt * 1.05, origin)),
            ("zoom in one level", at(1200.0, ppt * 1.19, origin)),
            ("zoom out one level", at(1200.0, ppt / 1.19, origin)),
            (
                "widen by the prefetch",
                at(1200.0 + tile_px * 1.5, ppt, origin),
            ),
        ];
        assert_eq!(level(1.05), 0);
        assert_eq!(level(1.19).abs(), 1);
        for (what, g) in &cases {
            let t = std::time::Instant::now();
            let (shapes, st, _) = frame(&ctx, &mut d, g, &a, 21);
            println!(
                "{what}: first frame {:?} ({} ready, {} stand-ins)",
                t.elapsed(),
                st.ready,
                st.stand_ins
            );
            assert!(shapes > 0, "{what}");
            assert_eq!(st.coarse, 0, "{what}: {st:?}");
            assert_eq!(st.ready + st.stand_ins, st.visible_tiles, "{what}: {st:?}");
            settle(&mut d, &base, &a, 21);
        }
        // An edit: the previous content stands in.
        let t = std::time::Instant::now();
        let (_, st, _) = frame(&ctx, &mut d, &base, &b, 22);
        println!(
            "edit: first frame {:?} ({} stand-ins)",
            t.elapsed(),
            st.stand_ins
        );
        assert_eq!(st.coarse, 0, "edit: {st:?}");
        assert_eq!(st.ready + st.stand_ins, st.visible_tiles, "edit: {st:?}");
    }

    #[test]
    fn dense_mode_switches_with_hysteresis() {
        let mut d = DenseRenderer::new(true);
        assert!(!d.wants_dense(Role::Track, ENTER_DENSE));
        assert!(d.wants_dense(Role::Track, ENTER_DENSE + 1));
        assert!(
            d.wants_dense(Role::Track, LEAVE_DENSE + 1),
            "stays dense above the lower bound"
        );
        assert!(!d.wants_dense(Role::Track, LEAVE_DENSE));
        assert!(
            !d.wants_dense(Role::Track, ENTER_DENSE - 1),
            "stays exact below the upper bound"
        );
        assert!(
            !d.wants_dense(Role::Onion, ENTER_DENSE),
            "roles are independent"
        );
    }

    #[test]
    fn the_cache_stays_within_its_budget() {
        let span = 960 * 4 * 512;
        let s = store(50_000, span, 3);
        let ctx = egui::Context::default();
        let mut d = DenseRenderer::new(true);
        let mut g = geometry(span / 64);
        // Walk across the song at a fine zoom: many distinct tiles.
        for k in 0..400u64 {
            g.view.time.origin_tick = k * span / 400;
            frame(&ctx, &mut d, &g, &s, 5);
            frame(&ctx, &mut d, &g, &s, 5);
            assert!(d.tiles.len() * tiles::TILE_BYTES <= BUDGET_BYTES + 64 * tiles::TILE_BYTES);
            assert_eq!(d.budget, BUDGET_BYTES);
        }
    }

    #[test]
    #[ignore = "measurement (release): cargo test --release -p plumspirit dense_ten_million -- --ignored --nocapture"]
    fn dense_ten_million_notes_are_never_blank() {
        let span = 960 * 4 * 256;
        let s = store(10_000_000, span, 10);
        let g = geometry(span);
        let ctx = egui::Context::default();
        let mut d = DenseRenderer::new(false);
        let t = std::time::Instant::now();
        let mut frames = 0;
        loop {
            let (shapes, st, _) = frame(&ctx, &mut d, &g, &s, 99);
            assert!(shapes > 0, "frame {frames}: blank");
            frames += 1;
            if st.ready == st.visible_tiles {
                println!(
                    "10M notes: {frames} frames, {:?} until every tile was ready; last tile {} µs",
                    t.elapsed(),
                    st.last_build_micros
                );
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
            assert!(frames < 100_000);
        }
    }

    /// L-G01 / L-G05 baseline (L-S25): the Onion Skin of many tracks
    /// (default 139 × 100,000 notes over 40 bars at 1,920 PPQN: the local
    /// density of the 95M-note `evans speedblack` video) at a 4-bar view, real worker threads, a
    /// frame every 16 ms. Prints, for activation, a hand pan (a tenth of the
    /// view a frame) and a jump: frames until every visible tile is ready,
    /// frames that showed the L4 placeholder (the "slab") and its mean share
    /// of the view. Env: PLUMSPIRIT_BENCH_ONION_TRACKS / _NOTES / _BARS.
    #[test]
    #[ignore = "measurement (release): cargo test --release -p plumspirit onion_pan_never_shows_unfinished_tiles -- --ignored --nocapture"]
    fn onion_pan_never_shows_unfinished_tiles() {
        let env = |k: &str, d: usize| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d)
        };
        let tracks = env("PLUMSPIRIT_BENCH_ONION_TRACKS", 139);
        let per = env("PLUMSPIRIT_BENCH_ONION_NOTES", 100_000);
        // The video's density: 95M notes over ~300 bars is ~1.3M notes in a
        // 4-bar view; 13.9M over 40 bars gives the same.
        let bars = env("PLUMSPIRIT_BENCH_ONION_BARS", 40) as u64;
        let span = 1920 * 4 * bars;
        let t = std::time::Instant::now();
        let stores: Vec<Arc<NoteStore>> = (0..tracks)
            .map(|k| Arc::new(store(per, span, k as u64)))
            .collect();
        println!(
            "onion: {tracks} tracks x {per} notes ({} M) over {bars} bars built in {:?}",
            tracks * per / 1_000_000,
            t.elapsed()
        );
        let fam = || Family {
            stores: stores.clone(),
            places: Vec::new(),
            readings: Vec::new(),
            colors: Colors {
                sources: (0..tracks)
                    .map(|k| {
                        SourceColors::new(Tint::Ghost {
                            base: [(k * 37 % 256) as u8, 90, 160],
                            alpha: 255,
                            palette: Default::default(),
                        })
                    })
                    .collect(),
                selection: false,
                muted_fill: None,
                muted: 90,
                ranked: false,
            },
            opt: TileOptions::default(),
        };
        let view = (1920 * 4 * 4) as f64;
        for overview in [false, true] {
            let ctx = egui::Context::default();
            let mut d = DenseRenderer::new(false);
            d.overview_on = overview;
            let mut g = geometry(1920 * 4 * 4);
            g.view.time.origin_tick = span / 10;
            let mut run = |g: &Geometry, d: &mut DenseRenderer| {
                let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                    let p = ui.ctx().layer_painter(egui::LayerId::background());
                    d.begin_frame();
                    d.draw(&p, ui.ctx(), g, Role::Onion, 77, fam);
                });
                out.textures_delta.clear();
                std::thread::sleep(std::time::Duration::from_millis(16));
                d.stats[Role::Onion as usize]
            };
            let report = |name: &str, sts: &[DenseStats], t: std::time::Duration| {
                let slab = sts.iter().filter(|s| s.coarse > 0).count();
                let share: f32 = sts.iter().map(|s| s.placeholder).sum::<f32>() / sts.len() as f32;
                let stand = sts.iter().filter(|s| s.stand_ins > 0).count();
                let held = sts.iter().filter(|s| s.held > 0).count();
                let ready = sts
                    .iter()
                    .position(|s| s.ready == s.visible_tiles)
                    .map_or(-1, |n| n as i64 + 1);
                println!(
                    "  {name}: {} frames in {t:?}; first all-ready frame {ready}; L4 slab in {slab} frames (mean share {:.1} %), stand-ins in {stand}, held (grace) in {held}",
                    sts.len(),
                    share * 100.0
                );
            };
            println!("overview {}:", if overview { "on" } else { "off" });
            let until_ready =
                |g: &Geometry,
                 d: &mut DenseRenderer,
                 run: &mut dyn FnMut(&Geometry, &mut DenseRenderer) -> DenseStats| {
                    let t = std::time::Instant::now();
                    let mut sts = Vec::new();
                    loop {
                        let st = run(g, d);
                        sts.push(st);
                        if st.ready == st.visible_tiles || sts.len() > 3000 {
                            break;
                        }
                    }
                    (sts, t.elapsed())
                };
            let (sts, t) = until_ready(&g, &mut d, &mut run);
            report("activation", &sts, t);
            // Let the background work finish (prefetch, overview).
            let t = std::time::Instant::now();
            while !d.pending.is_empty() && t.elapsed() < std::time::Duration::from_secs(120) {
                run(&g, &mut d);
            }
            println!("  background work done after {:?} more", t.elapsed());
            for (name, step, frames) in [
                ("pan 1/10 view a frame", 10.0, 60),
                ("pan 1/3 view a frame", 3.0, 30),
            ] {
                let t = std::time::Instant::now();
                let mut sts = Vec::new();
                for _ in 0..frames {
                    g.view.time.origin_tick += (view / step) as u64;
                    if g.view.time.origin_tick as f64 + view > span as f64 {
                        g.view.time.origin_tick = 0;
                    }
                    sts.push(run(&g, &mut d));
                }
                report(name, &sts, t.elapsed());
            }
            for (k, at) in [span * 4 / 5, span / 2, span * 7 / 10]
                .into_iter()
                .enumerate()
            {
                g.view.time.origin_tick = at;
                let (sts, t) = until_ready(&g, &mut d, &mut run);
                report(&format!("jump {}", k + 1), &sts, t);
            }
            println!(
                "  last tile {} us; cache {} tiles, {} MB",
                d.stats[Role::Onion as usize].last_build_micros,
                d.tiles.len(),
                d.tiles.values().map(Entry::bytes).sum::<usize>() >> 20
            );
        }
    }

    /// B1-0059: Rendering Quality moves the exact / dense switch (with the
    /// same three-quarter hysteresis); Balanced is the engine's limit.
    #[test]
    fn rendering_quality_moves_the_dense_threshold() {
        let mut d = DenseRenderer::new(true);
        assert_eq!(RenderQuality::default().exact_limit(), ENTER_DENSE);
        for q in RenderQuality::ALL {
            d.set_quality(q);
            let n = q.exact_limit();
            assert_eq!(d.enter_threshold(), n);
            assert!(!d.wants_dense(Role::Track, n), "{q:?}");
            assert!(d.wants_dense(Role::Track, n + 1), "{q:?}");
            assert!(d.wants_dense(Role::Track, n * 3 / 4 + 1), "stays dense");
            assert!(!d.wants_dense(Role::Track, n * 3 / 4), "leaves");
            assert_eq!(RenderQuality::from_key(q.key()), Some(q));
        }
        assert!(RenderQuality::Fast.exact_limit() < ENTER_DENSE);
        assert!(RenderQuality::Detailed.exact_limit() > ENTER_DENSE);
    }

    /// B1-0059: the prefetch distance and the cache budget from Settings
    /// decide which tiles are built and how many stay.
    #[test]
    fn prefetch_distance_and_cache_budget_follow_the_settings() {
        let span = 960 * 4 * 512;
        let s = store(50_000, span, 3);
        let ctx = egui::Context::default();
        let mut g = geometry(span / 64);
        g.view.time.origin_tick = span / 2;
        let indices = |prefetch: u8| {
            let mut d = DenseRenderer::new(true);
            d.configure(0, prefetch);
            frame(&ctx, &mut d, &g, &s, 5);
            let v: Vec<i64> = d.tile_hashes(5).iter().map(|t| t.1).collect();
            (v.len(), *v.first().unwrap(), *v.last().unwrap())
        };
        let (n0, lo0, hi0) = indices(0);
        let (n1, lo1, hi1) = indices(DEFAULT_PREFETCH_TILES);
        let (n3, lo3, hi3) = indices(3);
        assert_eq!(
            (lo1, hi1),
            (lo0 - 1, hi0 + 2),
            "the default: one before, two after"
        );
        assert_eq!((lo3, hi3), (lo0 - 3, hi0 + 4));
        assert_eq!((n1, n3), (n0 + 3, n0 + 7));
        assert_eq!(
            indices(200).1,
            lo0 - i64::from(MAX_PREFETCH_TILES),
            "capped"
        );
        // Budget: a small one holds fewer tiles; 0 is the automatic one.
        let mut d = DenseRenderer::new(true);
        d.configure(MIN_CACHE_MB, 1);
        assert_eq!(d.budget(), (MIN_CACHE_MB as usize) << 20);
        for k in 0..200u64 {
            g.view.time.origin_tick = k * span / 200;
            frame(&ctx, &mut d, &g, &s, 5);
            frame(&ctx, &mut d, &g, &s, 5);
            assert!(d.tiles.len() * tiles::TILE_BYTES <= d.budget() + 64 * tiles::TILE_BYTES);
        }
        d.configure(0, 1);
        assert_eq!(
            d.budget(),
            BUDGET_BYTES,
            "automatic (tests: the normal budget)"
        );
        d.configure(1, 1);
        assert_eq!(d.budget(), (MIN_CACHE_MB as usize) << 20, "clamped");
    }

    #[test]
    fn low_ram_pcs_get_the_smaller_tile_cache() {
        assert_eq!(budget_for(Some(2 << 30)), LOW_END_BUDGET_BYTES);
        assert_eq!(budget_for(Some(3 << 30)), LOW_END_BUDGET_BYTES);
        assert_eq!(budget_for(Some(4 << 30)), BUDGET_BYTES);
        assert_eq!(budget_for(None), BUDGET_BYTES);
    }
    /// One Track List frame drawing `clips` (each with its family maker).
    fn clip_frame(
        ctx: &egui::Context,
        d: &mut DenseRenderer,
        clips: &[(ClipTiles, &NoteStore)],
    ) -> (Vec<bool>, bool) {
        let mut complete = Vec::new();
        let mut pending = false;
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let p = ui.ctx().layer_painter(egui::LayerId::background());
            d.begin_clips(ui.ctx());
            for (c, s) in clips {
                let clipped = p.with_clip_rect(c.body);
                complete.push(d.draw_clip(&clipped, c, || clip_family(s)));
            }
            pending = d.end_clips(ui.ctx());
        });
        out.textures_delta.clear();
        (complete, pending)
    }

    /// A clip family: the content's notes folded onto 24 rows, the notes
    /// starting in `[0, span)`.
    fn clip_family(s: &NoteStore) -> Family {
        let mut f = family_of(s);
        f.opt = TileOptions {
            fold: Some(tiles::KeyFold::new(0, 127, 24)),
            window: Some((0, u64::MAX)),
            ..TileOptions::default()
        };
        f
    }

    fn clip_at(family: FamilyId, x0: f32, span: u64) -> ClipTiles {
        let ppt = 600.0 / span as f64;
        ClipTiles {
            family,
            previous: None,
            level: tiles::level_for(ppt),
            range: (0, span),
            origin: 0.0,
            x0,
            ppt,
            body: Rect::from_min_size(Pos2::new(x0, 40.0), egui::vec2(600.0, 24.0)),
        }
    }

    /// V09-S09 (§046, §273): a clip's tiles are built once and reused when the
    /// clip moves (same family, another place) — nothing is rebuilt — and two
    /// clips of one family (linked copies) share them.
    #[test]
    fn clip_tiles_are_built_once_and_shared_when_clips_move_or_repeat() {
        let span = 960 * 4 * 16;
        let s = store(30_000, span, 77);
        let ctx = egui::Context::default();
        let mut d = DenseRenderer::new(true);
        let (done, pending) = clip_frame(&ctx, &mut d, &[(clip_at(5, 10.0, span), &s)]);
        assert_eq!(
            (done, pending),
            (vec![false], false),
            "built inline at the frame's end"
        );
        let built = d.built;
        assert!(built >= 3, "{built}");
        // Moved, and a linked copy beside it: every tile ready, none rebuilt.
        let (done, _) = clip_frame(
            &ctx,
            &mut d,
            &[(clip_at(5, 300.0, span), &s), (clip_at(5, 900.0, span), &s)],
        );
        assert_eq!(done, vec![true, true]);
        assert_eq!(d.built, built);
        let st = d.stats[Role::Clip as usize];
        assert_eq!((st.stand_ins, st.coarse), (0, 0));
        assert_eq!(st.ready, st.visible_tiles);
    }

    /// V09-S09 (§045 never blank, §207): while a clip's tiles are queued it
    /// shows the neutral chunk-span band; a clip that is no longer drawn
    /// loses its queued builds; a changed content shows its previous
    /// family's tiles until the new ones arrive.
    #[test]
    fn clips_are_never_blank_and_unwanted_clip_builds_leave_the_queue() {
        let span = 960 * 4 * 16;
        let s = store(30_000, span, 78);
        let ctx = egui::Context::default();
        let mut d = DenseRenderer::new(false);
        d.worker_allowed = false;
        let (done, pending) = clip_frame(&ctx, &mut d, &[(clip_at(9, 10.0, span), &s)]);
        assert_eq!((done, pending), (vec![false], true));
        let st = d.stats[Role::Clip as usize];
        assert!(st.coarse > 0 && st.coarse == st.visible_tiles, "{st:?}");
        let queued = d.shared.queue.lock().unwrap().len();
        assert_eq!(queued, st.visible_tiles);
        assert!(d.shared.clips.lock().unwrap().contains(&9));
        // The clip scrolled out of view: its builds leave the queue.
        let (done, pending) = clip_frame(&ctx, &mut d, &[]);
        assert!(done.is_empty() && !pending);
        assert_eq!(d.shared.queue.lock().unwrap().len(), 0);
        assert!(d.shared.clips.lock().unwrap().is_empty());
        // Family 9 built (inline), then the content changes (family 10):
        // family 9's tiles stand in, nothing coarse.
        d.sync = true;
        clip_frame(&ctx, &mut d, &[(clip_at(9, 10.0, span), &s)]);
        d.sync = false;
        let mut next = clip_at(10, 10.0, span);
        next.previous = Some(9);
        let (done, _) = clip_frame(&ctx, &mut d, &[(next, &s)]);
        assert_eq!(done, vec![false]);
        let st = d.stats[Role::Clip as usize];
        assert_eq!((st.stand_ins, st.coarse), (st.visible_tiles, 0), "{st:?}");
    }
}
