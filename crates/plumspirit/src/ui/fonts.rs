//! Helvetica-first UI typography.
//!
//! Helvetica is a proprietary typeface and is **never bundled**. At startup PlumSpirit
//! looks for an installed Helvetica family (Windows font registry, the system and
//! per-user font folders, and `PLUMSPIRIT_FONT_DIRS` for testing), verifies every
//! candidate by reading the font's own `name` table, and loads the best match.
//! If none is installed it falls back to a conservative sans-serif (Arial on Windows,
//! Liberation Sans / Arimo / DejaVu Sans elsewhere) and finally to egui's built-in
//! font. The choice is logged; the app never claims Helvetica when a fallback is in
//! use.

use std::path::{Path, PathBuf};
#[path = "fonts_bounded.rs"]
pub(crate) mod bounded;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiFontKind {
    Helvetica,
    Fallback,
    BuiltIn,
}

#[derive(Clone, Debug)]
pub struct UiFont {
    pub kind: UiFontKind,
    /// Family name as stored in the font (or a description for the built-in font).
    pub family: String,
    pub style: String,
    pub path: Option<PathBuf>,
    pub bytes: Option<Vec<u8>>,
    pub face_index: u32,
}

impl UiFont {
    pub fn describe(&self) -> String {
        match (&self.kind, &self.path) {
            (UiFontKind::BuiltIn, _) => {
                "built-in egui font (no Helvetica or fallback found)".into()
            }
            (k, Some(p)) => format!(
                "{} {} ({}) from {}",
                self.family,
                self.style,
                if *k == UiFontKind::Helvetica {
                    "Helvetica"
                } else {
                    "fallback"
                },
                p.display()
            ),
            (_, None) => self.family.clone(),
        }
    }
}

// ------------------------------------------------------------ name table parsing

/// Names of one face inside a font file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FaceNames {
    pub index: u32,
    pub family: String,
    pub subfamily: String,
    pub typographic_family: Option<String>,
    pub typographic_subfamily: Option<String>,
}

impl FaceNames {
    pub fn effective_family(&self) -> &str {
        self.typographic_family.as_deref().unwrap_or(&self.family)
    }
    pub fn effective_style(&self) -> &str {
        self.typographic_subfamily
            .as_deref()
            .unwrap_or(&self.subfamily)
    }
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *b.get(at)?,
        *b.get(at + 1)?,
        *b.get(at + 2)?,
        *b.get(at + 3)?,
    ]))
}

fn decode_utf16be(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Where the `name` table of the face whose table directory starts at `dir`
/// is: (offset, length).
fn name_table_at(data: &[u8], dir: usize) -> Option<(usize, usize)> {
    let num_tables = usize::from(be16(data, dir + 4)?);
    for i in 0..num_tables.min(512) {
        let rec = dir + 12 + i * 16;
        if data.get(rec..rec + 4)? == b"name" {
            return Some((
                be32(data, rec + 8)? as usize,
                be32(data, rec + 12)? as usize,
            ));
        }
    }
    None
}

/// Parses the `name` table of the face whose table directory starts at `dir`.
fn parse_face(data: &[u8], dir: usize, index: u32) -> Option<FaceNames> {
    let (off, len) = name_table_at(data, dir)?;
    let table = data.get(off..off.checked_add(len)?)?;
    parse_names(table, index)
}

/// The names in a `name` table.
fn parse_names(table: &[u8], index: u32) -> Option<FaceNames> {
    parse_names_checked(table, index, usize::MAX, &mut |_| true)
}
fn parse_names_checked(
    table: &[u8],
    index: u32,
    max_name: usize,
    check: &mut impl FnMut(usize) -> bool,
) -> Option<FaceNames> {
    let count = usize::from(be16(table, 2)?);
    let strings = usize::from(be16(table, 4)?);
    // Candidates per name id: (priority, text); lower priority value wins.
    let mut best: [Option<(u8, String)>; 4] = Default::default();
    for i in 0..count.min(4096) {
        if i.is_multiple_of(64) && !check(0) {
            return None;
        }
        let r = 6 + i * 12;
        let platform = be16(table, r)?;
        let encoding = be16(table, r + 2)?;
        let language = be16(table, r + 4)?;
        let name_id = be16(table, r + 6)?;
        let length = usize::from(be16(table, r + 8)?);
        let offset = usize::from(be16(table, r + 10)?);
        let slot = match name_id {
            1 => 0,
            2 => 1,
            16 => 2,
            17 => 3,
            _ => continue,
        };
        let Some(raw) = table.get(strings + offset..strings + offset + length) else {
            continue;
        };
        if !check(raw.len()) {
            return None;
        }
        if raw.len() > max_name {
            continue;
        }
        let (priority, text) = match platform {
            3 if encoding == 1 || encoding == 10 => {
                (if language == 0x0409 { 0 } else { 1 }, decode_utf16be(raw))
            }
            0 => (2, decode_utf16be(raw)),
            1 if encoding == 0 && language == 0 => {
                (3, raw.iter().map(|&b| char::from(b)).collect())
            }
            _ => continue,
        };
        let text = text.trim_matches('\0').trim().to_owned();
        if text.is_empty() || text.len() > max_name {
            continue;
        }
        if best[slot].as_ref().is_none_or(|(p, _)| priority < *p) {
            best[slot] = Some((priority, text));
        }
    }
    let [family, subfamily, tfam, tsub] = best;
    Some(FaceNames {
        index,
        family: family?.1,
        subfamily: subfamily.map(|s| s.1).unwrap_or_else(|| "Regular".into()),
        typographic_family: tfam.map(|s| s.1),
        typographic_subfamily: tsub.map(|s| s.1),
    })
}

/// Lists the faces in a TrueType / OpenType file or collection. Malformed data
/// yields an empty list (never a panic).
pub fn inspect_font(data: &[u8]) -> Vec<FaceNames> {
    let Some(tag) = data.get(0..4) else {
        return Vec::new();
    };
    if tag == b"ttcf" {
        let n = be32(data, 8).unwrap_or(0).min(256) as usize;
        (0..n)
            .filter_map(|i| {
                let off = be32(data, 12 + i * 4)? as usize;
                parse_face(data, off, i as u32)
            })
            .collect()
    } else if matches!(tag, [0, 1, 0, 0] | b"OTTO" | b"true") {
        parse_face(data, 0, 0).into_iter().collect()
    } else {
        Vec::new()
    }
}

/// The faces of a font file, reading only its table directories and `name`
/// tables (a few KB, not the whole file).
trait NameReader {
    fn read_at(&mut self, off: u64, len: usize) -> Option<Vec<u8>>;
    fn max_name(&self) -> usize {
        usize::MAX
    }
    fn charge(&mut self, _bytes: usize) -> bool {
        true
    }
    fn face_limit(&mut self, n: usize) -> usize {
        n.min(256)
    }
}
struct NativeNameReader(std::fs::File);
impl NameReader for NativeNameReader {
    fn read_at(&mut self, off: u64, len: usize) -> Option<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        if len > 4 << 20 {
            return None;
        }
        self.0.seek(SeekFrom::Start(off)).ok()?;
        let mut bytes = vec![0u8; len];
        self.0.read_exact(&mut bytes).ok()?;
        Some(bytes)
    }
}
pub fn read_face_names(path: &Path) -> Vec<FaceNames> {
    std::fs::File::open(path)
        .map(|f| read_faces_with(&mut NativeNameReader(f)))
        .unwrap_or_default()
}
fn read_faces_with(reader: &mut impl NameReader) -> Vec<FaceNames> {
    let Some(head) = reader.read_at(0, 12) else {
        return Vec::new();
    };
    let dirs: Vec<(u64, u32)> = if &head[0..4] == b"ttcf" {
        let n = reader.face_limit(be32(&head, 8).unwrap_or(0) as usize);
        let Some(offs) = reader.read_at(12, n * 4) else {
            return Vec::new();
        };
        (0..n)
            .filter_map(|i| Some((u64::from(be32(&offs, i * 4)?), i as u32)))
            .collect()
    } else if matches!(&head[0..4], [0, 1, 0, 0] | b"OTTO" | b"true") {
        vec![(0, 0)]
    } else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (dir, index) in dirs {
        if !reader.charge(0) {
            break;
        }
        if let Some(names) = read_face_with(reader, dir, index) {
            out.push(names);
        }
    }
    out
}

/// Shared single-face metadata read; callers own traversal and work limits.
fn read_face_with(reader: &mut impl NameReader, dir: u64, index: u32) -> Option<FaceNames> {
    let h = reader.read_at(dir, 12)?;
    let n = usize::from(be16(&h, 4).unwrap_or(0)).min(512);
    let recs = reader.read_at(dir, 12 + n * 16)?;
    let (off, len) = name_table_at(&recs, 0)?;
    let table = reader.read_at(off as u64, len)?;
    parse_names_checked(&table, index, reader.max_name(), &mut |n| reader.charge(n))
}

/// One installed face.
#[derive(Clone, Debug)]
pub struct InstalledFace {
    pub names: FaceNames,
    pub path: PathBuf,
}

/// Every font face installed (system and per-user font folders, and
/// `PLUMSPIRIT_FONT_DIRS`), read from the files' own name tables. Bounded
/// (at most 20,000 directory entries); slow folders only cost a scan.
pub fn installed_faces() -> Vec<InstalledFace> {
    let mut files = Vec::new();
    let mut budget = 20_000usize;
    for d in override_dirs().into_iter().chain(system_font_dirs()) {
        scan_dir(&d, 4, "", &mut files, &mut budget);
    }
    files.sort();
    files.dedup();
    let mut out = Vec::new();
    for path in files {
        for names in read_face_names(&path) {
            out.push(InstalledFace {
                names,
                path: path.clone(),
            });
        }
    }
    out
}

// -------------------------------------------------------------------- scoring

fn is_regular_style(style: &str) -> bool {
    matches!(
        style.trim().to_ascii_lowercase().as_str(),
        "regular" | "roman" | "normal" | "book" | "plain" | "55 roman" | "text"
    )
}

/// Higher is better; `None` = not Helvetica.
pub fn helvetica_score(face: &FaceNames) -> Option<u32> {
    let fam = face.effective_family().to_ascii_lowercase();
    let style = face.effective_style().to_ascii_lowercase();
    if !fam.contains("helvetica") {
        return None;
    }
    let italic = style.contains("italic") || style.contains("oblique");
    let distorted = [
        "condensed",
        "compressed",
        "narrow",
        "extended",
        "rounded",
        "inserat",
    ]
    .iter()
    .any(|w| fam.contains(w) || style.contains(w));
    let mut score: u32 = if fam == "helvetica" {
        100
    } else if fam == "helvetica neue" {
        70
    } else {
        60
    };
    if is_regular_style(&style) {
        score += 20;
    }
    if italic {
        score = score.saturating_sub(60);
    }
    if distorted {
        score = score.saturating_sub(40);
    }
    Some(score)
}

// ------------------------------------------------------------------ discovery

fn is_font_file(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "ttf" | "otf" | "ttc" | "otc"
        )
    })
}

#[derive(Clone, Copy)]
struct ScanLimits {
    files: usize,
    path_bytes: usize,
    no_links: bool,
}
fn scan_dir(dir: &Path, depth: u32, name_filter: &str, out: &mut Vec<PathBuf>, budget: &mut usize) {
    let _ = scan_dir_checked(
        dir,
        depth,
        name_filter,
        out,
        budget,
        ScanLimits {
            files: usize::MAX,
            path_bytes: usize::MAX,
            no_links: false,
        },
        &mut || true,
    );
}
fn scan_dir_checked(
    dir: &Path,
    depth: u32,
    name_filter: &str,
    out: &mut Vec<PathBuf>,
    budget: &mut usize,
    limits: ScanLimits,
    check: &mut impl FnMut() -> bool,
) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return true;
    };
    for entry in rd.flatten() {
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        if !check() {
            return false;
        }
        let p = entry.path();
        if p.as_os_str().len() > limits.path_bytes {
            return false;
        }
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if limits.no_links && ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            if depth > 0
                && !scan_dir_checked(&p, depth - 1, name_filter, out, budget, limits, check)
            {
                return false;
            }
        } else if is_font_file(&p) {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if name.contains(name_filter) {
                if out.len() == limits.files {
                    return false;
                }
                out.push(p);
            }
        }
    }
    true
}

fn system_font_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    #[cfg(windows)]
    {
        let windir =
            std::env::var_os("WINDIR").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        v.push(windir.join("Fonts"));
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            v.push(
                PathBuf::from(local)
                    .join("Microsoft")
                    .join("Windows")
                    .join("Fonts"),
            );
        }
    }
    #[cfg(target_os = "macos")]
    {
        v.push(PathBuf::from("/System/Library/Fonts"));
        v.push(PathBuf::from("/Library/Fonts"));
        if let Some(h) = std::env::var_os("HOME") {
            v.push(PathBuf::from(h).join("Library/Fonts"));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        v.push(PathBuf::from("/usr/share/fonts"));
        v.push(PathBuf::from("/usr/local/share/fonts"));
        if let Some(h) = std::env::var_os("HOME") {
            v.push(PathBuf::from(&h).join(".local/share/fonts"));
            v.push(PathBuf::from(h).join(".fonts"));
        }
    }
    v
}

fn override_dirs() -> Vec<PathBuf> {
    std::env::var_os("PLUMSPIRIT_FONT_DIRS")
        .map(|v| std::env::split_paths(&v).collect())
        .unwrap_or_default()
}

/// Candidate files that may contain Helvetica. Only file names and registry entries
/// are examined here; contents are verified later.
fn helvetica_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut budget = 20_000usize;
    for d in override_dirs() {
        scan_dir(&d, 3, "", &mut out, &mut budget);
    }
    out.extend(crate::platform::registered_font_files("helvetica"));
    for d in system_font_dirs() {
        scan_dir(&d, 4, "helvet", &mut out, &mut budget);
    }
    out.sort();
    out.dedup();
    out
}

fn fallback_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    #[cfg(windows)]
    {
        let windir =
            std::env::var_os("WINDIR").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        v.push(windir.join("Fonts").join("arial.ttf"));
        v.push(windir.join("Fonts").join("segoeui.ttf"));
    }
    #[cfg(target_os = "macos")]
    {
        v.push(PathBuf::from(
            "/System/Library/Fonts/Supplemental/Arial.ttf",
        ));
        v.push(PathBuf::from("/Library/Fonts/Arial.ttf"));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for p in [
            "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
            "/usr/share/fonts/truetype/liberation2/LiberationSans-Regular.ttf",
            "/usr/share/fonts/liberation-sans/LiberationSans-Regular.ttf",
            "/usr/share/fonts/truetype/croscore/Arimo-Regular.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ] {
            v.push(PathBuf::from(p));
        }
    }
    v
}

/// Finds the UI font. Reads only a handful of files; never fails.
pub fn discover() -> UiFont {
    let mut best: Option<(u32, UiFont)> = None;
    for path in helvetica_candidates() {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        for face in inspect_font(&bytes) {
            let Some(score) = helvetica_score(&face) else {
                continue;
            };
            if best.as_ref().is_none_or(|(s, _)| score > *s) {
                best = Some((
                    score,
                    UiFont {
                        kind: UiFontKind::Helvetica,
                        family: face.effective_family().to_owned(),
                        style: face.effective_style().to_owned(),
                        path: Some(path.clone()),
                        bytes: None,
                        face_index: face.index,
                    },
                ));
            }
        }
    }
    if let Some((_, mut font)) = best
        && let Some(p) = &font.path
        && let Ok(bytes) = std::fs::read(p)
    {
        font.bytes = Some(bytes);
        return font;
    }
    for path in fallback_candidates() {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Some(face) = inspect_font(&bytes).into_iter().next() else {
            continue;
        };
        return UiFont {
            kind: UiFontKind::Fallback,
            family: face.effective_family().to_owned(),
            style: face.effective_style().to_owned(),
            path: Some(path),
            bytes: Some(bytes),
            face_index: face.index,
        };
    }
    UiFont {
        kind: UiFontKind::BuiltIn,
        family: "egui default".into(),
        style: String::new(),
        path: None,
        bytes: None,
        face_index: 0,
    }
}

/// Installs the UI font as the first choice for all proportional (and monospace)
/// text. egui's bundled fonts stay behind it as glyph fallbacks (symbols, emoji).
pub fn install(ctx: &egui::Context, font: &UiFont) {
    let mut defs = egui::FontDefinitions::default();
    if let Some(bytes) = &font.bytes {
        let mut data = egui::FontData::from_owned(bytes.clone());
        data.index = font.face_index;
        defs.font_data
            .insert("plumspirit-ui".into(), std::sync::Arc::new(data));
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            defs.families
                .entry(family)
                .or_default()
                .insert(0, "plumspirit-ui".into());
        }
    }
    ctx.set_fonts(defs);
}

#[cfg(test)]
pub(crate) use tests::fake_font;

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal sfnt with only a `name` table (enough for discovery).
    pub(crate) fn fake_font(family: &str, style: &str) -> Vec<u8> {
        let strings: Vec<(u16, Vec<u8>)> = [(1u16, family), (2, style)]
            .iter()
            .map(|(id, s)| (*id, s.encode_utf16().flat_map(u16::to_be_bytes).collect()))
            .collect();
        let mut name = Vec::new();
        name.extend(0u16.to_be_bytes());
        name.extend((strings.len() as u16).to_be_bytes());
        let string_off = 6 + 12 * strings.len();
        name.extend((string_off as u16).to_be_bytes());
        let mut blob: Vec<u8> = Vec::new();
        for (id, s) in &strings {
            for v in [3u16, 1, 0x0409, *id, s.len() as u16, blob.len() as u16] {
                name.extend(v.to_be_bytes());
            }
            blob.extend(s);
        }
        name.extend(blob);
        let mut f = Vec::new();
        f.extend([0, 1, 0, 0]);
        f.extend(1u16.to_be_bytes());
        f.extend([0u8; 6]);
        f.extend(b"name");
        f.extend(0u32.to_be_bytes());
        f.extend((28u32).to_be_bytes());
        f.extend((name.len() as u32).to_be_bytes());
        f.extend(name);
        f
    }

    #[test]
    fn parses_names_and_scores() {
        let f = fake_font("Helvetica", "Regular");
        let faces = inspect_font(&f);
        assert_eq!(faces.len(), 1);
        assert_eq!(faces[0].family, "Helvetica");
        assert_eq!(helvetica_score(&faces[0]), Some(120));
        let neue = inspect_font(&fake_font("Helvetica Neue", "Regular"));
        assert!(helvetica_score(&neue[0]).unwrap() < 120);
        let obl = inspect_font(&fake_font("Helvetica", "Oblique"));
        assert!(helvetica_score(&obl[0]).unwrap() < helvetica_score(&neue[0]).unwrap());
        assert_eq!(
            helvetica_score(&inspect_font(&fake_font("Arial", "Regular"))[0]),
            None
        );
    }

    #[test]
    fn malformed_fonts_do_not_panic() {
        let f = fake_font("Helvetica", "Regular");
        for cut in 0..f.len() {
            let _ = inspect_font(&f[..cut]);
        }
        let mut g = f.clone();
        for i in 0..g.len() {
            g[i] ^= 0xFF;
            let _ = inspect_font(&g);
            g[i] ^= 0xFF;
        }
        assert!(inspect_font(b"ttcf\x00\x01\x00\x00\xFF\xFF\xFF\xFF").is_empty());
    }

    #[test]
    fn real_system_font_if_present() {
        // Uses whatever fallback font the machine has; skips silently otherwise.
        for p in fallback_candidates() {
            if let Ok(bytes) = std::fs::read(&p) {
                let faces = inspect_font(&bytes);
                assert!(!faces.is_empty(), "{}", p.display());
                assert!(!faces[0].family.is_empty());
                return;
            }
        }
    }
}
