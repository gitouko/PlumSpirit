//! The Progressor (v0.6): a small, customizable readout of the project's
//! analytics at the playhead / edit position — notes passed, NPS, poly, PPS and
//! optional peaks — drawn in a corner of the piano roll.
//!
//! The numbers come from the analysis engine (`app::analysis`); this module only
//! formats and paints them. The text is a **template** with `{tokens}`: a pure
//! formatting system (no code is run, nothing is read from disk or the network).
//! Unknown tokens stay visible as written and are listed in the settings window.
//!
//! The template system is shared (v0.8 §147–149): [`Template`] is generic over
//! a [`Field`] set — the Progressor's [`Token`]s here, Selection Analytics'
//! fields in `selection_stats.rs` — with one parser, one renderer and one
//! editor ([`template_editor`]). Templates only format numbers the analysis
//! already holds: editing the text never recomputes the analysis (§202).
//!
//! Presentation settings are user preferences (`settings.json`), never project
//! data, so changing them never marks the project as changed.

use crate::ui::theme::{self, Palette, space};
use egui::{Color32, FontId, Pos2, Rect, RichText, Stroke, Vec2};
use serde_json::{Map, Value, json};

/// Longest template accepted (characters) and most lines drawn.
pub const MAX_TEMPLATE_CHARS: usize = 2000;
pub const MAX_LINES: usize = 24;

pub const DEFAULT_TEMPLATE: &str =
    "Notes: {notes}/{total_notes} · {scope}\nNPS: {nps}\nPoly: {poly}\nPPS: {pps}";

/// The default text before beta1: settings that still hold it unchanged get
/// [`DEFAULT_TEMPLATE`] (B1-0106: the readout says what it counts).
pub const PRE_B1_DEFAULT_TEMPLATE: &str =
    "Notes: {notes}/{total_notes}\nNPS: {nps}\nPoly: {poly}\nPPS: {pps}";

/// A value the template can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    Notes,
    TotalNotes,
    NotesRemaining,
    ProgressPercent,
    Nps,
    Poly,
    Pps,
    PeakNps,
    PeakPoly,
    PeakPoly1s,
    Time,
    Tick,
    Measure,
    Bpm,
    /// B1-0106: what the counts cover.
    Scope,
}

impl Token {
    pub const ALL: [Token; 15] = [
        Token::Notes,
        Token::TotalNotes,
        Token::NotesRemaining,
        Token::ProgressPercent,
        Token::Nps,
        Token::Poly,
        Token::Pps,
        Token::PeakNps,
        Token::PeakPoly,
        Token::PeakPoly1s,
        Token::Time,
        Token::Tick,
        Token::Measure,
        Token::Bpm,
        Token::Scope,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Token::Notes => "notes",
            Token::TotalNotes => "total_notes",
            Token::NotesRemaining => "notes_remaining",
            Token::ProgressPercent => "progress_percent",
            Token::Nps => "nps",
            Token::Poly => "poly",
            Token::Pps => "pps",
            Token::PeakNps => "peak_nps",
            Token::PeakPoly => "peak_poly",
            Token::PeakPoly1s => "peak_poly_1s",
            Token::Time => "time",
            Token::Tick => "tick",
            Token::Measure => "measure",
            Token::Bpm => "bpm",
            Token::Scope => "scope",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Token::Notes => "notes started so far (at or before the position)",
            Token::TotalNotes => "all notes in scope",
            Token::NotesRemaining => "notes not started yet",
            Token::ProgressPercent => "notes passed as a percentage of all notes",
            Token::Nps => "note starts in the last second of song time",
            Token::Poly => "notes sounding at the position",
            Token::Pps => "average poly over the last second (time-weighted)",
            Token::PeakNps => "highest NPS reached up to the position",
            Token::PeakPoly => "highest poly reached up to the position",
            Token::PeakPoly1s => "highest poly within the last second",
            Token::Time => "song time at the position",
            Token::Tick => "position in ticks",
            Token::Measure => "bar and beat",
            Token::Bpm => "Conductor BPM at the position",
            Token::Scope => {
                "what is counted: All Tracks, Audible Tracks (Mute / Solo respected) or Active Track + Subtracks"
            }
        }
    }

    pub fn needs_peaks(self) -> bool {
        matches!(self, Token::PeakNps | Token::PeakPoly)
    }
}

/// A set of values a template can show (`{key}`).
pub trait Field: Copy + Eq + std::fmt::Debug + 'static {
    /// Every field, in the order the field reference lists them.
    fn all() -> &'static [Self];
    fn key(self) -> &'static str;
    fn describe(self) -> &'static str;
    fn parse(name: &str) -> Option<Self> {
        Self::all().iter().copied().find(|t| t.key() == name)
    }
}

impl Field for Token {
    fn all() -> &'static [Token] {
        &Token::ALL
    }
    fn key(self) -> &'static str {
        Token::key(self)
    }
    fn describe(self) -> &'static str {
        Token::describe(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Piece<F> {
    Text(String),
    Token(F),
}

/// A parsed template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template<F = Token> {
    pieces: Vec<Piece<F>>,
    /// Problems found (unknown tokens, unclosed braces, too long).
    pub errors: Vec<String>,
}

impl<F> Default for Template<F> {
    fn default() -> Self {
        Template {
            pieces: Vec::new(),
            errors: Vec::new(),
        }
    }
}

impl<F: Field> Template<F> {
    /// Parses `{token}` fields; `{{` and `}}` are literal braces.
    pub fn parse(src: &str) -> Template<F> {
        let mut t = Template::default();
        let src: String = if src.chars().count() > MAX_TEMPLATE_CHARS {
            t.errors.push(format!(
                "The text is longer than {MAX_TEMPLATE_CHARS} characters; the rest is not shown."
            ));
            src.chars().take(MAX_TEMPLATE_CHARS).collect()
        } else {
            src.to_owned()
        };
        let mut text = String::new();
        let mut chars = src.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    text.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    text.push('}');
                }
                '{' => {
                    let mut name = String::new();
                    let mut closed = false;
                    for d in chars.by_ref() {
                        if d == '}' {
                            closed = true;
                            break;
                        }
                        name.push(d);
                    }
                    let key = name.trim();
                    match (closed, F::parse(key)) {
                        (true, Some(tok)) => {
                            if !text.is_empty() {
                                t.pieces.push(Piece::Text(std::mem::take(&mut text)));
                            }
                            t.pieces.push(Piece::Token(tok));
                        }
                        (true, None) => {
                            t.errors.push(format!("Unknown field {{{key}}}."));
                            text.push('{');
                            text.push_str(&name);
                            text.push('}');
                        }
                        (false, _) => {
                            t.errors
                                .push("A “{” is not closed; write {{ for a literal brace.".into());
                            text.push('{');
                            text.push_str(&name);
                        }
                    }
                }
                c => text.push(c),
            }
        }
        if !text.is_empty() {
            t.pieces.push(Piece::Text(text));
        }
        t
    }

    pub fn uses(&self, f: impl Fn(F) -> bool) -> bool {
        self.pieces
            .iter()
            .any(|p| matches!(p, Piece::Token(t) if f(*t)))
    }

    /// The text with every token replaced by `value(token)`.
    pub fn render(&self, mut value: impl FnMut(F) -> String) -> String {
        let mut out = String::new();
        for p in &self.pieces {
            match p {
                Piece::Text(s) => out.push_str(s),
                Piece::Token(t) => out.push_str(&value(*t)),
            }
        }
        out
    }
}

/// How large counts are written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NumberFormat {
    /// 40,000,000
    #[default]
    Grouped,
    /// 40000000
    Plain,
    /// 40.0M
    Compact,
}

impl NumberFormat {
    pub const ALL: [NumberFormat; 3] = [
        NumberFormat::Grouped,
        NumberFormat::Plain,
        NumberFormat::Compact,
    ];
    pub fn key(self) -> &'static str {
        match self {
            NumberFormat::Grouped => "grouped",
            NumberFormat::Plain => "plain",
            NumberFormat::Compact => "compact",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            NumberFormat::Grouped => "40,000,000",
            NumberFormat::Plain => "40000000",
            NumberFormat::Compact => "40.0M",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        NumberFormat::ALL.into_iter().find(|f| f.key() == s)
    }

    pub fn count(self, v: u64) -> String {
        match self {
            NumberFormat::Grouped => plumspirit_engine::midi::group_digits(v),
            NumberFormat::Plain => v.to_string(),
            NumberFormat::Compact => compact(v as f64, 1),
        }
    }

    /// A non-negative decimal with `decimals` digits after the point.
    pub fn decimal(self, v: f64, decimals: u8) -> String {
        let v = if v.is_finite() { v.max(0.0) } else { 0.0 };
        let d = usize::from(decimals.min(6));
        match self {
            NumberFormat::Compact if v >= 1000.0 => compact(v, decimals.min(3)),
            NumberFormat::Grouped => {
                let s = format!("{v:.d$}");
                let (int, frac) = s
                    .split_once('.')
                    .map_or((s.as_str(), None), |(a, b)| (a, Some(b)));
                let int: u64 = int.parse().unwrap_or(0);
                match frac {
                    Some(f) => format!("{}.{f}", plumspirit_engine::midi::group_digits(int)),
                    None => plumspirit_engine::midi::group_digits(int),
                }
            }
            _ => format!("{v:.d$}"),
        }
    }
}

fn compact(v: f64, decimals: u8) -> String {
    let d = usize::from(decimals);
    let (div, suffix) = if v >= 1e12 {
        (1e12, "T")
    } else if v >= 1e9 {
        (1e9, "B")
    } else if v >= 1e6 {
        (1e6, "M")
    } else if v >= 1e3 {
        (1e3, "K")
    } else {
        return format!("{}", v.round() as u64);
    };
    format!("{:.d$}{suffix}", v / div)
}

/// Where the readout sits inside the note area.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Corner {
    TopLeft,
    #[default]
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    pub const ALL: [Corner; 4] = [
        Corner::TopLeft,
        Corner::TopRight,
        Corner::BottomLeft,
        Corner::BottomRight,
    ];
    pub fn key(self) -> &'static str {
        match self {
            Corner::TopLeft => "top_left",
            Corner::TopRight => "top_right",
            Corner::BottomLeft => "bottom_left",
            Corner::BottomRight => "bottom_right",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Corner::TopLeft => "Top Left",
            Corner::TopRight => "Top Right",
            Corner::BottomLeft => "Bottom Left",
            Corner::BottomRight => "Bottom Right",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        Corner::ALL.into_iter().find(|c| c.key() == s)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

impl TextAlign {
    pub const ALL: [TextAlign; 3] = [TextAlign::Left, TextAlign::Center, TextAlign::Right];
    pub fn key(self) -> &'static str {
        match self {
            TextAlign::Left => "left",
            TextAlign::Center => "center",
            TextAlign::Right => "right",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            TextAlign::Left => "Left",
            TextAlign::Center => "Center",
            TextAlign::Right => "Right",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        TextAlign::ALL.into_iter().find(|c| c.key() == s)
    }
}

/// Which notes the analytics (Progressor) and the live keyboard colours cover.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ScopeSetting {
    /// Every note of the project, muted or not.
    Project,
    /// Only tracks that play now (Mute / Solo exactly as playback applies
    /// them: `playback::eligibility`). B1-0106: the Progressor's default
    /// ("Respect Track Mute/Solo in Progressor" on).
    #[default]
    Audible,
    /// The active track and its subtracks.
    ActiveTrack,
}

impl ScopeSetting {
    pub const ALL: [ScopeSetting; 3] = [
        ScopeSetting::Project,
        ScopeSetting::Audible,
        ScopeSetting::ActiveTrack,
    ];
    pub fn key(self) -> &'static str {
        match self {
            ScopeSetting::Project => "project",
            ScopeSetting::Audible => "audible",
            ScopeSetting::ActiveTrack => "active_track",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ScopeSetting::Project => "All Tracks (Mute / Solo Ignored)",
            ScopeSetting::Audible => "Audible Tracks (Mute / Solo Respected)",
            ScopeSetting::ActiveTrack => "Active Track + Subtracks",
        }
    }
    /// B1-0106: what the readout says it counts (`{scope}`).
    pub fn short(self) -> &'static str {
        match self {
            ScopeSetting::Project => "All Tracks",
            ScopeSetting::Audible => "Audible Tracks",
            ScopeSetting::ActiveTrack => "Active Track + Subtracks",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        ScopeSetting::ALL.into_iter().find(|c| c.key() == s)
    }
}

/// Progressor presentation (user preference).
#[derive(Clone, Debug, PartialEq)]
pub struct ProgressorSettings {
    pub enabled: bool,
    pub template: String,
    pub corner: Corner,
    /// Extra offset from the corner, points (positive = inwards).
    pub offset: [f32; 2],
    pub font_size: f32,
    /// `None`: the theme's text colour.
    pub text_color: Option<[u8; 3]>,
    /// 0–100 %.
    pub text_opacity: u8,
    pub background: bool,
    pub background_opacity: u8,
    pub align: TextAlign,
    pub line_spacing: f32,
    pub number_format: NumberFormat,
    pub pps_decimals: u8,
    /// NPS / PPS window, milliseconds (default 1000).
    pub window_ms: u32,
    pub scope: ScopeSetting,
    /// v0.8: the Selection Analytics text (same template system; its own
    /// fields, `selection_stats::SelField`). Shares the style above.
    pub selection_template: String,
    /// B1-0065: the readout shows its first line only (the chip at its
    /// corner toggles it).
    pub collapsed: bool,
    /// B1-0065: the Selection Analytics readout's own collapse and offset
    /// (dragged by its chip; from the Progressor's opposite corner).
    /// L-F01: collapsed = compact (its first line), the default; saved as
    /// `selection_compact` (a pre-launch `selection_collapsed` is not read:
    /// those files held the full box by default, so they open compact once).
    pub selection_collapsed: bool,
    pub selection_offset: [f32; 2],
}

pub const MIN_FONT: f32 = 8.0;
pub const MAX_FONT: f32 = 72.0;
pub const MIN_WINDOW_MS: u32 = 10;
pub const MAX_WINDOW_MS: u32 = 60_000;

impl Default for ProgressorSettings {
    fn default() -> Self {
        ProgressorSettings {
            enabled: true,
            template: DEFAULT_TEMPLATE.into(),
            corner: Corner::TopRight,
            offset: [0.0, 0.0],
            font_size: 12.5,
            text_color: None,
            text_opacity: 100,
            background: true,
            background_opacity: 78,
            align: TextAlign::Left,
            line_spacing: 1.0,
            number_format: NumberFormat::Grouped,
            pps_decimals: 1,
            window_ms: 1000,
            scope: ScopeSetting::Audible,
            selection_template: super::selection_stats::DEFAULT_TEMPLATE.into(),
            collapsed: false,
            selection_collapsed: true,
            selection_offset: [0.0, 0.0],
        }
    }
}

impl ProgressorSettings {
    pub fn window_ns(&self) -> u64 {
        u64::from(self.window_ms.clamp(MIN_WINDOW_MS, MAX_WINDOW_MS)) * 1_000_000
    }

    pub fn to_json(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "template": self.template,
            "corner": self.corner.key(),
            "offset": [self.offset[0], self.offset[1]],
            "font_size": self.font_size,
            "text_color": self.text_color.map(|c| format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])),
            "text_opacity": self.text_opacity,
            "background": self.background,
            "background_opacity": self.background_opacity,
            "align": self.align.key(),
            "line_spacing": self.line_spacing,
            "number_format": self.number_format.key(),
            "pps_decimals": self.pps_decimals,
            "window_ms": self.window_ms,
            "scope": self.scope.key(),
            "respect_mute_solo": self.scope == ScopeSetting::Audible,
            "selection_template": self.selection_template,
            "collapsed": self.collapsed,
            "selection_compact": self.selection_collapsed,
            "selection_offset": [self.selection_offset[0], self.selection_offset[1]],
        })
    }

    /// Lenient: every field falls back on its own (problems are reported).
    pub fn from_json(v: &Value, problems: &mut Vec<String>) -> ProgressorSettings {
        let mut s = ProgressorSettings::default();
        let Some(o) = v.as_object() else {
            problems.push("progressor is not an object; using the defaults".into());
            return s;
        };
        let mut bad =
            |k: &str| problems.push(format!("progressor.{k} is invalid; using the default"));
        let get = |k: &str| o.get(k);
        if let Some(x) = get("enabled") {
            match x.as_bool() {
                Some(b) => s.enabled = b,
                None => bad("enabled"),
            }
        }
        if let Some(x) = get("template") {
            match x.as_str() {
                // B1-0106: an untouched pre-beta1 default gains {scope}.
                Some(t) if t == PRE_B1_DEFAULT_TEMPLATE => s.template = DEFAULT_TEMPLATE.into(),
                Some(t) if t.chars().count() <= MAX_TEMPLATE_CHARS => s.template = t.to_owned(),
                _ => bad("template"),
            }
        }
        if let Some(x) = get("selection_template") {
            match x.as_str() {
                Some(t) if t.chars().count() <= MAX_TEMPLATE_CHARS => {
                    // v0.9: an untouched v0.8 default gains the peaks line;
                    // beta1: an untouched v0.9 default the gate line.
                    s.selection_template = if t == super::selection_stats::V08_DEFAULT_TEMPLATE
                        || t == super::selection_stats::V09_DEFAULT_TEMPLATE
                    {
                        super::selection_stats::DEFAULT_TEMPLATE.to_owned()
                    } else {
                        t.to_owned()
                    };
                }
                _ => bad("selection_template"),
            }
        }
        if let Some(x) = get("corner") {
            match x.as_str().and_then(Corner::parse) {
                Some(c) => s.corner = c,
                None => bad("corner"),
            }
        }
        let offset = |x: &Value| -> Option<[f32; 2]> {
            let a = x.as_array().filter(|a| a.len() == 2)?;
            let f = |v: &Value| {
                v.as_f64()
                    .filter(|f| f.is_finite())
                    .map(|f| f.clamp(-2000.0, 2000.0) as f32)
            };
            Some([f(&a[0])?, f(&a[1])?])
        };
        if let Some(x) = get("offset") {
            match offset(x) {
                Some(o) => s.offset = o,
                None => bad("offset"),
            }
        }
        if let Some(x) = get("selection_offset") {
            match offset(x) {
                Some(o) => s.selection_offset = o,
                None => bad("selection_offset"),
            }
        }
        for (key, slot) in [
            ("collapsed", &mut s.collapsed),
            ("selection_compact", &mut s.selection_collapsed),
        ] {
            if let Some(x) = get(key) {
                match x.as_bool() {
                    Some(b) => *slot = b,
                    None => bad(key),
                }
            }
        }
        if let Some(x) = get("font_size") {
            match x.as_f64().filter(|f| f.is_finite()) {
                Some(f) => s.font_size = (f as f32).clamp(MIN_FONT, MAX_FONT),
                None => bad("font_size"),
            }
        }
        match get("text_color") {
            None | Some(Value::Null) => {}
            Some(x) => match x.as_str().and_then(parse_hex) {
                Some(c) => s.text_color = Some(c),
                None => bad("text_color"),
            },
        }
        let pct = |x: &Value| x.as_u64().map(|v| v.min(100) as u8);
        if let Some(x) = get("text_opacity") {
            match pct(x) {
                Some(v) => s.text_opacity = v,
                None => bad("text_opacity"),
            }
        }
        if let Some(x) = get("background") {
            match x.as_bool() {
                Some(b) => s.background = b,
                None => bad("background"),
            }
        }
        if let Some(x) = get("background_opacity") {
            match pct(x) {
                Some(v) => s.background_opacity = v,
                None => bad("background_opacity"),
            }
        }
        if let Some(x) = get("align") {
            match x.as_str().and_then(TextAlign::parse) {
                Some(a) => s.align = a,
                None => bad("align"),
            }
        }
        if let Some(x) = get("line_spacing") {
            match x.as_f64().filter(|f| f.is_finite()) {
                Some(f) => s.line_spacing = (f as f32).clamp(0.8, 3.0),
                None => bad("line_spacing"),
            }
        }
        if let Some(x) = get("number_format") {
            match x.as_str().and_then(NumberFormat::parse) {
                Some(f) => s.number_format = f,
                None => bad("number_format"),
            }
        }
        if let Some(x) = get("pps_decimals") {
            match x.as_u64() {
                Some(v) => s.pps_decimals = v.min(3) as u8,
                None => bad("pps_decimals"),
            }
        }
        if let Some(x) = get("window_ms") {
            match x.as_u64() {
                Some(v) => s.window_ms = (v as u32).clamp(MIN_WINDOW_MS, MAX_WINDOW_MS),
                None => bad("window_ms"),
            }
        }
        if let Some(x) = get("scope") {
            match x.as_str().and_then(ScopeSetting::parse) {
                // B1-0106: before beta1, "project" was simply the default
                // (the file has no "respect_mute_solo"): the new default,
                // Respect Track Mute/Solo, applies.
                Some(ScopeSetting::Project) if get("respect_mute_solo").is_none() => {
                    s.scope = ScopeSetting::Audible
                }
                Some(c) => s.scope = c,
                None => bad("scope"),
            }
        }
        s
    }
}

fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some([p(0)?, p(2)?, p(4)?])
}

/// Store a map entry for `settings.json`.
pub fn insert_json(obj: &mut Map<String, Value>, s: &ProgressorSettings) {
    obj.insert("progressor".into(), s.to_json());
}

// ----------------------------------------------------------------- values

/// Everything a template can show, already measured.
#[derive(Clone, Debug, Default)]
pub struct Readout {
    pub notes: u64,
    pub total: u64,
    pub nps: u64,
    pub poly: u64,
    pub pps: f64,
    pub peak_nps: Option<u64>,
    pub peak_poly: Option<u64>,
    pub peak_poly_1s: Option<u64>,
    pub time: String,
    pub tick: u64,
    pub measure: String,
    pub bpm: String,
    /// B1-0106: what the counts cover (`ScopeSetting::short`).
    pub scope: &'static str,
}

/// The overlay text for `readout`.
pub fn render_text(template: &Template, r: &Readout, s: &ProgressorSettings) -> String {
    let f = s.number_format;
    let pending = "…".to_string();
    let text = template.render(|t| match t {
        Token::Notes => f.count(r.notes),
        Token::TotalNotes => f.count(r.total),
        Token::NotesRemaining => f.count(r.total.saturating_sub(r.notes)),
        Token::ProgressPercent => {
            if r.total == 0 {
                "0.0".into()
            } else {
                format!("{:.1}", r.notes as f64 * 100.0 / r.total as f64)
            }
        }
        Token::Nps => f.count(r.nps),
        Token::Poly => f.count(r.poly),
        Token::Pps => f.decimal(r.pps, s.pps_decimals),
        Token::PeakNps => r.peak_nps.map_or(pending.clone(), |v| f.count(v)),
        Token::PeakPoly => r.peak_poly.map_or(pending.clone(), |v| f.count(v)),
        Token::PeakPoly1s => r.peak_poly_1s.map_or(pending.clone(), |v| f.count(v)),
        Token::Time => r.time.clone(),
        Token::Tick => f.count(r.tick),
        Token::Measure => r.measure.clone(),
        Token::Bpm => r.bpm.clone(),
        Token::Scope => r.scope.to_string(),
    });
    // Bounded: at most MAX_LINES lines.
    text.lines().take(MAX_LINES).collect::<Vec<_>>().join("\n")
}

// ---------------------------------------------------------------- painting

/// B1-0065: how much of its opacity a readout keeps while the pointer is
/// over it, so the notes under it show.
pub const HOVER_FADE: f32 = 0.15;
/// B1-0065: the collapse / move chip at a readout's top-left corner.
pub const CHIP: f32 = 12.0;

/// B1-0065: what a readout reacts to this frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct OverlayState {
    pub pointer: Option<Pos2>,
    pub collapsed: bool,
    /// Draw the collapse / move chip (the Piano Roll's readouts).
    pub chip: bool,
    /// v1.0-Launch L-E01: draw the advanced-section caret at the top right
    /// (Selection Analytics: `Some(shown)`).
    pub advanced: Option<bool>,
}

/// B1-0065: where a readout was drawn (for its chip's hit test).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayView {
    pub rect: Rect,
    pub chip: Rect,
    /// L-E01: the advanced-section caret, when drawn.
    pub advanced: Option<Rect>,
}

/// The readouts' controls (chips and the caret, last frame): presses there
/// belong to the control, not to the view under it.
pub type OverlayControls = [Option<Rect>; 3];

/// The controls of the two readouts.
pub fn overlay_controls(views: &[Option<OverlayView>; 2]) -> OverlayControls {
    [
        views[0].map(|v| v.chip),
        views[1].map(|v| v.chip),
        views[1].and_then(|v| v.advanced),
    ]
}

/// B1-0065: the opacity factor of a readout drawn at `rect`.
pub fn overlay_fade(rect: Rect, pointer: Option<Pos2>) -> f32 {
    if pointer.is_some_and(|p| rect.contains(p)) {
        HOVER_FADE
    } else {
        1.0
    }
}

/// B1-0065: a readout's offset after its chip was dragged by `delta`
/// (offsets point inwards from its corner), kept inside `area`.
pub fn drag_offset(offset: [f32; 2], corner: Corner, delta: Vec2, area: Rect) -> [f32; 2] {
    let dx = match corner {
        Corner::TopLeft | Corner::BottomLeft => delta.x,
        _ => -delta.x,
    };
    let dy = match corner {
        Corner::TopLeft | Corner::TopRight => delta.y,
        _ => -delta.y,
    };
    [
        (offset[0] + dx).clamp(-8.0, area.width().max(0.0)),
        (offset[1] + dy).clamp(-8.0, area.height().max(0.0)),
    ]
}

/// Paints the readout inside `area` (the note area). `stale`: the numbers are
/// from before the last edit (an update is running). B1-0065: it fades while
/// the pointer is over it, shows only its first line when collapsed, and can
/// have a chip (collapse / move) at its top-left corner. Returns where it was
/// drawn.
pub fn paint_overlay(
    painter: &egui::Painter,
    area: Rect,
    text: &str,
    s: &ProgressorSettings,
    pal: &Palette,
    stale: bool,
    state: OverlayState,
) -> Option<OverlayView> {
    if text.is_empty() || area.width() < 40.0 || area.height() < 20.0 {
        return None;
    }
    let all: Vec<&str> = text.lines().collect();
    let shown_text;
    let text = if state.collapsed && all.len() > 1 {
        shown_text = format!("{} …", all[0]);
        shown_text.as_str()
    } else {
        text
    };
    let font = FontId::proportional(s.font_size.clamp(MIN_FONT, MAX_FONT));
    let base = s
        .text_color
        .map_or(pal.text, |c| Color32::from_rgb(c[0], c[1], c[2]));
    let mut alpha = f32::from(s.text_opacity.min(100)) / 100.0;
    if stale {
        alpha *= 0.55;
    }
    let color = base.gamma_multiply(alpha);
    let lines: Vec<&str> = text.lines().collect();
    let galleys: Vec<_> = lines
        .iter()
        .map(|l| painter.layout_no_wrap((*l).to_owned(), font.clone(), color))
        .collect();
    let row = painter.fonts_mut(|f| f.row_height(&font));
    let step = row * s.line_spacing.clamp(0.8, 3.0);
    let width = galleys.iter().map(|g| g.size().x).fold(0.0f32, f32::max);
    let mut height = step * (galleys.len().max(1) as f32 - 1.0) + row;
    let note = stale.then(|| {
        painter.layout_no_wrap(
            "Updating analysis…".into(),
            FontId::proportional(theme::size::SMALL),
            pal.text_weak,
        )
    });
    let width = width.max(note.as_ref().map_or(0.0, |g| g.size().x));
    if let Some(n) = &note {
        height += n.size().y + 2.0;
    }
    let pad = Vec2::new(7.0, 5.0);
    let chip_w = if state.chip { CHIP + 4.0 } else { 0.0 };
    let caret_w = if state.advanced.is_some() {
        CHIP + 4.0
    } else {
        0.0
    };
    let size = Vec2::new(
        width + chip_w + caret_w,
        height.max(if state.chip { CHIP } else { 0.0 }),
    ) + pad * 2.0;
    let inset = 8.0;
    let (ox, oy) = (s.offset[0], s.offset[1]);
    let x = match s.corner {
        Corner::TopLeft | Corner::BottomLeft => area.left() + inset + ox,
        _ => area.right() - inset - ox - size.x,
    };
    let y = match s.corner {
        Corner::TopLeft | Corner::TopRight => area.top() + inset + oy,
        _ => area.bottom() - inset - oy - size.y,
    };
    let x = x.clamp(area.left(), (area.right() - size.x).max(area.left()));
    let y = y.clamp(area.top(), (area.bottom() - size.y).max(area.top()));
    let rect = Rect::from_min_size(Pos2::new(x, y), size);
    let fade = overlay_fade(rect, state.pointer);
    let color = color.gamma_multiply(fade);
    if s.background && s.background_opacity > 0 {
        let a = f32::from(s.background_opacity.min(100)) / 100.0 * fade;
        painter.rect_filled(rect, 0.0, pal.panel_bg.gamma_multiply(a));
        painter.rect_stroke(
            rect,
            0.0,
            Stroke::new(1.0, pal.panel_border.gamma_multiply(a)),
            egui::StrokeKind::Inside,
        );
    }
    let mut yy = rect.top() + pad.y;
    for g in galleys {
        let w = g.size().x;
        let xx = match s.align {
            TextAlign::Left => rect.left() + pad.x + chip_w,
            TextAlign::Center => rect.center().x + chip_w / 2.0 - w / 2.0,
            TextAlign::Right => rect.right() - pad.x - caret_w - w,
        };
        // The galleys were laid out before the rect (and so the fade) was
        // known: the faded colour overrides theirs.
        painter.galley_with_override_text_color(Pos2::new(xx, yy), g, color);
        yy += step;
    }
    if let Some(n) = note {
        let at = Pos2::new(
            rect.left() + pad.x + chip_w,
            rect.bottom() - pad.y - n.size().y,
        );
        painter.galley_with_override_text_color(at, n, pal.text_weak.gamma_multiply(fade));
    }
    let chip = Rect::from_min_size(rect.min + pad, Vec2::splat(CHIP));
    if state.chip {
        // ▸ collapsed / ▾ expanded, painted (no font glyph needed).
        let c = chip.center();
        let col = pal.text_weak.gamma_multiply(fade.max(0.6));
        let pts = if state.collapsed {
            vec![
                c + Vec2::new(-2.5, -4.0),
                c + Vec2::new(3.5, 0.0),
                c + Vec2::new(-2.5, 4.0),
            ]
        } else {
            vec![
                c + Vec2::new(-4.0, -2.5),
                c + Vec2::new(4.0, -2.5),
                c + Vec2::new(0.0, 3.5),
            ]
        };
        painter.add(egui::Shape::convex_polygon(pts, col, Stroke::NONE));
    }
    // L-E01: the caret of the advanced section (▸ hidden / ▾ shown) at the
    // top right, ringed so it reads as a control of its own.
    let advanced = state.advanced.map(|shown| {
        let r = Rect::from_min_size(
            egui::pos2(rect.right() - pad.x - CHIP, rect.top() + pad.y),
            Vec2::splat(CHIP),
        );
        let c = r.center();
        let col = pal.text_weak.gamma_multiply(fade.max(0.6));
        let pts = if shown {
            vec![
                c + Vec2::new(-3.5, -2.0),
                c + Vec2::new(3.5, -2.0),
                c + Vec2::new(0.0, 3.0),
            ]
        } else {
            vec![
                c + Vec2::new(-2.0, -3.5),
                c + Vec2::new(3.0, 0.0),
                c + Vec2::new(-2.0, 3.5),
            ]
        };
        painter.rect_stroke(
            r,
            0.0,
            Stroke::new(1.0, col.gamma_multiply(0.7)),
            egui::StrokeKind::Inside,
        );
        painter.add(egui::Shape::convex_polygon(pts, col, Stroke::NONE));
        r
    });
    Some(OverlayView {
        rect,
        chip,
        advanced,
    })
}

// ----------------------------------------------------------- settings window

/// The standard lines the field checkboxes add / remove.
pub const FIELDS: [(&str, &str); 11] = [
    ("Notes", "Notes: {notes}/{total_notes}"),
    ("NPS", "NPS: {nps}"),
    ("Poly", "Poly: {poly}"),
    ("PPS", "PPS: {pps}"),
    ("Peak NPS", "Peak NPS: {peak_nps}"),
    ("Peak Poly", "Peak Poly: {peak_poly}"),
    ("Peak Poly (1 s)", "Peak Poly 1s: {peak_poly_1s}"),
    ("Progress", "Progress: {progress_percent}%"),
    ("Time", "Time: {time}"),
    ("Bar", "Bar: {measure}"),
    ("BPM", "BPM: {bpm}"),
];

/// The template editor both readouts use: field checkboxes that add / remove
/// a standard line, the text (advanced), its first problem, and the field
/// reference. `id`: a unique salt per window.
pub fn template_editor<F: Field>(
    ui: &mut egui::Ui,
    id: &str,
    template: &mut String,
    fields: &[(&str, &str)],
) {
    ui.label(RichText::new("Fields").strong());
    ui.horizontal_wrapped(|ui| {
        for &(label, line) in fields {
            let mut on = template.lines().any(|l| l == line);
            if ui.checkbox(&mut on, label).changed() {
                let mut lines: Vec<String> = template.lines().map(str::to_owned).collect();
                if on {
                    lines.push(line.to_owned());
                } else {
                    lines.retain(|l| l != line);
                }
                *template = lines.join("\n");
            }
        }
    });
    ui.add_space(space::MICRO);
    ui.label(RichText::new("Text (advanced)").strong())
        .on_hover_text(
            "Any text; {field} shows a value (see Field reference). Write {{ or }} for a brace.",
        );
    // Fixed height (scrolls): ticking a field must not resize the window and
    // move the checkboxes under the pointer.
    egui::ScrollArea::vertical()
        .id_salt((id, "template"))
        .max_height(96.0)
        .min_scrolled_height(96.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(template)
                    .desired_rows(5)
                    .desired_width(f32::INFINITY)
                    .char_limit(MAX_TEMPLATE_CHARS)
                    .font(egui::TextStyle::Monospace),
            );
        });
    let t = Template::<F>::parse(template);
    // One line, always present (the window keeps its size while typing).
    match t.errors.as_slice() {
        [] => ui.label(
            RichText::new("Fields in braces show live values; other text is shown as typed.")
                .size(theme::size::SMALL)
                .weak(),
        ),
        [e] => ui.colored_label(
            ui.visuals().warn_fg_color,
            RichText::new(e).size(theme::size::SMALL),
        ),
        [e, rest @ ..] => ui.colored_label(
            ui.visuals().warn_fg_color,
            RichText::new(format!("{e} (and {} more)", rest.len())).size(theme::size::SMALL),
        ),
    };
    egui::CollapsingHeader::new("Field reference")
        .id_salt((id, "fields"))
        .show(ui, |ui| {
            egui::Grid::new((id, "field-grid"))
                .num_columns(2)
                .show(ui, |ui| {
                    for &f in F::all() {
                        ui.monospace(format!("{{{}}}", f.key()));
                        ui.label(RichText::new(f.describe()).size(theme::size::SMALL));
                        ui.end_row();
                    }
                });
        });
}

/// State of the open Progressor Settings window.
pub struct SettingsWindow {
    /// Settings when the window opened (Cancel restores them).
    pub original: ProgressorSettings,
}

/// What the window asks the app to do this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowOutcome {
    Open,
    /// Keep the edited settings (save them).
    Ok,
    /// Restore the original settings.
    Cancel,
}

/// Shows the window; edits `s` in place (live preview in the piano roll).
pub fn settings_window(ctx: &egui::Context, s: &mut ProgressorSettings) -> WindowOutcome {
    let mut outcome = WindowOutcome::Open;
    let mut open = true;
    egui::Window::new("Progressor Settings")
        .id(egui::Id::new("progressor-settings"))
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        // Opens over the left of the piano roll, so the readout (top right by
        // default) stays visible while it is styled; it can be moved.
        .default_pos(ctx.content_rect().left_top() + Vec2::new(236.0, 96.0))
        .show(ctx, |ui| {
            ui.set_width(380.0);
            ui.checkbox(&mut s.enabled, "Show the Progressor");
            ui.add_space(space::MICRO);
            template_editor::<Token>(ui, "prog", &mut s.template, &FIELDS);
            ui.add_space(space::NORMAL);
            egui::Grid::new("progressor-grid")
                .num_columns(2)
                .spacing([space::NORMAL, space::MICRO])
                .show(ui, |ui| {
                    ui.label("Position");
                    egui::ComboBox::from_id_salt("prog-corner")
                        .selected_text(s.corner.label())
                        .show_ui(ui, |ui| {
                            for c in Corner::ALL {
                                ui.selectable_value(&mut s.corner, c, c.label());
                            }
                        });
                    ui.end_row();
                    ui.label("Offset");
                    ui.horizontal(|ui| {
                        ui.add(crate::ui::stepper::Stepper::new(&mut s.offset[0]).range(-2000.0..=2000.0).prefix("x "));
                        ui.add(crate::ui::stepper::Stepper::new(&mut s.offset[1]).range(-2000.0..=2000.0).prefix("y "));
                    });
                    ui.end_row();
                    ui.label("Font size");
                    crate::ui::stepper::slider(ui, &mut s.font_size, MIN_FONT..=MAX_FONT, "");
                    ui.end_row();
                    ui.label("Alignment");
                    ui.horizontal(|ui| {
                        for a in TextAlign::ALL {
                            ui.selectable_value(&mut s.align, a, a.label());
                        }
                    });
                    ui.end_row();
                    ui.label("Line spacing");
                    crate::ui::stepper::slider(ui, &mut s.line_spacing, 0.8..=3.0, "");
                    ui.end_row();
                    ui.label("Text color");
                    ui.horizontal(|ui| {
                        let mut custom = s.text_color.is_some();
                        if ui.checkbox(&mut custom, "Custom").changed() {
                            s.text_color = custom.then_some([0xF0, 0xE6, 0xF5]);
                        }
                        if let Some(c) = s.text_color.as_mut() {
                            ui.color_edit_button_srgb(c);
                        }
                    });
                    ui.end_row();
                    ui.label("Text opacity");
                    ui.add(egui::Slider::new(&mut s.text_opacity, 0..=100).suffix("%"));
                    ui.end_row();
                    ui.label("Background");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut s.background, "");
                        ui.add_enabled(
                            s.background,
                            egui::Slider::new(&mut s.background_opacity, 0..=100).suffix("%"),
                        );
                    });
                    ui.end_row();
                    ui.label("Numbers");
                    egui::ComboBox::from_id_salt("prog-numbers")
                        .selected_text(s.number_format.label())
                        .show_ui(ui, |ui| {
                            for f in NumberFormat::ALL {
                                ui.selectable_value(&mut s.number_format, f, f.label());
                            }
                        });
                    ui.end_row();
                    ui.label("PPS decimals");
                    ui.add(egui::Slider::new(&mut s.pps_decimals, 0..=3));
                    ui.end_row();
                    ui.label("Counts");
                    egui::ComboBox::from_id_salt("prog-scope")
                        .selected_text(s.scope.label())
                        .show_ui(ui, |ui| {
                            for c in ScopeSetting::ALL {
                                ui.selectable_value(&mut s.scope, c, c.label());
                            }
                        })
                        .response
                        .on_hover_text("Audible Tracks (the default, \u{201C}Respect Track Mute/Solo in Progressor\u{201D}) counts what plays: muted tracks are left out and, while any track is soloed, only soloed tracks count, exactly as playback applies Mute / Solo. All Tracks counts muted tracks too (the music as written). Onion Skin ghosts are never counted twice. The readout's {scope} says which.");
                    ui.end_row();
                    ui.label("NPS / PPS window");
                    ui.add(
                        crate::ui::stepper::Stepper::new(&mut s.window_ms)
                            .range(MIN_WINDOW_MS..=MAX_WINDOW_MS)
                            .suffix(" ms"),
                    )
                    .on_hover_text("Song time counted by NPS and averaged by PPS (default 1000 ms).");
                    ui.end_row();
                });
            ui.add_space(space::SECTION);
            ui.horizontal(|ui| {
                if ui.button("Reset to Default").clicked() {
                    // The Selection Analytics text has its own window.
                    let keep = std::mem::take(&mut s.selection_template);
                    *s = ProgressorSettings::default();
                    s.selection_template = keep;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("OK").clicked() {
                        outcome = WindowOutcome::Ok;
                    }
                    if ui.button("Cancel").clicked() {
                        outcome = WindowOutcome::Cancel;
                    }
                });
            });
        });
    if !open {
        outcome = WindowOutcome::Cancel;
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_replace_known_fields_and_flag_unknown_ones() {
        let t = Template::parse("LET'S GO! {notes}/{total_notes} {{x}} {nope} ✓ {pps");
        assert_eq!(t.errors.len(), 2, "{:?}", t.errors);
        let r = Readout {
            notes: 1234,
            total: 40_000_000,
            pps: 12_504.25,
            ..Readout::default()
        };
        let s = ProgressorSettings::default();
        let out = render_text(&t, &r, &s);
        assert_eq!(out, "LET'S GO! 1,234/40,000,000 {x} {nope} ✓ {pps");
        assert!(!t.uses(Token::needs_peaks));
        assert!(Template::parse("{ peak_poly }").uses(Token::needs_peaks));
    }

    #[test]
    fn number_formats() {
        assert_eq!(NumberFormat::Grouped.count(40_000_000), "40,000,000");
        assert_eq!(NumberFormat::Plain.count(40_000_000), "40000000");
        assert_eq!(NumberFormat::Compact.count(40_000_000), "40.0M");
        assert_eq!(NumberFormat::Compact.count(999), "999");
        assert_eq!(NumberFormat::Grouped.decimal(12_504.25, 1), "12,504.2");
        assert_eq!(NumberFormat::Plain.decimal(4.0, 1), "4.0");
        assert_eq!(NumberFormat::Grouped.decimal(f64::NAN, 1), "0.0");
        assert_eq!(NumberFormat::Compact.decimal(1_500_000.0, 1), "1.5M");
    }

    #[test]
    fn progress_percent_handles_an_empty_project_and_lines_are_bounded() {
        let t = Template::parse("{progress_percent}");
        let s = ProgressorSettings::default();
        assert_eq!(render_text(&t, &Readout::default(), &s), "0.0");
        let r = Readout {
            notes: 1,
            total: 3,
            ..Readout::default()
        };
        assert_eq!(render_text(&t, &r, &s), "33.3");
        let many = "x\n".repeat(100);
        let out = render_text(&Template::parse(&many), &r, &s);
        assert_eq!(out.lines().count(), MAX_LINES);
        let long = "y".repeat(MAX_TEMPLATE_CHARS + 50);
        assert!(!Template::<Token>::parse(&long).errors.is_empty());
    }

    #[test]
    fn settings_round_trip_and_bad_fields_fall_back() {
        let s = ProgressorSettings {
            template: "Ω {nps}".into(),
            corner: Corner::BottomLeft,
            text_color: Some([1, 2, 3]),
            number_format: NumberFormat::Compact,
            window_ms: 500,
            scope: ScopeSetting::Audible,
            selection_template: "Σ {notes} {unique_poly}".into(),
            ..ProgressorSettings::default()
        };
        let mut p = Vec::new();
        assert_eq!(ProgressorSettings::from_json(&s.to_json(), &mut p), s);
        assert!(p.is_empty());
        let bad = json!({"enabled": "yes", "corner": "middle", "font_size": 1000, "text_color": "#zz0000", "window_ms": 0});
        let got = ProgressorSettings::from_json(&bad, &mut p);
        assert!(got.enabled);
        assert_eq!(got.corner, Corner::TopRight);
        assert_eq!(got.font_size, MAX_FONT);
        assert_eq!(got.text_color, None);
        assert_eq!(got.window_ms, MIN_WINDOW_MS);
        assert_eq!(p.len(), 3, "{p:?}");
    }
}

#[cfg(test)]
mod overlay_tests {
    use super::*;

    /// B1-0065: a readout fades to 15 % under the pointer only.
    #[test]
    fn a_readout_fades_only_under_the_pointer() {
        let r = Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::new(100.0, 40.0));
        assert_eq!(overlay_fade(r, None), 1.0);
        assert_eq!(overlay_fade(r, Some(Pos2::new(300.0, 30.0))), 1.0);
        assert_eq!(overlay_fade(r, Some(Pos2::new(50.0, 30.0))), HOVER_FADE);
    }

    /// Dragging the chip moves the readout with the pointer from any corner
    /// (offsets point inwards), never out of the note area.
    #[test]
    fn dragging_moves_the_readout_with_the_pointer_from_every_corner() {
        let area = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 400.0));
        let d = Vec2::new(30.0, 20.0);
        assert_eq!(
            drag_offset([0.0, 0.0], Corner::TopLeft, d, area),
            [30.0, 20.0]
        );
        assert_eq!(
            drag_offset([0.0, 0.0], Corner::BottomRight, -d, area),
            [30.0, 20.0]
        );
        assert_eq!(
            drag_offset([40.0, 0.0], Corner::TopRight, d, area),
            [10.0, 20.0]
        );
        assert_eq!(
            drag_offset([0.0, 40.0], Corner::BottomLeft, d, area),
            [30.0, 20.0]
        );
        assert_eq!(
            drag_offset([0.0, 0.0], Corner::TopLeft, Vec2::new(5000.0, -500.0), area),
            [800.0, -8.0]
        );
    }

    /// Collapsed: the first line and "…"; the chip sits at the top-left
    /// inside the readout; the new settings persist and fall back alone.
    #[test]
    fn a_collapsed_readout_shows_one_line_and_settings_persist() {
        let ctx = egui::Context::default();
        let area = Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0));
        let s = ProgressorSettings::default();
        let pal = &crate::ui::theme::DARK;
        let mut views = Vec::new();
        let mut o = ctx.run_ui(egui::RawInput::default(), |ui| {
            let p = ui.painter().with_clip_rect(area);
            for collapsed in [false, true] {
                views.push(paint_overlay(
                    &p,
                    area,
                    "Notes: 1\nNPS: 2\nPoly: 3",
                    &s,
                    pal,
                    false,
                    OverlayState {
                        advanced: None,
                        pointer: None,
                        collapsed,
                        chip: true,
                    },
                ));
            }
        });
        let shown: Vec<String> = crate::ui::testkit::texts(&o.shapes)
            .into_iter()
            .map(|t| t.0)
            .collect();
        o.textures_delta.clear();
        assert!(shown.contains(&"Notes: 1 …".to_owned()), "{shown:?}");
        assert_eq!(
            shown.iter().filter(|t| t.starts_with("Poly")).count(),
            1,
            "{shown:?}"
        );
        let (full, small) = (views[0].unwrap(), views[1].unwrap());
        assert!(small.rect.height() < full.rect.height());
        for v in [full, small] {
            assert!(v.rect.contains_rect(v.chip));
            assert!((v.chip.left() - v.rect.left()) < 10.0 && (v.chip.top() - v.rect.top()) < 10.0);
        }
        let s2 = ProgressorSettings {
            collapsed: true,
            selection_collapsed: true,
            selection_offset: [12.0, -4.0],
            ..Default::default()
        };
        let mut problems = Vec::new();
        let back = ProgressorSettings::from_json(&s2.to_json(), &mut problems);
        assert_eq!(back, s2);
        assert!(problems.is_empty());
        // L-F01: the Selection readout is compact by default; a pre-launch
        // file's `selection_collapsed` (whose default was the full box) is
        // not read, the new key is.
        assert!(ProgressorSettings::default().selection_collapsed);
        let old =
            ProgressorSettings::from_json(&json!({ "selection_collapsed": false }), &mut problems);
        assert!(old.selection_collapsed);
        let detailed = ProgressorSettings {
            selection_collapsed: false,
            ..Default::default()
        };
        assert_eq!(
            ProgressorSettings::from_json(&detailed.to_json(), &mut problems),
            detailed
        );
        assert!(problems.is_empty(), "{problems:?}");
        let bad =
            serde_json::json!({"collapsed": "yes", "selection_offset": [1], "font_size": 14.0});
        let back = ProgressorSettings::from_json(&bad, &mut problems);
        assert!(!back.collapsed && back.selection_offset == [0.0, 0.0]);
        assert_eq!(back.font_size, 14.0, "others kept");
        assert_eq!(problems.len(), 2, "{problems:?}");
    }
}

/// B1-0106: Respect Track Mute/Solo in Progressor.
#[cfg(test)]
mod mute_solo_tests {
    use super::super::test_support::Harness;
    use super::*;
    use crate::app::commands::Command;
    use plumspirit_engine::history::{History, Operation};
    use plumspirit_engine::model::{Bpm, NoteAttrs, NoteOffKind, NoteRecord, PlumDocument};
    use std::time::{Duration, Instant};

    /// Tracks A (10 notes) with subtrack S (5), B (20), C (40); notes every
    /// 100 ticks from tick 0.
    fn song() -> PlumDocument {
        let mut d = PlumDocument::new_project("ms".into(), Bpm::DEFAULT, 480);
        d.add_track().unwrap();
        d.add_track().unwrap();
        History::default()
            .apply(
                &mut d,
                Operation::AddSubtracks {
                    parent: 0,
                    count: 1,
                },
            )
            .unwrap();
        assert_eq!(
            d.track(1).unwrap().parent(),
            Some(d.track(0).unwrap().id()),
            "S under A"
        );
        for (ti, n) in [(0usize, 10u64), (1, 5), (2, 20), (3, 40)] {
            let recs: Vec<NoteRecord> = (0..n)
                .map(|k| {
                    NoteRecord::new(
                        k * 100,
                        50,
                        NoteAttrs::new(60, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                    )
                })
                .collect();
            d.track_mut(ti)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }
        d
    }

    /// The readout "{notes}/{total_notes}|{scope}" at `tick`.
    fn readout(h: &mut Harness, tick: u64) -> String {
        h.app.settings.progressor.template = "{notes}/{total_notes}|{scope}".into();
        h.app.seek_to(tick);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let (doc, generation) = {
                let o = h.app.open.as_ref().unwrap();
                (o.doc.clone(), o.generation)
            };
            h.app.analysis.sync(&doc, generation);
            if let Some((t, false)) = h.app.progressor_text() {
                return t;
            }
            assert!(Instant::now() < deadline, "the analysis never caught up");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Starts at or before `tick` and all notes of the tracks playback plays
    /// (`playback::eligibility`), as "passed/total".
    fn brute(d: &PlumDocument, tick: u64) -> String {
        let el = plumspirit_engine::playback::eligibility(d);
        let (mut passed, mut total) = (0, 0);
        for (i, on) in el.iter().enumerate() {
            if *on {
                for n in d.track(i).unwrap().notes.iter() {
                    total += 1;
                    passed += u64::from(n.start_tick <= tick);
                }
            }
        }
        format!("{passed}/{total}")
    }

    #[test]
    fn the_progressor_counts_what_plays_by_default_and_everything_when_off() {
        type Setup = fn(&mut PlumDocument);
        let cases: [(&str, Setup, &str); 6] = [
            ("nothing muted", |_| {}, "75/75"),
            ("B muted", |d| d.track_mut(2).unwrap().mute = true, "55/55"),
            (
                "two solos (S and C) with B muted",
                |d| {
                    d.track_mut(2).unwrap().mute = true;
                    d.track_mut(1).unwrap().solo = true;
                    d.track_mut(3).unwrap().solo = true;
                },
                "45/45",
            ),
            (
                "solo of a subtrack under a muted parent (plays, as in playback)",
                |d| {
                    d.track_mut(0).unwrap().mute = true;
                    d.track_mut(1).unwrap().solo = true;
                },
                "5/5",
            ),
            (
                "solo of the parent only: its subtrack is not soloed",
                |d| d.track_mut(0).unwrap().solo = true,
                "10/10",
            ),
            (
                "C muted and soloed: Mute wins, no solo is in force",
                |d| {
                    d.track_mut(3).unwrap().solo = true;
                    d.track_mut(3).unwrap().mute = true;
                },
                "35/35",
            ),
        ];
        for (what, setup, want) in cases {
            let mut d = song();
            setup(&mut d);
            let mut h = Harness::new(d.clone());
            assert_eq!(h.app.settings.progressor.scope, ScopeSetting::Audible);
            let end = 10_000;
            assert_eq!(
                readout(&mut h, end),
                format!("{want}|Audible Tracks"),
                "{what}"
            );
            assert_eq!(
                format!("{}|Audible Tracks", brute(&d, end)),
                readout(&mut h, end)
            );
            // Seek back (current, not peak): the passed notes follow.
            for tick in [0, 250, 999, 1_950] {
                assert_eq!(
                    readout(&mut h, tick),
                    format!("{}|Audible Tracks", brute(&d, tick)),
                    "{what} at {tick}"
                );
            }
            // Off: every track, muted or not.
            assert!(h.command(Command::ToggleProgressorMuteSolo));
            assert_eq!(h.app.settings.progressor.scope, ScopeSetting::Project);
            assert_eq!(readout(&mut h, end), "75/75|All Tracks", "{what}");
            assert_eq!(readout(&mut h, 250), "12/75|All Tracks", "{what}");
            assert!(h.command(Command::ToggleProgressorMuteSolo));
            assert_eq!(readout(&mut h, end), format!("{want}|Audible Tracks"));
        }
    }

    /// Default ON; a pre-beta1 settings file (no "respect_mute_solo") with
    /// the old default "project" gets it, a beta1 file keeps its choice; the
    /// untouched old default template gains {scope}.
    #[test]
    fn respect_mute_solo_defaults_on_and_old_settings_migrate() {
        let d = ProgressorSettings::default();
        assert_eq!(d.scope, ScopeSetting::Audible);
        assert!(d.template.contains("{scope}"));
        let mut p = Vec::new();
        let old = json!({"scope": "project", "template": PRE_B1_DEFAULT_TEMPLATE});
        let got = ProgressorSettings::from_json(&old, &mut p);
        assert_eq!(got.scope, ScopeSetting::Audible);
        assert_eq!(got.template, DEFAULT_TEMPLATE);
        let off = ProgressorSettings {
            scope: ScopeSetting::Project,
            ..ProgressorSettings::default()
        };
        let back = ProgressorSettings::from_json(&off.to_json(), &mut p);
        assert_eq!(
            back.scope,
            ScopeSetting::Project,
            "an explicit Off stays off"
        );
        assert_eq!(off.to_json()["respect_mute_solo"], json!(false));
        let active = json!({"scope": "active_track"});
        assert_eq!(
            ProgressorSettings::from_json(&active, &mut p).scope,
            ScopeSetting::ActiveTrack
        );
        assert!(p.is_empty(), "{p:?}");
        assert_eq!(ScopeSetting::Project.short(), "All Tracks");
        assert_eq!(ScopeSetting::Audible.short(), "Audible Tracks");
        let r = Readout {
            scope: ScopeSetting::Audible.short(),
            notes: 3,
            total: 9,
            ..Readout::default()
        };
        let t = Template::parse("{notes}/{total_notes} · {scope}");
        assert_eq!(
            render_text(&t, &r, &ProgressorSettings::default()),
            "3/9 · Audible Tracks"
        );
    }
}
