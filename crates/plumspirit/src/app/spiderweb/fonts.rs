//! Fonts for Spiderweb's Text tool (PlumSpirit expansion, contract §033):
//! the installed families (read in the background from the font files' own
//! name tables), and a face's glyph outlines through skrifa (unhinted, in em
//! units, quadratics raised to cubics) as the engine's `FontSource`.
//! Spiderweb reads outlines through Windows GDI; text typed in PlumSpirit
//! therefore matches Spiderweb's only approximately, but saved text shapes
//! make exactly the same notes (they keep their outlines).
//!
//! Bold / italic: the family's own bold / italic face when it has one, else
//! made from the regular face (outlines widened by the weight; slanted 12°).

use crate::ui::fonts::{self as uifonts, InstalledFace};
use plumspirit_engine::spiderweb::paths::P;
use plumspirit_engine::spiderweb::text::{self, FontSource};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{
    DrawSettings, Hinting, OutlineGlyphCollection, OutlineGlyphFormat, OutlinePen,
};
use skrifa::raw::TableProvider;
use skrifa::raw::types::GlyphId;
use skrifa::{FontRef, MetadataProvider};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

/// Slant of a made-up italic (tan 12°).
const SLANT: f64 = 0.212_556_56;
/// A glyph: advance and closed cubic contours (em units).
type Glyph = (f64, Vec<Vec<P>>);
/// Glyphs kept per loaded font.
const GLYPH_CACHE: usize = 4096;

/// One face of a family.
#[derive(Clone, Debug)]
pub struct Face {
    pub path: PathBuf,
    pub index: u32,
    pub style: String,
    pub weight: u16,
    pub italic: bool,
}

/// Style name → (weight, italic).
pub fn style_of(style: &str) -> (u16, bool) {
    let s = style.to_ascii_lowercase();
    let italic = s.contains("italic") || s.contains("oblique");
    let weight = [
        ("hairline", 100),
        ("thin", 100),
        ("extralight", 200),
        ("extra light", 200),
        ("ultralight", 200),
        ("light", 300),
        ("semibold", 600),
        ("semi bold", 600),
        ("demibold", 600),
        ("demi bold", 600),
        ("extrabold", 800),
        ("extra bold", 800),
        ("ultrabold", 800),
        ("black", 900),
        ("heavy", 900),
        ("bold", 700),
        ("medium", 500),
    ]
    .iter()
    .find(|(w, _)| s.contains(w))
    .map_or(400, |(_, v)| *v);
    (weight, italic)
}

/// The installed fonts by family.
#[derive(Default)]
pub struct Library {
    pub families: BTreeMap<String, Vec<Face>>,
}

impl Library {
    pub fn from_faces(faces: Vec<InstalledFace>) -> Library {
        let mut families: BTreeMap<String, Vec<Face>> = BTreeMap::new();
        for f in faces {
            let fam = f.names.effective_family().trim().to_owned();
            if fam.is_empty() || fam.starts_with('.') {
                continue;
            }
            let style = f.names.effective_style().to_owned();
            let (weight, italic) = style_of(&style);
            families.entry(fam).or_default().push(Face {
                path: f.path,
                index: f.names.index,
                style,
                weight,
                italic,
            });
        }
        for v in families.values_mut() {
            v.sort_by(|a, b| (a.italic, a.weight, &a.style).cmp(&(b.italic, b.weight, &b.style)));
            v.dedup_by(|a, b| a.style == b.style);
        }
        Library { families }
    }

    /// The family's name as listed (case-insensitive match).
    pub fn find(&self, family: &str) -> Option<&str> {
        let f = family.to_lowercase();
        self.families
            .keys()
            .find(|k| k.to_lowercase() == f)
            .map(String::as_str)
    }

    /// The family to start with: Arial (Spiderweb's default), the UI font's
    /// family, any sans, or the first one.
    pub fn default_family(&self, ui_family: &str) -> Option<String> {
        [
            "Arial",
            "Helvetica",
            ui_family,
            "Liberation Sans",
            "Arimo",
            "DejaVu Sans",
        ]
        .iter()
        .find_map(|f| self.find(f))
        .or_else(|| self.families.keys().next().map(String::as_str))
        .map(str::to_owned)
    }

    /// The face nearest the wanted weight and slant: (face, made-up bold
    /// amount in em, made-up italic).
    pub fn pick(&self, family: &str, weight: u16, italic: bool) -> Option<(Face, f64, bool)> {
        let faces = self.families.get(self.find(family)?)?;
        let score = |f: &Face| {
            let slant = if f.italic == italic { 0 } else { 1000 };
            // lighter faces first when too heavy: a face can be made bolder,
            // not lighter
            let w = i32::from(f.weight) - i32::from(weight);
            slant + if w > 0 { w * 2 } else { -w }
        };
        let best = faces.iter().min_by_key(|f| score(f))?.clone();
        let fake_bold = if weight >= best.weight + 150 {
            f64::from(weight - best.weight) / 300.0 * 0.03
        } else {
            0.0
        };
        let fake_italic = italic && !best.italic;
        Some((best, fake_bold, fake_italic))
    }
}

/// Scans the installed fonts on a worker (first use of the Text tool).
#[derive(Default)]
pub struct Scan {
    rx: Option<Receiver<Library>>,
    pub library: Option<Arc<Library>>,
}

impl Scan {
    pub fn start(&mut self) {
        if self.rx.is_some() || self.library.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let ok = std::thread::Builder::new()
            .name("plumspirit-fonts".into())
            .spawn(move || {
                let _ = tx.send(Library::from_faces(uifonts::installed_faces()));
            })
            .is_ok();
        if ok {
            self.rx = Some(rx);
        } else {
            self.library = Some(Arc::new(Library::default()));
        }
    }
    pub fn poll(&mut self) -> bool {
        if let Some(rx) = &self.rx
            && let Ok(lib) = rx.try_recv()
        {
            self.library = Some(Arc::new(lib));
            self.rx = None;
            return true;
        }
        false
    }
    pub fn busy(&self) -> bool {
        self.rx.is_some()
    }
}

/// A pen collecting closed cubic contours in em units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutlineError {
    Unsupported,
    Points,
    Contours,
    Stopped,
    Invalid,
    Draw,
}
struct Control<'a> {
    max_points: usize,
    max_contours: usize,
    points: usize,
    stopped: Option<OutlineError>,
    check: &'a mut dyn FnMut() -> bool,
}
struct Pen<'a> {
    upem: f64,
    slant: f64,
    contours: Vec<Vec<P>>,
    cur: Vec<P>,
    control: Option<Control<'a>>,
}

impl Pen<'_> {
    fn append(&mut self, points: &[P]) {
        if let Some(control) = &mut self.control {
            if control.stopped.is_some() {
                return;
            }
            if !(control.check)() {
                control.stopped = Some(OutlineError::Stopped);
                return;
            }
            if control.points + points.len() > control.max_points {
                control.stopped = Some(OutlineError::Points);
                return;
            }
            if points
                .iter()
                .flatten()
                .any(|v| !v.is_finite() || v.abs() > 1e12)
            {
                control.stopped = Some(OutlineError::Invalid);
                return;
            }
            control.points += points.len();
        }
        self.cur.extend_from_slice(points);
    }
    fn p(&self, x: f32, y: f32) -> P {
        let (x, y) = (f64::from(x) / self.upem, f64::from(y) / self.upem);
        [x + y * self.slant, y]
    }
    fn last(&self) -> P {
        *self.cur.last().unwrap_or(&[0.0, 0.0])
    }
    fn line(&mut self, b: P) {
        let a = self.last();
        self.append(&[
            [a[0] + (b[0] - a[0]) / 3.0, a[1] + (b[1] - a[1]) / 3.0],
            [
                a[0] + 2.0 * (b[0] - a[0]) / 3.0,
                a[1] + 2.0 * (b[1] - a[1]) / 3.0,
            ],
            b,
        ]);
    }
    fn finish(&mut self) {
        if self.control.as_ref().is_some_and(|c| c.stopped.is_some()) {
            self.cur.clear();
            return;
        }
        if self.cur.len() >= 2 {
            let start = self.cur[0];
            if self.last() != start {
                self.line(start);
            }
        }
        if self.control.as_ref().is_some_and(|c| c.stopped.is_some()) {
            self.cur.clear();
            return;
        }
        if self.cur.len() >= 4 {
            if let Some(control) = &mut self.control
                && self.contours.len() >= control.max_contours
            {
                control.stopped = Some(OutlineError::Contours);
                self.cur.clear();
                return;
            }
            self.contours.push(std::mem::take(&mut self.cur));
        } else {
            self.cur.clear();
        }
    }
}

impl OutlinePen for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.finish();
        let p = self.p(x, y);
        self.append(&[p]);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.p(x, y);
        self.line(p);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let a = self.last();
        let c = self.p(cx, cy);
        let b = self.p(x, y);
        self.append(&[
            [
                a[0] + 2.0 / 3.0 * (c[0] - a[0]),
                a[1] + 2.0 / 3.0 * (c[1] - a[1]),
            ],
            [
                b[0] + 2.0 / 3.0 * (c[0] - b[0]),
                b[1] + 2.0 / 3.0 * (c[1] - b[1]),
            ],
            b,
        ]);
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (c0, c1, b) = (self.p(cx0, cy0), self.p(cx1, cy1), self.p(x, y));
        self.append(&[c0, c1, b]);
    }
    fn close(&mut self) {
        self.finish();
    }
}

pub(crate) fn outline_format(font: &FontRef<'_>) -> &'static str {
    if font.varc().is_ok() {
        "varc"
    } else if font.glyf().is_ok() {
        "glyf"
    } else if font.cff().is_ok() {
        "cff"
    } else if font.cff2().is_ok() {
        "cff2"
    } else {
        "none"
    }
}
fn bounded_glyphs<'a>(font: &FontRef<'a>) -> Result<OutlineGlyphCollection<'a>, OutlineError> {
    // Unhinted glyf decoding has the library's64Kpoint/recursion bounds. CFF's
    // subroutine nesting limit alone is not a total operation budget.
    if outline_format(font) != "glyf" {
        return Err(OutlineError::Unsupported);
    }
    OutlineGlyphCollection::with_format(font, OutlineGlyphFormat::Glyf)
        .ok_or(OutlineError::Unsupported)
}
pub(crate) fn outline_memory(font: &FontRef<'_>, glyph: u32) -> Result<usize, OutlineError> {
    Ok(bounded_glyphs(font)?
        .get(GlyphId::new(glyph))
        .map_or(0, |g| g.draw_memory_size(Hinting::None)))
}
pub(crate) fn bounded_outline(
    font: &FontRef<'_>,
    glyph: u32,
    max_points: usize,
    max_contours: usize,
    memory: &mut [u8],
    check: &mut dyn FnMut() -> bool,
) -> Result<Vec<Vec<P>>, OutlineError> {
    if !check() {
        return Err(OutlineError::Stopped);
    }
    let glyphs = bounded_glyphs(font)?;
    let Some(glyph) = glyphs.get(GlyphId::new(glyph)) else {
        return Ok(Vec::new());
    };
    let metrics = font.metrics(Size::unscaled(), LocationRef::default());
    let mut pen = Pen {
        upem: f64::from(metrics.units_per_em.max(1)),
        slant: 0.,
        contours: Vec::new(),
        cur: Vec::new(),
        control: Some(Control {
            max_points,
            max_contours,
            points: 0,
            stopped: None,
            check,
        }),
    };
    let result = glyph.draw(
        DrawSettings::unhinted(Size::unscaled(), LocationRef::default()).with_memory(Some(memory)),
        &mut pen,
    );
    pen.finish();
    if let Some(error) = pen.control.as_ref().and_then(|c| c.stopped) {
        return Err(error);
    }
    result.map_err(|_| OutlineError::Draw)?;
    if pen
        .contours
        .iter()
        .any(|c| c.len() < 4 || !(c.len() - 1).is_multiple_of(3) || c.first() != c.last())
    {
        return Err(OutlineError::Invalid);
    }
    Ok(pen.contours)
}

/// A loaded face as the engine's font source.
pub struct LoadedFont {
    data: Arc<Vec<u8>>,
    index: u32,
    upem: f64,
    slant: f64,
    bold: f64,
    line_height: f64,
    cap: f64,
    underline: (f64, f64),
    strike: (f64, f64),
    cache: RefCell<HashMap<char, Glyph>>,
}

impl LoadedFont {
    /// Reads a face (None: not a usable outline font).
    pub fn load(
        data: Arc<Vec<u8>>,
        index: u32,
        fake_bold: f64,
        fake_italic: bool,
    ) -> Option<LoadedFont> {
        let font = FontRef::from_index(&data, index).ok()?;
        let m = font.metrics(Size::unscaled(), LocationRef::default());
        let upem = f64::from(m.units_per_em.max(1));
        let em = |v: f32| f64::from(v) / upem;
        let line_height = em(m.ascent - m.descent + m.leading).max(0.5);
        let mut lf = LoadedFont {
            data: data.clone(),
            index,
            upem,
            slant: if fake_italic { SLANT } else { 0.0 },
            bold: fake_bold.clamp(0.0, 0.1),
            line_height,
            cap: m.cap_height.map(em).filter(|c| *c > 0.05).unwrap_or(0.7),
            underline: m
                .underline
                .map_or((-0.1, 0.05), |d| (em(d.offset), em(d.thickness).max(0.01))),
            strike: m
                .strikeout
                .map_or((0.3, 0.05), |d| (em(d.offset), em(d.thickness).max(0.01))),
            cache: RefCell::new(HashMap::new()),
        };
        // cap height from the "H" when the font does not say (as Spiderweb)
        if m.cap_height.is_none() {
            let (_, h) = lf.glyph_raw('H');
            let top = h
                .iter()
                .flatten()
                .map(|p| p[1])
                .fold(f64::NEG_INFINITY, f64::max);
            if top.is_finite() && top > 0.05 {
                lf.cap = top;
            }
        }
        Some(lf)
    }

    /// Loads the wanted family / weight / slant from the library.
    pub fn from_library(
        lib: &Library,
        family: &str,
        weight: u16,
        italic: bool,
    ) -> Option<LoadedFont> {
        let (face, bold, slant) = lib.pick(family, weight, italic)?;
        let data = std::fs::read(&face.path).ok()?;
        LoadedFont::load(Arc::new(data), face.index, bold, slant)
    }

    /// egui's built-in font (when no font is installed).
    pub fn built_in() -> Option<LoadedFont> {
        let defs = egui::FontDefinitions::default();
        let data = defs
            .font_data
            .get("Ubuntu-Light")
            .or_else(|| defs.font_data.values().next())?;
        LoadedFont::load(Arc::new(data.font.to_vec()), data.index, 0.0, false)
    }

    fn font(&self) -> Option<FontRef<'_>> {
        FontRef::from_index(&self.data, self.index).ok()
    }

    fn glyph_raw(&self, ch: char) -> (f64, Vec<Vec<P>>) {
        let Some(font) = self.font() else {
            return (0.5, Vec::new());
        };
        let gid = font
            .charmap()
            .map(ch)
            .or_else(|| font.charmap().map('?'))
            .unwrap_or_default();
        let advance = font
            .glyph_metrics(Size::unscaled(), LocationRef::default())
            .advance_width(gid)
            .map_or(0.5, |a| f64::from(a) / self.upem);
        let mut pen = Pen {
            upem: self.upem,
            slant: self.slant,
            contours: Vec::new(),
            cur: Vec::new(),
            control: None,
        };
        if let Some(g) = font.outline_glyphs().get(gid) {
            let _ = g.draw(
                DrawSettings::unhinted(Size::unscaled(), LocationRef::default()),
                &mut pen,
            );
        }
        pen.finish();
        (advance, pen.contours)
    }

    /// Outlines widened by `d` em (holes narrowed), as straight pieces.
    fn embolden(contours: Vec<Vec<P>>, d: f64) -> Vec<Vec<P>> {
        let numbered: Vec<(usize, Vec<P>)> = contours.into_iter().map(|c| (0, c)).collect();
        let holes = text::find_holes(&numbered);
        numbered
            .into_iter()
            .enumerate()
            .map(|(i, (_, c))| {
                let flat = text::flatten(&c, 0.002);
                let poly = text::offset(&flat, if holes.contains(&i) { -d } else { d });
                let mut out = vec![poly[0]];
                for w in poly.windows(2) {
                    let (a, b) = (w[0], w[1]);
                    out.push([a[0] + (b[0] - a[0]) / 3.0, a[1] + (b[1] - a[1]) / 3.0]);
                    out.push([
                        a[0] + 2.0 * (b[0] - a[0]) / 3.0,
                        a[1] + 2.0 * (b[1] - a[1]) / 3.0,
                    ]);
                    out.push(b);
                }
                out
            })
            .filter(|c| c.len() >= 4)
            .collect()
    }

    fn kern_pair(&self, a: char, b: char) -> f64 {
        let Some(font) = self.font() else {
            return 0.0;
        };
        let (Some(ga), Some(gb)) = (font.charmap().map(a), font.charmap().map(b)) else {
            return 0.0;
        };
        let Ok(kern) = font.kern() else {
            return 0.0;
        };
        for st in kern.subtables().flatten() {
            if let Ok(skrifa::raw::tables::kern::SubtableKind::Format0(f0)) = st.kind()
                && let Some(v) = f0.kerning(ga, gb)
            {
                return f64::from(v) / self.upem;
            }
        }
        0.0
    }
}

impl FontSource for LoadedFont {
    fn glyph(&self, ch: char) -> (f64, Vec<Vec<P>>) {
        if let Some(g) = self.cache.borrow().get(&ch) {
            return g.clone();
        }
        let (adv, mut c) = self.glyph_raw(ch);
        if self.bold > 0.0 && !c.is_empty() {
            c = LoadedFont::embolden(c, self.bold);
        }
        let g = (adv + 2.0 * self.bold, c);
        let mut cache = self.cache.borrow_mut();
        if cache.len() >= GLYPH_CACHE {
            cache.clear();
        }
        cache.insert(ch, g.clone());
        g
    }
    fn kerning(&self, a: char, b: char) -> f64 {
        self.kern_pair(a, b)
    }
    fn line_height(&self) -> f64 {
        self.line_height
    }
    fn cap(&self) -> f64 {
        self.cap
    }
    fn underline(&self) -> (f64, f64) {
        self.underline
    }
    fn strikeout(&self) -> (f64, f64) {
        self.strike
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_glyf_conversion_matches_the_existing_native_source() {
        let defs = egui::FontDefinitions::default();
        let data = Arc::new(defs.font_data["Ubuntu-Light"].font.to_vec());
        let legacy = LoadedFont::load(Arc::clone(&data), 0, 0., false).unwrap();
        let face = FontRef::from_index(&data, 0).unwrap();
        for ch in ['O', 'A', 'g', 'é', ' '] {
            let id = face.charmap().map(ch).unwrap().to_u32();
            let size = outline_memory(&face, id).unwrap();
            let mut memory = vec![0; size];
            let actual = bounded_outline(&face, id, 4096, 64, &mut memory, &mut || true).unwrap();
            let expected = legacy.glyph(ch).1;
            assert_eq!(actual.len(), expected.len());
            for (a, b) in actual.iter().zip(expected.iter()) {
                assert_eq!(a.len(), b.len());
                for (a, b) in a.iter().zip(b.iter()) {
                    assert_eq!(a.map(f64::to_bits), b.map(f64::to_bits));
                }
            }
        }
        let id = face.charmap().map('O').unwrap().to_u32();
        assert!(outline_memory(&face, id).unwrap() > 0);
        assert_eq!(
            bounded_outline(&face, id, 4096, 64, &mut [], &mut || true),
            Err(OutlineError::Draw)
        );
    }

    #[test]
    fn bounded_outline_collector_refuses_nonfinite_and_excess_points() {
        let mut check = || true;
        let mut pen = Pen {
            upem: 1.,
            slant: 0.,
            contours: Vec::new(),
            cur: Vec::new(),
            control: Some(Control {
                max_points: 3,
                max_contours: 1,
                points: 0,
                stopped: None,
                check: &mut check,
            }),
        };
        pen.move_to(0., 0.);
        pen.line_to(1., 1.);
        pen.finish();
        assert_eq!(
            pen.control.as_ref().unwrap().stopped,
            Some(OutlineError::Points)
        );
        assert!(pen.contours.is_empty());
        let mut pen = Pen {
            upem: 1.,
            slant: 0.,
            contours: Vec::new(),
            cur: Vec::new(),
            control: Some(Control {
                max_points: 10,
                max_contours: 1,
                points: 0,
                stopped: None,
                check: &mut check,
            }),
        };
        pen.move_to(f32::NAN, 0.);
        assert_eq!(
            pen.control.as_ref().unwrap().stopped,
            Some(OutlineError::Invalid)
        );
    }

    /// Explicit refactoring fixture: never runs in ordinary App tests.
    #[test]
    #[ignore]
    fn native_font_digest_dump() {
        use std::io::Write;
        let path = std::env::var("PLUMSPIRIT_NATIVE_FONT_DIGEST_OUT").expect("digest output path");
        let defs = egui::FontDefinitions::default();
        let data = Arc::new(defs.font_data["Ubuntu-Light"].font.to_vec());
        let mut out = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
        for (bold, italic) in [(0., false), (0.03, false), (0., true), (0.03, true)] {
            let font = LoadedFont::load(Arc::clone(&data), 0, bold, italic).unwrap();
            for ch in ['O', 'A', 'W', 'g', 'é', 'Σ', '♯', 'Ж', '♪', '😀', ' '] {
                let (advance, contours) = font.glyph(ch);
                let mut hash = 0xcbf29ce484222325u64;
                let mut add = |value: u64| {
                    hash ^= value;
                    hash = hash.wrapping_mul(0x100000001b3);
                };
                add(advance.to_bits());
                for contour in &contours {
                    add(contour.len() as u64);
                    for p in contour {
                        add(p[0].to_bits());
                        add(p[1].to_bits());
                    }
                }
                writeln!(
                    out,
                    "{bold} {italic} {ch:?} {} {:016x}",
                    contours.len(),
                    hash
                )
                .unwrap();
            }
        }
    }

    #[test]
    fn style_names_give_weight_and_slant() {
        assert_eq!(style_of("Regular"), (400, false));
        assert_eq!(style_of("Bold Italic"), (700, true));
        assert_eq!(style_of("SemiBold"), (600, false));
        assert_eq!(style_of("Light Oblique"), (300, true));
        assert_eq!(style_of("Black"), (900, false));
    }

    #[test]
    fn the_built_in_font_gives_closed_cubic_outlines_and_metrics() {
        let f = LoadedFont::built_in().expect("egui's font");
        let (adv, c) = f.glyph('O');
        assert!(adv > 0.3 && adv < 1.5, "{adv}");
        assert_eq!(c.len(), 2, "O has an outside and a hole");
        for k in &c {
            assert_eq!((k.len() - 1) % 3, 0);
            assert_eq!(k[0], *k.last().unwrap());
        }
        assert!(f.cap() > 0.5 && f.cap() < 0.9, "{}", f.cap());
        assert!(f.line_height() > 0.9);
        assert!(f.glyph(' ').1.is_empty());
    }

    #[test]
    fn made_up_bold_and_italic_change_the_outline() {
        let data = {
            let defs = egui::FontDefinitions::default();
            let d = defs.font_data.get("Ubuntu-Light").unwrap();
            Arc::new(d.font.to_vec())
        };
        let plain = LoadedFont::load(data.clone(), 0, 0.0, false).unwrap();
        let bold = LoadedFont::load(data.clone(), 0, 0.03, false).unwrap();
        let slant = LoadedFont::load(data, 0, 0.0, true).unwrap();
        let width = |f: &LoadedFont| {
            let pts: Vec<P> = f.glyph('l').1.concat();
            let lo = pts.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
            let hi = pts.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max);
            hi - lo
        };
        assert!(width(&bold) > width(&plain) + 0.04);
        // slanted: the highest point moves right by tan 12° × its height
        let top = |f: &LoadedFont| {
            f.glyph('l')
                .1
                .concat()
                .into_iter()
                .fold(
                    [0.0, f64::NEG_INFINITY],
                    |a: P, p: P| if p[1] > a[1] { p } else { a },
                )
        };
        let (tp, ts) = (top(&plain), top(&slant));
        assert!(
            (ts[0] - tp[0] - SLANT * tp[1]).abs() < 1e-9,
            "{tp:?} {ts:?}"
        );
    }

    #[test]
    fn the_library_picks_the_nearest_face_and_makes_up_the_rest() {
        let face = |style: &str| InstalledFace {
            names: uifonts::FaceNames {
                index: 0,
                family: "Test Sans".into(),
                subfamily: style.into(),
                typographic_family: None,
                typographic_subfamily: None,
            },
            path: PathBuf::from(format!("/fonts/{style}.ttf")),
        };
        let lib = Library::from_faces(vec![face("Regular"), face("Italic")]);
        assert_eq!(lib.find("test sans"), Some("Test Sans"));
        let (f, bold, slant) = lib.pick("Test Sans", 700, true).unwrap();
        assert_eq!(f.style, "Italic");
        assert!(bold > 0.0);
        assert!(!slant);
        let (f, bold, slant) = lib.pick("Test Sans", 400, false).unwrap();
        assert_eq!((f.style.as_str(), bold, slant), ("Regular", 0.0, false));
        assert_eq!(lib.default_family("Nope").as_deref(), Some("Test Sans"));
    }
}
