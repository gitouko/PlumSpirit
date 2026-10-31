//! PlumSpirit Help (B1-0050 / B1-0051 / B1-0052): the Help window (F1, Help →
//! PlumSpirit Help…) with topics, search, back / forward, text size, context
//! "?" links from other windows, and the start screen's quick-start card.
//!
//! Everything is offline: the topics are text files in `assets/help/`
//! compiled into the program. A topic is parsed the first time it is shown
//! and the search index is built on the first search (lazy), so Help costs
//! nothing until it is opened. The text is a small Markdown subset:
//!
//! * `# ` / `## ` / `### ` headings, blank-line paragraphs, `* ` bullets (two
//!   spaces more per level), `1. ` steps, `> ` tips, `| … |` tables,
//!   fenced code;
//! * inline `**bold**`, `` `code` ``, `[text](#topic-id)` links;
//! * `{key:Command}` — the command's **current** shortcut (keybind overrides
//!   included) and `{cmd:Command}` — its menu label with the shortcut, so
//!   Help never names a shortcut or a menu entry that does not exist
//!   (`app::keybinds::command_id` names; the tests check every reference);
//! * a line `{fig:name}` — a drawn figure of a gesture ([`figure`]);
//! * a line `{shortcuts}` — the live table of every shortcut.

use super::commands::Command;
use crate::ui::theme::Palette;
use egui::{Color32, RichText, Stroke, Ui, Vec2};
use std::collections::HashMap;

/// One Help topic: an `assets/help/<id>.md` file.
pub struct Topic {
    pub id: &'static str,
    /// The group it is listed under.
    pub group: &'static str,
    pub text: &'static str,
}

impl Topic {
    /// The first line's `# ` heading.
    pub fn title(&self) -> &'static str {
        self.text
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("# "))
            .unwrap_or(self.id)
            .trim()
    }
}

macro_rules! topic {
    ($id:literal, $group:literal) => {
        Topic {
            id: $id,
            group: $group,
            text: include_str!(concat!("../../../../assets/help/", $id, ".md")),
        }
    };
}

/// Every topic, in the order the window lists them.
pub static TOPICS: &[Topic] = &[
    topic!("introduction", "Getting Started"),
    topic!("quick-start", "Getting Started"),
    topic!("glossary", "Getting Started"),
    topic!("shortcuts", "Getting Started"),
    topic!("projects-files", "Projects and Files"),
    topic!("midi-import-export", "Projects and Files"),
    topic!("track-list", "Arranging"),
    topic!("tracks", "Arranging"),
    topic!("conductor", "Arranging"),
    topic!("piano-roll", "Editing"),
    topic!("view", "Editing"),
    topic!("graphs", "Editing"),
    topic!("tools", "Tools and Plugins"),
    topic!("generators", "Tools and Plugins"),
    topic!("builtin-plugins", "Tools and Plugins"),
    topic!("lua-plugins", "Tools and Plugins"),
    topic!("analytics", "Listening and Analysis"),
    topic!("player-inspector", "Listening and Analysis"),
    topic!("playback", "Listening and Analysis"),
    topic!("settings", "Settings and Support"),
    topic!("performance", "Settings and Support"),
    topic!("troubleshooting", "Settings and Support"),
];

/// Topics the program opens by id (F1 context, "?" buttons, the quick-start
/// card); the tests check each exists.
#[cfg(test)]
pub const CONTEXT_TOPICS: &[&str] = &[
    "settings",
    "lua-plugins",
    "playback",
    "generators",
    "tools",
    "quick-start",
    "player-inspector",
    "track-list",
    "piano-roll",
];

/// The index of topic `id`.
pub fn topic_index(id: &str) -> Option<usize> {
    TOPICS.iter().position(|t| t.id == id)
}

/// Inline text of a paragraph, bullet, cell….
#[derive(Clone, Debug, PartialEq)]
pub enum Span {
    Text(String),
    Bold(String),
    Code(String),
    /// `[text](#topic)`.
    Link {
        text: String,
        topic: String,
    },
    /// `{key:Command}`: the command's current shortcut.
    Key(String),
    /// `{cmd:Command}`: the command's label and shortcut.
    Cmd(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading(u8, String),
    Para(Vec<Span>),
    /// A bullet with its level (0 = top).
    Bullet(u8, Vec<Span>),
    /// A numbered step.
    Step(u32, Vec<Span>),
    /// A `> ` tip.
    Tip(Vec<Span>),
    Code(String),
    /// Rows of cells; the first row is the header.
    Table(Vec<Vec<Vec<Span>>>),
    Figure(String),
    Shortcuts,
    /// `{tools}`: every tool of the Tools menu with its summary.
    Tools,
}

/// Parses a topic's text.
pub fn parse(text: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut para: Vec<String> = Vec::new();
    let mut table: Vec<Vec<Vec<Span>>> = Vec::new();
    let mut code: Option<String> = None;
    let flush_para = |para: &mut Vec<String>, out: &mut Vec<Block>| {
        if !para.is_empty() {
            out.push(Block::Para(spans(&para.join(" "))));
            para.clear();
        }
    };
    let flush_table = |table: &mut Vec<Vec<Vec<Span>>>, out: &mut Vec<Block>| {
        if !table.is_empty() {
            out.push(Block::Table(std::mem::take(table)));
        }
    };
    // Continuation lines of a bullet / step / tip join the item.
    let mut item: Option<Block> = None;
    let flush_item = |item: &mut Option<Block>, out: &mut Vec<Block>| {
        if let Some(b) = item.take() {
            out.push(b);
        }
    };
    for raw in text.lines() {
        if let Some(c) = code.as_mut() {
            if raw.trim_start().starts_with("```") {
                out.push(Block::Code(std::mem::take(c)));
                code = None;
            } else {
                if !c.is_empty() {
                    c.push('\n');
                }
                c.push_str(raw);
            }
            continue;
        }
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        let indent = (line.len() - trimmed.len()) as u8;
        if trimmed.starts_with("```") {
            flush_para(&mut para, &mut out);
            flush_item(&mut item, &mut out);
            flush_table(&mut table, &mut out);
            code = Some(String::new());
            continue;
        }
        if trimmed.is_empty() {
            flush_para(&mut para, &mut out);
            flush_item(&mut item, &mut out);
            flush_table(&mut table, &mut out);
            continue;
        }
        if trimmed.starts_with('|') {
            flush_para(&mut para, &mut out);
            flush_item(&mut item, &mut out);
            let cells: Vec<&str> = trimmed
                .trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect();
            if cells
                .iter()
                .all(|c| !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':'))
            {
                continue; // the header separator
            }
            table.push(cells.iter().map(|c| spans(c)).collect());
            continue;
        }
        flush_table(&mut table, &mut out);
        let heading = trimmed
            .strip_prefix("### ")
            .map(|t| (3, t))
            .or_else(|| trimmed.strip_prefix("## ").map(|t| (2, t)))
            .or_else(|| trimmed.strip_prefix("# ").map(|t| (1, t)));
        if let Some((level, t)) = heading {
            flush_para(&mut para, &mut out);
            flush_item(&mut item, &mut out);
            out.push(Block::Heading(level, t.trim().to_owned()));
            continue;
        }
        if trimmed == "{shortcuts}" || trimmed == "{tools}" {
            flush_para(&mut para, &mut out);
            flush_item(&mut item, &mut out);
            out.push(if trimmed == "{tools}" {
                Block::Tools
            } else {
                Block::Shortcuts
            });
            continue;
        }
        if let Some(name) = trimmed
            .strip_prefix("{fig:")
            .and_then(|r| r.strip_suffix('}'))
        {
            flush_para(&mut para, &mut out);
            flush_item(&mut item, &mut out);
            out.push(Block::Figure(name.to_owned()));
            continue;
        }
        let bullet = trimmed
            .strip_prefix("* ")
            .or_else(|| trimmed.strip_prefix("- "));
        if let Some(t) = bullet {
            flush_para(&mut para, &mut out);
            flush_item(&mut item, &mut out);
            item = Some(Block::Bullet((indent / 2).min(3), spans(t)));
            continue;
        }
        if let Some((n, t)) = step_of(trimmed) {
            flush_para(&mut para, &mut out);
            flush_item(&mut item, &mut out);
            item = Some(Block::Step(n, spans(t)));
            continue;
        }
        if let Some(t) = trimmed
            .strip_prefix("> ")
            .or_else(|| (trimmed == ">").then_some(""))
        {
            flush_para(&mut para, &mut out);
            match item.as_mut() {
                Some(Block::Tip(s)) => extend(s, t),
                _ => {
                    flush_item(&mut item, &mut out);
                    item = Some(Block::Tip(spans(t)));
                }
            }
            continue;
        }
        // A continuation of the open item, or paragraph text.
        match item.as_mut() {
            Some(Block::Bullet(_, s) | Block::Step(_, s) | Block::Tip(s)) if indent > 0 => {
                extend(s, trimmed);
            }
            _ => {
                flush_item(&mut item, &mut out);
                para.push(trimmed.to_owned());
            }
        }
    }
    if let Some(c) = code {
        out.push(Block::Code(c));
    }
    flush_para(&mut para, &mut out);
    flush_item(&mut item, &mut out);
    flush_table(&mut table, &mut out);
    out
}

fn extend(s: &mut Vec<Span>, more: &str) {
    if more.is_empty() {
        return;
    }
    s.push(Span::Text(" ".into()));
    s.extend(spans(more));
}

/// `12. text` → (12, text).
fn step_of(line: &str) -> Option<(u32, &str)> {
    let dot = line.find(". ")?;
    let n = line[..dot].parse().ok()?;
    Some((n, &line[dot + 2..]))
}

/// The inline spans of `text`.
pub fn spans(text: &str) -> Vec<Span> {
    let mut out = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    let flush = |plain: &mut String, out: &mut Vec<Span>| {
        if !plain.is_empty() {
            out.push(Span::Text(std::mem::take(plain)));
        }
    };
    while let Some(c) = rest.chars().next() {
        let taken = match c {
            '*' if rest.starts_with("**") => rest[2..]
                .find("**")
                .map(|end| (Span::Bold(rest[2..2 + end].to_owned()), 4 + end)),
            '`' => rest[1..]
                .find('`')
                .map(|end| (Span::Code(rest[1..1 + end].to_owned()), 2 + end)),
            '[' => link_at(rest),
            '{' => ref_at(rest),
            _ => None,
        };
        match taken {
            Some((span, len)) => {
                flush(&mut plain, &mut out);
                out.push(span);
                rest = &rest[len..];
            }
            None => {
                plain.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    flush(&mut plain, &mut out);
    out
}

/// `[text](#topic)` at the start of `s`, and its length.
fn link_at(s: &str) -> Option<(Span, usize)> {
    let close = s.find("](#")?;
    let end = s[close..].find(')')? + close;
    let text = &s[1..close];
    if text.contains('[') {
        return None;
    }
    Some((
        Span::Link {
            text: text.to_owned(),
            topic: s[close + 3..end].to_owned(),
        },
        end + 1,
    ))
}

/// `{key:X}` / `{cmd:X}` at the start of `s`, and its length.
fn ref_at(s: &str) -> Option<(Span, usize)> {
    let end = s.find('}')?;
    let inner = &s[1..end];
    if let Some(id) = inner.strip_prefix("key:") {
        return Some((Span::Key(id.to_owned()), end + 1));
    }
    inner
        .strip_prefix("cmd:")
        .map(|id| (Span::Cmd(id.to_owned()), end + 1))
}

/// Commands of the menu bar (and a few fixed ones) Help may name besides
/// the keybind table's rows.
const MENU_ONLY: &[Command] = &[
    Command::AddClip,
    Command::AddMarker,
    Command::AddSubchannel,
    Command::AddSubchannels,
    Command::AddSubtrack,
    Command::AddSubtracks,
    Command::AddTempoChange,
    Command::AddTimeSignature,
    Command::AddTrack,
    Command::ChooseOnionTracks,
    Command::OnionAddSelected,
    Command::OnionRemoveSelected,
    Command::OnionUseForEveryTrack,
    Command::ChooseSoundFont,
    Command::ClearLoop,
    Command::CloseProject,
    Command::Copy,
    Command::CustomFunctions,
    Command::Cut,
    Command::Delete,
    Command::DeleteSelectedTracks,
    Command::DuplicateOctaveDown,
    Command::DuplicateOctaveUp,
    Command::Exit,
    Command::Export,
    Command::FitAllKeys,
    Command::FitKeys128,
    Command::FitSong,
    Command::FitTrackEnd,
    Command::FlipX,
    Command::FlipY,
    Command::FollowPosition,
    Command::GoToEnd,
    Command::GoToStart,
    Command::GroupNotes,
    Command::ImportIntoTrack,
    Command::IndentTrack,
    Command::InsertScore,
    Command::LoopToSelection,
    Command::MidiPlayerSettings,
    Command::MoveToSubchannel,
    Command::MoveTrackDown,
    Command::MoveTrackUp,
    Command::MuteNotes,
    Command::NewProject,
    Command::NextMeasure,
    Command::OnionOpacity,
    Command::Open,
    Command::OpenSettings,
    Command::OutdentTrack,
    Command::PaintCurrentSubchannel,
    Command::Paste,
    Command::PasteRepeat,
    Command::PlayAll,
    Command::PlayPause,
    Command::PlayingTime,
    Command::PreviousMeasure,
    Command::ProgressorSettings,
    Command::ProjectProperties,
    Command::Quantize,
    Command::RecentTasks,
    Command::Redo,
    Command::ReopenLastTool,
    Command::RepeatLastTool,
    Command::RepeatLastToolNewSeed,
    Command::ResetUiScale,
    Command::ResetView,
    Command::ResetZoom,
    Command::Save,
    Command::SaveAs,
    Command::SaveAutomationAs,
    Command::SaveDiagnostics,
    Command::SaveScoreAs,
    Command::ScaleX,
    Command::ScaleY,
    Command::ScrubTool,
    Command::SelectAll,
    Command::SelectConductor,
    Command::SelectRange,
    Command::SelectionAnalyticsSettings,
    Command::SelectionOpacity,
    Command::ShowAbout,
    Command::ShowHelp,
    Command::ShowKeybinds,
    Command::ShowLuaPlugins,
    Command::ShowQuickStart,
    Command::SliceNotes,
    Command::Stop,
    Command::Subchannels,
    Command::SwitchView,
    Command::ToggleAnchorPlayhead,
    Command::ToggleAudition,
    Command::ToggleDisplayNoteColors,
    Command::ToggleEventList,
    Command::ToggleExplicitStatus,
    Command::ToggleFollow,
    Command::ToggleGraph2,
    Command::ToggleHighContrast,
    Command::ToggleKeyGuide,
    Command::ToggleLiveKeys,
    Command::ToggleLoudnessReference,
    Command::ToggleLockFitRange,
    Command::ToggleLoop,
    Command::ToggleMetrics,
    Command::ToggleMidiInspector,
    Command::ToggleMidiPlayer,
    Command::ToggleMuteNotes,
    Command::ToggleOnionOutlines,
    Command::ToggleOnionPerTrack,
    Command::ToggleOrientation,
    Command::ToggleOverlapAcross,
    Command::ToggleOverlapExisting,
    Command::ToggleOverlapGeneratedOnly,
    Command::ToggleOverlapPrevention,
    Command::TogglePlayerSafeStart,
    Command::TogglePreview,
    Command::ToggleProgressor,
    Command::ToggleProgressorMuteSolo,
    Command::ToggleRunLuaPlugins,
    Command::ToggleScaleYDuplicate,
    Command::ToggleScaleYOverlap,
    Command::ToggleSelectionAnalytics,
    Command::ToggleSelectionCharts,
    Command::ToggleSkipChannel10,
    Command::ToggleSmoothFollow,
    Command::ToggleTextOverlays,
    Command::ToggleVelocityLane,
    Command::ToggleYZoomBeyondKeys,
    Command::TrackProperties,
    Command::Undo,
    Command::UndoHistory,
    Command::UngroupNotes,
    Command::UnmuteNotes,
    Command::ZoomBack,
    Command::ZoomForward,
    Command::ZoomInY,
    Command::ZoomOutY,
    Command::ZoomTool,
    Command::OpenClip,
];

/// The command a `{key:…}` / `{cmd:…}` reference names.
pub fn command_named(id: &str) -> Option<Command> {
    super::keybinds::command_from_id(id).or_else(|| {
        MENU_ONLY
            .iter()
            .copied()
            .find(|c| super::keybinds::command_id(*c) == id)
    })
}

/// What a `{key:…}` shows: the current shortcut(s), or a note that there
/// is none.
pub fn key_text(id: &str) -> String {
    match command_named(id) {
        Some(c) => {
            let s = c.shortcut();
            if s.is_empty() {
                "(no shortcut)".to_owned()
            } else {
                s
            }
        }
        None => format!("{{{id}?}}"),
    }
}

/// What a `{cmd:…}` shows: the menu label, with its shortcut.
pub fn cmd_text(id: &str) -> String {
    match command_named(id) {
        Some(c) => {
            let s = c.shortcut();
            if s.is_empty() {
                c.label().to_owned()
            } else {
                format!("{} ({s})", c.label())
            }
        }
        None => format!("{{{id}?}}"),
    }
}

/// The plain text of spans (search, snippets).
fn plain_spans(s: &[Span], out: &mut String) {
    for sp in s {
        match sp {
            Span::Text(t) | Span::Bold(t) | Span::Code(t) => out.push_str(t),
            Span::Link { text, .. } => out.push_str(text),
            Span::Key(id) => out.push_str(&key_text(id)),
            Span::Cmd(id) => out.push_str(&cmd_text(id)),
        }
    }
}

/// One searchable piece of a topic (a heading or a paragraph / item / row).
struct Piece {
    heading: bool,
    lower: String,
    text: String,
}

/// The search index: every topic's pieces, built on the first search.
pub struct Index {
    topics: Vec<(String, Vec<Piece>)>,
}

impl Index {
    pub fn build() -> Index {
        let topics = TOPICS
            .iter()
            .map(|t| {
                let mut pieces = Vec::new();
                for b in parse(t.text) {
                    let (heading, mut text) = (matches!(b, Block::Heading(..)), String::new());
                    match &b {
                        Block::Heading(_, h) => text.push_str(h),
                        Block::Para(s)
                        | Block::Bullet(_, s)
                        | Block::Step(_, s)
                        | Block::Tip(s) => plain_spans(s, &mut text),
                        Block::Table(rows) => {
                            for r in rows {
                                let mut row = String::new();
                                for c in r {
                                    if !row.is_empty() {
                                        row.push_str(" · ");
                                    }
                                    plain_spans(c, &mut row);
                                }
                                pieces.push(Piece {
                                    heading: false,
                                    lower: row.to_lowercase(),
                                    text: row,
                                });
                            }
                            continue;
                        }
                        Block::Code(c) => text.push_str(c),
                        Block::Figure(_) => continue,
                        Block::Tools => {
                            for (section, name, summary) in tool_rows() {
                                let row = format!("{section} · {name} · {summary}");
                                pieces.push(Piece {
                                    heading: false,
                                    lower: row.to_lowercase(),
                                    text: row,
                                });
                            }
                            continue;
                        }
                        Block::Shortcuts => {
                            for k in super::keybinds::effective().iter() {
                                let row = format!(
                                    "{} · {}",
                                    k.command.label(),
                                    super::keybinds::chord_text(k.mods, k.key)
                                );
                                pieces.push(Piece {
                                    heading: false,
                                    lower: row.to_lowercase(),
                                    text: row,
                                });
                            }
                            continue;
                        }
                    }
                    pieces.push(Piece {
                        heading,
                        lower: text.to_lowercase(),
                        text,
                    });
                }
                (t.title().to_lowercase(), pieces)
            })
            .collect();
        Index { topics }
    }

    /// Topics containing every word of `query` (case ignored), best first:
    /// (topic index, score, a snippet).
    pub fn search(&self, query: &str) -> Vec<(usize, u32, String)> {
        let words: Vec<String> = query
            .split_whitespace()
            .map(str::to_lowercase)
            .filter(|w| !w.is_empty())
            .collect();
        if words.is_empty() {
            return Vec::new();
        }
        let mut hits = Vec::new();
        for (i, (title, pieces)) in self.topics.iter().enumerate() {
            let mut score = 0u32;
            let mut all = true;
            for w in &words {
                let mut found = 0u32;
                if title.contains(w.as_str()) {
                    found += 20;
                }
                for p in pieces {
                    let n = p.lower.matches(w.as_str()).count() as u32;
                    found += n * if p.heading { 5 } else { 1 };
                }
                if found == 0 {
                    all = false;
                    break;
                }
                score += found;
            }
            if !all {
                continue;
            }
            let snippet = pieces
                .iter()
                .find(|p| !p.heading && words.iter().all(|w| p.lower.contains(w.as_str())))
                .or_else(|| {
                    pieces
                        .iter()
                        .find(|p| !p.heading && p.lower.contains(words[0].as_str()))
                })
                .map(|p| snippet_of(&p.text, &p.lower, &words[0]))
                .unwrap_or_default();
            hits.push((i, score, snippet));
        }
        hits.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        hits
    }
}

/// About 120 characters of `text` around the first `word`.
fn snippet_of(text: &str, lower: &str, word: &str) -> String {
    let at = lower.find(word).unwrap_or(0);
    // `lower` may differ in byte length from `text` for a few characters:
    // fall back to the start then.
    let at = if lower.len() == text.len() { at } else { 0 };
    let mut start = at.saturating_sub(40);
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (start + 120).min(text.len());
    while !text.is_char_boundary(end) {
        end += 1;
    }
    let mut s = String::new();
    if start > 0 {
        s.push('…');
    }
    s.push_str(text[start..end].trim());
    if end < text.len() {
        s.push('…');
    }
    s
}

/// The Help window's state (view state; the text size is a setting).
#[derive(Default)]
pub struct HelpWindow {
    pub open: bool,
    pub current: usize,
    back: Vec<usize>,
    forward: Vec<usize>,
    pub query: String,
    focus_search: bool,
    scroll_top: bool,
    parsed: HashMap<usize, Vec<Block>>,
    index: Option<Index>,
}

impl HelpWindow {
    /// Opens the window at topic `id` (unknown ids: the introduction).
    pub fn show_topic(&mut self, id: &str) {
        let i = topic_index(id).unwrap_or(0);
        if self.open && i != self.current {
            self.back.push(self.current);
            self.forward.clear();
        }
        self.current = i;
        self.open = true;
        self.query.clear();
        self.scroll_top = true;
    }

    fn go(&mut self, i: usize) {
        if i != self.current {
            self.back.push(self.current);
            self.forward.clear();
            self.current = i;
            self.scroll_top = true;
        }
    }

    /// The parsed blocks of topic `i` (parsed once).
    fn blocks(&mut self, i: usize) -> &[Block] {
        self.parsed
            .entry(i)
            .or_insert_with(|| parse(TOPICS[i].text))
    }

    /// Search results for the current query (the index is built once).
    pub fn results(&mut self) -> Vec<(usize, u32, String)> {
        if self.query.trim().is_empty() {
            return Vec::new();
        }
        self.index
            .get_or_insert_with(Index::build)
            .search(&self.query)
    }

    /// The window was built (tests: lazy loading).
    #[cfg(test)]
    pub fn index_built(&self) -> bool {
        self.index.is_some()
    }

    #[cfg(test)]
    pub fn parsed_count(&self) -> usize {
        self.parsed.len()
    }
}

/// Text sizes at `percent` of the normal size.
struct Sizes {
    body: f32,
    h1: f32,
    h2: f32,
    h3: f32,
    small: f32,
}

impl Sizes {
    fn at(percent: u16) -> Sizes {
        let k = f32::from(percent.clamp(HELP_TEXT_MIN, HELP_TEXT_MAX)) / 100.0;
        Sizes {
            body: 14.0 * k,
            h1: 22.0 * k,
            h2: 18.0 * k,
            h3: 15.5 * k,
            small: 12.0 * k,
        }
    }
}

/// The Help text size range (percent of normal; B1-0050 accessible sizes).
pub const HELP_TEXT_MIN: u16 = 80;
pub const HELP_TEXT_MAX: u16 = 200;
pub const HELP_TEXT_STEP: u16 = 10;

/// The topic list's preferred width (points).
pub const HELP_LIST_WIDTH: f32 = 230.0;
/// The narrowest the topic list gets before the article gives way.
pub const HELP_LIST_MIN: f32 = 140.0;
/// The narrowest article the list makes room for (about 40 characters of
/// body text at 100 %).
pub const HELP_ARTICLE_MIN: f32 = 260.0;
/// The widest article line (a comfortable reading measure).
pub const HELP_ARTICLE_MAX: f32 = 760.0;

/// (list, article) widths for `avail` points of window content (B2-D03): the
/// list keeps its width while the article has at least [`HELP_ARTICLE_MIN`],
/// then shrinks down to [`HELP_LIST_MIN`]; the article takes the rest.
pub fn pane_widths(avail: f32) -> (f32, f32) {
    let avail = avail.max(0.0);
    let list = (avail - HELP_ARTICLE_MIN).clamp(HELP_LIST_MIN, HELP_LIST_WIDTH);
    (list, (avail - list).max(0.0))
}

/// The article's wrap width inside its pane.
pub fn article_width(pane: f32) -> f32 {
    pane.clamp(0.0, HELP_ARTICLE_MAX)
}

/// What the window asked the app for.
#[derive(Debug, PartialEq)]
pub enum HelpAction {
    Run(Command),
    TextSize(u16),
}

/// Draws the Help window; returns what it asks the app to do.
pub fn help_window(
    ctx: &egui::Context,
    w: &mut HelpWindow,
    text_percent: u16,
    pal: &Palette,
) -> Vec<HelpAction> {
    let mut actions = Vec::new();
    if !w.open {
        return actions;
    }
    let mut open = true;
    let sizes = Sizes::at(text_percent);
    egui::Window::new("PlumSpirit Help")
        .id(egui::Id::new("plumspirit-help"))
        .open(&mut open)
        .default_size(Vec2::new(860.0, 600.0))
        .min_width(420.0)
        .min_height(260.0)
        .resizable(true)
        .collapsible(false)
        .show(ctx, |ui| {
            // Toolbar: Back / Forward, search, text size, shortcuts. It wraps
            // onto a second row in a narrow window instead of widening it.
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(!w.back.is_empty(), egui::Button::new("◀ Back"))
                    .on_hover_text("The topic you read before.")
                    .clicked()
                {
                    let prev = w.back.pop().unwrap_or(0);
                    w.forward.push(w.current);
                    w.current = prev;
                    w.scroll_top = true;
                }
                if ui
                    .add_enabled(!w.forward.is_empty(), egui::Button::new("Forward ▶"))
                    .on_hover_text("Forward again.")
                    .clicked()
                {
                    let next = w.forward.pop().unwrap_or(0);
                    w.back.push(w.current);
                    w.current = next;
                    w.scroll_top = true;
                }
                ui.separator();
                let search = ui.add(
                    egui::TextEdit::singleline(&mut w.query)
                        .hint_text("Search Help")
                        .desired_width(220.0),
                );
                if w.focus_search {
                    search.request_focus();
                    w.focus_search = false;
                }
                if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::F)) {
                    search.request_focus();
                }
                if !w.query.is_empty()
                    && ui
                        .small_button("×")
                        .on_hover_text("Clear the search.")
                        .clicked()
                {
                    w.query.clear();
                }
                ui.separator();
                ui.label("Text size").on_hover_text(
                    "The size of the Help text only (Settings → Interface & Accessibility → UI Scale changes everything).",
                );
                if ui
                    .add_enabled(text_percent > HELP_TEXT_MIN, egui::Button::new("A−"))
                    .on_hover_text("Smaller Help text.")
                    .clicked()
                {
                    actions.push(HelpAction::TextSize(
                        text_percent
                            .saturating_sub(HELP_TEXT_STEP)
                            .max(HELP_TEXT_MIN),
                    ));
                }
                ui.label(format!("{text_percent} %"));
                if ui
                    .add_enabled(text_percent < HELP_TEXT_MAX, egui::Button::new("A+"))
                    .on_hover_text("Larger Help text.")
                    .clicked()
                {
                    actions.push(HelpAction::TextSize(
                        (text_percent + HELP_TEXT_STEP).min(HELP_TEXT_MAX),
                    ));
                }
                if text_percent != 100
                    && ui
                        .small_button("100 %")
                        .on_hover_text("Normal size.")
                        .clicked()
                {
                    actions.push(HelpAction::TextSize(100));
                }
                ui.separator();
                if ui
                    .button("Keyboard Shortcuts…")
                    .on_hover_text("Change shortcuts in the Keyboard Shortcuts window.")
                    .clicked()
                {
                    actions.push(HelpAction::Run(Command::ShowKeybinds));
                }
            });
            ui.separator();
            let results = w.results();
            let searching = !w.query.trim().is_empty();
            // B2-D03: the topic list and the article are separate panes. The
            // article is laid out top to bottom at a known reading width; in a
            // narrow window the list gives way first, never the article.
            let (list_w, _) = pane_widths(ui.available_width());
            egui::Panel::left("help-topic-list")
                .resizable(false)
                .exact_size(list_w)
                .frame(egui::Frame::new().inner_margin(egui::Margin {
                    right: 6,
                    ..Default::default()
                }))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("help-topics")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if searching {
                                if results.is_empty() {
                                    ui.label(
                                        RichText::new("No topic contains every word.")
                                            .color(pal.text_weak),
                                    );
                                }
                                for (i, _, snippet) in &results {
                                    let r = ui.selectable_label(
                                        *i == w.current,
                                        RichText::new(TOPICS[*i].title()).strong(),
                                    );
                                    ui.label(
                                        RichText::new(snippet.as_str())
                                            .size(sizes.small)
                                            .color(pal.text_weak),
                                    );
                                    ui.add_space(4.0);
                                    if r.clicked() {
                                        w.go(*i);
                                    }
                                }
                            } else {
                                let mut group = "";
                                for (i, t) in TOPICS.iter().enumerate() {
                                    if t.group != group {
                                        group = t.group;
                                        ui.add_space(4.0);
                                        ui.label(RichText::new(group).color(pal.text_weak).small());
                                    }
                                    if ui.selectable_label(i == w.current, t.title()).clicked() {
                                        w.go(i);
                                    }
                                }
                            }
                        });
                });
            egui::CentralPanel::default()
                .frame(egui::Frame::new().inner_margin(egui::Margin {
                    left: 8,
                    ..Default::default()
                }))
                .show(ui, |ui| {
                    let mut scroll = egui::ScrollArea::vertical()
                        .id_salt(("help-page", w.current))
                        .auto_shrink([false, false]);
                    if std::mem::take(&mut w.scroll_top) {
                        scroll = scroll.vertical_scroll_offset(0.0);
                    }
                    let cur = w.current;
                    let blocks = w.blocks(cur).to_vec();
                    let mut goto = None;
                    scroll.show(ui, |ui| {
                        // Top to bottom, at the reading width (the scroll
                        // area's own layout follows its parent's).
                        let width = article_width(ui.available_width());
                        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                            ui.set_width(width);
                            goto = show_blocks(ui, &blocks, &sizes, pal);
                        });
                    });
                    if let Some(i) = goto.as_deref().and_then(topic_index) {
                        w.go(i);
                    }
                });
        });
    if !open {
        w.open = false;
    }
    actions
}

/// Draws `blocks`; returns a clicked link's topic.
fn show_blocks(ui: &mut Ui, blocks: &[Block], sz: &Sizes, pal: &Palette) -> Option<String> {
    let mut goto = None;
    for b in blocks {
        match b {
            Block::Heading(level, t) => {
                let size = match level {
                    1 => sz.h1,
                    2 => sz.h2,
                    _ => sz.h3,
                };
                ui.add_space(if *level == 1 { 2.0 } else { 10.0 });
                ui.label(RichText::new(t).size(size).strong().color(pal.text));
                ui.add_space(4.0);
            }
            Block::Para(s) => {
                inline(ui, s, sz, pal, &mut goto);
                ui.add_space(6.0);
            }
            Block::Bullet(level, s) => {
                ui.horizontal_top(|ui| {
                    ui.add_space(6.0 + 18.0 * f32::from(*level));
                    ui.label(RichText::new("•").size(sz.body));
                    ui.vertical(|ui| inline(ui, s, sz, pal, &mut goto));
                });
                ui.add_space(2.0);
            }
            Block::Step(n, s) => {
                ui.horizontal_top(|ui| {
                    ui.add_space(4.0);
                    ui.label(RichText::new(format!("{n}.")).size(sz.body).strong());
                    ui.vertical(|ui| inline(ui, s, sz, pal, &mut goto));
                });
                ui.add_space(3.0);
            }
            Block::Tip(s) => {
                egui::Frame::new()
                    .fill(pal.accent_weak.gamma_multiply(0.35))
                    .stroke(Stroke::new(1.0, pal.accent_weak))
                    .corner_radius(crate::ui::theme::CORNER)
                    .inner_margin(8.0)
                    .show(ui, |ui| inline(ui, s, sz, pal, &mut goto));
                ui.add_space(6.0);
            }
            Block::Code(c) => {
                egui::Frame::new()
                    .fill(pal.panel_bg)
                    .stroke(Stroke::new(1.0, pal.panel_border))
                    .inner_margin(6.0)
                    .show(ui, |ui| {
                        ui.label(RichText::new(c).monospace().size(sz.small));
                    });
                ui.add_space(6.0);
            }
            Block::Table(rows) => {
                let cols = rows.iter().map(Vec::len).max().unwrap_or(1);
                let cw = cell_width(ui.available_width(), cols, 14.0);
                egui::Grid::new(ui.next_auto_id())
                    .striped(true)
                    .spacing(Vec2::new(14.0, 4.0))
                    .max_col_width(cw)
                    .show(ui, |ui| {
                        for (r, row) in rows.iter().enumerate() {
                            for cell in row {
                                ui.vertical(|ui| {
                                    ui.set_max_width(cw);
                                    if r == 0 {
                                        let mut t = String::new();
                                        plain_spans(cell, &mut t);
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(t).size(sz.body).strong(),
                                            )
                                            .wrap(),
                                        );
                                    } else {
                                        inline(ui, cell, sz, pal, &mut goto);
                                    }
                                });
                            }
                            ui.end_row();
                        }
                    });
                ui.add_space(8.0);
            }
            Block::Figure(name) => {
                figure(ui, name, pal);
                ui.add_space(8.0);
            }
            Block::Shortcuts => {
                shortcut_table(ui, sz);
                ui.add_space(8.0);
            }
            Block::Tools => {
                tool_table(ui, sz, pal);
                ui.add_space(8.0);
            }
        }
    }
    goto
}

/// A paragraph's spans, wrapped.
fn inline(ui: &mut Ui, s: &[Span], sz: &Sizes, pal: &Palette, goto: &mut Option<String>) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for sp in s {
            match sp {
                Span::Text(t) => {
                    ui.label(RichText::new(t).size(sz.body).color(pal.text));
                }
                Span::Bold(t) => {
                    ui.label(RichText::new(t).size(sz.body).strong().color(pal.text));
                }
                Span::Code(t) => {
                    ui.label(RichText::new(t).size(sz.body).code());
                }
                Span::Link { text, topic } => {
                    let title = topic_index(topic).map_or("", |i| TOPICS[i].title());
                    if ui
                        .link(RichText::new(text).size(sz.body))
                        .on_hover_text(format!("Help: {title}"))
                        .clicked()
                    {
                        *goto = Some(topic.clone());
                    }
                }
                Span::Key(id) => {
                    ui.label(RichText::new(key_text(id)).size(sz.body).strong());
                }
                Span::Cmd(id) => {
                    ui.label(RichText::new(cmd_text(id)).size(sz.body).strong());
                }
            }
        }
    });
}

/// The width of each of `cols` table columns sharing `avail` points with
/// `spacing` between them (B2-D03: a table never widens the window; at most
/// 360 pt per column, as before).
fn cell_width(avail: f32, cols: usize, spacing: f32) -> f32 {
    let cols = cols.max(1) as f32;
    ((avail - spacing * (cols - 1.0)) / cols).clamp(40.0, 360.0)
}

/// The live shortcut table, by section of the Keyboard Shortcuts window.
fn shortcut_table(ui: &mut Ui, sz: &Sizes) {
    let table = super::keybinds::effective();
    let mut groups: Vec<&'static str> = Vec::new();
    for k in table.iter() {
        if !groups.contains(&k.group) {
            groups.push(k.group);
        }
    }
    for g in groups {
        ui.add_space(6.0);
        ui.label(RichText::new(g).size(sz.h3).strong());
        let cw = cell_width(ui.available_width(), 2, 18.0);
        egui::Grid::new(("help-keys", g))
            .striped(true)
            .spacing(Vec2::new(18.0, 2.0))
            .max_col_width(cw)
            .show(ui, |ui| {
                let mut seen: Vec<Command> = Vec::new();
                for k in table.iter().filter(|k| k.group == g) {
                    if seen.contains(&k.command) {
                        continue;
                    }
                    seen.push(k.command);
                    ui.add(egui::Label::new(RichText::new(k.command.label()).size(sz.body)).wrap());
                    ui.add(
                        egui::Label::new(
                            RichText::new(k.command.shortcut()).size(sz.body).strong(),
                        )
                        .wrap(),
                    );
                    ui.end_row();
                }
            });
    }
}

/// Every tool of the Tools menu: (section, name, summary), in menu order.
pub fn tool_rows() -> Vec<(&'static str, &'static str, &'static str)> {
    let mut v = Vec::new();
    for s in crate::ui::menu::tool_menu_sections(&plumspirit_engine::transform::definition::LEGACY)
    {
        for id in &s.tools {
            if let Some(def) = plumspirit_engine::transform::tool(id) {
                let spec = (def.spec)();
                v.push((s.label, spec.name, spec.summary));
            }
        }
    }
    v
}

/// The live tool list, by Tools menu section.
fn tool_table(ui: &mut Ui, sz: &Sizes, pal: &Palette) {
    let rows = tool_rows();
    let mut section = "";
    for (s, name, summary) in rows {
        if s != section {
            section = s;
            ui.add_space(6.0);
            ui.label(RichText::new(format!("Tools → {s}")).size(sz.h3).strong());
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.label(RichText::new(name).size(sz.body).strong().color(pal.text));
            ui.label(
                RichText::new(format!(" — {summary}"))
                    .size(sz.body)
                    .color(pal.text),
            );
        });
    }
}

/// The figures Help can draw (`{fig:name}`; the tests check every use).
#[cfg(test)]
pub const FIGURES: &[&str] = &[
    "scale-handles",
    "rotate",
    "ruler-selection",
    "track-list-clips",
    "draw-notes",
    "onion-skin",
];

/// The narrowest a figure's drawing gets (its shapes reach about 300 pt).
pub const FIGURE_MIN: f32 = 300.0;

/// Draws figure `name`: a small diagram of a gesture in the theme's colors
/// (no bitmaps; it follows the theme and the text size), its caption wrapped
/// below it. B2-D03: in an article narrower than [`FIGURE_MIN`] the drawing
/// scrolls sideways on its own instead of widening the window.
pub fn figure(ui: &mut Ui, name: &str, pal: &Palette) {
    let avail = ui.available_width();
    let caption = if avail >= FIGURE_MIN {
        figure_drawing(ui, name, pal, avail.min(520.0))
    } else {
        egui::ScrollArea::horizontal()
            .id_salt(("help-figure", name))
            .show(ui, |ui| figure_drawing(ui, name, pal, FIGURE_MIN))
            .inner
    };
    ui.add_space(2.0);
    ui.add(egui::Label::new(RichText::new(caption).size(12.0).color(pal.text_weak)).wrap());
}

/// The drawing of figure `name`, `w` points wide; returns its caption.
fn figure_drawing(ui: &mut Ui, name: &str, pal: &Palette, w: f32) -> String {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 150.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, pal.roll_bg);
    p.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, pal.panel_border),
        egui::StrokeKind::Inside,
    );
    let note = |r: egui::Rect, selected: bool| {
        p.rect_filled(
            r,
            0.0,
            if selected {
                pal.note_selected_fill
            } else {
                pal.note_fill
            },
        );
        p.rect_stroke(
            r,
            0.0,
            Stroke::new(
                1.0,
                if selected {
                    pal.note_selected_border
                } else {
                    pal.note_border
                },
            ),
            egui::StrokeKind::Inside,
        );
    };
    let arrow = |from: egui::Pos2, to: egui::Pos2| {
        p.arrow(from, to - from, Stroke::new(2.0, pal.accent));
    };
    let mut cap = String::new();
    let mut caption = |text: &str| cap = text.to_owned();
    let o = rect.left_top();
    let at = |x: f32, y: f32, ww: f32, hh: f32| {
        egui::Rect::from_min_size(o + Vec2::new(x, y), Vec2::new(ww, hh))
    };
    let handle = |c: egui::Pos2| {
        p.rect_filled(
            egui::Rect::from_center_size(c, Vec2::splat(7.0)),
            0.0,
            pal.accent,
        );
    };
    match name {
        "scale-handles" => {
            let sel = at(40.0, 30.0, 150.0, 70.0);
            for (x, y, l) in [
                (40.0, 30.0, 40.0),
                (90.0, 50.0, 60.0),
                (60.0, 70.0, 50.0),
                (130.0, 88.0, 60.0),
            ] {
                note(at(x, y, l, 10.0), true);
            }
            p.rect_stroke(
                sel,
                0.0,
                Stroke::new(1.0, pal.box_select_stroke),
                egui::StrokeKind::Outside,
            );
            for c in [
                sel.left_top(),
                sel.right_top(),
                sel.left_bottom(),
                sel.right_bottom(),
                sel.right_center(),
                sel.center_bottom(),
            ] {
                handle(c);
            }
            arrow(
                sel.right_center(),
                sel.right_center() + Vec2::new(70.0, 0.0),
            );
            arrow(
                sel.center_bottom(),
                sel.center_bottom() + Vec2::new(0.0, 30.0),
            );
            caption("Drag an edge handle to stretch time or pitch; a corner scales both.");
        }
        "rotate" => {
            let sel = at(70.0, 35.0, 120.0, 60.0);
            for (x, y, l) in [(70.0, 35.0, 50.0), (100.0, 60.0, 50.0), (130.0, 85.0, 60.0)] {
                note(at(x, y, l, 10.0), true);
            }
            p.rect_stroke(
                sel,
                0.0,
                Stroke::new(1.0, pal.box_select_stroke),
                egui::StrokeKind::Outside,
            );
            handle(sel.right_top());
            p.circle_stroke(sel.center(), 4.0, Stroke::new(1.5, pal.accent));
            let r = sel.right_top() - sel.center();
            let turn = egui::emath::Rot2::from_angle(0.6) * r;
            arrow(sel.right_top(), sel.center() + turn);
            caption("Alt + drag a corner handle turns the selection (Shift: 15° steps).");
        }
        "ruler-selection" => {
            let ruler = at(0.0, 0.0, w, 24.0);
            p.rect_filled(ruler, 0.0, pal.ruler_bg);
            let span = at(90.0, 0.0, 170.0, 110.0);
            p.rect_filled(span, 0.0, pal.accent_weak.gamma_multiply(0.4));
            p.rect_filled(at(90.0, 0.0, 170.0, 24.0), 0.0, pal.accent_weak);
            for (x, y, l) in [(60.0, 50.0, 60.0), (120.0, 70.0, 80.0), (200.0, 40.0, 90.0)] {
                note(at(x, y, l, 10.0), false);
            }
            arrow(o + Vec2::new(90.0, 12.0), o + Vec2::new(260.0, 12.0));
            caption("Drag along the ruler to select a time range (Loop Selected Time plays it).");
        }
        "track-list-clips" => {
            for row in 0..3 {
                p.line_segment(
                    [
                        o + Vec2::new(0.0, 30.0 + 36.0 * row as f32),
                        o + Vec2::new(w, 30.0 + 36.0 * row as f32),
                    ],
                    Stroke::new(1.0, pal.grid_minor),
                );
            }
            let clip = |r: egui::Rect, label: &str| {
                p.rect_filled(r, 0.0, pal.accent_weak.gamma_multiply(0.5));
                p.rect_filled(
                    egui::Rect::from_min_size(r.min, Vec2::new(r.width(), 9.0)),
                    0.0,
                    pal.accent,
                );
                p.text(
                    r.min + Vec2::new(3.0, 0.0),
                    egui::Align2::LEFT_TOP,
                    label,
                    egui::FontId::proportional(8.0),
                    Color32::WHITE,
                );
            };
            clip(at(20.0, 32.0, 120.0, 32.0), "Clip");
            clip(at(170.0, 68.0, 90.0, 32.0), "Clip (linked)");
            arrow(o + Vec2::new(80.0, 48.0), o + Vec2::new(200.0, 84.0));
            p.line_segment(
                [o + Vec2::new(140.0, 32.0), o + Vec2::new(140.0, 64.0)],
                Stroke::new(2.0, pal.accent),
            );
            caption(
                "Drag a clip to move it; drag its right edge to resize; double-click opens it.",
            );
        }
        "draw-notes" => {
            for (x, y, l, s) in [
                (30.0, 40.0, 40.0, false),
                (80.0, 60.0, 40.0, false),
                (140.0, 50.0, 70.0, true),
            ] {
                note(at(x, y, l, 10.0), s);
            }
            arrow(o + Vec2::new(140.0, 55.0), o + Vec2::new(230.0, 55.0));
            p.text(
                o + Vec2::new(150.0, 70.0),
                egui::Align2::LEFT_TOP,
                "drag to set the length",
                egui::FontId::proportional(11.0),
                pal.text_weak,
            );
            caption(
                "Click an empty place to add a note; drag a note to move it, its edge to resize.",
            );
        }
        "onion-skin" => {
            for (x, y, l) in [(30.0, 40.0, 50.0), (90.0, 70.0, 60.0), (170.0, 45.0, 80.0)] {
                p.rect_filled(at(x, y, l, 10.0), 0.0, pal.note_ghost);
            }
            for (x, y, l) in [(50.0, 55.0, 40.0), (120.0, 85.0, 50.0)] {
                note(at(x, y, l, 10.0), false);
            }
            caption("Onion Skin shows other tracks' notes as ghosts behind the edited track.");
        }
        _ => {
            caption(name);
        }
    }
    cap
}

/// A small "?" button that opens Help at `topic` (context help). Returns
/// true when clicked.
pub fn help_button(ui: &mut Ui, topic: &str) -> bool {
    let title = topic_index(topic).map_or("PlumSpirit Help", |i| TOPICS[i].title());
    ui.small_button("?")
        .on_hover_text(format!(
            "Help: {title}{}",
            super::keybinds::hint(Command::ShowHelp)
        ))
        .clicked()
}

impl super::PlumSpiritApp {
    /// The Help topic for what is shown now (F1, B1-0050): the open window
    /// that has its own topic, else the main view.
    pub(super) fn help_context(&self) -> &'static str {
        use plumspirit_engine::transform::registry::Family;
        if self.settings_window.open {
            "settings"
        } else if self.lua_plugins_window.open {
            "lua-plugins"
        } else if self.audio_window.open {
            "playback"
        } else if let Some(t) = &self.tool {
            if t.spec.family == Family::Generator {
                "generators"
            } else {
                "tools"
            }
        } else if self.open.is_none() {
            "quick-start"
        } else if self.player_shown() || self.inspector_shown() {
            "player-inspector"
        } else if self.main_view == super::track_list::MainView::TrackList {
            "track-list"
        } else {
            "piano-roll"
        }
    }

    /// Opens Help at `topic` (a window's "?" button).
    pub(super) fn open_help(&mut self, topic: &str) {
        self.help.show_topic(topic);
    }

    pub(super) fn help_window_ui(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if !self.help.open {
            return;
        }
        let pal = &crate::ui::theme::ui_palette(ctx);
        for a in help_window(ctx, &mut self.help, self.settings.help_text_percent, pal) {
            match a {
                HelpAction::Run(c) => {
                    if self.enabled(c) {
                        self.execute(c, ctx, frame);
                    }
                }
                HelpAction::TextSize(p) => {
                    self.settings.help_text_percent = p;
                    self.save_settings();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every text galley drawn: (screen rect, text, characters in its widest
    /// row — a span that starts mid-line has a short first row by design).
    fn galleys(shapes: &[egui::epaint::ClippedShape]) -> Vec<(egui::Rect, String, usize)> {
        fn walk(s: &egui::Shape, out: &mut Vec<(egui::Rect, String, usize)>) {
            match s {
                egui::Shape::Text(t) => out.push((
                    t.galley.rect.translate(t.pos.to_vec2()),
                    t.galley.text().to_owned(),
                    t.galley
                        .rows
                        .iter()
                        .map(|r| r.glyphs.len())
                        .max()
                        .unwrap_or(0),
                )),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for c in shapes {
            walk(&c.shape, &mut out);
        }
        out
    }

    /// B2-D03 (owner screenshot 010510: paragraphs squeezed into 1 – 3
    /// letters per line, side by side): at every window width (a narrow
    /// window, the 175 % / 30 % UI scales of a 1280-pixel screen in points,
    /// a wide screen) and Help text size, the article runs top to bottom —
    /// the first heading above the next — long text wraps into readable
    /// lines and nothing is drawn outside the window, which fits the screen.
    #[test]
    fn help_articles_wrap_top_to_bottom_at_every_width_and_text_size() {
        let pal = &crate::ui::theme::DARK;
        for screen_w in [480.0f32, 731.0, 900.0, 1366.0, 4267.0] {
            for percent in [HELP_TEXT_MIN, 100, HELP_TEXT_MAX] {
                let ctx = egui::Context::default();
                let mut w = HelpWindow::default();
                w.show_topic("introduction");
                let screen =
                    egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(screen_w, 800.0));
                let mut shapes = Vec::new();
                for _ in 0..3 {
                    let raw = egui::RawInput {
                        screen_rect: Some(screen),
                        ..Default::default()
                    };
                    let mut out = ctx.run_ui(raw, |ui| {
                        let _ = help_window(ui.ctx(), &mut w, percent, pal);
                    });
                    out.textures_delta.clear();
                    shapes = out.shapes;
                }
                let at = format!("{screen_w} pt, text {percent} %");
                let win = ctx
                    .memory(|m| m.area_rect(egui::Id::new("plumspirit-help")))
                    .expect("the Help window is shown");
                assert!(
                    win.width() <= screen_w + 1.0,
                    "{at}: the window fits the screen: {win:?}"
                );
                let g = galleys(&shapes);
                for (r, t, widest) in &g {
                    assert!(
                        r.min.x >= win.min.x - 1.0 && r.max.x <= win.max.x + 1.0,
                        "{at}: {t:?} at {r:?} is outside the window {win:?}"
                    );
                    if t.chars().count() >= 30 {
                        assert!(
                            *widest >= 12,
                            "{at}: {t:?} is squeezed (widest line {widest} characters)"
                        );
                    }
                }
                let find = |p: &str| {
                    g.iter()
                        .find(|(_, t, _)| t.starts_with(p))
                        .unwrap_or_else(|| panic!("{at}: {p:?} not drawn"))
                        .0
                };
                // The bug drew the heading and its paragraph side by side.
                // The article's heading, not the topic list's entry.
                let heading = g
                    .iter()
                    .filter(|(_, t, _)| t == "Introduction")
                    .map(|(r, _, _)| *r)
                    .max_by(|a, b| a.min.x.total_cmp(&b.min.x))
                    .unwrap_or_else(|| panic!("{at}: no Introduction heading"));
                let intro = find("PlumSpirit is a MIDI sequencer");
                assert!(
                    intro.min.y >= heading.max.y - 0.5,
                    "{at}: the paragraph {intro:?} follows the heading {heading:?}"
                );
                assert!(
                    (heading.min.x - intro.min.x).abs() < 2.0,
                    "{at}: one left edge for the article ({heading:?} / {intro:?})"
                );
                // The next heading, when it is in view, is below the paragraph.
                if let Some((next, _, _)) = g.iter().find(|(_, t, _)| t == "The window") {
                    assert!(
                        next.min.y >= intro.max.y - 0.5,
                        "{at}: {next:?} / {intro:?}"
                    );
                }
            }
        }
    }

    /// B2-D03: every topic (tables, lists, figures, code, the shortcut and
    /// tool tables) stays inside a narrow window and wraps readably.
    #[test]
    fn every_help_topic_stays_inside_a_narrow_window() {
        let pal = &crate::ui::theme::DARK;
        let ctx = egui::Context::default();
        let mut w = HelpWindow {
            open: true,
            ..HelpWindow::default()
        };
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(480.0, 800.0));
        for (i, topic) in TOPICS.iter().enumerate() {
            w.current = i;
            let mut shapes = Vec::new();
            for _ in 0..2 {
                let raw = egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                };
                let mut out = ctx.run_ui(raw, |ui| {
                    let _ = help_window(ui.ctx(), &mut w, 100, pal);
                });
                out.textures_delta.clear();
                shapes = out.shapes;
            }
            let win = ctx
                .memory(|m| m.area_rect(egui::Id::new("plumspirit-help")))
                .unwrap();
            assert!(win.width() <= 481.0, "{}: {win:?}", topic.id);
            for (r, t, widest) in galleys(&shapes) {
                assert!(
                    r.min.x >= win.min.x - 1.0 && r.max.x <= win.max.x + 1.0,
                    "{}: {t:?} at {r:?} outside {win:?}",
                    topic.id
                );
                if t.chars().count() >= 30 {
                    assert!(widest >= 12, "{}: {t:?} widest line {widest}", topic.id);
                }
            }
        }
    }

    /// B2-D03: the list gives way before the article, never below its minimum.
    #[test]
    fn help_panes_keep_a_readable_article_width() {
        assert_eq!(pane_widths(1200.0).0, HELP_LIST_WIDTH);
        // The narrowest window (420 pt) leaves about 404 pt: list 144, article 260.
        let (l, a) = pane_widths(404.0);
        assert!(
            (l - 144.0).abs() < 1e-3 && (a - HELP_ARTICLE_MIN).abs() < 1e-3,
            "{l} {a}"
        );
        // Narrower still: the list stops at its minimum, the article takes the rest.
        let (l, a) = pane_widths(380.0);
        assert_eq!((l, a), (HELP_LIST_MIN, 240.0));
        let (l, a) = pane_widths(450.0);
        assert!(l < HELP_LIST_WIDTH && a >= HELP_ARTICLE_MIN, "{l} {a}");
        assert_eq!(pane_widths(0.0), (HELP_LIST_MIN, 0.0));
        assert_eq!(article_width(2000.0), HELP_ARTICLE_MAX);
    }

    /// L-F12 (contract §022): the Introduction opens with the owner's exact
    /// sentence, and no topic promises that PlumSpirit fits a fixed amount of
    /// RAM (what fits is measured, `docs/PERFORMANCE.md`).
    #[test]
    fn the_introduction_says_exactly_what_was_asked_and_no_topic_promises_2_gb() {
        const INTRO: &str = "PlumSpirit is a MIDI sequencer and editor built for Black MIDI: \
            songs with thousands to hundreds of millions of notes. It is made to stay fast and \
            small enough for low-end Windows PCs, while giving you exact control over every \
            note, tempo and channel.";
        let intro = TOPICS.iter().find(|t| t.id == "introduction").unwrap();
        let blocks = parse(intro.text);
        let Some(Block::Para(first)) = blocks.get(1) else {
            panic!(
                "the introduction starts with a paragraph: {:?}",
                blocks.get(1)
            );
        };
        let text: String = first
            .iter()
            .map(|s| match s {
                Span::Text(t) => t.as_str(),
                other => panic!("plain text only: {other:?}"),
            })
            .collect();
        assert_eq!(text.split_whitespace().collect::<Vec<_>>().join(" "), INTRO);
        for t in TOPICS {
            let flat = t.text.split_whitespace().collect::<Vec<_>>().join(" ");
            for claim in ["2 GB of RAM", "2 GB RAM", "2GB", "(2 GB"] {
                assert!(!flat.contains(claim), "{}: {claim:?}", t.id);
            }
        }
    }

    #[test]
    fn every_topic_parses_and_has_a_title() {
        assert!(TOPICS.len() >= 4);
        let mut ids = std::collections::HashSet::new();
        for t in TOPICS {
            assert!(ids.insert(t.id), "duplicate topic {}", t.id);
            assert!(t.text.starts_with("# "), "{} starts with its title", t.id);
            let blocks = parse(t.text);
            assert!(matches!(blocks.first(), Some(Block::Heading(1, _))));
            assert!(blocks.len() >= 2, "{} has content", t.id);
        }
    }

    #[test]
    fn the_markup_subset_parses_exactly() {
        let b = parse(
            "# T\n\nOne **bold** and `code` with [a link](#glossary) and {key:Undo}.\nSame paragraph.\n\n* item\n  continued\n  * nested\n1. step\n> tip\n\n| A | B |\n|---|---|\n| x | {cmd:Undo} |\n\n{fig:rotate}\n{shortcuts}\n```\ncode\n```\n",
        );
        assert_eq!(b[0], Block::Heading(1, "T".into()));
        let Block::Para(p) = &b[1] else {
            panic!("{b:?}")
        };
        assert_eq!(p[1], Span::Bold("bold".into()));
        assert_eq!(p[3], Span::Code("code".into()));
        assert_eq!(
            p[5],
            Span::Link {
                text: "a link".into(),
                topic: "glossary".into()
            }
        );
        assert_eq!(p[7], Span::Key("Undo".into()));
        assert!(matches!(&p[8], Span::Text(t) if t.ends_with("Same paragraph.")));
        assert!(matches!(&b[2], Block::Bullet(0, s) if s.len() == 3));
        assert!(matches!(&b[3], Block::Bullet(1, _)));
        assert!(matches!(&b[4], Block::Step(1, _)));
        assert!(matches!(&b[5], Block::Tip(_)));
        assert!(matches!(&b[6], Block::Table(rows) if rows.len() == 2));
        assert_eq!(b[7], Block::Figure("rotate".into()));
        assert_eq!(b[8], Block::Shortcuts);
        assert_eq!(b[9], Block::Code("code".into()));
    }

    /// Every `{key:}` / `{cmd:}` names a real command, every link a real
    /// topic and every figure a drawn one: Help never shows a phantom.
    #[test]
    fn every_reference_in_help_exists() {
        fn check(s: &[Span], id: &str) {
            for sp in s {
                match sp {
                    Span::Key(c) | Span::Cmd(c) => {
                        assert!(command_named(c).is_some(), "{id}: unknown command {c}")
                    }
                    Span::Link { topic, .. } => {
                        assert!(topic_index(topic).is_some(), "{id}: no topic {topic}")
                    }
                    _ => {}
                }
            }
        }
        for t in TOPICS {
            for b in parse(t.text) {
                match &b {
                    Block::Para(s) | Block::Bullet(_, s) | Block::Step(_, s) | Block::Tip(s) => {
                        check(s, t.id)
                    }
                    Block::Table(rows) => rows.iter().flatten().for_each(|c| check(c, t.id)),
                    Block::Figure(f) => {
                        assert!(FIGURES.contains(&f.as_str()), "{}: figure {f}", t.id)
                    }
                    _ => {}
                }
            }
        }
    }

    /// Menu paths written as **Menu → Item → …** name menus and entries the
    /// menu bar really has (a label in `ui/menu.rs` or a command label).
    #[test]
    fn menu_paths_in_help_name_real_menus() {
        const MENU_SOURCE: &str = include_str!("../ui/menu.rs");
        const TOP: &[&str] = &[
            "File", "Edit", "Tools", "Plugins", "Song", "View", "Playback", "Help",
        ];
        let labels: Vec<String> = crate::app::keybinds::bindable()
            .into_iter()
            .chain(MENU_ONLY.iter().copied())
            .map(|c| c.label().to_owned())
            .collect();
        for t in TOPICS {
            for b in parse(t.text) {
                let spans: Vec<Span> = match b {
                    Block::Para(s) | Block::Bullet(_, s) | Block::Step(_, s) | Block::Tip(s) => s,
                    Block::Table(rows) => rows.into_iter().flatten().flatten().collect(),
                    _ => continue,
                };
                for sp in spans {
                    let Span::Bold(path) = sp else { continue };
                    let parts: Vec<&str> = path.split(" → ").map(str::trim).collect();
                    if parts.len() < 2 || !TOP.contains(&parts[0]) {
                        continue;
                    }
                    for part in &parts[1..] {
                        let bare = part.trim_end_matches('…');
                        let known = MENU_SOURCE.contains(&format!("\"{bare}"))
                            || labels.iter().any(|l| l.trim_end_matches('…') == bare)
                            || EXTRA_MENU_ENTRIES.contains(&bare)
                            || tool_rows().iter().any(|r| r.0 == bare || r.1 == bare);
                        assert!(
                            known,
                            "{}: “{part}” in **{path}** is not in the menus",
                            t.id
                        );
                    }
                }
            }
        }
    }

    /// Menu entries built from data rather than literals in `ui/menu.rs`
    /// (the Tools menu's sections come from [`tool_rows`]).
    const EXTRA_MENU_ENTRIES: &[&str] = &["Custom", "Legacy"];

    #[test]
    fn every_topic_the_program_opens_exists_and_the_tool_list_is_complete() {
        for id in CONTEXT_TOPICS {
            assert!(topic_index(id).is_some(), "no topic {id}");
        }
        let rows = tool_rows();
        // Every tool but the built-in plugins' (they are in the Plugins menu).
        let in_menu = plumspirit_engine::transform::registry()
            .iter()
            .filter(|t| (t.spec)().family != plumspirit_engine::transform::registry::Family::Plugin)
            .count();
        assert_eq!(rows.len(), in_menu, "every tool listed once");
        assert!(rows.iter().all(|r| !r.1.is_empty() && !r.2.is_empty()));
        // Searching finds a tool by its name in the Note Tools topic.
        let (_, name, _) = rows[0];
        let hits = Index::build().search(name);
        assert!(
            hits.iter().any(|h| TOPICS[h.0].id == "tools"),
            "{name}: {hits:?}"
        );
    }

    #[test]
    fn search_finds_topics_by_every_word_and_ranks_titles_first() {
        let idx = Index::build();
        let r = idx.search("quick start");
        assert_eq!(TOPICS[r[0].0].id, "quick-start", "{r:?}");
        assert!(idx.search("zzzz-not-a-word").is_empty());
        assert!(idx.search("   ").is_empty());
        // Every result contains every word (in its text as shown).
        let hits = idx.search("track clip");
        assert!(!hits.is_empty());
        for (i, _, _) in hits {
            let (title, pieces) = &idx.topics[i];
            for w in ["track", "clip"] {
                assert!(
                    title.contains(w) || pieces.iter().any(|p| p.lower.contains(w)),
                    "{w} in {}",
                    TOPICS[i].id
                );
            }
        }
    }

    #[test]
    fn shortcuts_follow_the_users_keybinds() {
        assert_eq!(key_text("ShowHelp"), "F1");
        assert_eq!(cmd_text("ShowHelp"), "PlumSpirit Help… (F1)");
        assert!(key_text("NoSuchCommand").contains('?'));
    }

    #[test]
    fn help_is_lazy_until_it_is_used() {
        let mut w = HelpWindow::default();
        assert!(!w.index_built() && w.parsed_count() == 0);
        w.show_topic("quick-start");
        assert!(w.open);
        assert_eq!(TOPICS[w.current].id, "quick-start");
        let _ = w.blocks(w.current);
        assert_eq!(w.parsed_count(), 1);
        assert!(!w.index_built());
        w.query = "track".into();
        assert!(!w.results().is_empty());
        assert!(w.index_built());
        // Back after following a link.
        w.go(topic_index("glossary").unwrap());
        assert_eq!(w.back, vec![topic_index("quick-start").unwrap()]);
    }
}

#[cfg(test)]
mod app_tests {
    use super::*;
    use crate::app::test_support::Harness;
    use crate::app::track_list::MainView;
    use plumspirit_engine::model::{Bpm, PlumDocument};

    fn doc() -> PlumDocument {
        PlumDocument::new_project("Help".into(), Bpm::DEFAULT, 960)
    }

    fn shown(h: &Harness) -> &'static str {
        TOPICS[h.app.help.current].id
    }

    #[test]
    fn f1_opens_help_at_what_is_shown_and_shift_f1_the_shortcuts() {
        let mut h = Harness::new(doc());
        h.app.main_view = MainView::TrackList;
        h.press(egui::Key::F1);
        assert!(h.app.help.open);
        assert_eq!(shown(&h), "track-list");
        h.app.help.open = false;
        h.app.main_view = MainView::PianoRoll;
        h.press(egui::Key::F1);
        assert_eq!(shown(&h), "piano-roll");
        // An open window with its own topic wins.
        assert!(h.command(Command::OpenSettings));
        assert!(h.command(Command::ShowHelp));
        assert_eq!(shown(&h), "settings");
        // Shift+F1: the Keyboard Shortcuts window.
        assert_eq!(Command::ShowKeybinds.shortcut(), "Shift+F1");
        assert_eq!(Command::ShowHelp.shortcut(), "F1");
        // The window draws every topic without a panic, at any text size.
        for i in 0..TOPICS.len() {
            h.app.help.current = i;
            for p in [HELP_TEXT_MIN, 100, HELP_TEXT_MAX] {
                h.app.settings.help_text_percent = p;
                h.app_frame(egui::vec2(1366.0, 768.0), 1.0);
            }
        }
        assert_eq!(h.app.help.parsed_count(), TOPICS.len());
        // Quick Start from the Help menu.
        assert!(h.command(Command::ShowQuickStart));
        assert_eq!(shown(&h), "quick-start");
    }

    #[test]
    fn the_quick_start_card_can_be_dismissed_and_brought_back() {
        let mut h = Harness::new(doc());
        assert!(h.app.settings.show_orientation, "shown by default");
        assert!(h.command(Command::ToggleOrientation));
        assert!(!h.app.settings.show_orientation);
        assert!(h.command(Command::ToggleOrientation));
        assert!(h.app.settings.show_orientation);
        // The start screen (no project) draws the card.
        assert!(h.command(Command::CloseProject));
        h.app_frame(egui::vec2(1366.0, 768.0), 1.0);
        h.app_frame(egui::vec2(1366.0, 768.0), 1.0);
        assert!(h.app.open.is_none());
        assert_eq!(h.app.help_context(), "quick-start");
        // The steps the card lists are the Quick Start topic's.
        assert_eq!(crate::ui::empty_state::ORIENTATION_STEPS.len(), 6);
    }
}
