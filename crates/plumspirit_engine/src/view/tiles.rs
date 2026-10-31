//! Dense-note tiles (v0.8 contract §106–§114, §208, §214, §224–§226; v0.9
//! §028, §178, §179): the colour-aware raster behind the Piano Roll's dense
//! levels of detail, for the active track and Onion Skin alike.
//!
//! The time axis is cut into tiles of [`TILE_COLS`] columns at discrete zoom
//! levels (`LEVELS_PER_OCTAVE` per doubling), so scrolling and autoscroll reuse
//! every still-visible tile and build only newly exposed ones; each tile holds
//! all 128 pitch rows. A texel shows exactly one note's colour: its
//! [`Winner`], the note the exact renderer would draw last there (selection
//! in front, then layers by rank, then stored order). The winner's colour,
//! source and subchannel travel with it (v0.8 counted at most 32 colour
//! classes per tile and hashed further ones into existing classes — §028);
//! memory per tile is fixed whatever the number of colours, sources or notes.
//! The opacity grows with the number of overlapping notes. A one-column apron
//! on both sides makes outlines continuous across tile seams. Same input →
//! same pixels (no frame-to-frame variation, no dependence on chunking).

use crate::model::clip::ClipPlacement;
use crate::model::correction::{CorrectedRuns, Correction};
use crate::model::note::NoteRecord;
use crate::model::note_store::{NoteStore, NoteView};
use crate::model::project::KeyRange;

/// Columns per tile (physical pixels at the tile's level).
pub const TILE_COLS: usize = 256;
/// Pitch rows of a standard tile (keys 0–127).
pub const ROWS: usize = 128;
/// Pitch rows of a 256-Key Mode tile (v0.9 §036).
pub const MAX_ROWS: usize = 256;
/// Zoom levels per doubling of the scale.
pub const LEVELS_PER_OCTAVE: f64 = 4.0;
/// Bytes of one standard tile's pixels (RGBA8).
pub const TILE_BYTES: usize = TILE_COLS * ROWS * 4;

/// Bytes of one tile's pixels for a key range (256-Key Mode tiles are twice
/// as tall; standard ones cost exactly what they did — §206).
pub fn tile_bytes(keys: KeyRange) -> usize {
    TILE_COLS * keys.keys() * 4
}

/// The coarsest level at or below `ppt` (pixels per tick): its tiles are
/// stretched by at most 2^(1/4) ≈ 1.19 on screen, never shrunk, so no
/// one-column note can fall between screen pixels.
pub fn level_for(ppt: f64) -> i32 {
    if !(ppt.is_finite() && ppt > 0.0) {
        return 0;
    }
    (ppt.log2() * LEVELS_PER_OCTAVE + 1e-9).floor() as i32
}

/// Pixels per tick of `level`.
pub fn level_ppt(level: i32) -> f64 {
    (f64::from(level) / LEVELS_PER_OCTAVE).exp2()
}

/// Ticks covered by one tile of `level`.
pub fn tile_span(level: i32) -> f64 {
    TILE_COLS as f64 / level_ppt(level)
}

/// First tick of tile `index` at `level`.
pub fn tile_start(level: i32, index: i64) -> f64 {
    index as f64 * tile_span(level)
}

/// Tile indices (inclusive) covering ticks `[t0, t1]` at `level`.
pub fn tile_range(level: i32, t0: u64, t1: u64) -> (i64, i64) {
    let s = tile_span(level);
    (
        (t0 as f64 / s).floor() as i64,
        (t1 as f64 / s).floor() as i64,
    )
}

/// Colours of the notes drawn into tiles. `source` is the index into the
/// stores passed to [`build_tile`].
pub trait TileColors: Sync {
    /// Unmultiplied RGBA of a note — the exact renderer's fill for it
    /// (subchannel, velocity, selection, mute: v0.9 §029) — and whether it is
    /// drawn as selected (in front of every other note).
    fn color(&self, source: usize, note: &NoteView) -> ([u8; 4], bool);
    /// Layer rank of a source (higher = in front, §116): of the notes covering
    /// a texel, those of the highest rank win it. Equal ranks are drawn in
    /// source order. Default: one layer.
    fn rank(&self, _source: usize) -> u16 {
        0
    }
    /// The border colour the exact renderer gives the winning note `w`, used
    /// for its edge texels; `None` (default): [`outline_shade`] of its fill at
    /// the outline darkness.
    fn border(&self, _w: &Winner) -> Option<[u8; 3]> {
        None
    }
}

/// Outline colour at `darkness` percent (v0.8 §121): 0 % total white, 50 %
/// the colour itself, 100 % total black, linear in between (sRGB). Not an
/// opacity. The one implementation (the app's theme uses it too).
pub fn outline_shade(rgb: [u8; 3], darkness: u8) -> [u8; 3] {
    let t = f32::from(darkness.min(100)) / 100.0;
    let (target, f) = if t < 0.5 {
        (255.0, 1.0 - t * 2.0)
    } else {
        (0.0, (t - 0.5) * 2.0)
    };
    rgb.map(|v| (f32::from(v) + (target - f32::from(v)) * f).round() as u8)
}

/// Shortest run of texels (along a pitch row) that gets outlined at its ends,
/// like the exact renderer, which borders notes at least 4 px wide: a shorter
/// run keeps its fill (an outline would replace it entirely).
pub const MIN_RUN: usize = 4;
/// Shortest stack of pitch rows of one structure whose top and bottom rows
/// are outlined (when [`Outline::vertical`]).
pub const MIN_STACK: usize = 3;

/// Outlines of dense notes (v0.9 §026, §027, §180; v0.8 §120–§122). A texel
/// is an *edge* when it ends a run of at least [`MIN_RUN`] texels along its
/// row — a run of one note (per-note boundaries) or of one structure (source,
/// subchannel, selection), whatever is next to it: nothing, another structure
/// or another note — or, with `vertical`, when it is the top or bottom row of
/// a stack of at least [`MIN_STACK`] rows of one structure. Edges are drawn
/// in [`outline_shade`] of the winner's colour, selected ones in `selection`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Outline {
    /// Edge texels are drawn at least this opaque.
    pub alpha: u8,
    /// Outline darkness (percent, see [`outline_shade`]).
    pub darkness: u8,
    /// Colour of the edges of selected notes (the selection border); `None`:
    /// shaded like the others.
    pub selection: Option<[u8; 3]>,
    /// Outline tops and bottoms too (pitch rows tall enough on screen for a
    /// texel-high border, as the exact renderer's 4 px rule).
    pub vertical: bool,
    /// v1.0-beta2 B2-D02: also return each texel's border colour
    /// ([`TileOut::borders`]) so the caller draws every note's top and bottom
    /// border in screen space — one physical line per row edge however tall
    /// the rows are, between every two notes, also of one track and
    /// subchannel. The roll uses this with `vertical: false` (a texel-high
    /// top / bottom edge is a whole row tall on screen and only ends stacks).
    pub borders: bool,
}

impl Outline {
    /// Outlines at `darkness` and opacity `alpha`, vertical edges included,
    /// selection shaded like everything else.
    pub fn new(alpha: u8, darkness: u8) -> Outline {
        Outline {
            alpha,
            darkness,
            selection: None,
            vertical: true,
            borders: false,
        }
    }
}

/// How texels are shaded. A texel's opacity is always that of the notes
/// covering it composited like the exact renderer's translucent rectangles —
/// `n` notes of the winner's alpha `a`: 1 − (1 − a)^n — so one note shows
/// exactly its own opacity (100 % ghosts are fully opaque, 0 % invisible)
/// and no hidden density factor exists (v0.9 §025, §186).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TileOptions {
    /// Outlines; `None`: none.
    pub outline: Option<Outline>,
    /// Pitch rows: 128, or 256 in 256-Key Mode (v0.9 §036). Notes above the
    /// range are not drawn.
    pub keys: KeyRange,
    /// v0.9 (V09-S09): keys folded onto fewer texel rows (clip mini notes);
    /// `None`: one row per key of `keys`.
    pub fold: Option<KeyFold>,
    /// v0.9 (V09-S09): only the notes starting in ticks `[a, b)` — a clip's
    /// window, whose notes are the ones it plays; `None`: every note.
    pub window: Option<(u64, u64)>,
}

/// Where one source's notes are drawn (v0.9 V09-S11 step 4, the Onion
/// through clip placements — `docs/CLIP_PIANO_ROLL.md`): content tick `c`
/// at tile tick `c + shift`, and only the notes starting in content ticks
/// `window` (the notes the placement plays). [`SourcePlace::IDENTITY`] is a
/// track's own content where it is (every source before clips).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SourcePlace {
    /// Tile (song) tick minus content tick: a placement's `start − offset`.
    pub shift: i128,
    /// Content ticks `[a, b)` whose note starts are drawn.
    pub window: (u64, u64),
}

/// `t` moved by `shift`, clamped to the tick range.
pub fn shifted(t: u64, shift: i128) -> u64 {
    (i128::from(t) + shift).clamp(0, i128::from(u64::MAX)) as u64
}

impl SourcePlace {
    pub const IDENTITY: SourcePlace = SourcePlace {
        shift: 0,
        window: (0, u64::MAX),
    };

    /// Where placement `p` shows its content.
    pub fn of(p: &ClipPlacement) -> SourcePlace {
        SourcePlace {
            shift: i128::from(p.start) - i128::from(p.offset),
            window: p.content_window(),
        }
    }

    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }

    /// Content ticks `[a, b)` whose notes can sound in tile ticks
    /// `[t0, t1)` (`None`: none can). Not cut to the window: a note started
    /// in it may sound long after its end; [`SourcePlace::start`] filters.
    pub fn content_range(&self, t0: u64, t1: u64) -> Option<(u64, u64)> {
        let a = (i128::from(t0) - self.shift).max(0);
        let b = (i128::from(t1) - self.shift).min(i128::from(u64::MAX));
        (b > a).then_some((a as u64, b as u64))
    }

    /// Exact number of `store`'s notes this place draws that start in tile
    /// (song) ticks `[t0, t1]` (the Onion's density ranking through clips;
    /// no note is visited).
    pub fn count_starting_in(&self, store: &NoteStore, t0: u64, t1: u64) -> usize {
        let (w0, w1) = self.window;
        let a = (i128::from(t0) - self.shift).max(i128::from(w0));
        let b = (i128::from(t1) - self.shift)
            .min(i128::from(w1) - 1)
            .min(i128::from(u64::MAX));
        if b < a {
            return 0;
        }
        store.count_starting_in(a as u64, b as u64)
    }

    /// The tile tick of a note starting at content tick `c` (`None`: the
    /// placement does not play it, or it lands before tick 0).
    #[inline(always)]
    pub fn start(&self, c: u64) -> Option<u64> {
        if c < self.window.0 || c >= self.window.1 {
            return None;
        }
        u64::try_from(i128::from(c) + self.shift).ok()
    }
}

/// How one source's notes are read (v0.9 V09-S17, `docs/PLAYER.md` P1 /
/// P3). The editor's views draw what is stored; the MIDI Player draws what
/// the exported file holds — without a copy of the notes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Reading {
    /// Every stored note, muted ones and the selection included.
    #[default]
    Stored,
    /// The notes as the track plays and exports them: muted notes left out,
    /// none drawn as selected, and the track's correction (if any) applied
    /// after the source's placement — tick shift from its start, key shift,
    /// gate, key range — exactly as `midi::export::export_notes_as_stored`
    /// composes them ([`CorrectedRuns`]), in the same order. The notes are
    /// read in place from the store's range mapped back through the
    /// correction.
    Played(Option<Correction>),
}

/// A record as a drawn note, never selected (an export note).
#[inline(always)]
fn played_view(r: NoteRecord) -> NoteView {
    NoteView {
        start_tick: r.start_tick,
        duration_ticks: r.duration_ticks,
        attrs: r.attrs,
        sub: r.sub,
        group: r.group,
        selected: false,
    }
}

/// Keys `lo ..= hi` folded onto `rows` texel rows, row 0 = the highest band
/// (v0.9 V09-S09, the Track List's clip mini notes, `docs/TRACK_LIST.md`). A
/// row covers a band of adjacent keys; a texel's winner is the note drawn
/// last over its band and column, its count the notes covering them, and
/// outlines follow the winners — the dense raster's rules, unchanged. Keys
/// outside `lo ..= hi` are not drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyFold {
    lo: u8,
    hi: u8,
    rows: u16,
}

impl KeyFold {
    /// `lo` and `hi` in either order; `rows` is clamped to 1 ..= the number
    /// of keys (a key never spans several rows: the texture is stretched).
    pub fn new(lo: u8, hi: u8, rows: usize) -> KeyFold {
        let (lo, hi) = (lo.min(hi), lo.max(hi));
        let keys = usize::from(hi - lo) + 1;
        KeyFold {
            lo,
            hi,
            rows: rows.clamp(1, keys) as u16,
        }
    }

    pub fn lo(&self) -> u8 {
        self.lo
    }

    pub fn hi(&self) -> u8 {
        self.hi
    }

    /// Texel rows.
    pub fn rows(&self) -> usize {
        usize::from(self.rows)
    }

    /// Keys folded.
    pub fn keys(&self) -> usize {
        usize::from(self.hi - self.lo) + 1
    }

    /// The row of `pitch` (0 = the band holding `hi`); `None` outside.
    pub fn row_of(&self, pitch: u8) -> Option<usize> {
        (self.lo..=self.hi)
            .contains(&pitch)
            .then(|| usize::from(self.hi - pitch) * self.rows() / self.keys())
    }
}

/// Opacity (0–255) of `n` stacked layers of opacity `a` (0–255).
pub fn stacked_alpha(a: u8, n: u32) -> u8 {
    let a = f64::from(a) / 255.0;
    let n = n.clamp(1, 4096) as i32;
    (255.0 * (1.0 - (1.0 - a).powi(n))).round() as u8
}

/// A finished tile.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TileOut {
    /// `TILE_COLS × rows` unmultiplied RGBA, row 0 = the highest pitch.
    pub pixels: Vec<[u8; 4]>,
    /// Pitch rows (128 or 256).
    pub rows: usize,
    /// Notes drawn into the tile.
    pub notes: usize,
    /// Most notes overlapping one texel.
    pub max_count: u32,
    /// Texels with at least one note.
    pub occupied: usize,
    /// Outlined texels.
    pub edges: usize,
    /// B2-D02 ([`Outline::borders`]): `TILE_COLS × rows` unmultiplied RGBA,
    /// each texel's border colour — the winner's [`TileColors::border`], else
    /// [`outline_shade`] of its fill; the selection colour for a selected
    /// winner when [`Outline::selection`] is set — at least the outline's
    /// opacity; transparent where empty or where the winner's run along the
    /// row is shorter than [`MIN_RUN`] (the exact renderer borders no note
    /// narrower than 4 px). Empty without `borders`.
    pub borders: Vec<[u8; 4]>,
}

impl TileOut {
    /// FNV-1a over the pixels (idle-frame stability tests, diagnostics).
    pub fn hash(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for p in self.pixels.iter().chain(&self.borders) {
            for b in p {
                h ^= u64::from(*b);
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        h
    }
}

/// Reusable working memory of tile builds (one per worker). After a completed
/// build it still holds that tile's winners ([`Scratch::winner`]).
#[derive(Default)]
pub struct Scratch {
    /// Note counts per (row, internal column): difference arrays, then
    /// prefix sums (`ROWS × STRIDE`).
    diff: Vec<i32>,
    /// Winner keys per row: a bottom-up segment tree of `2 × W` nodes over
    /// the row's internal columns (range max on insert, pushed down to the
    /// leaves at the end; `ROWS × 2W`).
    tree: Vec<Key>,
    /// Rows written since the last clear.
    dirty: Dirty,
    /// Rows the buffers are sized for (128 or 256).
    rows: usize,
    /// Sources in painter order (rank, then index).
    order: Vec<usize>,
    texel: Vec<[u8; 4]>,
    /// Edge flags per internal texel (`ROWS × W`).
    edge: Vec<bool>,
    /// How the last build packed its winner keys ([`Layout`]).
    layout: Layout,
    /// [`Layout::Start`]: notes of the current source seen per pitch row at
    /// its current start tick, and the rows to reset when the start moves.
    ties: Vec<u8>,
    tied: Vec<usize>,
}

/// How winner keys order notes (v1.0-beta1 B1-0141).
/// * `Painter` (the editor's tiles): `[selected 1][order 33][velocity 7]
///   [source 23][rgba 32][subchannel 32]`, `order` = the painter sequence.
/// * `Start` (the MIDI Player, PFA painter's order — `docs/references/
///   v1_0_beta1/PFAVIZ_PLAYER.md` §2): `[selected 1][start 33][place 23]
///   [tie 7][rgba 32][subchannel 32]` — a later start covers an earlier one
///   whatever the source; at one start the source later in painter order
///   (its place: rank, then index), then the later stored note of that
///   source on that pitch
///   (`tie`, saturating at 127). Sources are filled one after the other, so
///   nothing is merged or gathered. Velocity is not kept (the Player's
///   colours never depend on it: [`Winner::velocity`] reads 127); a start
///   beyond 2^33 − 1 ticks is clamped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Layout {
    #[default]
    Painter,
    Start,
}

const START_SHIFT: u32 = 127 - ORDER_BITS;
const START_SOURCE_SHIFT: u32 = START_SHIFT - SOURCE_BITS;
const TIE_SHIFT: u32 = 64;

fn pack_start(start: u64, source: usize, tie: u8, rgba: [u8; 4], sub: u32) -> Key {
    let start = start.min((1 << ORDER_BITS) - 1);
    let source = (source as u64).min((1 << SOURCE_BITS) - 1);
    (u128::from(start) << START_SHIFT)
        | (u128::from(source) << START_SOURCE_SHIFT)
        | (u128::from(tie & 0x7F) << TIE_SHIFT)
        | (u128::from(u32::from_le_bytes(rgba)) << 32)
        | u128::from(sub)
}

/// Apron columns on each side of a tile: enough to see whether a run that
/// reaches the tile's first or last column is [`MIN_RUN`] long, so outlines
/// continue across tile seams exactly.
const APRON: usize = MIN_RUN - 1;
/// Internal columns: the tile's plus the aprons.
const W: usize = TILE_COLS + 2 * APRON;
const STRIDE: usize = W + 1;

/// A texel's winner (v0.9 §028, §178): of the notes covering the texel, the
/// one the exact renderer draws last — selected notes above all others, then
/// painter order (sources by [`TileColors::rank`], then by index; a source's
/// notes in stored order). The note's own colour and identity are packed into
/// the key, so the winner can never take another class's identity and no
/// colour table (or bound on the number of colours) exists:
/// `[selected 1][order 33][velocity 7][source 23][rgba 32][subchannel 32]`,
/// 0 = empty. (2^33 notes per tile, 2^23 sources: far above any project.)
type Key = u128;
const ORDER_BITS: u32 = 33;
const ORDER_SHIFT: u32 = 127 - ORDER_BITS;
const VEL_SHIFT: u32 = ORDER_SHIFT - 7;
const SOURCE_BITS: u32 = 23;

fn pack(selected: bool, order: u64, velocity: u8, source: usize, rgba: [u8; 4], sub: u32) -> Key {
    let order = order.min((1 << ORDER_BITS) - 1);
    let source = (source as u64).min((1 << SOURCE_BITS) - 1);
    (u128::from(selected) << 127)
        | (u128::from(order) << ORDER_SHIFT)
        | (u128::from(velocity & 0x7F) << VEL_SHIFT)
        | (u128::from(source) << 64)
        | (u128::from(u32::from_le_bytes(rgba)) << 32)
        | u128::from(sub)
}

/// The note that owns a texel, with its true identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Winner {
    /// Drawn as selected (priority colour).
    pub selected: bool,
    /// Index into the stores passed to [`build_tile`].
    pub source: usize,
    /// The note's own colour from [`TileColors::color`].
    pub rgba: [u8; 4],
    /// The note's subchannel.
    pub sub: u32,
    /// The note's velocity.
    pub velocity: u8,
}

/// The structure a winner belongs to: (selected, source, subchannel).
fn structure(k: Key, layout: Layout) -> u128 {
    let source_shift = match layout {
        Layout::Painter => 64,
        Layout::Start => START_SOURCE_SHIFT,
    };
    k & ((1u128 << 127) | (((1u128 << SOURCE_BITS) - 1) << source_shift) | 0xFFFF_FFFF)
}

/// The note that won: its painter order, or (start, source, tie) — unique
/// within a build.
fn note_of(k: Key, layout: Layout) -> u64 {
    match layout {
        Layout::Painter => ((k >> ORDER_SHIFT) as u64) & ((1 << ORDER_BITS) - 1),
        Layout::Start => ((k >> TIE_SHIFT) as u64) & ((1 << 63) - 1),
    }
}

/// `order`: the build's sources in painter order (a [`Layout::Start`] key
/// holds the source's place in it).
fn unpack(k: Key, layout: Layout, order: &[usize]) -> Option<Winner> {
    (k != 0).then(|| match layout {
        Layout::Painter => Winner {
            selected: k >> 127 == 1,
            source: ((k >> 64) as u64 & ((1 << SOURCE_BITS) - 1)) as usize,
            rgba: ((k >> 32) as u32).to_le_bytes(),
            sub: k as u32,
            velocity: ((k >> VEL_SHIFT) as u8) & 0x7F,
        },
        Layout::Start => Winner {
            selected: false,
            source: {
                let pos = ((k >> START_SOURCE_SHIFT) as u64 & ((1 << SOURCE_BITS) - 1)) as usize;
                order.get(pos).copied().unwrap_or(pos)
            },
            rgba: ((k >> 32) as u32).to_le_bytes(),
            sub: k as u32,
            velocity: 127,
        },
    })
}

/// Internal columns `[x0, x1]` covered by a note `[start, start + dur)` in a
/// tile starting at tick `t0` (`ppt` pixels per tick; the first and last
/// [`APRON`] columns are the aprons); at least one column; `None` outside.
fn columns(start: u64, dur: u64, t0: f64, ppt: f64) -> Option<(usize, usize)> {
    let s = start as f64;
    let e = start.saturating_add(dur.max(1)) as f64;
    let a = APRON as f64;
    let x0 = ((s - t0) * ppt).floor() + a;
    let x1 = (((e - t0) * ppt).ceil() - 1.0).max(x0 - a) + a;
    if x1 < 0.0 || x0 > (W - 1) as f64 {
        return None;
    }
    let x0 = x0.max(0.0) as usize;
    Some((x0, (x1.min((W - 1) as f64) as usize).max(x0)))
}

/// Range max of `v` over leaves `[l, r]` of one row's tree.
fn chmax(tree: &mut [Key], l: usize, r: usize, v: Key) {
    let (mut l, mut r) = (l + W, r + 1 + W);
    while l < r {
        if l & 1 == 1 {
            tree[l] = tree[l].max(v);
            l += 1;
        }
        if r & 1 == 1 {
            r -= 1;
            tree[r] = tree[r].max(v);
        }
        l >>= 1;
        r >>= 1;
    }
}

/// A set of pitch rows (up to [`MAX_ROWS`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Dirty([u128; 2]);

impl Dirty {
    fn set(&mut self, row: usize) {
        self.0[row >> 7] |= 1 << (row & 127);
    }

    /// The rows, ascending.
    fn rows(self) -> impl Iterator<Item = usize> {
        (0..2).flat_map(move |h| {
            let mut d = self.0[h];
            std::iter::from_fn(move || {
                (d != 0).then(|| {
                    let r = d.trailing_zeros() as usize;
                    d &= d - 1;
                    (h << 7) + r
                })
            })
        })
    }
}

impl Scratch {
    /// Zeroes the rows the previous build wrote and sizes the buffers for
    /// `rows` pitch rows (reallocated only when the row count changes).
    fn clear(&mut self, rows: usize) {
        if self.rows != rows || self.tree.len() != rows * 2 * W {
            self.rows = rows;
            self.tree = vec![0; rows * 2 * W];
            self.diff = vec![0; rows * STRIDE];
            self.dirty = Dirty::default();
        }
        for r in self.dirty.rows() {
            self.tree[r * 2 * W..(r + 1) * 2 * W].fill(0);
            self.diff[r * STRIDE..(r + 1) * STRIDE].fill(0);
        }
        self.dirty = Dirty::default();
    }

    /// The winner of tile column `col` (0 … [`TILE_COLS`] − 1) and row `row`
    /// (0 = the highest pitch) of the last completed build; `None`: empty
    /// texel.
    pub fn winner(&self, row: usize, col: usize) -> Option<Winner> {
        if row >= self.rows || col >= TILE_COLS || self.tree.is_empty() {
            return None;
        }
        unpack(self.leaf(row, col + APRON), self.layout, &self.order)
    }

    /// Winner key of internal texel (`row`, `x`) after a completed build.
    fn leaf(&self, row: usize, x: usize) -> Key {
        self.tree[row * 2 * W + W + x]
    }
}

/// Where a note goes in a tile: its texel row, and whether it is drawn at
/// all (`build_tile` is specialised per map: [`Plain`] for the roll).
trait RowMap: Copy {
    fn row(&self, note: &crate::model::note_store::NoteView) -> Option<usize>;
}

/// One row per key, key 0 at the bottom (the roll's tiles).
#[derive(Clone, Copy)]
struct Plain(usize);

impl RowMap for Plain {
    #[inline(always)]
    fn row(&self, note: &crate::model::note_store::NoteView) -> Option<usize> {
        let p = usize::from(note.attrs.pitch());
        (p < self.0).then(|| self.0 - 1 - p)
    }
}

/// Keys folded into bands, notes starting in a window only (clip mini notes).
#[derive(Clone, Copy)]
struct Folded {
    fold: KeyFold,
    window: (u64, u64),
}

impl RowMap for Folded {
    #[inline(always)]
    fn row(&self, note: &crate::model::note_store::NoteView) -> Option<usize> {
        let (a, b) = self.window;
        if note.start_tick < a || note.start_tick >= b {
            return None;
        }
        self.fold.row_of(note.attrs.pitch())
    }
}

/// Where a source's notes land in the tile ([`fill`] is specialised per
/// kind, so unplaced sources run the loop they ran before clips).
trait Placing: Copy {
    /// Content ticks to query for tile ticks `[q0, q1)` (`None`: none).
    fn range(&self, q0: u64, q1: u64) -> Option<(u64, u64)>;
    /// The tile tick of a note starting at content tick `c`.
    fn start(&self, c: u64) -> Option<u64>;
}

/// Sources drawn where they are (the identity).
#[derive(Clone, Copy)]
struct Unplaced;

impl Placing for Unplaced {
    #[inline(always)]
    fn range(&self, q0: u64, q1: u64) -> Option<(u64, u64)> {
        Some((q0, q1))
    }
    #[inline(always)]
    fn start(&self, c: u64) -> Option<u64> {
        Some(c)
    }
}

impl Placing for SourcePlace {
    #[inline(always)]
    fn range(&self, q0: u64, q1: u64) -> Option<(u64, u64)> {
        self.content_range(q0, q1)
    }
    #[inline(always)]
    fn start(&self, c: u64) -> Option<u64> {
        SourcePlace::start(self, c)
    }
}

/// FILL of [`build_tile`]: every note of the query range into the winner
/// trees and counts of its row. Returns whether the build was cancelled.
/// Families whose sources are all where they are (every roll and clip
/// family, and the Onion before clips) take the unplaced specialisation.
#[allow(clippy::too_many_arguments)]
fn fill<M: RowMap>(
    stores: &[&NoteStore],
    places: &[SourcePlace],
    colors: &dyn TileColors,
    order: &[usize],
    q: (u64, u64, f64, f64),
    map: M,
    scratch: &mut Scratch,
    out: &mut TileOut,
    cancelled: &dyn Fn() -> bool,
) -> bool {
    if places.iter().all(SourcePlace::is_identity) {
        fill_placed(
            stores,
            |_| Unplaced,
            colors,
            order,
            q,
            map,
            scratch,
            out,
            cancelled,
        )
    } else {
        let place = |si: usize| places.get(si).copied().unwrap_or(SourcePlace::IDENTITY);
        fill_placed(
            stores, place, colors, order, q, map, scratch, out, cancelled,
        )
    }
}

/// [`fill`] with source `si` drawn where `place(si)` puts it.
#[allow(clippy::too_many_arguments)]
fn fill_placed<M: RowMap, P: Placing>(
    stores: &[&NoteStore],
    place: impl Fn(usize) -> P,
    colors: &dyn TileColors,
    order: &[usize],
    (q0, q1, t0, ppt): (u64, u64, f64, f64),
    map: M,
    scratch: &mut Scratch,
    out: &mut TileOut,
    cancelled: &dyn Fn() -> bool,
) -> bool {
    // Painter order across the whole tile: later = in front (0 = empty).
    let mut seq = 0u64;
    let mut stop = false;
    for &si in order {
        if stop {
            break;
        }
        let pl = place(si);
        let Some((a, b)) = pl.range(q0, q1) else {
            continue;
        };
        stores[si].query(a, b, |_, v| {
            if stop {
                return;
            }
            seq += 1;
            if seq.is_multiple_of(65_536) && cancelled() {
                stop = true;
                return;
            }
            let Some(start) = pl.start(v.start_tick) else {
                return;
            };
            put(si, seq, start, &v, colors, (t0, ppt), map, scratch, out);
        });
    }
    stop
}

/// One note into the tile: note `v` of source `si`, painter order `seq`,
/// drawn from tile tick `start` (`v`'s own start may be a content tick: the
/// row map reads it).
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn put<M: RowMap>(
    si: usize,
    seq: u64,
    start: u64,
    v: &NoteView,
    colors: &dyn TileColors,
    (t0, ppt): (f64, f64),
    map: M,
    scratch: &mut Scratch,
    out: &mut TileOut,
) {
    let Some((x0, x1)) = columns(start, v.duration_ticks, t0, ppt) else {
        return;
    };
    let Some(row) = map.row(v) else {
        return;
    };
    let (c, prio) = colors.color(si, v);
    scratch.dirty.set(row);
    let tree = &mut scratch.tree[row * 2 * W..(row + 1) * 2 * W];
    chmax(
        tree,
        x0,
        x1,
        pack(prio, seq, v.attrs.velocity(), si, c, v.sub.0),
    );
    let base = row * STRIDE;
    scratch.diff[base + x0] += 1;
    scratch.diff[base + x1 + 1] -= 1;
    out.notes += 1;
}

/// FILL for families with a [`Reading`] other than [`Reading::Stored`] (the
/// MIDI Player): each source as it is read, keys in [`Layout::Start`] — the
/// PFA painter's order (v1.0-beta1 B1-0141) — with no merge across sources.
/// Only these builds take this path, so the editor's tiles run the loop they
/// ran before. A [`Reading::Stored`] source here is drawn as stored but
/// never as selected (the Player shows no selection).
#[allow(clippy::too_many_arguments)]
fn fill_read<M: RowMap>(
    stores: &[&NoteStore],
    places: &[SourcePlace],
    readings: &[Reading],
    colors: &dyn TileColors,
    order: &[usize],
    q: (u64, u64, f64, f64),
    map: M,
    scratch: &mut Scratch,
    out: &mut TileOut,
    cancelled: &dyn Fn() -> bool,
) -> bool {
    let (q0, q1, t0, ppt) = q;
    scratch.ties.clear();
    scratch.ties.resize(MAX_ROWS, 0);
    scratch.tied.clear();
    let mut seen = 0u64;
    for (pos, &si) in order.iter().enumerate() {
        let pl = places.get(si).copied().unwrap_or(SourcePlace::IDENTITY);
        let mut f = StartFill {
            si,
            pos,
            colors,
            geo: (t0, ppt),
            map,
            cur: u64::MAX,
            seen: &mut seen,
            stop: false,
            cancelled,
        };
        match readings.get(si).copied().unwrap_or_default() {
            Reading::Played(Some(c)) => {
                for n in corrected_records(stores[si], pl, c, q0, q1) {
                    if n.start_tick > q1 || n.start_tick.saturating_add(n.duration_ticks) < q0 {
                        continue;
                    }
                    let v = played_view(n);
                    f.put(v.start_tick, &v, scratch, out);
                    if f.stop {
                        break;
                    }
                }
            }
            read => {
                let stored = read == Reading::Stored;
                if let Some((a, b)) = pl.content_range(q0, q1) {
                    stores[si].query(a, b, |_, v| {
                        if f.stop || (!stored && v.attrs.muted()) {
                            return;
                        }
                        let Some(start) = pl.start(v.start_tick) else {
                            return;
                        };
                        // As it plays: never selected.
                        let v = NoteView {
                            selected: v.selected && stored,
                            ..v
                        };
                        f.put(start, &v, scratch, out);
                    });
                }
            }
        }
        if f.stop {
            return true;
        }
        for r in scratch.tied.drain(..) {
            scratch.ties[r] = 0;
        }
    }
    false
}

/// The note a key of the MIDI Player's keyboard lights at `tick`
/// (v1.0-beta1 B1-0111, PFA — `docs/references/v1_0_beta1/PFAVIZ_PLAYER.md`
/// §4): of the notes sounding there (start ≤ `tick` < end), the latest
/// starting, ties broken as the rainfall's [`Layout::Start`] (the later
/// source, then the later stored note), so a key always shows the colour of
/// the note falling onto it. Upcoming notes never light a key. `out[k]` =
/// (source index, note as played, in song ticks) for keys `0..keys`;
/// `None` = not sounding.
/// Sources are read as the tiles read them (readings, placements,
/// corrections; muted notes never sound).
///
/// Cost (v1.0-beta1 B1-0145): sources are read from the last (the top of
/// the painter's order) to the first. A plain source is read latest start
/// first and stops once every key of `masks[si]` (the pitches the source
/// can hold: [`NoteStore::pitch_mask`], a superset is fine; missing = all)
/// is decided — lit by this source, or by a later source's note starting at
/// or after the notes still to read. So a dense passage costs a few notes
/// per key instead of every sounding note; a key the source holds but does
/// not sound reads back to the store's chunk skipping (O(sounding notes)).
/// Corrected sources are read in full (O(their sounding notes)).
pub fn lit_keys(
    stores: &[&NoteStore],
    places: &[SourcePlace],
    readings: &[Reading],
    masks: &[[u64; 4]],
    tick: u64,
    keys: usize,
    out: &mut Vec<Option<(usize, NoteView)>>,
) {
    foremost_keys(stores, places, readings, &[], masks, tick, keys, out);
}

/// [`lit_keys`] under layers (v1.0-beta1 B1-0110, the Piano Roll keyboard):
/// a key shows the sounding note of the highest `ranks[si]` (missing: 0);
/// within one rank the latest start, then the later source, then the later
/// stored note — so with every rank equal this is exactly [`lit_keys`]. A
/// key a higher layer lit is final: lower layers stop as soon as every key
/// of their mask is decided.
#[allow(clippy::too_many_arguments)]
pub fn foremost_keys(
    stores: &[&NoteStore],
    places: &[SourcePlace],
    readings: &[Reading],
    ranks: &[u32],
    masks: &[[u64; 4]],
    tick: u64,
    keys: usize,
    out: &mut Vec<Option<(usize, NoteView)>>,
) {
    out.clear();
    out.resize(keys, None);
    let rank = |si: usize| ranks.get(si).copied().unwrap_or(0);
    // Each key's current note: (rank, start, source, stored order).
    let mut best: Vec<(u32, u64, usize, u64)> = vec![(0, 0, 0, 0); keys];
    let mut decided_by: Vec<(u64, usize)> = Vec::new();
    // Highest layer first; within a layer the later source first.
    let mut order: Vec<usize> = (0..stores.len()).collect();
    order.sort_by_key(|&a| std::cmp::Reverse((rank(a), a)));
    for si in order {
        let store = stores[si];
        let r = rank(si);
        let pl = places.get(si).copied().unwrap_or(SourcePlace::IDENTITY);
        let lit = |k: usize, start: u64, v: NoteView, out: &mut Vec<Option<(usize, NoteView)>>| {
            // In song ticks (a placed note's own start is a content tick).
            out[k] = Some((
                si,
                NoteView {
                    start_tick: start,
                    selected: false,
                    ..v
                },
            ));
        };
        let read = readings.get(si).copied().unwrap_or_default();
        if let Reading::Played(Some(c)) = read {
            for (idx, n) in corrected_records(store, pl, c, tick, tick).enumerate() {
                let v = played_view(n);
                let (k, start) = (usize::from(v.attrs.pitch()), v.start_tick);
                if start > tick || start.saturating_add(v.duration_ticks) <= tick || k >= keys {
                    continue;
                }
                let key = (r, start, si, idx as u64);
                if out[k].is_none() || key > best[k] {
                    best[k] = key;
                    lit(k, start, v, out);
                }
            }
            continue;
        }
        let stored = read == Reading::Stored;
        let Some((a, b)) = pl.content_range(tick, tick.saturating_add(1)) else {
            continue;
        };
        // Keys still open for this source: in its mask and not lit by a
        // higher layer, nor by its own layer at or after the notes still to
        // read (sources read earlier only lose ties).
        let mask = masks.get(si).copied().unwrap_or([u64::MAX; 4]);
        let held = |k: usize| k < keys && mask[k / 64] >> (k % 64) & 1 == 1;
        let mut done = [0u64; 4];
        let mut open = 0usize;
        decided_by.clear();
        for k in (0..keys).filter(|&k| held(k)) {
            if out[k].is_some() && best[k].0 > r {
                done[k / 64] |= 1 << (k % 64);
            } else {
                open += 1;
                if out[k].is_some() {
                    decided_by.push((best[k].1, k));
                }
            }
        }
        decided_by.sort_unstable_by(|x, y| y.cmp(x));
        let mut next = 0;
        if open == 0 {
            continue;
        }
        store.query_rev(a, b.saturating_sub(1).max(a), |loc, v| {
            let Some(start) = pl.start(v.start_tick) else {
                return true;
            };
            // Keys a source of this layer lit at or after this start are
            // decided.
            while next < decided_by.len() && decided_by[next].0 >= start {
                let k = decided_by[next].1;
                if done[k / 64] >> (k % 64) & 1 == 0 {
                    done[k / 64] |= 1 << (k % 64);
                    open -= 1;
                }
                next += 1;
            }
            if open == 0 {
                return false;
            }
            let k = usize::from(v.attrs.pitch());
            if !held(k)
                || done[k / 64] >> (k % 64) & 1 == 1
                || (!stored && v.attrs.muted())
                || start > tick
                || start.saturating_add(v.duration_ticks) <= tick
            {
                return true;
            }
            // The first sounding note met is this source's latest; it beats
            // every note of its layer met before (they all start earlier)
            // and every lower layer.
            best[k] = (
                r,
                start,
                si,
                (u64::from(loc.chunk) << 32) | u64::from(loc.index),
            );
            lit(k, start, v, out);
            done[k / 64] |= 1 << (k % 64);
            open -= 1;
            open > 0
        });
    }
}

/// The key mask [`lit_keys`] reads source `store` with: the store's
/// pitches ([`NoteStore::pitch_mask`]), or every key for a corrected
/// reading (read in full). O(notes): taken with the Player's snapshot.
pub fn lit_mask(store: &NoteStore, reading: Reading) -> [u64; 4] {
    match reading {
        Reading::Played(Some(_)) => [u64::MAX; 4],
        _ => store.pitch_mask(),
    }
}

/// One source's notes into a [`Layout::Start`] tile (see [`fill_read`]).
struct StartFill<'a, M> {
    si: usize,
    /// The source's place in painter order (what the key holds).
    pos: usize,
    colors: &'a dyn TileColors,
    geo: (f64, f64),
    map: M,
    /// The start tick the ties count.
    cur: u64,
    seen: &'a mut u64,
    stop: bool,
    cancelled: &'a dyn Fn() -> bool,
}

impl<M: RowMap> StartFill<'_, M> {
    /// Note `v` drawn from tile tick `start` (notes come in start order).
    #[inline(always)]
    fn put(&mut self, start: u64, v: &NoteView, scratch: &mut Scratch, out: &mut TileOut) {
        *self.seen += 1;
        if self.seen.is_multiple_of(65_536) && (self.cancelled)() {
            self.stop = true;
            return;
        }
        let Some(row) = self.map.row(v) else {
            return;
        };
        if start != self.cur {
            self.cur = start;
            for r in scratch.tied.drain(..) {
                scratch.ties[r] = 0;
            }
        }
        let tie = scratch.ties[row];
        if tie == 0 {
            scratch.tied.push(row);
        }
        scratch.ties[row] = tie.saturating_add(1).min(127);
        let (t0, ppt) = self.geo;
        // PFA's sub-pixel cull (PFAVIZ_PLAYER.md §2): a note whose rounded
        // length is no whole texel covers nothing and is not drawn.
        if (v.duration_ticks as f64 * ppt + 0.5).floor() < 1.0 {
            return;
        }
        let Some((x0, x1)) = columns(start, v.duration_ticks, t0, ppt) else {
            return;
        };
        let (c, _) = self.colors.color(self.si, v);
        scratch.dirty.set(row);
        let tree = &mut scratch.tree[row * 2 * W..(row + 1) * 2 * W];
        chmax(tree, x0, x1, pack_start(start, self.pos, tie, c, v.sub.0));
        let base = row * STRIDE;
        scratch.diff[base + x0] += 1;
        scratch.diff[base + x1 + 1] -= 1;
        out.notes += 1;
    }
}

/// The notes of one source with the track's correction `c` applied after
/// its placement `pl` (`docs/PLAYER.md` P3). The source's notes in song
/// ticks are two sorted runs — starting before `c.from_tick` (unchanged but
/// for the key range) and from it on (corrected) — merged by
/// [`CorrectedRuns`], the export's own composition, so the notes, their
/// order and the muted / overflow / range rules are the export's. Each run
/// reads only the store's range that can reach the tile once corrected:
/// the tile mapped back through the placement and the tick shift, looking
/// back by the gate's growth of the longest note, and every note the shift
/// clamps to tick 0 while the tile is within reach of tick 0. A note is
/// drawn when its corrected extent meets the tile's query range, the rule
/// [`NoteStore::query`] applies to stored notes ([`corrected_run`] filters
/// by it).
fn corrected_records(
    store: &NoteStore,
    pl: SourcePlace,
    c: Correction,
    q0: u64,
    q1: u64,
) -> impl Iterator<Item = NoteRecord> + '_ {
    const MAX: i128 = u64::MAX as i128;
    let (w0, w1) = pl.window;
    let shift = pl.shift;
    let ticks = i128::from(c.ticks);
    // Content tick of song tick `t`, clamped to the tick range.
    let content = |t: i128| (t - shift).clamp(0, MAX) as u64;
    let split = content(i128::from(c.from_tick)).clamp(w0, w1.max(w0));
    // A length's upper bound once gated (the gate never shortens below what
    // the store's look-back already covers when it is 100 % or less).
    let gate = c.gate;
    let grows = gate.hundredths() > 10_000;
    let len = move |d: u64| {
        if grows {
            gate.apply(d).unwrap_or(u64::MAX)
        } else {
            d
        }
    };
    // Run a: starts before the correction start, drawn where they are.
    let a = {
        let t1 = i128::from(q1) - shift;
        let (t0a, t1a) = (content(i128::from(q0)), t1.clamp(0, MAX) as u64);
        let s1 = if t1 < 0 { w0 } else { split };
        store.overlapping(t0a, t1a, w0, s1, |d| d)
    };
    // Run b: starts from the correction start on. A note starting at content
    // tick `s` lands at song tick `s + shift + ticks` (clamped to 0, the
    // prefix of the run below `zero`).
    let t1b = i128::from(q1) - shift - ticks;
    let (b_lo, b_hi) = if t1b < 0 { (w1, w1) } else { (split, w1) };
    let t1b = t1b.clamp(0, MAX) as u64;
    let zero = content(-ticks).clamp(b_lo, b_hi.max(b_lo));
    // Clamped notes all start at song tick 0: they reach the tile only by
    // their (gated) length, so every one is read while the longest gated
    // length reaches it; otherwise none.
    let reach_zero = ticks < 0 && zero > b_lo && {
        let max = store.max_duration();
        len(max) >= q0
    };
    let b0 = store.overlapping(0, t1b, b_lo, if reach_zero { zero } else { b_lo }, len);
    let t0b = content(i128::from(q0) - ticks);
    let b1 = store.overlapping(t0b, t1b, zero, b_hi, len);
    let to_song = move |r: NoteRecord| NoteRecord {
        start_tick: shifted(r.start_tick, shift),
        ..r
    };
    CorrectedRuns::from_runs(a.map(to_song), b0.chain(b1).map(to_song), c)
}

/// Builds tile `index` of `level` from `stores` (drawn together; colours from
/// `colors`) in three passes (v0.9 §175 / §177): FILL — every texel shows its
/// [`Winner`]'s colour at the opacity of the notes covering it
/// ([`stacked_alpha`]);
/// OUTLINE — edges from the winners' identities ([`Outline`]); SELECTION —
/// edges of selected notes in the selection colour. (The playhead is drawn
/// by the Piano Roll above the tiles.) Memory is fixed per tile (no state per
/// colour, source or note); a note costs O(log columns). `cancelled` is
/// polled every 65,536 notes: a build that is no longer wanted stops and
/// returns `None`.
pub fn build_tile(
    stores: &[&NoteStore],
    colors: &dyn TileColors,
    level: i32,
    index: i64,
    opt: TileOptions,
    scratch: &mut Scratch,
    cancelled: &dyn Fn() -> bool,
) -> Option<TileOut> {
    build_tile_placed(stores, &[], colors, level, index, opt, scratch, cancelled)
}

/// [`build_tile`] with source `i` drawn where `places[i]` puts it (v0.9
/// V09-S11: the Onion through clip placements); sources without an entry
/// (an empty `places`) are drawn where they are.
#[allow(clippy::too_many_arguments)]
pub fn build_tile_placed(
    stores: &[&NoteStore],
    places: &[SourcePlace],
    colors: &dyn TileColors,
    level: i32,
    index: i64,
    opt: TileOptions,
    scratch: &mut Scratch,
    cancelled: &dyn Fn() -> bool,
) -> Option<TileOut> {
    build_tile_read(
        stores,
        places,
        &[],
        colors,
        level,
        index,
        opt,
        scratch,
        cancelled,
    )
}

/// [`build_tile_placed`] with source `i` read as `readings[i]` says (v0.9
/// V09-S17, the MIDI Player — `docs/PLAYER.md` P3: what the exported file
/// holds, corrections applied in the fill, no copy of the notes); sources
/// without an entry are [`Reading::Stored`]. A tile of a
/// [`Reading::Played`] source equals the tile of a store holding that
/// source's export notes (`tests/v09_player.rs`).
#[allow(clippy::too_many_arguments)]
pub fn build_tile_read(
    stores: &[&NoteStore],
    places: &[SourcePlace],
    readings: &[Reading],
    colors: &dyn TileColors,
    level: i32,
    index: i64,
    opt: TileOptions,
    scratch: &mut Scratch,
    cancelled: &dyn Fn() -> bool,
) -> Option<TileOut> {
    let ppt = level_ppt(level);
    let t0 = tile_start(level, index);
    let span = tile_span(level);
    let rows = opt.fold.map_or(opt.keys.keys(), |f| f.rows());
    scratch.clear(rows);
    let mut out = TileOut {
        pixels: vec![[0; 4]; TILE_COLS * rows],
        rows,
        ..Default::default()
    };
    if t0 + span < 0.0 {
        return Some(out);
    }
    // Aprons (internal column = tile column + APRON).
    let a0 = (t0 - APRON as f64 / ppt).max(0.0);
    let a1 = t0 + span + APRON as f64 / ppt;
    let q0 = a0.floor() as u64;
    let q1 = a1.ceil().min(u64::MAX as f64) as u64;
    scratch.order.clear();
    scratch.order.extend(0..stores.len());
    scratch.order.sort_by_key(|&si| (colors.rank(si), si));
    let order = std::mem::take(&mut scratch.order);
    // The fill loop is specialised (monomorphised) per row map, so the
    // roll's plain tiles pay nothing for the mini notes' fold and window, nor
    // for the Player's readings.
    let read = readings.iter().any(|r| *r != Reading::Stored);
    scratch.layout = if read { Layout::Start } else { Layout::Painter };
    let stop = match (opt.fold, opt.window) {
        (None, None) if read => fill_read(
            stores,
            places,
            readings,
            colors,
            &order,
            (q0, q1, t0, ppt),
            Plain(rows),
            scratch,
            &mut out,
            cancelled,
        ),
        (None, None) => fill(
            stores,
            places,
            colors,
            &order,
            (q0, q1, t0, ppt),
            Plain(rows),
            scratch,
            &mut out,
            cancelled,
        ),
        (fold, window) => {
            let map = Folded {
                fold: fold.unwrap_or(KeyFold::new(0, (rows - 1) as u8, rows)),
                window: window.unwrap_or((0, u64::MAX)),
            };
            if read {
                fill_read(
                    stores,
                    places,
                    readings,
                    colors,
                    &order,
                    (q0, q1, t0, ppt),
                    map,
                    scratch,
                    &mut out,
                    cancelled,
                )
            } else {
                fill(
                    stores,
                    places,
                    colors,
                    &order,
                    (q0, q1, t0, ppt),
                    map,
                    scratch,
                    &mut out,
                    cancelled,
                )
            }
        }
    };
    scratch.order = order;
    if stop {
        return None;
    }
    // Counts (prefix sums) and winners (push the range maxima down to the
    // leaves) of the written rows; compose their texels, apron included.
    scratch.texel.clear();
    scratch.texel.resize(W * rows, [0; 4]);
    let layout = scratch.layout;
    for r in scratch.dirty.rows() {
        let tree = &mut scratch.tree[r * 2 * W..(r + 1) * 2 * W];
        for i in 1..W {
            let p = tree[i];
            if p != 0 {
                tree[2 * i] = tree[2 * i].max(p);
                tree[2 * i + 1] = tree[2 * i + 1].max(p);
            }
        }
        let base = r * STRIDE;
        let mut acc = 0;
        for x in 0..W {
            acc += scratch.diff[base + x];
            scratch.diff[base + x] = acc;
            let Some(w) = unpack(tree[W + x], layout, &scratch.order) else {
                debug_assert_eq!(acc, 0);
                continue;
            };
            let total = acc.max(1) as u32;
            let alpha = stacked_alpha(w.rgba[3], total);
            scratch.texel[r * W + x] = [w.rgba[0], w.rgba[1], w.rgba[2], alpha];
            if (APRON..APRON + TILE_COLS).contains(&x) {
                out.occupied += 1;
                out.max_count = out.max_count.max(total);
            }
        }
    }
    if let Some(o) = opt.outline {
        edge_mask(scratch, o.vertical);
        // Border colours come from the fills, before the edges are shaded.
        if o.borders {
            out.borders = border_texels(scratch, o, colors);
        }
        outline_pass(scratch, o, colors);
        selection_pass(scratch, o);
    }
    for r in 0..rows {
        let src = &scratch.texel[r * W + APRON..r * W + APRON + TILE_COLS];
        out.pixels[r * TILE_COLS..(r + 1) * TILE_COLS].copy_from_slice(src);
        if opt.outline.is_some() {
            let e = &scratch.edge[r * W + APRON..r * W + APRON + TILE_COLS];
            out.edges += e.iter().filter(|&&b| b).count();
        }
    }
    Some(out)
}

/// OUTLINE, step 1: which internal texels are edges ([`Outline`]), from the
/// winners alone (never from colours or counts).
fn edge_mask(sc: &mut Scratch, vertical: bool) {
    sc.edge.clear();
    let rows = sc.rows;
    sc.edge.resize(rows * W, false);
    let (tree, edge, layout) = (&sc.tree, &mut sc.edge, sc.layout);
    let leaf = |r: usize, x: usize| tree[r * 2 * W + W + x];
    for r in sc.dirty.rows() {
        // Runs along the row: of one note (per-note boundaries), and of one
        // structure (its perimeter, whatever is next to it).
        for by_note in [true, false] {
            let id = |x: usize| match leaf(r, x) {
                0 => None,
                k if by_note => Some(u128::from(note_of(k, layout))),
                k => Some(structure(k, layout)),
            };
            let mut a = 0;
            while a < W {
                let ka = id(a);
                let mut b = a;
                while b + 1 < W && id(b + 1) == ka {
                    b += 1;
                }
                if ka.is_some() && b + 1 - a >= MIN_RUN {
                    edge[r * W + a] = true;
                    edge[r * W + b] = true;
                }
                a = b + 1;
            }
        }
    }
    if !vertical {
        return;
    }
    // Stacks of one structure down each column (all 128 pitches are in the
    // tile, so no apron is needed).
    let id = |r: usize, x: usize| match leaf(r, x) {
        0 => None,
        k => Some(structure(k, layout)),
    };
    for x in 0..W {
        let mut a = 0;
        while a < rows {
            let ka = id(a, x);
            let mut b = a;
            while b + 1 < rows && id(b + 1, x) == ka {
                b += 1;
            }
            if ka.is_some() && b + 1 - a >= MIN_STACK {
                edge[a * W + x] = true;
                edge[b * W + x] = true;
            }
            a = b + 1;
        }
    }
}

/// B2-D02: every tile texel's border colour ([`TileOut::borders`]): the
/// colour the exact renderer gives the winning note's border, transparent for
/// empty texels and for runs of one note along the row shorter than
/// [`MIN_RUN`] (the apron makes runs continuous across tile seams).
fn border_texels(sc: &Scratch, o: Outline, colors: &dyn TileColors) -> Vec<[u8; 4]> {
    let rows = sc.rows;
    let mut out = vec![[0u8; 4]; rows * TILE_COLS];
    let layout = sc.layout;
    for r in sc.dirty.rows() {
        let mut a = 0;
        while a < W {
            let k = sc.leaf(r, a);
            let mut b = a;
            while b + 1 < W
                && k != 0
                && note_of(sc.leaf(r, b + 1), layout) == note_of(k, layout)
                && sc.leaf(r, b + 1) != 0
            {
                b += 1;
            }
            if k != 0
                && b + 1 - a >= MIN_RUN
                && let Some(w) = unpack(k, layout, &sc.order)
            {
                let px = sc.texel[r * W + a];
                let rgb = match (w.selected, o.selection) {
                    (true, Some(sel)) => sel,
                    _ => colors
                        .border(&w)
                        .unwrap_or_else(|| outline_shade([px[0], px[1], px[2]], o.darkness)),
                };
                for x in a.max(APRON)..=b.min(APRON + TILE_COLS - 1) {
                    let alpha = sc.texel[r * W + x][3].max(o.alpha);
                    out[r * TILE_COLS + x - APRON] = [rgb[0], rgb[1], rgb[2], alpha];
                }
            }
            a = b + 1;
        }
    }
    out
}

/// OUTLINE, step 2: edges take their winner's border colour
/// ([`TileColors::border`], else the outline shade of its fill) and at least
/// the outline opacity.
fn outline_pass(sc: &mut Scratch, o: Outline, colors: &dyn TileColors) {
    for r in sc.dirty.rows() {
        for x in 0..W {
            let i = r * W + x;
            let px = sc.texel[i];
            if !sc.edge[i] || px[3] == 0 {
                continue;
            }
            let Some(w) = unpack(sc.leaf(r, x), sc.layout, &sc.order) else {
                continue;
            };
            let c = colors
                .border(&w)
                .unwrap_or_else(|| outline_shade([px[0], px[1], px[2]], o.darkness));
            sc.texel[i] = [c[0], c[1], c[2], px[3].max(o.alpha)];
        }
    }
}

/// SELECTION: edges of selected notes in the selection colour.
fn selection_pass(sc: &mut Scratch, o: Outline) {
    let Some(c) = o.selection else {
        return;
    };
    for r in sc.dirty.rows() {
        for x in 0..W {
            let i = r * W + x;
            if sc.edge[i] && sc.leaf(r, x) >> 127 == 1 {
                let a = sc.texel[i][3];
                sc.texel[i] = [c[0], c[1], c[2], a];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::note::{NoteAttrs, NoteOffKind, NoteRecord, SubchannelId};
    use crate::model::project::KeyRange;

    struct Subs;
    impl TileColors for Subs {
        fn color(&self, _s: usize, n: &NoteView) -> ([u8; 4], bool) {
            if n.selected {
                return ([255, 255, 255, 255], true);
            }
            ([(n.sub.0 * 60 % 256) as u8, 100, 200, 255], false)
        }
    }

    fn rec(start: u64, dur: u64, pitch: u8, sub: u32) -> NoteRecord {
        NoteRecord {
            start_tick: start,
            duration_ticks: dur,
            attrs: NoteAttrs::new(pitch, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
            sub: SubchannelId(sub),
            group: 0,
        }
    }

    fn store(mut v: Vec<NoteRecord>) -> NoteStore {
        v.sort_by_key(|r| r.start_tick);
        NoteStore::from_sorted_records(&v, false).unwrap()
    }

    fn px(t: &TileOut, col: usize, pitch: u8) -> [u8; 4] {
        t.pixels[(ROWS - 1 - usize::from(pitch)) * TILE_COLS + col]
    }

    fn border(t: &TileOut, col: usize, pitch: u8) -> [u8; 4] {
        t.borders[(ROWS - 1 - usize::from(pitch)) * TILE_COLS + col]
    }

    fn row_borders() -> TileOptions {
        TileOptions {
            outline: Some(Outline {
                vertical: false,
                borders: true,
                ..Outline::new(255, 75)
            }),
            ..TileOptions::default()
        }
    }

    /// B2-D02 (owner screenshots 003815 good / 003928 bad): fourteen
    /// adjacent pitch rows of one track and subchannel. beta1 (texel-high
    /// tops / bottoms, `vertical`): only the stack's first and last rows are
    /// edges — each a whole row tall on screen — and nothing separates the
    /// twelve rows inside: one fused slab. beta2 (`borders`): no row-high
    /// edge texels; every row carries its notes' border colour, from which
    /// the roll draws a 1-point line at each row's top and bottom.
    #[test]
    fn every_row_of_a_same_structure_stack_gets_its_own_border() {
        let s = store((40..54u8).map(|k| rec(20, 200, k, 3)).collect());
        let fill = [180u8, 100, 200];
        let shade = outline_shade(fill, 75);
        let mut sc = Scratch::default();
        let beta1 = TileOptions {
            outline: Some(Outline::new(255, 75)),
            ..TileOptions::default()
        };
        let t = build_tile(&[&s], &Subs, 0, 0, beta1, &mut sc, &|| false).unwrap();
        assert_eq!(
            px(&t, 100, 53)[..3],
            shade,
            "the stack's top row is all edge"
        );
        assert_eq!(px(&t, 100, 40)[..3], shade, "and its bottom row");
        for k in 41..53 {
            assert_eq!(px(&t, 100, k)[..3], fill, "row {k}: nothing separates it");
        }
        assert!(t.borders.is_empty());
        let t = build_tile(&[&s], &Subs, 0, 0, row_borders(), &mut sc, &|| false).unwrap();
        for k in 40..54 {
            assert_eq!(px(&t, 100, k)[..3], fill, "row {k}: no row-high edge texel");
            assert_eq!(
                border(&t, 100, k),
                [shade[0], shade[1], shade[2], 255],
                "row {k}"
            );
        }
        // The note ends stay texel edges (columns are physical pixels).
        assert_eq!(px(&t, 20, 45)[..3], shade);
        assert_eq!(px(&t, 219, 45)[..3], shade);
        // Empty texels have no border.
        assert_eq!(border(&t, 100, 60)[3], 0);
        assert_eq!(border(&t, 10, 45)[3], 0);
        assert_eq!(border(&t, 230, 45)[3], 0);
    }

    /// B2-D02: like the exact renderer (borders on notes at least 4 px wide),
    /// a run of one note shorter than MIN_RUN texels has no border; a run
    /// crossing a tile seam is measured whole (the apron), so both tiles
    /// border it; the winner's own border colour (`TileColors::border`),
    /// the selection colour and the outline opacity are used; borders are
    /// part of the tile's identity hash.
    #[test]
    fn row_borders_follow_the_exact_renderers_rules() {
        struct Bordered;
        impl TileColors for Bordered {
            fn color(&self, _s: usize, n: &NoteView) -> ([u8; 4], bool) {
                (
                    [90, 90, 90, if n.attrs.pitch() == 70 { 100 } else { 255 }],
                    n.selected,
                )
            }
            fn border(&self, w: &Winner) -> Option<[u8; 3]> {
                (w.sub == 7).then_some([1, 2, 3])
            }
        }
        let mut notes = vec![
            rec(10, 3, 60, 0),  // 3 texels: no border
            rec(30, 4, 61, 0),  // 4 texels: bordered
            rec(253, 6, 62, 0), // 3 + 3 texels across the seam: bordered in both tiles
            rec(40, 50, 63, 7), // own border colour
            rec(40, 50, 70, 0), // translucent fill: the outline's opacity
        ];
        notes.push(rec(100, 50, 65, 0));
        let mut s = store(notes);
        s.select_in_region(100, 120, 65, 65);
        let mut sc = Scratch::default();
        let mut o = row_borders();
        o.outline.as_mut().unwrap().alpha = 200;
        o.outline.as_mut().unwrap().selection = Some([255, 230, 0]);
        let t0 = build_tile(&[&s], &Bordered, 0, 0, o, &mut sc, &|| false).unwrap();
        let t1 = build_tile(&[&s], &Bordered, 0, 1, o, &mut sc, &|| false).unwrap();
        let shade = outline_shade([90, 90, 90], 75);
        assert_eq!(border(&t0, 11, 60)[3], 0, "narrower than MIN_RUN");
        assert_eq!(border(&t0, 31, 61), [shade[0], shade[1], shade[2], 255]);
        assert_eq!(border(&t0, 254, 62)[3], 255, "seam run, left tile");
        assert_eq!(border(&t1, 1, 62)[3], 255, "seam run, right tile");
        assert_eq!(border(&t0, 60, 63), [1, 2, 3, 255], "TileColors::border");
        assert_eq!(border(&t0, 60, 70)[3], 200, "at least the outline opacity");
        assert_eq!(border(&t0, 110, 65), [255, 230, 0, 255], "selection color");
        let again = build_tile(&[&s], &Bordered, 0, 0, o, &mut sc, &|| false).unwrap();
        assert_eq!(t0.hash(), again.hash(), "deterministic");
        let mut moved = again.clone();
        moved.borders[0] = [9, 9, 9, 9];
        assert_ne!(t0.hash(), moved.hash(), "borders are part of the identity");
    }

    #[test]
    fn levels_are_quarter_octaves_and_never_shrink_tiles() {
        for ppt in [1e-6, 0.003, 0.1, 0.5, 1.0, 1.3, 7.7, 64.0] {
            let l = level_for(ppt);
            assert!(level_ppt(l) <= ppt * (1.0 + 1e-9), "{ppt}");
            assert!(level_ppt(l + 1) > ppt, "{ppt}");
        }
        assert_eq!(level_for(1.0), 0);
        assert_eq!(level_for(2.0), 4);
        let (a, b) = tile_range(0, 0, 1000);
        assert_eq!((a, b), (0, 3));
    }

    #[test]
    fn notes_land_on_their_columns_and_rows_with_their_colours() {
        // Level 0: one column per tick.
        let s = store(vec![rec(10, 5, 60, 1), rec(300, 10, 61, 2)]);
        let mut sc = Scratch::default();
        let t = build_tile(&[&s], &Subs, 0, 0, TileOptions::default(), &mut sc, &|| {
            false
        })
        .unwrap();
        assert_eq!(t.notes, 1, "the second note belongs to tile 1");
        for c in 10..15 {
            assert_eq!(px(&t, c, 60)[0], 60, "column {c}");
        }
        assert_eq!(px(&t, 9, 60)[3], 0);
        assert_eq!(px(&t, 15, 60)[3], 0);
        assert_eq!(px(&t, 12, 61)[3], 0);
        let t1 = build_tile(&[&s], &Subs, 0, 1, TileOptions::default(), &mut sc, &|| {
            false
        })
        .unwrap();
        assert_eq!(px(&t1, 300 - 256, 61)[0], 120);
        // Zoomed out (level −16: 1/16 pixel per tick): a 1-tick note still
        // covers one column.
        let s = store(vec![rec(1000, 1, 60, 1)]);
        let t = build_tile(
            &[&s],
            &Subs,
            -16,
            0,
            TileOptions::default(),
            &mut sc,
            &|| false,
        )
        .unwrap();
        assert_eq!(t.occupied, 1);
    }

    #[test]
    fn dense_texels_show_the_note_drawn_last_with_the_selection_in_front() {
        // Ten notes of sub 1 and ten of sub 2 on one texel: the texel takes
        // the solid colour of the note the exact renderer draws last (stored
        // order), never a blend (v0.9 §029).
        let mut v = Vec::new();
        for k in 0..10 {
            v.push(rec(k, 1, 64, 1));
            v.push(rec(k, 1, 64, 2));
        }
        let a = store(v.clone());
        v.reverse();
        let b = store(v);
        let mut sc = Scratch::default();
        let lvl = -8; // 1/4 pixel per tick: ticks 0 … 9 → columns 0 … 2
        let build = |s: &NoteStore, sc: &mut Scratch| {
            build_tile(&[s], &Subs, lvl, 0, TileOptions::default(), sc, &|| false).unwrap()
        };
        let ta = build(&a, &mut sc);
        // Ticks 8 and 9 (column 2): sub 2 is stored after sub 1 in `a`, before
        // it in `b`.
        assert_eq!(px(&ta, 2, 64)[0], 120, "sub 2 drawn last");
        assert_eq!(sc.winner(ROWS - 1 - 64, 2).map(|w| w.sub), Some(2));
        assert_eq!(px(&build(&b, &mut sc), 2, 64)[0], 60, "sub 1 drawn last");
        assert_eq!(
            build(&a, &mut sc).hash(),
            ta.hash(),
            "same input, same pixels"
        );
        // A selected note is in front wherever it is stored.
        let mut a2 = a.clone();
        a2.select_in_region(0, 0, 64, 64);
        let t = build(&a2, &mut sc);
        assert_eq!(px(&t, 0, 64)[..3], [255, 255, 255]);
        assert!(sc.winner(ROWS - 1 - 64, 0).unwrap().selected);
        assert_eq!(px(&t, 2, 64)[0], 120, "other columns unchanged");
        // Opacity: n notes of alpha a composite to 1 − (1 − a)^n, like the
        // exact renderer's stacked translucent rectangles (§025, §186).
        struct Translucent;
        impl TileColors for Translucent {
            fn color(&self, _s: usize, _n: &NoteView) -> ([u8; 4], bool) {
                ([10, 20, 30, 100], false)
            }
        }
        let opt = TileOptions::default();
        for n in [1u64, 2, 3, 50] {
            let s = store((0..n).map(|_| rec(0, 1, 64, 1)).collect());
            let t = build_tile(&[&s], &Translucent, 0, 0, opt, &mut sc, &|| false).unwrap();
            assert_eq!(px(&t, 0, 64)[3], stacked_alpha(100, n as u32), "{n} notes");
        }
        assert_eq!(stacked_alpha(100, 1), 100);
        assert_eq!(stacked_alpha(100, 2), 161);
        assert_eq!(stacked_alpha(255, 1), 255);
        assert_eq!(stacked_alpha(0, 1_000_000), 0);
        assert_eq!(px(&ta, 0, 64)[3], 255, "opaque colors stay opaque");
    }

    #[test]
    fn outlines_are_continuous_across_tile_seams() {
        // One long note across the seam of tiles 0 and 1 (level 0), 3 rows tall.
        let s = store((60..63).map(|p| rec(200, 120, p, 1)).collect());
        let opt = TileOptions {
            outline: Some(Outline::new(250, 100)),
            ..TileOptions::default()
        };
        let mut sc = Scratch::default();
        let a = build_tile(&[&s], &Subs, 0, 0, opt, &mut sc, &|| false).unwrap();
        let b = build_tile(&[&s], &Subs, 0, 1, opt, &mut sc, &|| false).unwrap();
        let fill = [60, 100, 200];
        // Middle row, last column of tile 0 / first of tile 1: interior.
        assert_eq!(px(&a, 255, 61)[..3], fill, "no seam edge on the left tile");
        assert_eq!(px(&b, 0, 61)[..3], fill, "no seam edge on the right tile");
        // True edges are outlined.
        assert_eq!(px(&a, 200, 61)[..3], [0, 0, 0]);
        assert_eq!(px(&a, 230, 62)[..3], [0, 0, 0]);
        assert_eq!(px(&b, 319 - 256, 61)[..3], [0, 0, 0]);
    }

    #[test]
    fn a_cancelled_build_returns_nothing() {
        let s = store(
            (0..200_000u64)
                .map(|i| rec(i % 256, 1, (i % 128) as u8, 0))
                .collect(),
        );
        let mut sc = Scratch::default();
        assert!(
            build_tile(&[&s], &Subs, 0, 0, TileOptions::default(), &mut sc, &|| {
                true
            })
            .is_none()
        );
    }

    /// One colour per subchannel, all distinct (v0.9 §028 fixtures).
    struct Distinct;
    impl Distinct {
        fn of(source: usize, sub: u32) -> [u8; 4] {
            let h = crate::transform::rng::mix(u64::from(sub) * 131 + source as u64);
            let c = h.to_le_bytes();
            [c[0], c[1], c[2], 255]
        }
    }
    impl TileColors for Distinct {
        fn color(&self, source: usize, n: &NoteView) -> ([u8; 4], bool) {
            (Distinct::of(source, n.sub.0), n.selected)
        }
    }

    #[test]
    fn sixty_four_classes_in_one_tile_keep_their_own_colours() {
        // v0.9 §028: 64 distinct colour classes, one note per pitch row, so
        // every texel holds exactly one note: each must show that note's own
        // colour (v0.8 hashed the 33rd+ class into an existing one).
        let s = store((0..64u32).map(|k| rec(20, 30, 30 + k as u8, k)).collect());
        let mut sc = Scratch::default();
        let t = build_tile(
            &[&s],
            &Distinct,
            0,
            0,
            TileOptions::default(),
            &mut sc,
            &|| false,
        )
        .unwrap();
        for k in 0..64u32 {
            let want = Distinct::of(0, k);
            for c in [20usize, 35, 49] {
                assert_eq!(
                    px(&t, c, 30 + k as u8)[..3],
                    want[..3],
                    "class {k}, column {c}"
                );
            }
        }
    }

    #[test]
    fn many_colours_need_no_colour_table_and_keep_exact_colours() {
        // 500 distinct colours on one row: every column shows the colour of
        // its last stored note.
        let s = store(
            (0..5_000u64)
                .map(|i| rec(i % 256, 1, 60, (i % 500) as u32 * 7))
                .collect(),
        );
        let mut sc = Scratch::default();
        let t = build_tile(&[&s], &Subs, 0, 0, TileOptions::default(), &mut sc, &|| {
            false
        })
        .unwrap();
        assert_eq!(t.notes, 5_000);
        assert_eq!(t.occupied, 256);
        for c in 0..256u64 {
            let last = (0..5_000u64).filter(|i| i % 256 == c).max().unwrap();
            let sub = (last % 500) as u32 * 7;
            assert_eq!(
                u32::from(px(&t, c as usize, 60)[0]),
                sub * 60 % 256,
                "column {c}"
            );
            assert_eq!(sc.winner(ROWS - 1 - 60, c as usize).unwrap().sub, sub);
        }
        assert_eq!(sc.tree.len(), ROWS * 2 * W, "fixed memory per tile");
    }

    /// Ranked layers: source = rank (Onion, back to front).
    struct Ranked;
    impl TileColors for Ranked {
        fn color(&self, source: usize, n: &NoteView) -> ([u8; 4], bool) {
            Distinct.color(source, n)
        }
        fn rank(&self, source: usize) -> u16 {
            source as u16
        }
    }

    /// Ranks given in reverse of the source order.
    struct Reversed(usize);
    impl TileColors for Reversed {
        fn color(&self, source: usize, n: &NoteView) -> ([u8; 4], bool) {
            Distinct.color(source, n)
        }
        fn rank(&self, source: usize) -> u16 {
            (self.0 - source) as u16
        }
    }

    /// The brute-force painter: every note's rectangle drawn in the exact
    /// renderer's order (normal notes by rank / source / stored order, then
    /// the selected ones on top), sampled per tile texel.
    fn painter(
        stores: &[&NoteStore],
        colors: &dyn TileColors,
        level: i32,
        index: i64,
    ) -> Vec<Option<Winner>> {
        painter_notes(stores, colors, level, index)
            .into_iter()
            .map(|w| w.map(|w| w.0))
            .collect()
    }

    /// [`painter`] with each winner's note identity (source, stored index).
    fn painter_notes(
        stores: &[&NoteStore],
        colors: &dyn TileColors,
        level: i32,
        index: i64,
    ) -> Vec<Option<(Winner, (usize, usize))>> {
        let ppt = level_ppt(level);
        let t0 = tile_start(level, index);
        let mut out = vec![None; ROWS * TILE_COLS];
        let mut order: Vec<usize> = (0..stores.len()).collect();
        order.sort_by_key(|&si| (colors.rank(si), si));
        for pass in [false, true] {
            for &si in &order {
                for (ni, v) in stores[si].iter().enumerate() {
                    let (c, prio) = colors.color(si, &v);
                    if prio != pass {
                        continue;
                    }
                    let Some((x0, x1)) = columns(v.start_tick, v.duration_ticks, t0, ppt) else {
                        continue;
                    };
                    let row = ROWS - 1 - usize::from(v.attrs.pitch());
                    for x in x0.max(APRON)..=x1.min(APRON + TILE_COLS - 1) {
                        let w = Winner {
                            selected: prio,
                            source: si,
                            rgba: c,
                            sub: v.sub.0,
                            velocity: v.attrs.velocity(),
                        };
                        out[row * TILE_COLS + x - APRON] = Some((w, (si, ni)));
                    }
                }
            }
        }
        out
    }

    /// Adversarial fixture: `sources` stores of `n` notes on a narrow pitch
    /// band, `subs` subchannels each (all colours distinct), short and long
    /// notes (some across many tiles), random selection.
    fn adversarial(seed: u64, sources: usize, n: usize, subs: u32) -> Vec<NoteStore> {
        let mut rng = crate::transform::rng::Rng::new(seed);
        (0..sources)
            .map(|_| {
                let v: Vec<NoteRecord> = (0..n)
                    .map(|_| {
                        let long = rng.unit() < 0.05;
                        let dur = if long {
                            rng.range_i64(100, 1_500)
                        } else {
                            rng.range_i64(1, 20)
                        } as u64;
                        let start = rng.range_i64(0, 3_000) as u64;
                        let pitch = rng.range_i64(50, 70) as u8;
                        rec(
                            start,
                            dur,
                            pitch,
                            rng.range_i64(0, i64::from(subs) - 1) as u32,
                        )
                    })
                    .collect();
                let mut s = store(v);
                let t = rng.range_i64(0, 2_900) as u64;
                s.select_in_region(t, t + 60, 55, 60);
                s
            })
            .collect()
    }

    fn check_against_painter(
        stores: &[&NoteStore],
        colors: &dyn TileColors,
        level: i32,
        index: i64,
    ) {
        let mut sc = Scratch::default();
        let t = build_tile(
            stores,
            colors,
            level,
            index,
            TileOptions::default(),
            &mut sc,
            &|| false,
        )
        .unwrap();
        let want = painter(stores, colors, level, index);
        for (i, w) in want.iter().enumerate() {
            let (row, col) = (i / TILE_COLS, i % TILE_COLS);
            assert_eq!(
                sc.winner(row, col),
                *w,
                "level {level} tile {index} row {row} col {col}"
            );
            match w {
                Some(w) => assert_eq!(t.pixels[i][..3], w.rgba[..3]),
                None => assert_eq!(t.pixels[i][3], 0),
            }
        }
    }

    /// [`painter_notes`] with keys folded into rows ([`KeyFold`]) and only the
    /// notes starting in `window`: each texel's winner and the number of notes
    /// covering its band and column.
    fn painter_folded(
        stores: &[&NoteStore],
        colors: &dyn TileColors,
        level: i32,
        index: i64,
        fold: KeyFold,
        window: Option<(u64, u64)>,
    ) -> (Vec<Option<Winner>>, Vec<u32>) {
        let ppt = level_ppt(level);
        let t0 = tile_start(level, index);
        let rows = fold.rows();
        let mut out = vec![None; rows * TILE_COLS];
        let mut count = vec![0u32; rows * TILE_COLS];
        let mut order: Vec<usize> = (0..stores.len()).collect();
        order.sort_by_key(|&si| (colors.rank(si), si));
        for pass in [false, true] {
            for &si in &order {
                for v in stores[si].iter() {
                    if window.is_some_and(|(a, b)| v.start_tick < a || v.start_tick >= b) {
                        continue;
                    }
                    let (c, prio) = colors.color(si, &v);
                    let Some((x0, x1)) = columns(v.start_tick, v.duration_ticks, t0, ppt) else {
                        continue;
                    };
                    let Some(row) = fold.row_of(v.attrs.pitch()) else {
                        continue;
                    };
                    for x in x0.max(APRON)..=x1.min(APRON + TILE_COLS - 1) {
                        let i = row * TILE_COLS + x - APRON;
                        if prio == pass {
                            out[i] = Some(Winner {
                                selected: prio,
                                source: si,
                                rgba: c,
                                sub: v.sub.0,
                                velocity: v.attrs.velocity(),
                            });
                        }
                        if !pass {
                            count[i] += 1;
                        }
                    }
                }
            }
        }
        (out, count)
    }

    /// Translucent colours per subchannel (so the opacity shows the count).
    struct Half;
    impl TileColors for Half {
        fn color(&self, _s: usize, n: &NoteView) -> ([u8; 4], bool) {
            if n.selected {
                return ([255, 255, 255, 200], true);
            }
            ([(n.sub.0 * 37 % 256) as u8, 90, 160, 120], false)
        }
    }

    /// v0.9 V09-S09 (clip mini notes): with a key fold and a start window,
    /// every texel equals the brute-force painter folded the same way —
    /// winner, colour, and the stacked opacity of the notes covering its band
    /// — for many bands per row, one key per row, one row for everything and
    /// keys outside the fold; 256-key notes fold like the others.
    #[test]
    fn folded_tiles_equal_the_painter_with_keys_folded_into_rows() {
        let stores = adversarial(0x5EED_0909, 2, 2_000, 40);
        let refs: Vec<&NoteStore> = stores.iter().collect();
        let folds = [
            KeyFold::new(50, 69, 20),
            KeyFold::new(69, 50, 7),
            KeyFold::new(55, 64, 3),
            KeyFold::new(0, 127, 1),
            KeyFold::new(60, 60, 5),
        ];
        for fold in folds {
            for window in [None, Some((700, 1_900))] {
                for (level, tiles) in [(0, 0..12), (-6, 0..2)] {
                    for index in tiles {
                        let opt = TileOptions {
                            fold: Some(fold),
                            window,
                            ..TileOptions::default()
                        };
                        let mut sc = Scratch::default();
                        let t = build_tile(&refs, &Half, level, index, opt, &mut sc, &|| false)
                            .unwrap();
                        assert_eq!(t.rows, fold.rows());
                        let (want, count) =
                            painter_folded(&refs, &Half, level, index, fold, window);
                        for (i, w) in want.iter().enumerate() {
                            let (row, col) = (i / TILE_COLS, i % TILE_COLS);
                            let at = format!(
                                "{fold:?} {window:?} level {level} tile {index} row {row} col {col}"
                            );
                            assert_eq!(sc.winner(row, col), *w, "{at}");
                            match w {
                                Some(w) => {
                                    assert_eq!(t.pixels[i][..3], w.rgba[..3], "{at}");
                                    assert_eq!(
                                        t.pixels[i][3],
                                        stacked_alpha(w.rgba[3], count[i]),
                                        "{at}"
                                    );
                                }
                                None => assert_eq!(t.pixels[i][3], 0, "{at}"),
                            }
                        }
                    }
                }
            }
        }
        // 256 keys: notes at 0, 127, 128, 200 and 255 in 8 bands of 32 keys.
        let wide = store(
            [0u8, 127, 128, 200, 255]
                .iter()
                .map(|&p| NoteRecord {
                    attrs: NoteAttrs::new_wide(p, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
                    ..rec(10, 5, 60, 1)
                })
                .collect(),
        );
        let fold = KeyFold::new(0, 255, 8);
        let opt = TileOptions {
            keys: KeyRange::Extended256,
            fold: Some(fold),
            ..TileOptions::default()
        };
        let mut sc = Scratch::default();
        let t = build_tile(&[&wide], &Subs, 0, 0, opt, &mut sc, &|| false).unwrap();
        let rows: Vec<usize> = (0..8).filter(|&r| sc.winner(r, 12).is_some()).collect();
        assert_eq!(rows, vec![0, 1, 3, 4, 7], "255 / 200 / 128 / 127 / 0");
        assert_eq!((t.notes, t.occupied), (5, 25));
    }

    #[test]
    fn every_texel_shows_the_true_identity_of_its_winner_with_hundreds_of_classes() {
        // v0.9 §028 / §178: 3 sources × 100 subchannels = 300 colour classes
        // (≥ 64 per tile), dense overlap, seams, several zoom levels: each
        // texel equals the brute-force painter — colour, source, subchannel
        // and selection of the note drawn last there.
        let stores = adversarial(0x5EED_0028, 3, 3_000, 100);
        let refs: Vec<&NoteStore> = stores.iter().collect();
        for (level, tiles) in [(0, 0..13), (-8, 0..4), (3, 0..4), (-20, 0..1)] {
            for index in tiles {
                check_against_painter(&refs, &Ranked, level, index);
                check_against_painter(&refs, &Subs, level, index);
            }
        }
    }

    #[test]
    fn rank_not_argument_order_decides_the_layers() {
        // The same layers passed in reverse with reversed ranks give the
        // same picture (source indices aside).
        let stores = adversarial(7, 4, 1_500, 64);
        let fwd: Vec<&NoteStore> = stores.iter().collect();
        let rev: Vec<&NoteStore> = stores.iter().rev().collect();
        let mut sc = Scratch::default();
        for index in 0..12 {
            let opt = TileOptions::default();
            let a = build_tile(&fwd, &Ranked, 0, index, opt, &mut sc, &|| false).unwrap();
            let wa: Vec<_> = (0..ROWS * TILE_COLS)
                .map(|i| {
                    sc.winner(i / TILE_COLS, i % TILE_COLS)
                        .map(|w| (w.rgba, w.sub))
                })
                .collect();
            let b = build_tile(&rev, &Reversed(3), 0, index, opt, &mut sc, &|| false).unwrap();
            let wb: Vec<_> = (0..ROWS * TILE_COLS)
                .map(|i| {
                    sc.winner(i / TILE_COLS, i % TILE_COLS)
                        .map(|w| (w.rgba, w.sub))
                })
                .collect();
            // Colours depend on the source index in `Distinct`; compare the
            // subchannels and occupancy.
            assert_eq!(
                wa.iter().map(|w| w.map(|w| w.1)).collect::<Vec<_>>(),
                wb.iter().map(|w| w.map(|w| w.1)).collect::<Vec<_>>(),
                "tile {index}"
            );
            assert_eq!(a.occupied, b.occupied);
            check_against_painter(&rev, &Reversed(3), 0, index);
        }
    }

    /// v1.0-beta1 B1-0110: the Piano Roll keyboard's foremost note per key —
    /// the highest layer first, then the latest start, the later source, the
    /// later stored note — equals the brute force on random multi-source
    /// fixtures (placements, corrections, equal and distinct ranks, dense
    /// few-key cases, with and without key masks).
    #[test]
    fn foremost_keys_take_the_highest_layer_then_the_latest_start() {
        let mut r = Rnd(0x0B1_0110);
        let mut out = Vec::new();
        for case in 0..150 {
            let n_src = 1 + r.below(5) as usize;
            let top = if case % 2 == 0 {
                127
            } else {
                2 + r.below(10) as u8
            };
            let stores: Vec<NoteStore> = (0..n_src)
                .map(|_| {
                    let n = r.below(800) as usize;
                    random_store(&mut r, n, top)
                })
                .collect();
            let places: Vec<SourcePlace> = (0..n_src).map(|_| random_place(&mut r)).collect();
            let reads: Vec<Reading> = (0..n_src)
                .map(|_| match r.below(3) {
                    0 => Reading::Played(Some(random_correction(&mut r, 127))),
                    _ => Reading::Played(None),
                })
                .collect();
            // Ranks: often shared (clips of one track), sometimes distinct.
            let ranks: Vec<u32> = (0..n_src).map(|_| r.below(3) as u32).collect();
            let src: Vec<&NoteStore> = stores.iter().collect();
            let played: Vec<NoteStore> = (0..n_src)
                .map(|k| export_store(&stores[k], places[k], reads[k]))
                .collect();
            let masks: Vec<[u64; 4]> = (0..n_src).map(|k| lit_mask(&stores[k], reads[k])).collect();
            for t in 0..6 {
                let tick = r.below(25_000);
                let m: &[[u64; 4]] = if t % 2 == 0 { &masks } else { &[] };
                foremost_keys(&src, &places, &reads, &ranks, m, tick, 128, &mut out);
                // (rank, start, source, stored index, attributes).
                type Lit = (u32, u64, usize, usize, u32);
                let mut want: Vec<Option<Lit>> = vec![None; 128];
                for (si, p) in played.iter().enumerate() {
                    for (ni, v) in p.iter().enumerate() {
                        let k = usize::from(v.attrs.pitch());
                        if v.start_tick <= tick && v.end_tick() > tick && k < 128 {
                            let key = (ranks[si], v.start_tick, si, ni, v.attrs.raw());
                            if want[k]
                                .is_none_or(|w| (w.0, w.1, w.2, w.3) < (key.0, key.1, key.2, key.3))
                            {
                                want[k] = Some(key);
                            }
                        }
                    }
                }
                for k in 0..128 {
                    let got = out[k].map(|(si, v)| (si, v.start_tick, v.attrs.raw()));
                    let exp = want[k].map(|w| (w.2, w.1, w.4));
                    assert_eq!(got, exp, "case {case} tick {tick} key {k} ranks {ranks:?}");
                }
            }
        }
    }

    /// Ranks given by a permutation of the sources (Onion order).
    struct Permuted(Vec<u16>);
    impl TileColors for Permuted {
        fn color(&self, source: usize, n: &NoteView) -> ([u8; 4], bool) {
            Distinct.color(source, n)
        }
        fn rank(&self, source: usize) -> u16 {
            self.0[source]
        }
    }

    /// v1.0-beta1 B1-0154 (contract §022-C): 2 / 16 / 64 / 256 overlapping
    /// colour classes, ranked by a shuffled order, every texel of tiles at
    /// three levels and on both sides of moving tile boundaries equals the
    /// brute-force painter — the top-ranked class wins wherever it has a
    /// note, the next one where it has none, and no class takes another's
    /// colour. Re-ranking (another view's order) changes only which class
    /// wins, never a class's colour.
    #[test]
    fn ranked_classes_win_by_rank_across_tile_boundaries_at_every_count() {
        let mut rng = crate::transform::rng::Rng::new(0x0154);
        for n in [2usize, 16, 64, 256] {
            let stores: Vec<NoteStore> = (0..n)
                .map(|k| {
                    // Every class: a long note on key 60 crossing several
                    // tile boundaries, plus short notes of its own.
                    let mut v = vec![rec(100 + k as u64 % 7, 2_000, 60, k as u32 % 3)];
                    v.extend((0..20).map(|_| {
                        rec(
                            rng.range_i64(0, 2_400) as u64,
                            rng.range_i64(1, 300) as u64,
                            rng.range_i64(55, 66) as u8,
                            k as u32 % 3,
                        )
                    }));
                    store(v)
                })
                .collect();
            let refs: Vec<&NoteStore> = stores.iter().collect();
            let mut perm: Vec<u16> = (0..n as u16).collect();
            for i in (1..n).rev() {
                perm.swap(i, rng.range_i64(0, i as i64) as usize);
            }
            for ranks in [
                Permuted(perm.clone()),
                Permuted(perm.iter().map(|r| n as u16 - 1 - r).collect()),
            ] {
                let top = (0..n).max_by_key(|&k| ranks.0[k]).unwrap();
                for (level, indices) in [(0i32, 0..10i64), (-4, 0..2), (2, 2..14)] {
                    for index in indices {
                        check_against_painter(&refs, &ranks, level, index);
                    }
                }
                // Key 60 at tick 1,000 (inside every class's long note):
                // the top-ranked class's own colour.
                let mut sc = Scratch::default();
                let t = build_tile(
                    &refs,
                    &ranks,
                    0,
                    1_000 / TILE_COLS as i64,
                    TileOptions::default(),
                    &mut sc,
                    &|| false,
                )
                .unwrap();
                let w = sc.winner(ROWS - 1 - 60, 1_000 % TILE_COLS).unwrap();
                assert_eq!(w.rgba, Distinct::of(top, top as u32 % 3), "{n} classes");
                assert!(t.occupied > 0);
            }
        }
    }

    #[test]
    fn chunking_and_insertion_order_do_not_change_a_tile() {
        // Same records (distinct starts per pitch so stored order is unique)
        // inserted shuffled, into stores with tiny and default chunks.
        let mut rng = crate::transform::rng::Rng::new(99);
        let mut v: Vec<NoteRecord> = (0..4_000u64)
            .map(|i| {
                rec(
                    i * 3 % 7_919,
                    1 + i % 40,
                    40 + (i % 30) as u8,
                    (i % 97) as u32,
                )
            })
            .collect();
        let a = store(v.clone());
        for i in (1..v.len()).rev() {
            v.swap(i, rng.range_i64(0, i as i64) as usize);
        }
        v.sort_by_key(|r| r.start_tick);
        let mut b = NoteStore::with_chunk_target(16);
        b.insert_sorted_batch(&v, false).unwrap();
        let mut sc = Scratch::default();
        for (level, index) in [(0, 0), (0, 7), (-6, 0), (-6, 1), (-12, 0)] {
            let opt = TileOptions {
                outline: Some(Outline::new(230, 80)),
                ..TileOptions::default()
            };
            let ta = build_tile(&[&a], &Distinct, level, index, opt, &mut sc, &|| false).unwrap();
            let tb = build_tile(&[&b], &Distinct, level, index, opt, &mut sc, &|| false).unwrap();
            assert_eq!(ta.hash(), tb.hash(), "level {level} tile {index}");
            assert_eq!(ta.occupied, tb.occupied);
        }
    }

    #[test]
    fn touching_structures_keep_a_boundary_between_them() {
        // v0.9 §027: two structures (subchannels 1 and 2) side by side on the
        // same rows with no empty texel between them: the boundary columns are
        // outlined on both sides (v0.8 only outlined next to empty texels).
        let mut v: Vec<NoteRecord> = Vec::new();
        for p in 60..66 {
            v.push(rec(20, 40, p, 1));
            v.push(rec(60, 40, p, 2));
        }
        let s = store(v);
        let opt = TileOptions {
            outline: Some(Outline::new(255, 100)),
            ..TileOptions::default()
        };
        let mut sc = Scratch::default();
        let t = build_tile(&[&s], &Subs, 0, 0, opt, &mut sc, &|| false).unwrap();
        let fill1 = [60, 100, 200];
        let fill2 = [120, 100, 200];
        assert_eq!(px(&t, 40, 62)[..3], fill1, "interior keeps its fill");
        assert_eq!(px(&t, 80, 62)[..3], fill2, "interior keeps its fill");
        assert_ne!(
            px(&t, 59, 62)[..3],
            fill1,
            "last column of structure 1 is outlined"
        );
        assert_ne!(
            px(&t, 60, 62)[..3],
            fill2,
            "first column of structure 2 is outlined"
        );
    }

    #[test]
    fn notes_of_one_structure_are_separated_and_thin_runs_keep_their_fill() {
        // One subchannel, one row: three 10-tick notes end to end, then
        // twenty 1-tick notes; level 0 (one column per tick).
        let mut v: Vec<NoteRecord> = (0..3).map(|k| rec(10 + k * 10, 10, 60, 1)).collect();
        v.extend((0..20).map(|k| rec(100 + k, 1, 60, 1)));
        v.push(rec(200, 2, 60, 1)); // a 2-column note alone
        let s = store(v);
        let opt = TileOptions {
            outline: Some(Outline::new(255, 100)),
            ..TileOptions::default()
        };
        let mut sc = Scratch::default();
        let t = build_tile(&[&s], &Subs, 0, 0, opt, &mut sc, &|| false).unwrap();
        let fill = [60, 100, 200];
        let black = [0, 0, 0];
        // Each long note is bordered at both ends (Kiva-like separation).
        for c in [10, 19, 20, 29, 30, 39] {
            assert_eq!(px(&t, c, 60)[..3], black, "column {c}");
        }
        for c in [11, 15, 25, 35] {
            assert_eq!(px(&t, c, 60)[..3], fill, "column {c}");
        }
        // The run of 1-column notes is outlined as one structure at its ends
        // only; inside, the notes are too thin for borders.
        assert_eq!(px(&t, 100, 60)[..3], black);
        assert_eq!(px(&t, 119, 60)[..3], black);
        for c in 101..119 {
            assert_eq!(px(&t, c, 60)[..3], fill, "column {c}");
        }
        // A 2-column note keeps its fill (no room for a border).
        assert_eq!(px(&t, 200, 60)[..3], fill);
        assert_eq!(px(&t, 201, 60)[..3], fill);
        assert!(t.edges > 0);
    }

    #[test]
    fn selected_notes_are_outlined_in_the_selection_colour_and_vertical_edges_can_be_off() {
        // A 4-row block of 40 columns, its right half selected.
        let mut s = store(
            (60..64)
                .map(|p| rec(0, 40, p, 1))
                .chain((60..64).map(|p| rec(40, 40, p, 1)))
                .collect(),
        );
        s.select_in_region(50, 50, 60, 63);
        let mut o = Outline::new(255, 100);
        o.selection = Some([250, 200, 10]);
        let opt = |o| TileOptions {
            outline: Some(o),
            ..TileOptions::default()
        };
        let mut sc = Scratch::default();
        let t = build_tile(&[&s], &Subs, 0, 0, opt(o), &mut sc, &|| false).unwrap();
        assert_eq!(
            px(&t, 40, 61)[..3],
            [250, 200, 10],
            "selected note's left edge"
        );
        assert_eq!(
            px(&t, 79, 62)[..3],
            [250, 200, 10],
            "selected note's right edge"
        );
        assert_eq!(
            px(&t, 60, 63)[..3],
            [250, 200, 10],
            "top of the selected block"
        );
        assert_eq!(px(&t, 60, 61)[..3], [255, 255, 255], "selected interior");
        assert_eq!(px(&t, 39, 61)[..3], [0, 0, 0], "unselected neighbor's edge");
        assert_eq!(
            px(&t, 20, 63)[..3],
            [0, 0, 0],
            "top of the unselected block"
        );
        o.vertical = false;
        let t = build_tile(&[&s], &Subs, 0, 0, opt(o), &mut sc, &|| false).unwrap();
        assert_eq!(px(&t, 20, 63)[..3], [60, 100, 200], "no top edge");
        assert_eq!(px(&t, 39, 61)[..3], [0, 0, 0], "ends still outlined");
    }

    /// Outlined pixels from a whole-song brute-force raster: tiles 0 … n − 1
    /// of `level` side by side, edge rules applied to the joined raster.
    fn painter_outlined(
        stores: &[&NoteStore],
        colors: &dyn TileColors,
        level: i32,
        n: i64,
        o: Outline,
    ) -> Vec<Vec<[u8; 3]>> {
        let cols = TILE_COLS * n as usize;
        let mut raster = vec![None; ROWS * cols];
        for i in 0..n {
            let t = painter_notes(stores, colors, level, i);
            for r in 0..ROWS {
                for c in 0..TILE_COLS {
                    raster[r * cols + i as usize * TILE_COLS + c] = t[r * TILE_COLS + c];
                }
            }
        }
        let st =
            |w: &Option<(Winner, (usize, usize))>| w.map(|w| (w.0.selected, w.0.source, w.0.sub));
        let note = |w: &Option<(Winner, (usize, usize))>| w.map(|w| w.1);
        let mut edge = vec![false; ROWS * cols];
        for r in 0..ROWS {
            let row = &raster[r * cols..(r + 1) * cols];
            let mut mark = |same: &dyn Fn(usize, usize) -> bool, some: &dyn Fn(usize) -> bool| {
                let mut a = 0;
                while a < cols {
                    let mut b = a;
                    while b + 1 < cols && same(a, b + 1) {
                        b += 1;
                    }
                    if some(a) && b + 1 - a >= MIN_RUN {
                        edge[r * cols + a] = true;
                        edge[r * cols + b] = true;
                    }
                    a = b + 1;
                }
            };
            mark(&|a, b| note(&row[a]) == note(&row[b]), &|a| {
                row[a].is_some()
            });
            mark(&|a, b| st(&row[a]) == st(&row[b]), &|a| row[a].is_some());
        }
        if o.vertical {
            for c in 0..cols {
                let mut a = 0;
                while a < ROWS {
                    let mut b = a;
                    while b + 1 < ROWS
                        && st(&raster[(b + 1) * cols + c]) == st(&raster[a * cols + c])
                    {
                        b += 1;
                    }
                    if raster[a * cols + c].is_some() && b + 1 - a >= MIN_STACK {
                        edge[a * cols + c] = true;
                        edge[b * cols + c] = true;
                    }
                    a = b + 1;
                }
            }
        }
        (0..n as usize)
            .map(|i| {
                let mut px = vec![[0; 3]; ROWS * TILE_COLS];
                for r in 0..ROWS {
                    for c in 0..TILE_COLS {
                        let g = r * cols + i * TILE_COLS + c;
                        let Some((w, _)) = raster[g] else { continue };
                        let rgb = [w.rgba[0], w.rgba[1], w.rgba[2]];
                        px[r * TILE_COLS + c] = match (edge[g], w.selected, o.selection) {
                            (true, true, Some(sel)) => sel,
                            (true, _, _) => outline_shade(rgb, o.darkness),
                            _ => rgb,
                        };
                    }
                }
                px
            })
            .collect()
    }

    #[test]
    fn outlines_equal_a_whole_song_painter_across_every_seam() {
        // Adversarial overlap, 3 layers × 100 subchannels, selection: every
        // tile's pixels equal the edge rules applied to the joined raster —
        // so outlines neither break nor double at tile seams.
        let stores = adversarial(0x0E06E, 3, 3_000, 100);
        let refs: Vec<&NoteStore> = stores.iter().collect();
        let mut sc = Scratch::default();
        for (level, vertical) in [(0, true), (0, false), (-3, true), (2, true)] {
            let o = Outline {
                alpha: 255,
                darkness: 85,
                selection: Some([255, 230, 0]),
                vertical,
                borders: false,
            };
            let opt = TileOptions {
                outline: Some(o),
                ..TileOptions::default()
            };
            // One tile more than compared: the raster must not end at a seam.
            let want = painter_outlined(&refs, &Ranked, level, 14, o);
            for (i, want) in want.iter().take(13).enumerate() {
                let t =
                    build_tile(&refs, &Ranked, level, i as i64, opt, &mut sc, &|| false).unwrap();
                for (k, w) in want.iter().enumerate() {
                    if t.pixels[k][3] != 0 {
                        assert_eq!(
                            t.pixels[k][..3],
                            w[..],
                            "level {level} tile {i} row {} col {}",
                            k / TILE_COLS,
                            k % TILE_COLS
                        );
                    }
                }
            }
        }
    }

    /// Outline stress (v0.9 §265): 64 layers × 16 subchannels on the same
    /// and neighbouring pitches (60–63), notes 1 tick apart and overlapping,
    /// some selected: every tile equals the whole-song painter with the edge
    /// rules (across seams, three zoom levels, vertical on / off), and
    /// rebuilding gives the same hashes.
    #[test]
    fn outlines_stay_exact_and_stable_under_extreme_overlap() {
        let mut rng = crate::transform::rng::Rng::new(265);
        let stores: Vec<NoteStore> = (0..64)
            .map(|layer| {
                let v: Vec<NoteRecord> = (0..400u64)
                    .map(|k| {
                        rec(
                            k * 3 + layer % 3,
                            1 + rng.range_i64(0, 12) as u64,
                            60 + rng.range_i64(0, 3) as u8,
                            rng.range_i64(0, 15) as u32,
                        )
                    })
                    .collect();
                let mut s = store(v);
                if layer % 9 == 0 {
                    s.select_in_region(200, 260, 60, 63);
                }
                s
            })
            .collect();
        let refs: Vec<&NoteStore> = stores.iter().collect();
        let mut sc = Scratch::default();
        for (level, vertical) in [(0, true), (-4, true), (2, false)] {
            let o = Outline {
                alpha: 255,
                darkness: 90,
                selection: Some([250, 250, 120]),
                vertical,
                borders: false,
            };
            let opt = TileOptions {
                outline: Some(o),
                ..TileOptions::default()
            };
            let n = 6;
            let want = painter_outlined(&refs, &Ranked, level, n + 1, o);
            for (i, want) in want.iter().take(n as usize).enumerate() {
                let t =
                    build_tile(&refs, &Ranked, level, i as i64, opt, &mut sc, &|| false).unwrap();
                for (k, w) in want.iter().enumerate() {
                    if t.pixels[k][3] != 0 {
                        assert_eq!(t.pixels[k][..3], w[..], "level {level} tile {i} texel {k}");
                    }
                }
                let again = build_tile(
                    &refs,
                    &Ranked,
                    level,
                    i as i64,
                    opt,
                    &mut Scratch::default(),
                    &|| false,
                )
                .unwrap();
                assert_eq!(t.hash(), again.hash());
                assert!(t.occupied == 0 || t.edges > 0, "level {level} tile {i}");
            }
        }
    }

    /// v0.9 §036 / §206: 256-Key Mode tiles have 256 rows (pitch 255 on
    /// top) and draw every pitch 0–255; standard tiles keep 128 rows — the
    /// v0.8 size and memory — and leave out pitches above 127.
    #[test]
    fn tiles_have_one_row_per_key_of_the_key_range() {
        let v: Vec<NoteRecord> = (0..=255u8)
            .map(|p| NoteRecord {
                start_tick: u64::from(p),
                duration_ticks: 1,
                attrs: NoteAttrs::new_wide(p, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
                sub: SubchannelId(1),
                group: 0,
            })
            .collect();
        let s = store(v);
        let wide = TileOptions {
            keys: KeyRange::Extended256,
            ..TileOptions::default()
        };
        let mut sc = Scratch::default();
        let t = build_tile(&[&s], &Subs, 0, 0, wide, &mut sc, &|| false).unwrap();
        assert_eq!(
            (t.rows, t.pixels.len(), t.notes, t.occupied),
            (256, 256 * TILE_COLS, 256, 256)
        );
        for p in 0..=255usize {
            assert_eq!(sc.winner(255 - p, p).map(|w| w.sub), Some(1), "pitch {p}");
            assert_ne!(t.pixels[(255 - p) * TILE_COLS + p][3], 0);
        }
        assert_eq!(tile_bytes(KeyRange::Extended256), 2 * TILE_BYTES);
        // Standard: 128 rows, pitches 128+ left out, same scratch size as v0.8.
        let t = build_tile(&[&s], &Subs, 0, 0, TileOptions::default(), &mut sc, &|| {
            false
        })
        .unwrap();
        assert_eq!(
            (t.rows, t.pixels.len(), t.notes, t.occupied),
            (ROWS, ROWS * TILE_COLS, 128, 128)
        );
        assert_eq!(sc.tree.len(), ROWS * 2 * W);
        assert_eq!(tile_bytes(KeyRange::Standard), TILE_BYTES);
        let fresh = build_tile(
            &[&s],
            &Subs,
            0,
            0,
            TileOptions::default(),
            &mut Scratch::default(),
            &|| false,
        )
        .unwrap();
        assert_eq!(t.hash(), fresh.hash(), "no leak from the 256-row build");
    }

    #[test]
    fn seam_texels_agree_with_the_neighbour_tiles_apron() {
        // A tile's right apron covers the next tile's first columns and its
        // last columns are the next tile's left apron: both builds must give
        // them the same winners (same note, same identity), so outlines and
        // colours continue across seams (also at levels whose tile span is
        // not a whole number of ticks).
        let stores = adversarial(31, 2, 2_000, 80);
        let refs: Vec<&NoteStore> = stores.iter().collect();
        let mut sc = Scratch::default();
        let opt = TileOptions::default();
        let leaf = |sc: &Scratch, r: usize, x: usize| unpack(sc.leaf(r, x), sc.layout, &sc.order);
        for level in [0, -3, 5] {
            for index in 0..6 {
                build_tile(&refs, &Ranked, level, index, opt, &mut sc, &|| false).unwrap();
                let right: Vec<Vec<_>> = (0..ROWS)
                    .map(|r| {
                        (0..2 * APRON)
                            .map(|k| leaf(&sc, r, TILE_COLS + k))
                            .collect()
                    })
                    .collect();
                build_tile(&refs, &Ranked, level, index + 1, opt, &mut sc, &|| false).unwrap();
                for (r, right) in right.iter().enumerate() {
                    for (k, w) in right.iter().enumerate() {
                        assert_eq!(
                            *w,
                            leaf(&sc, r, k),
                            "level {level} seam {index} row {r} +{k}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn scratch_reuse_never_leaks_a_previous_tile() {
        let busy = adversarial(5, 2, 2_000, 70);
        let refs: Vec<&NoteStore> = busy.iter().collect();
        let sparse = store(vec![rec(10, 2, 60, 3)]);
        let mut sc = Scratch::default();
        let opt = TileOptions::default();
        build_tile(&refs, &Ranked, 0, 3, opt, &mut sc, &|| false).unwrap();
        let t = build_tile(&[&sparse], &Subs, 0, 0, opt, &mut sc, &|| false).unwrap();
        let fresh = build_tile(
            &[&sparse],
            &Subs,
            0,
            0,
            opt,
            &mut Scratch::default(),
            &|| false,
        )
        .unwrap();
        assert_eq!(t.hash(), fresh.hash());
        assert_eq!(t.occupied, 2);
        assert_eq!(sc.winner(ROWS - 1 - 61, 10), None);
        // A cancelled build is cleared by the next one, too.
        let s = store(
            (0..200_000u64)
                .map(|i| rec(i % 256, 1, (i % 128) as u8, 0))
                .collect(),
        );
        assert!(build_tile(&[&s], &Subs, 0, 0, opt, &mut sc, &|| true).is_none());
        let t = build_tile(&[&sparse], &Subs, 0, 0, opt, &mut sc, &|| false).unwrap();
        assert_eq!(t.hash(), fresh.hash());
    }

    /// v0.9 V09-S11 step 4 (the Onion through clip placements): a source
    /// drawn through a placement (`shift`, start `window`) gives exactly the
    /// tile of a copy of its notes moved by the shift, keeping only the notes
    /// starting in the window — pixels, note count and winners, painter order
    /// and selection included — with negative and positive shifts, windows
    /// open and closed, plain, outlined and folded tiles, at two levels. The
    /// identity place is the plain build.
    #[test]
    fn placed_sources_draw_like_copies_moved_by_their_placements() {
        let stores = adversarial(0x0510_0004, 3, 2_000, 40);
        let refs: Vec<&NoteStore> = stores.iter().collect();
        let places = [
            SourcePlace::IDENTITY,
            SourcePlace {
                shift: 1_500,
                window: (500, 2_200),
            },
            SourcePlace {
                shift: -700,
                window: (800, u64::MAX),
            },
        ];
        // The oracle: copies of the placed notes at their song ticks.
        let copies: Vec<NoteStore> = stores
            .iter()
            .zip(places)
            .map(|(s, pl)| {
                let mut c = NoteStore::new();
                for n in s.iter() {
                    if let Some(t) = pl.start(n.start_tick) {
                        let r = NoteRecord {
                            start_tick: t,
                            ..n.record()
                        };
                        c.insert_sorted_batch(&[r], n.selected).unwrap();
                    }
                }
                c
            })
            .collect();
        let crefs: Vec<&NoteStore> = copies.iter().collect();
        assert!(crefs.iter().all(|c| !c.is_empty()));
        let outlined = TileOptions {
            outline: Some(Outline::new(200, 60)),
            ..TileOptions::default()
        };
        let folded = TileOptions {
            fold: Some(KeyFold::new(50, 69, 7)),
            ..TileOptions::default()
        };
        for opt in [TileOptions::default(), outlined, folded] {
            for (level, tiles) in [(0, 0..18), (-6, 0..2)] {
                for index in tiles {
                    let mut sa = Scratch::default();
                    let mut sb = Scratch::default();
                    let a = build_tile_placed(
                        &refs,
                        &places,
                        &Half,
                        level,
                        index,
                        opt,
                        &mut sa,
                        &|| false,
                    )
                    .unwrap();
                    let b =
                        build_tile(&crefs, &Half, level, index, opt, &mut sb, &|| false).unwrap();
                    let at = format!("{opt:?} level {level} tile {index}");
                    assert_eq!(a.notes, b.notes, "{at}");
                    assert_eq!(a.hash(), b.hash(), "{at}");
                    assert!(a.pixels == b.pixels, "{at}");
                    for row in 0..a.rows {
                        for col in 0..TILE_COLS {
                            assert_eq!(
                                sa.winner(row, col),
                                sb.winner(row, col),
                                "{at} {row} {col}"
                            );
                        }
                    }
                }
            }
        }
        // Identity places (or none) are the plain build.
        let plain = build_tile(
            &refs,
            &Half,
            0,
            3,
            TileOptions::default(),
            &mut Scratch::default(),
            &|| false,
        )
        .unwrap();
        let ident = build_tile_placed(
            &refs,
            &[SourcePlace::IDENTITY; 3],
            &Half,
            0,
            3,
            TileOptions::default(),
            &mut Scratch::default(),
            &|| false,
        )
        .unwrap();
        assert!(plain.pixels == ident.pixels && plain.notes == ident.notes);
        // A placement entirely outside the tile draws nothing of its source.
        let far = SourcePlace {
            shift: 1_000_000,
            window: (0, u64::MAX),
        };
        assert_eq!(far.content_range(0, 256), None);
        let t = build_tile_placed(
            &refs[..1],
            &[far],
            &Half,
            0,
            0,
            TileOptions::default(),
            &mut Scratch::default(),
            &|| false,
        )
        .unwrap();
        assert_eq!(t.notes, 0);
        assert_eq!(t.occupied, 0);
    }

    // ---- V09-S17 step 3: readings (the MIDI Player, docs/PLAYER.md P3) ----

    /// Colours that show what a reading changes: the key (key correction),
    /// the subchannel and the source; selection is not drawn (export notes
    /// are never selected), velocity sets the opacity (stacking).
    struct Played;
    impl TileColors for Played {
        fn color(&self, s: usize, n: &NoteView) -> ([u8; 4], bool) {
            let h = (s as u32)
                .wrapping_mul(0x9E37_79B1)
                .wrapping_add(n.sub.0.wrapping_mul(0x85EB_CA6B))
                .wrapping_add(u32::from(n.attrs.pitch()).wrapping_mul(0xC2B2_AE35));
            let c = h.to_le_bytes();
            let a = if n.attrs.velocity() > 64 { 255 } else { 150 };
            ([c[0], c[1], c[2], a], false)
        }
        fn rank(&self, s: usize) -> u16 {
            (s % 2) as u16
        }
    }

    struct Rnd(u64);
    impl Rnd {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n.max(1)
        }
        fn signed(&mut self, r: i64) -> i64 {
            self.below(2 * r as u64 + 1) as i64 - r
        }
    }

    /// A store of `n` notes (keys up to `top`): a few long and zero-length
    /// notes, muted notes, equal starts, some notes selected.
    fn random_store(r: &mut Rnd, n: usize, top: u8) -> NoteStore {
        let mut v: Vec<NoteRecord> = (0..n)
            .map(|k| {
                let start = if k > 0 && r.below(5) == 0 {
                    u64::MAX // marker: repeat the previous start
                } else {
                    r.below(20_000)
                };
                let dur = match r.below(40) {
                    0 => r.below(30_000),
                    1 => 0,
                    _ => 1 + r.below(400),
                };
                let attrs = NoteAttrs::new_wide(
                    r.below(u64::from(top) + 1) as u8,
                    1 + r.below(127) as u8,
                    r.below(16) as u8,
                    64,
                    NoteOffKind::NoteOff,
                )
                .unwrap()
                .with_muted(r.below(7) == 0);
                NoteRecord {
                    start_tick: start,
                    duration_ticks: dur,
                    attrs,
                    sub: SubchannelId(r.below(5) as u32),
                    group: 0,
                }
            })
            .collect();
        for k in 1..v.len() {
            if v[k].start_tick == u64::MAX {
                v[k].start_tick = v[k - 1].start_tick;
            }
        }
        if v.first().is_some_and(|f| f.start_tick == u64::MAX) {
            v[0].start_tick = 0;
        }
        v.sort_by_key(|x| x.start_tick);
        let mut s = NoteStore::from_sorted_records(&v, false).unwrap();
        let a = r.below(20_000);
        s.select_in_region(a, a + r.below(5_000), 0, top);
        s
    }

    fn random_correction(r: &mut Rnd, top: u8) -> Correction {
        use crate::model::track::GatePercent;
        let from_tick = match r.below(4) {
            0 => 0,
            1 => u64::MAX,
            _ => r.below(25_000),
        };
        let ticks = match r.below(4) {
            0 => 0,
            1 => -(r.below(30_000) as i64), // moves notes before tick 0
            _ => r.signed(6_000),
        };
        let keys = if r.below(3) == 0 {
            0
        } else {
            r.signed(40) as i16
        };
        let gate = [10_000, 2_500, 5_000, 15_000, 25_000, 100_000, 1][r.below(7) as usize];
        let range = if r.below(2) == 0 {
            (0, top)
        } else {
            let lo = r.below(u64::from(top)) as u8;
            (lo, lo + r.below(u64::from(top - lo) + 1) as u8)
        };
        Correction {
            from_tick,
            ticks,
            keys,
            gate: GatePercent::from_hundredths(gate).unwrap(),
            range,
            top,
        }
    }

    fn random_place(r: &mut Rnd) -> SourcePlace {
        if r.below(3) == 0 {
            return SourcePlace::IDENTITY;
        }
        let offset = r.below(5_000);
        let start = r.below(12_000);
        let len = r.below(18_000);
        SourcePlace {
            shift: i128::from(start) - i128::from(offset),
            window: (offset, offset + len),
        }
    }

    /// The source's notes as export composes them
    /// (`midi::export::export_notes_as_stored`, one lane), in a store.
    fn export_store(s: &NoteStore, pl: SourcePlace, read: Reading) -> NoteStore {
        use crate::model::EventStore;
        use crate::model::correction::CorrectedNotes;
        use crate::model::placed::{PlacedLane, PlacedNotes};
        let lane = [PlacedLane::new(
            s.snapshot(),
            EventStore::default(),
            pl.window.0,
            pl.window.1,
            pl.shift,
        )];
        let recs: Vec<NoteRecord> = match (read, pl.is_identity()) {
            (Reading::Stored, _) => unreachable!(),
            (Reading::Played(None), true) => s.iter_audible().map(|v| v.record()).collect(),
            (Reading::Played(None), false) => PlacedNotes::new(&lane)
                .filter(|r| !r.attrs.muted())
                .collect(),
            (Reading::Played(Some(c)), true) => CorrectedNotes::new(s.snapshot(), c).collect(),
            (Reading::Played(Some(c)), false) => CorrectedRuns::from_runs(
                PlacedNotes::range(&lane, 0, c.from_tick),
                PlacedNotes::range(&lane, c.from_tick, u64::MAX),
                c,
            )
            .collect(),
        };
        NoteStore::from_sorted_records(&recs, false).unwrap()
    }

    #[test]
    fn a_played_source_draws_exactly_the_tiles_of_its_export_notes() {
        let mut r = Rnd(0x5170_0003);
        let mut sc = Scratch::default();
        let mut checked = 0;
        let mut drawn = 0;
        let mut kinds = [0usize; 4];
        for case in 0..400 {
            let keys = if case % 5 == 4 {
                KeyRange::Extended256
            } else {
                KeyRange::Standard
            };
            let top = keys.max_pitch();
            let n_src = 1 + r.below(3) as usize;
            let stores: Vec<NoteStore> = (0..n_src)
                .map(|_| {
                    let n = r.below(1_500) as usize;
                    random_store(&mut r, n, top)
                })
                .collect();
            let places: Vec<SourcePlace> = (0..n_src).map(|_| random_place(&mut r)).collect();
            let reads: Vec<Reading> = (0..n_src)
                .map(|_| match r.below(5) {
                    0 => Reading::Stored,
                    1 => Reading::Played(None),
                    _ => Reading::Played(Some(random_correction(&mut r, top))),
                })
                .collect();
            for read in &reads {
                let kind = match read {
                    Reading::Stored => 0,
                    Reading::Played(None) => 1,
                    Reading::Played(Some(c)) if c.ticks < 0 => 3,
                    Reading::Played(Some(_)) => 2,
                };
                kinds[kind] += 1;
            }
            // The reference family: stored sources as they are, played ones
            // as stores of their export notes drawn where they are.
            let refs: Vec<NoteStore> = (0..n_src)
                .map(|k| match reads[k] {
                    Reading::Stored => stores[k].clone(),
                    read => export_store(&stores[k], places[k], read),
                })
                .collect();
            let ref_places: Vec<SourcePlace> = (0..n_src)
                .map(|k| match reads[k] {
                    Reading::Stored => places[k],
                    _ => SourcePlace::IDENTITY,
                })
                .collect();
            // v1.0-beta1 (B1-0141): a family with readings paints in one
            // order across its sources (start, source, stored order), so the
            // reference is read the same way — the export notes as stored.
            let ref_reads: Vec<Reading> = (0..n_src)
                .map(|k| match reads[k] {
                    Reading::Stored => Reading::Stored,
                    _ => Reading::Played(None),
                })
                .collect();
            let got_src: Vec<&NoteStore> = stores.iter().collect();
            let ref_src: Vec<&NoteStore> = refs.iter().collect();
            let outline = (r.below(2) == 0).then(|| Outline::new(255, 90));
            // Half the cases draw the selection (in front, white): a played
            // source never shows it, a stored one does.
            let colors: &dyn TileColors = if case % 2 == 0 { &Played } else { &Subs };
            let opt = TileOptions {
                outline,
                keys,
                ..TileOptions::default()
            };
            for _ in 0..4 {
                let level = r.below(30) as i32 - 28; // 2^-7 … 2^0.25 px per tick
                let (a, b) = tile_range(level, 0, 60_000);
                let index = a - 1 + r.below((b - a + 3) as u64) as i64;
                let got = build_tile_read(
                    &got_src,
                    &places,
                    &reads,
                    colors,
                    level,
                    index,
                    opt,
                    &mut sc,
                    &|| false,
                )
                .unwrap();
                let want = build_tile_read(
                    &ref_src,
                    &ref_places,
                    &ref_reads,
                    colors,
                    level,
                    index,
                    opt,
                    &mut sc,
                    &|| false,
                )
                .unwrap();
                assert_eq!(
                    got, want,
                    "case {case}, level {level}, tile {index}: {reads:?} {places:?}"
                );
                checked += 1;
                drawn += got.notes;
            }
        }
        assert!(kinds.iter().all(|&k| k > 50), "{kinds:?}");
        assert!(
            checked == 1_600 && drawn > 50_000,
            "{checked} tiles, {drawn} notes"
        );
    }

    /// v1.0-beta1 B1-0141 (PFA painter's order, PFAVIZ_PLAYER.md §2 and §6
    /// targets 1, 2 and 7): in the Player's tiles a later-starting note covers
    /// an earlier one whatever the source (track); at one start the later
    /// source, then the later stored note, is on top. Checked texel by texel
    /// against a brute-force painter of every note in (start, source, stored
    /// order), on random multi-source fixtures with long holds, zero-length
    /// notes and repeated starts.
    #[test]
    fn player_tiles_paint_later_starting_notes_over_earlier_ones_across_sources() {
        let mut r = Rnd(0x5170_0141);
        let mut sc = Scratch::default();
        let mut covered_by_older_source = 0usize;
        for case in 0..120 {
            let n_src = 2 + r.below(3) as usize;
            let stores: Vec<NoteStore> = (0..n_src)
                .map(|_| {
                    let n = r.below(600) as usize;
                    random_store(&mut r, n, 127)
                })
                .collect();
            let src: Vec<&NoteStore> = stores.iter().collect();
            let reads = vec![Reading::Played(None); n_src];
            let mut order: Vec<usize> = (0..n_src).collect();
            order.sort_by_key(|&si| (Played.rank(si), si));
            // Every audible note in PFA order: (start, source position, index).
            let mut all: Vec<(u64, usize, usize, NoteView)> = Vec::new();
            for (pos, &si) in order.iter().enumerate() {
                for (ni, v) in stores[si].iter().enumerate() {
                    if !v.attrs.muted() {
                        all.push((
                            v.start_tick,
                            pos,
                            ni,
                            NoteView {
                                selected: false,
                                ..v
                            },
                        ));
                    }
                }
            }
            all.sort_by_key(|n| (n.0, n.1, n.2));
            for _ in 0..3 {
                let level = r.below(24) as i32 - 22;
                let (a, b) = tile_range(level, 0, 25_000);
                let index = a + r.below((b - a + 1) as u64) as i64;
                let t = build_tile_read(
                    &src,
                    &[],
                    &reads,
                    &Played,
                    level,
                    index,
                    TileOptions::default(),
                    &mut sc,
                    &|| false,
                )
                .unwrap();
                let (ppt, t0) = (level_ppt(level), tile_start(level, index));
                let mut want: Vec<Option<(Winner, usize)>> = vec![None; ROWS * TILE_COLS];
                for &(_, pos, _, v) in &all {
                    let si = order[pos];
                    // Sub-pixel notes are not drawn (PFA §2, target 5).
                    if (v.duration_ticks as f64 * ppt + 0.5).floor() < 1.0 {
                        continue;
                    }
                    let Some((x0, x1)) = columns(v.start_tick, v.duration_ticks, t0, ppt) else {
                        continue;
                    };
                    let row = ROWS - 1 - usize::from(v.attrs.pitch());
                    let (c, _) = Played.color(si, &v);
                    for x in x0.max(APRON)..=x1.min(APRON + TILE_COLS - 1) {
                        let cell = &mut want[row * TILE_COLS + x - APRON];
                        if cell.is_some_and(|(_, p)| p > pos) {
                            covered_by_older_source += 1;
                        }
                        *cell = Some((
                            Winner {
                                selected: false,
                                source: si,
                                rgba: c,
                                sub: v.sub.0,
                                // Layout::Start keeps no velocity.
                                velocity: 127,
                            },
                            pos,
                        ));
                    }
                }
                for (i, w) in want.iter().enumerate() {
                    let (row, col) = (i / TILE_COLS, i % TILE_COLS);
                    let w = w.map(|w| w.0);
                    assert_eq!(
                        sc.winner(row, col),
                        w,
                        "case {case} level {level} tile {index} row {row} col {col}"
                    );
                    match w {
                        Some(w) => assert_eq!(t.pixels[i][..3], w.rgba[..3]),
                        None => assert_eq!(t.pixels[i][3], 0),
                    }
                }
            }
        }
        // The fixtures do exercise the cross-source rule (a later-starting
        // note of an earlier source covering a later source's note).
        assert!(covered_by_older_source > 1_000, "{covered_by_older_source}");
    }

    /// v1.0-beta1 B1-0111 (PFAVIZ_PLAYER.md §4, target 6): a key lights in
    /// the colour of the latest-starting note sounding on it — across
    /// sources, ties to the later source then the later stored note —
    /// upcoming and ended notes never light it, muted notes never sound;
    /// equal to a brute-force walk on random fixtures (placements and
    /// corrections included), the same at every call (paused frames).
    #[test]
    fn a_lit_key_shows_the_latest_starting_sounding_note() {
        // Hand case: key 60 held by an old long note of source 1, a newer
        // short note of source 0 (covers it), an upcoming one of source 1.
        let s0 = store(vec![rec(100, 50, 60, 0), rec(0, 10, 62, 0)]);
        let s1 = store(vec![
            rec(0, 1_000, 60, 1),
            rec(130, 10, 60, 1),
            rec(5, 5, 62, 1),
        ]);
        let src = [&s0, &s1];
        let reads = [Reading::Played(None); 2];
        let mut out = Vec::new();
        lit_keys(&src, &[], &reads, &[], 120, 128, &mut out);
        let (si, v) = out[60].unwrap();
        assert_eq!(
            (si, v.start_tick),
            (0, 100),
            "the newer note covers the held one"
        );
        assert!(out[62].is_none(), "ended notes do not light");
        lit_keys(&src, &[], &reads, &[], 99, 128, &mut out);
        assert_eq!(
            out[60].map(|(s, v)| (s, v.start_tick)),
            Some((1, 0)),
            "upcoming ones do not"
        );
        lit_keys(&src, &[], &reads, &[], 135, 128, &mut out);
        assert_eq!(out[60].map(|(s, v)| (s, v.start_tick)), Some((1, 130)));
        lit_keys(&src, &[], &reads, &[], 150, 128, &mut out);
        assert_eq!(
            out[60].map(|(s, v)| (s, v.start_tick)),
            Some((1, 0)),
            "ends are exclusive"
        );
        // Equal starts: the later source wins.
        let a = store(vec![rec(10, 100, 64, 0)]);
        let b = store(vec![rec(10, 5, 64, 2)]);
        lit_keys(&[&a, &b], &[], &reads, &[], 12, 128, &mut out);
        assert_eq!(out[64].unwrap().0, 1);
        lit_keys(&[&a, &b], &[], &reads, &[], 20, 128, &mut out);
        assert_eq!(out[64].unwrap().0, 0);

        let mut r = Rnd(0x5170_0111);
        for case in 0..150 {
            let n_src = 1 + r.below(4) as usize;
            // Odd cases: few keys, dense — lit keys stop early (B1-0145).
            let top = if case % 2 == 0 {
                127
            } else {
                2 + r.below(10) as u8
            };
            let stores: Vec<NoteStore> = (0..n_src)
                .map(|_| {
                    let n = r.below(800) as usize;
                    random_store(&mut r, n, top)
                })
                .collect();
            let places: Vec<SourcePlace> = (0..n_src).map(|_| random_place(&mut r)).collect();
            let reads: Vec<Reading> = (0..n_src)
                .map(|_| match r.below(3) {
                    0 => Reading::Played(None),
                    _ => Reading::Played(Some(random_correction(&mut r, 127))),
                })
                .collect();
            let src: Vec<&NoteStore> = stores.iter().collect();
            // The notes as played, in song ticks and painter order per source.
            let played: Vec<NoteStore> = (0..n_src)
                .map(|k| export_store(&stores[k], places[k], reads[k]))
                .collect();
            let exact: Vec<[u64; 4]> = (0..n_src).map(|k| lit_mask(&stores[k], reads[k])).collect();
            let wide: Vec<[u64; 4]> = exact
                .iter()
                .map(|m| [m[0] | 0x00F0_0000_0000_000F, m[1], !0, m[3]])
                .collect();
            for t in 0..6 {
                let tick = r.below(25_000);
                let masks: &[[u64; 4]] = match t % 3 {
                    0 => &exact,
                    1 => &wide,
                    _ => &[],
                };
                lit_keys(&src, &places, &reads, masks, tick, 128, &mut out);
                let mut want: Vec<Option<(u64, usize, usize, u32)>> = vec![None; 128];
                for (si, p) in played.iter().enumerate() {
                    for (ni, v) in p.iter().enumerate() {
                        let k = usize::from(v.attrs.pitch());
                        if v.start_tick <= tick && v.end_tick() > tick && k < 128 {
                            let key = (v.start_tick, si, ni, v.attrs.raw());
                            if want[k].is_none_or(|w| (w.0, w.1, w.2) < (key.0, key.1, key.2)) {
                                want[k] = Some(key);
                            }
                        }
                    }
                }
                for k in 0..128 {
                    let got = out[k].map(|(si, v)| (v.start_tick, si, v.attrs.raw()));
                    let exp = want[k].map(|w| (w.0, w.1, w.3));
                    assert_eq!(got, exp, "case {case} tick {tick} key {k}");
                }
                let again = out.clone();
                lit_keys(&src, &places, &reads, &[], tick, 128, &mut out);
                assert_eq!(out, again, "deterministic, with or without masks");
            }
        }
    }

    #[test]
    fn a_family_without_readings_builds_the_tiles_it_built_before() {
        // Reading::Stored everywhere takes the editor's path; equal to it.
        let mut r = Rnd(0x5170_0004);
        let mut sc = Scratch::default();
        for _ in 0..40 {
            let stores: Vec<NoteStore> = (0..3).map(|_| random_store(&mut r, 300, 127)).collect();
            let src: Vec<&NoteStore> = stores.iter().collect();
            let places: Vec<SourcePlace> = (0..3).map(|_| random_place(&mut r)).collect();
            let level = r.signed(10) as i32 - 4;
            let (a, b) = tile_range(level, 0, 40_000);
            let index = a + r.below((b - a + 1) as u64) as i64;
            let opt = TileOptions {
                outline: Some(Outline::new(255, 90)),
                ..TileOptions::default()
            };
            let plain =
                build_tile_placed(&src, &places, &Subs, level, index, opt, &mut sc, &|| false);
            let stored = build_tile_read(
                &src,
                &places,
                &[Reading::Stored; 3],
                &Subs,
                level,
                index,
                opt,
                &mut sc,
                &|| false,
            );
            assert_eq!(plain, stored);
        }
    }

    #[test]
    fn a_corrected_build_stops_when_cancelled() {
        // Long enough to be drawn at level −40 (sub-pixel notes are culled).
        let v: Vec<NoteRecord> = (0..140_000u64).map(|k| rec(k / 4, 2_048, 60, 0)).collect();
        let s = store(v);
        let c = Correction {
            from_tick: 0,
            ticks: 7,
            keys: 1,
            gate: crate::model::track::GatePercent::IDENTITY,
            range: (0, 127),
            top: 127,
        };
        let mut sc = Scratch::default();
        let read = [Reading::Played(Some(c))];
        let build = |sc: &mut Scratch, cancel: bool| {
            build_tile_read(
                &[&s],
                &[],
                &read,
                &Played,
                -40,
                0,
                TileOptions::default(),
                sc,
                &move || cancel,
            )
        };
        assert!(build(&mut sc, true).is_none());
        let t = build(&mut sc, false).unwrap();
        assert_eq!(t.notes, 140_000);
        // Every note moved up one key and 7 ticks on.
        assert!(
            t.pixels[(ROWS - 1 - 61) * TILE_COLS]
                .iter()
                .any(|&b| b != 0)
        );
        assert_eq!(t.pixels[(ROWS - 1 - 60) * TILE_COLS][3], 0);
    }
}
