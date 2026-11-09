//! The central keybind registry (contract §147): every keyboard shortcut is one
//! row here. The shortcut handlers, the menus' shortcut texts and the Keyboard
//! Shortcuts window (Help → Keyboard Shortcuts…, F1) all read this table, so a
//! shortcut cannot be shown without working or work without being shown, and a
//! test proves no two rows share a chord.

use super::commands::Command;
use crate::piano_roll::Tool;
use egui::{Key, Modifiers};
use plumspirit_engine::editor::clip_merge::MergeKind;

/// One shortcut.
#[derive(Clone, Copy, Debug)]
pub struct Keybind {
    pub mods: Modifiers,
    pub key: Key,
    pub command: Command,
    /// Section of the Keyboard Shortcuts window.
    pub group: &'static str,
    /// Read before any widget runs (transport keys: a focused button must not
    /// also react to Space).
    pub transport: bool,
}

const NONE: Modifiers = Modifiers::NONE;
const CTRL: Modifiers = Modifiers::COMMAND;
const CTRL_SHIFT: Modifiers = Modifiers {
    alt: false,
    ctrl: false,
    shift: true,
    mac_cmd: false,
    command: true,
};
const ALT: Modifiers = Modifiers::ALT;
const CTRL_ALT: Modifiers = Modifiers {
    alt: true,
    ctrl: false,
    shift: false,
    mac_cmd: false,
    command: true,
};
const SHIFT: Modifiers = Modifiers::SHIFT;
const CTRL_ALT_SHIFT: Modifiers = Modifiers {
    alt: true,
    ctrl: false,
    shift: true,
    mac_cmd: false,
    command: true,
};

const fn kb(mods: Modifiers, key: Key, command: Command, group: &'static str) -> Keybind {
    Keybind {
        mods,
        key,
        command,
        group,
        transport: false,
    }
}

const fn tr(mods: Modifiers, key: Key, command: Command) -> Keybind {
    Keybind {
        mods,
        key,
        command,
        group: "Playback",
        transport: true,
    }
}

/// Every keyboard shortcut. Rows with Shift are matched before rows without
/// it (egui matches Shift loosely; see [`ordered`]).
pub const KEYBINDS: &[Keybind] = &[
    // File
    kb(CTRL, Key::N, Command::NewProject, "File"),
    kb(CTRL, Key::O, Command::Open, "File"),
    kb(CTRL, Key::S, Command::Save, "File"),
    kb(CTRL_SHIFT, Key::S, Command::SaveAs, "File"),
    kb(CTRL, Key::E, Command::Export, "File"),
    kb(CTRL, Key::W, Command::CloseProject, "File"),
    // Edit
    kb(CTRL, Key::Z, Command::Undo, "Edit"),
    kb(CTRL, Key::Y, Command::Redo, "Edit"),
    kb(CTRL_SHIFT, Key::Z, Command::Redo, "Edit"),
    kb(CTRL, Key::A, Command::SelectAll, "Edit"),
    // v0.9 §088 / §198 (V09-S13): the selected notes or clips (the last
    // reserved Track List chord).
    kb(CTRL, Key::B, Command::PasteRepeat, "Edit"),
    kb(NONE, Key::Delete, Command::Delete, "Edit"),
    kb(CTRL, Key::R, Command::SelectRange, "Edit"),
    kb(CTRL, Key::G, Command::GroupNotes, "Edit"),
    kb(CTRL_SHIFT, Key::G, Command::UngroupNotes, "Edit"),
    kb(CTRL_SHIFT, Key::M, Command::UnmuteNotes, "Edit"),
    kb(CTRL, Key::M, Command::MuteNotes, "Edit"),
    kb(CTRL, Key::K, Command::SliceNotes, "Edit"),
    // v0.9 §083: Ctrl+P (Ctrl+Shift+P stays Paint Color…).
    kb(CTRL, Key::P, Command::PaintCurrentSubchannel, "Edit"),
    kb(CTRL_ALT, Key::ArrowUp, Command::DuplicateOctaveUp, "Edit"),
    kb(
        CTRL_ALT,
        Key::ArrowDown,
        Command::DuplicateOctaveDown,
        "Edit",
    ),
    // Ctrl+X arrives as a Cut event from egui-winit.
    kb(CTRL, Key::X, Command::Cut, "Edit"),
    kb(CTRL, Key::Q, Command::Quantize, "Note Tools"),
    // Piano Roll mouse tools
    kb(NONE, Key::Num1, Command::Tool(Tool::Select), "Piano Roll"),
    kb(NONE, Key::Num2, Command::Tool(Tool::Draw), "Piano Roll"),
    kb(NONE, Key::Num3, Command::Tool(Tool::Erase), "Piano Roll"),
    // B1-0088: listed before Ctrl+0 (egui matches Shift loosely).
    kb(CTRL_SHIFT, Key::Num0, Command::FitKeys128, "Piano Roll"),
    kb(CTRL, Key::Num0, Command::ResetZoom, "Piano Roll"),
    kb(NONE, Key::F8, Command::ZoomTool, "Piano Roll"),
    kb(CTRL, Key::OpenBracket, Command::ZoomBack, "Piano Roll"),
    kb(CTRL, Key::CloseBracket, Command::ZoomForward, "Piano Roll"),
    // Note tools (v0.7)
    kb(NONE, Key::F2, Command::OpenTool("note_props"), "Note Tools"),
    kb(
        CTRL_SHIFT,
        Key::H,
        Command::OpenTool("humanize"),
        "Note Tools",
    ),
    kb(CTRL, Key::H, Command::UndoHistory, "Edit"),
    kb(
        CTRL_SHIFT,
        Key::U,
        Command::OpenTool("transpose"),
        "Note Tools",
    ),
    kb(
        CTRL_SHIFT,
        Key::L,
        Command::OpenTool("legato"),
        "Note Tools",
    ),
    kb(CTRL_SHIFT, Key::J, Command::OpenTool("join"), "Note Tools"),
    kb(
        CTRL_SHIFT,
        Key::A,
        Command::OpenTool("arpeggiate"),
        "Note Tools",
    ),
    kb(
        CTRL_SHIFT,
        Key::R,
        Command::OpenTool("randomize"),
        "Note Tools",
    ),
    kb(
        CTRL_SHIFT,
        Key::K,
        Command::OpenTool("clip_mask"),
        "Note Tools",
    ),
    kb(
        CTRL_SHIFT,
        Key::P,
        Command::OpenTool("paint_color"),
        "Note Tools",
    ),
    kb(CTRL, Key::D, Command::OpenTool("duplicate"), "Note Tools"),
    kb(SHIFT, Key::F5, Command::ReopenLastTool, "Note Tools"),
    kb(CTRL, Key::F5, Command::RepeatLastToolNewSeed, "Note Tools"),
    kb(NONE, Key::F5, Command::RepeatLastTool, "Note Tools"),
    // Built-in plugins
    kb(ALT, Key::H, Command::OpenTool("hz_chop"), "Plugins"),
    kb(ALT, Key::O, Command::OpenTool("safor"), "Plugins"),
    kb(
        ALT,
        Key::I,
        Command::OpenPlugin(plumspirit_engine::plugins::PluginId::ImageToMidi),
        "Plugins",
    ),
    kb(
        ALT,
        Key::W,
        Command::OpenPlugin(plumspirit_engine::plugins::PluginId::Spiderweb),
        "Plugins",
    ),
    kb(
        ALT,
        Key::R,
        Command::OpenPlugin(plumspirit_engine::plugins::PluginId::AudioReference),
        "Plugins",
    ),
    // Tracks
    kb(CTRL, Key::I, Command::TrackProperties, "Tracks"),
    kb(CTRL_SHIFT, Key::I, Command::ImportIntoTrack, "Tracks"),
    kb(CTRL_SHIFT, Key::N, Command::AddTrack, "Tracks"),
    kb(CTRL_ALT, Key::N, Command::AddTracks, "Tracks"),
    kb(CTRL_SHIFT, Key::T, Command::AddSubtrack, "Tracks"),
    kb(
        CTRL_SHIFT,
        Key::Delete,
        Command::DeleteSelectedTracks,
        "Tracks",
    ),
    kb(ALT, Key::ArrowUp, Command::MoveTrackUp, "Tracks"),
    kb(ALT, Key::ArrowDown, Command::MoveTrackDown, "Tracks"),
    kb(ALT, Key::ArrowRight, Command::IndentTrack, "Tracks"),
    kb(ALT, Key::ArrowLeft, Command::OutdentTrack, "Tracks"),
    // L-F08 / L-F10: monitoring and the current track's Mute / Solo.
    kb(ALT, Key::M, Command::MuteCurrentTrack, "Tracks"),
    kb(ALT, Key::S, Command::SoloCurrentTrack, "Tracks"),
    kb(ALT, Key::P, Command::PlayOnlySelected, "Tracks"),
    kb(ALT, Key::A, Command::RestoreAllTracks, "Tracks"),
    // Track List (v0.9 §198). Delete, Ctrl+A, Ctrl+K, Ctrl+D, Ctrl+B and the
    // clipboard keys act on the selected clips while it is shown
    // (`track_list_meaning`); B2-F29 adds the nudges.
    kb(NONE, Key::Enter, Command::OpenClip, "Track List"),
    kb(NONE, Key::ArrowLeft, Command::NudgeClipsLeft, "Track List"),
    kb(
        NONE,
        Key::ArrowRight,
        Command::NudgeClipsRight,
        "Track List",
    ),
    kb(NONE, Key::Insert, Command::AddClip, "Track List"),
    kb(
        CTRL,
        Key::J,
        Command::MergeClips(MergeKind::Sequence),
        "Track List",
    ),
    kb(
        CTRL_ALT,
        Key::J,
        Command::MergeClips(MergeKind::Stack),
        "Track List",
    ),
    kb(CTRL, Key::ArrowUp, Command::PreviousTrack, "Track List"),
    kb(CTRL, Key::ArrowDown, Command::NextTrack, "Track List"),
    // View. B1-0061: Tab is read before any widget (a focused button must
    // not take it as focus traversal), like the transport keys.
    Keybind {
        mods: NONE,
        key: Key::Tab,
        command: Command::SwitchView,
        group: "View",
        transport: true,
    },
    kb(CTRL, Key::Comma, Command::OpenSettings, "Edit"),
    // B1-0050: F1 is Help; the shortcut list moved to Shift+F1 (Shift rows
    // before the same key without Shift).
    kb(SHIFT, Key::F1, Command::ShowKeybinds, "View"),
    kb(NONE, Key::F1, Command::ShowHelp, "View"),
    // B1-0062: reachable when a tiny UI scale makes Settings hard to use.
    kb(CTRL_ALT, Key::Num0, Command::ResetUiScale, "View"),
    // B1-0064: Onion Skin from the keyboard.
    // (Ctrl+Alt+Shift+O first: egui matches extra modifiers loosely.)
    kb(
        CTRL_ALT_SHIFT,
        Key::O,
        Command::OnionShowOnlySelected,
        "View",
    ),
    kb(CTRL_SHIFT, Key::O, Command::ToggleOnion, "View"),
    kb(CTRL_ALT, Key::O, Command::OnionSelectedTracks, "View"),
    kb(NONE, Key::F6, Command::ToggleSelectionAnalytics, "View"),
    kb(NONE, Key::F7, Command::ToggleOnionOutlines, "View"),
    kb(NONE, Key::F10, Command::ToggleMidiPlayer, "View"),
    kb(NONE, Key::F11, Command::ToggleMidiInspector, "View"),
    kb(NONE, Key::F12, Command::ToggleDevOverlay, "View"),
    // B2-F29: both views (the selected clips / notes; the playhead).
    kb(CTRL_SHIFT, Key::F, Command::ZoomToSelection, "View"),
    kb(CTRL_SHIFT, Key::Home, Command::JumpToPlayhead, "View"),
    // Playback
    tr(CTRL_SHIFT, Key::Space, Command::ToggleAnchorPlayhead),
    tr(SHIFT, Key::Space, Command::Stop),
    tr(NONE, Key::Space, Command::PlayPause),
    tr(NONE, Key::Home, Command::GoToStart),
    tr(NONE, Key::End, Command::GoToEnd),
    tr(CTRL, Key::ArrowLeft, Command::PreviousMeasure),
    tr(CTRL, Key::ArrowRight, Command::NextMeasure),
    kb(CTRL, Key::L, Command::ToggleLoop, "Playback"),
    kb(NONE, Key::F9, Command::ScrubTool, "Playback"),
    kb(CTRL, Key::T, Command::PlayingTime, "Playback"),
];

/// Shortcuts handled outside the table (system clipboard events), shown in
/// the Keyboard Shortcuts window.
pub const EXTRA: &[(&str, &str, &str)] = &[
    ("File", "Alt+F4", "Exit"),
    (
        "Track List",
        "Ctrl+C / Ctrl+V (Track List shown)",
        "Copy the selected clips / paste the copied clips on the active track at the edit cursor (in the Piano Roll: notes)",
    ),
    (
        "Track List",
        "Alt+drag a clip",
        "Linked copies where you drop them",
    ),
    ("Edit", "Ctrl+C", "Copy"),
    ("Edit", "Ctrl+V", "Paste"),
    (
        "Piano Roll",
        "Escape",
        "Cancel the current drag / scale gesture",
    ),
    (
        "Piano Roll",
        "Shift (while scaling)",
        "Snap the dragged edges to the Snap setting",
    ),
    (
        "Piano Roll",
        "Ctrl+drag a selection corner",
        "Skew: the corner's edge slides in time or pitch, the opposite edge stays",
    ),
    (
        "Piano Roll",
        "Ctrl+Shift+drag a selection corner",
        "Perspective: only that corner moves, the other three stay",
    ),
    (
        "Piano Roll",
        "Hold Z (Zoom window open)",
        "Magnifier lens around the pointer",
    ),
    (
        "Piano Roll",
        "Drag (Draw / Note Brush window open)",
        "Draw a stroke; each stroke is one undo step",
    ),
    (
        "Piano Roll",
        "Click (Select polygon lasso)",
        "Add a lasso corner",
    ),
    (
        "Tracks",
        "Ctrl+click / Shift+click",
        "Add to the track selection / select a range",
    ),
    (
        "Tracks",
        "Drag a track",
        "Move it (onto a track's middle: make it a subtrack)",
    ),
];

/// Chords kept free for workflows that do not exist yet (v0.9 §198, Track
/// List keybinds, requirement V09-0142 in slice V09-S08): (chord, key,
/// intended action). They are not rows of [`KEYBINDS`] — a shortcut is shown
/// only once it works — but a test keeps them collision-free and out of the
/// clipboard chords, so the Track List can take them as planned. Ctrl+D
/// (Duplicate) is meant to duplicate clips while the Track List has focus.
// Read by the tests until V09-S08 binds them.
#[cfg_attr(not(test), allow(dead_code))]
pub const RESERVED: &[(Modifiers, Key, &str)] = &[];

/// L-G04 (contract §014): consumes a press of `s` — Shift matched loosely
/// as egui does (the rows with Shift come first), Ctrl / Command exactly as
/// egui does, and **Alt exactly**: egui's `consume_shortcut` also fires a
/// shortcut without Alt while Alt is held, so another program's global
/// Alt+Shift+… hotkey (OBS's Alt+Shift+7 while recording) reached PlumSpirit
/// as the plain key — Alt+Shift+F7 toggled Ghost Outlines, Alt+Shift+1 – 3
/// switched the tool.
pub fn consume(i: &mut egui::InputState, s: &egui::KeyboardShortcut) -> bool {
    let mut hit = false;
    i.events.retain(|e| {
        let m = matches!(e, egui::Event::Key { key, pressed: true, modifiers, .. }
            if *key == s.logical_key
                && modifiers.matches_logically(s.modifiers)
                && modifiers.alt == s.modifiers.alt);
        hit |= m;
        !m
    });
    hit
}

/// The rows of one kind (transport or not), Shift rows first — from the
/// table in force (defaults plus the user's keybinds, [`effective`]).
pub fn ordered(transport: bool) -> Vec<Keybind> {
    let mut v: Vec<Keybind> = effective()
        .iter()
        .copied()
        .filter(|k| k.transport == transport)
        .collect();
    // Most specific first (extra Shift / Alt are ignored when matching).
    v.sort_by_key(|k| (!k.mods.shift, !k.mods.alt));
    v
}

/// "Ctrl+Shift+G" style text of a chord (Windows conventions).
pub fn chord_text(mods: Modifiers, key: Key) -> String {
    let mut s = String::new();
    if mods.command || mods.ctrl {
        s.push_str("Ctrl+");
    }
    if mods.alt {
        s.push_str("Alt+");
    }
    if mods.shift {
        s.push_str("Shift+");
    }
    s.push_str(match key {
        Key::ArrowLeft => "←",
        Key::ArrowRight => "→",
        Key::ArrowUp => "↑",
        Key::ArrowDown => "↓",
        Key::Delete => "Del",
        Key::Space => "Space",
        Key::Num0 => "0",
        Key::Num1 => "1",
        Key::Num2 => "2",
        Key::Num3 => "3",
        Key::OpenBracket => "[",
        Key::CloseBracket => "]",
        Key::Comma => ",",
        k => k.name(),
    });
    s
}

/// egui-winit emits Cut for Ctrl+X and, on Windows, Shift+Delete.
pub fn cut_command(mods: Modifiers, windows: bool) -> Command {
    if windows && mods.shift {
        if mods.command || mods.ctrl {
            Command::DeleteSelectedTracks
        } else {
            Command::Delete
        }
    } else {
        Command::Cut
    }
}

/// Every chord bound to `cmd`, in table order (the table in force).
/// B1-0066: the command's live shortcut(s) for a tooltip — " (Ctrl+Z)", or ""
/// when it has none. Follows the user's keybinds, so a tooltip never names a
/// key that no longer does what it says.
pub fn hint(cmd: Command) -> String {
    let s = cmd.shortcut();
    if s.is_empty() {
        String::new()
    } else {
        format!(" ({s})")
    }
}

/// The command's first live shortcut ("" when it has none) — for a menu's
/// shortcut column.
pub fn first_key(cmd: Command) -> String {
    cmd.shortcut()
        .split(", ")
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// The command's first live shortcut, or `menu` (a menu path such as
/// "Edit → Undo") when the user removed every shortcut — for messages that
/// say how to do something ("You can undo this with Ctrl+Z.").
pub fn key_or_menu(cmd: Command, menu: &str) -> String {
    match first_key(cmd) {
        k if k.is_empty() => menu.to_owned(),
        k => k,
    }
}

pub fn shortcuts_of(cmd: Command) -> Vec<String> {
    effective()
        .iter()
        .filter(|k| k.command == cmd)
        .map(|k| chord_text(k.mods, k.key))
        .collect()
}

// ------------------------------------------------------------------------
// B1-0060: the user's keybinds over the defaults.

/// A chord: modifiers (Ctrl is `command`, as in [`KEYBINDS`]) and key.
pub type Chord = (Modifiers, Key);

/// The user's keybinds: for each changed command its chords in order (empty =
/// no shortcut). Commands not listed keep their default rows. Stored in
/// `settings.json` as `keybinds: {"<command id>": ["Ctrl+Shift+G", …]}`.
pub type Overrides = Vec<(Command, Vec<Chord>)>;

/// The stable `settings.json` name of a command (its Rust name, e.g.
/// `Undo`, `OpenTool("humanize")`, `Tool(Select)`).
pub fn command_id(c: Command) -> String {
    format!("{c:?}")
}

/// The commands that have a keybind row by default, in table order, once
/// each — the ones the user can rebind.
pub fn bindable() -> Vec<Command> {
    let mut v: Vec<Command> = Vec::new();
    for k in KEYBINDS {
        if !v.contains(&k.command) {
            v.push(k.command);
        }
    }
    v
}

/// The bindable command named `id` ([`command_id`]).
pub fn command_from_id(id: &str) -> Option<Command> {
    bindable().into_iter().find(|c| command_id(*c) == id)
}

/// Reads "Ctrl+Shift+G" style text ([`chord_text`] and the usual spellings:
/// "Control", "Left" / "←", "Delete" / "Del", "Esc", case ignored).
pub fn parse_chord(text: &str) -> Result<Chord, String> {
    let t = text.trim();
    let bad = || format!("“{t}” is not a shortcut (for example Ctrl+Shift+G or F4).");
    if t.is_empty() {
        return Err("Press a key or type a shortcut such as Ctrl+Shift+G.".to_owned());
    }
    // The last part is the key ("Ctrl++" is not supported: "+" is not bindable).
    let parts: Vec<&str> = t.split('+').map(str::trim).collect();
    let (key_text, mod_texts) = parts.split_last().ok_or_else(bad)?;
    let mut mods = Modifiers::NONE;
    for m in mod_texts {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "cmd" | "command" => mods.command = true,
            "shift" => mods.shift = true,
            "alt" | "option" => mods.alt = true,
            _ => return Err(bad()),
        }
    }
    let k = key_text.to_ascii_lowercase();
    let key = match k.as_str() {
        "←" | "left" | "arrowleft" => Key::ArrowLeft,
        "→" | "right" | "arrowright" => Key::ArrowRight,
        "↑" | "up" | "arrowup" => Key::ArrowUp,
        "↓" | "down" | "arrowdown" => Key::ArrowDown,
        "del" | "delete" => Key::Delete,
        "ins" | "insert" => Key::Insert,
        "esc" | "escape" => Key::Escape,
        "space" | "spacebar" => Key::Space,
        "enter" | "return" => Key::Enter,
        "pgup" | "pageup" => Key::PageUp,
        "pgdn" | "pagedown" => Key::PageDown,
        "[" => Key::OpenBracket,
        "]" => Key::CloseBracket,
        "," => Key::Comma,
        _ => Key::ALL
            .iter()
            .copied()
            .find(|c| {
                c.name().eq_ignore_ascii_case(key_text)
                    || c.symbol_or_name().eq_ignore_ascii_case(key_text)
            })
            .ok_or_else(bad)?,
    };
    Ok((mods, key))
}

/// Ctrl / Shift / Alt / key, the way two chords are compared.
fn chord_key(c: Chord) -> (bool, bool, bool, Key) {
    (c.0.command || c.0.ctrl, c.0.shift, c.0.alt, c.1)
}

/// A clipboard event egui-winit sends *instead of* the key press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClipboardChord {
    Cut,
    Copy,
    Paste,
}

/// egui-winit 0.36 (`is_cut_command` / `is_copy_command` /
/// `is_paste_command`): Ctrl+X / C / V everywhere and, on Windows only,
/// Shift+Delete (Cut), Ctrl+Insert (Copy) and Shift+Insert (Paste) arrive as
/// clipboard events and never as key presses, so a row on such a chord
/// would silently do something else (v0.9 §197 audit).
pub(crate) fn clipboard_chord(mods: Modifiers, key: Key, windows: bool) -> Option<ClipboardChord> {
    let ctrl = mods.command || mods.ctrl;
    match key {
        Key::X if ctrl => Some(ClipboardChord::Cut),
        Key::C if ctrl => Some(ClipboardChord::Copy),
        Key::V if ctrl => Some(ClipboardChord::Paste),
        Key::Delete if windows && mods.shift => Some(ClipboardChord::Cut),
        Key::Insert if windows && ctrl => Some(ClipboardChord::Copy),
        Key::Insert if windows && mods.shift => Some(ClipboardChord::Paste),
        _ => None,
    }
}

/// Why `chord` cannot be given to `cmd` in `table` (none: it can). The
/// rows of `cmd` itself never conflict.
pub fn refusal(table: &[Keybind], cmd: Command, chord: Chord) -> Option<String> {
    let text = chord_text(chord.0, chord.1);
    if chord.1 == Key::Escape {
        return Some(format!(
            "{text} is kept for canceling gestures and dialogs."
        ));
    }
    for windows in [false, true] {
        match clipboard_chord(chord.0, chord.1, windows) {
            None => {}
            // A Cut chord runs only the command the Cut event runs.
            Some(ClipboardChord::Cut) if cut_command(chord.0, windows) == cmd => {}
            Some(_) => {
                return Some(format!(
                    "{text} is a clipboard shortcut{}.",
                    if windows { " on Windows" } else { "" }
                ));
            }
        }
    }
    if let Some(e) = EXTRA.iter().find(|e| e.1 == text) {
        return Some(format!("{text} is already used by {}.", e.2));
    }
    table
        .iter()
        .find(|k| k.command != cmd && chord_key((k.mods, k.key)) == chord_key(chord))
        .map(|k| format!("{text} is already used by {}.", plain(k.command)))
}

/// The table for `o` over the defaults, and one sentence per chord that
/// was refused (it is left out; the command's other chords stay).
pub fn build(o: &Overrides) -> (Vec<Keybind>, Vec<String>) {
    let mut table: Vec<Keybind> = KEYBINDS
        .iter()
        .copied()
        .filter(|k| !o.iter().any(|(c, _)| *c == k.command))
        .collect();
    let mut problems = Vec::new();
    for (cmd, chords) in o {
        let Some(proto) = KEYBINDS.iter().find(|k| k.command == *cmd).copied() else {
            problems.push(format!(
                "{} has no keyboard shortcut to change.",
                plain(*cmd)
            ));
            continue;
        };
        for &chord in chords {
            if table
                .iter()
                .any(|k| k.command == *cmd && chord_key((k.mods, k.key)) == chord_key(chord))
            {
                continue;
            }
            match refusal(&table, *cmd, chord) {
                Some(why) => problems.push(format!("{}: {why}", plain(*cmd))),
                None => table.push(Keybind {
                    mods: Modifiers {
                        command: chord.0.command || chord.0.ctrl,
                        ctrl: false,
                        ..chord.0
                    },
                    key: chord.1,
                    ..proto
                }),
            }
        }
    }
    (table, problems)
}

#[cfg(not(test))]
static EFFECTIVE: std::sync::RwLock<Option<std::sync::Arc<Vec<Keybind>>>> =
    std::sync::RwLock::new(None);

// Tests run on their own threads: each sees only the keybinds it installs.
#[cfg(test)]
thread_local! {
    static EFFECTIVE: std::cell::RefCell<Option<std::sync::Arc<Vec<Keybind>>>> =
        const { std::cell::RefCell::new(None) };
}

/// The table in force: the defaults until [`set_overrides`] installs the
/// user's keybinds (swapped whole; readers keep the copy they hold).
pub fn effective() -> std::sync::Arc<Vec<Keybind>> {
    #[cfg(not(test))]
    let t = EFFECTIVE.read().ok().and_then(|g| g.clone());
    #[cfg(test)]
    let t = EFFECTIVE.with(|e| e.borrow().clone());
    t.unwrap_or_else(|| std::sync::Arc::new(KEYBINDS.to_vec()))
}

/// Installs the table for `o` ([`build`]); returns the refusals.
pub fn set_overrides(o: &Overrides) -> Vec<String> {
    let (table, problems) = build(o);
    let table = (!o.is_empty()).then(|| std::sync::Arc::new(table));
    #[cfg(not(test))]
    if let Ok(mut g) = EFFECTIVE.write() {
        *g = table;
    }
    #[cfg(test)]
    EFFECTIVE.with(|e| *e.borrow_mut() = table);
    problems
}

/// `o` as JSON: `{"<command id>": ["chord", …]}` (`settings.json`, export).
pub fn overrides_to_json(o: &Overrides) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    for (cmd, chords) in o {
        m.insert(
            command_id(*cmd),
            serde_json::Value::Array(
                chords
                    .iter()
                    .map(|c| serde_json::Value::String(chord_text(c.0, c.1)))
                    .collect(),
            ),
        );
    }
    serde_json::Value::Object(m)
}

/// Reads [`overrides_to_json`]'s shape; each command and each chord falls
/// back alone (an unknown command or an unreadable chord is left out with
/// a problem, the others apply). Commands come in command-id order.
pub fn overrides_from_json(v: &serde_json::Value) -> (Overrides, Vec<String>) {
    let mut o = Overrides::new();
    let mut problems = Vec::new();
    let Some(m) = v.as_object() else {
        return (o, vec![format!("keybinds has an invalid value ({v})")]);
    };
    for (id, v) in m {
        let Some(cmd) = command_from_id(id) else {
            problems.push(format!("keybinds: unknown command {id:?}; left out"));
            continue;
        };
        let Some(list) = v.as_array() else {
            problems.push(format!(
                "keybinds.{id} has an invalid value ({v}); using the default"
            ));
            continue;
        };
        let mut chords = Vec::new();
        for c in list {
            match c.as_str().map(parse_chord) {
                Some(Ok(ch)) => chords.push(ch),
                _ => problems.push(format!("keybinds.{id}: {c} is not a shortcut; left out")),
            }
        }
        o.push((cmd, chords));
    }
    (o, problems)
}

/// The exported file: every rebindable command with the chords in force
/// (so the file reads on its own), `{"version": 1, "keybinds": {…}}`.
pub fn export_text() -> String {
    let table = effective();
    let all: Overrides = bindable()
        .into_iter()
        .map(|c| (c, chords_in(&table, c)))
        .collect();
    let v = serde_json::json!({
        "version": 1,
        "app": "PlumSpirit keyboard shortcuts",
        "keybinds": overrides_to_json(&all),
    });
    serde_json::to_string_pretty(&v).unwrap_or_default() + "\n"
}

/// v1.0-beta2 (B2-F29, §023): what a note command's chord does while the
/// Track List is shown — it acts on the selected clips (placements, never
/// the Piano Roll's notes). Shown beside the command in the Keyboard
/// Shortcuts window, so it follows the user's chords.
pub fn track_list_meaning(cmd: Command) -> Option<&'static str> {
    Some(match cmd {
        Command::Delete => "deletes the selected clips",
        Command::Cut => "copies and removes the selected clips",
        Command::SelectAll => "selects every clip",
        Command::SliceNotes => "slices the selected clips at the edit cursor",
        Command::OpenTool("duplicate") => {
            "duplicates the selected clips (linked), each right after itself"
        }
        Command::PasteRepeat => "repeats the selected clips (a dialog first)",
        _ => return None,
    })
}

/// Groups in display order.
pub const GROUPS: &[&str] = &[
    "File",
    "Edit",
    "Piano Roll",
    "Note Tools",
    "Plugins",
    "Tracks",
    "Track List",
    "View",
    "Playback",
];

/// B1-0060: the keybind editor in Help → Keyboard Shortcuts.
#[derive(Default)]
pub struct EditState {
    /// Waiting for the next chord: the command, and whether it is added to
    /// its chords (else it replaces them).
    capture: Option<(Command, bool)>,
    /// A chord another command holds: (target, chord, add, holder).
    conflict: Option<(Command, Chord, bool, Command)>,
    /// The last refusal or confirmation (one sentence).
    message: Option<String>,
}

impl EditState {
    /// While a chord is being captured no shortcut runs.
    pub fn capturing(&self) -> bool {
        self.capture.is_some()
    }
}

/// A command's label as a sentence part (without a trailing "…").
fn plain(c: Command) -> &'static str {
    c.label().trim_end_matches('…')
}

/// The default chords of `cmd`.
fn default_chords(cmd: Command) -> Vec<Chord> {
    KEYBINDS
        .iter()
        .filter(|k| k.command == cmd)
        .map(|k| (k.mods, k.key))
        .collect()
}

/// The chords of `cmd` in `table`.
fn chords_in(table: &[Keybind], cmd: Command) -> Vec<Chord> {
    table
        .iter()
        .filter(|k| k.command == cmd)
        .map(|k| (k.mods, k.key))
        .collect()
}

fn same_chords(a: &[Chord], b: &[Chord]) -> bool {
    a.len() == b.len()
        && a.iter()
            .all(|x| b.iter().any(|y| chord_key(*x) == chord_key(*y)))
}

/// The chord a key event in the editor stands for (clipboard events count
/// as their chords so they get their refusal). Bare modifiers are not.
fn captured_chord(e: &egui::Event) -> Option<Chord> {
    match e {
        egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => Some((*modifiers, *key)),
        egui::Event::Cut => Some((CTRL, Key::X)),
        egui::Event::Copy => Some((CTRL, Key::C)),
        egui::Event::Paste(_) => Some((CTRL, Key::V)),
        _ => None,
    }
}

impl super::PlumSpiritApp {
    /// B1-0060: installs `o` as the user's keybinds, keeps in the settings
    /// what is in force (refused chords left out) and saves them. Returns
    /// the refusals (sentences).
    pub(super) fn set_keybinds(&mut self, o: Overrides) -> Vec<String> {
        let problems = set_overrides(&o);
        let table = effective();
        self.settings.keybinds = o
            .iter()
            .map(|(cmd, _)| {
                let chords = table
                    .iter()
                    .filter(|k| k.command == *cmd)
                    .map(|k| (k.mods, k.key))
                    .collect();
                (*cmd, chords)
            })
            .collect();
        // Overrides equal to the defaults are not kept; the rest in the order
        // settings.json keeps them (command ids sorted).
        self.settings
            .keybinds
            .retain(|(c, chords)| !same_chords(chords, &default_chords(*c)));
        self.settings.keybinds.sort_by_key(|(c, _)| command_id(*c));
        self.save_settings();
        problems
    }

    /// B1-0060: gives `cmd` the chord `chord` (added to its chords or in
    /// place of them). With `steal`, a command holding the chord loses it;
    /// without, such a conflict is returned for confirmation. Other
    /// refusals (clipboard, reserved) come back as the message.
    pub(super) fn assign_chord(
        &mut self,
        cmd: Command,
        chord: Chord,
        add: bool,
        steal: bool,
    ) -> Result<(), String> {
        let table = effective();
        let text = chord_text(chord.0, chord.1);
        let holder = table
            .iter()
            .find(|k| k.command != cmd && chord_key((k.mods, k.key)) == chord_key(chord))
            .map(|k| k.command);
        let mut o = self.settings.keybinds.clone();
        let upsert = |o: &mut Overrides, c: Command, chords: Vec<Chord>| match o
            .iter_mut()
            .find(|(x, _)| *x == c)
        {
            Some(e) => e.1 = chords,
            None => o.push((c, chords)),
        };
        match holder {
            Some(h) if !steal => {
                self.keybind_edit.conflict = Some((cmd, chord, add, h));
                return Err(format!("{text} is already used by {}.", plain(h)));
            }
            Some(h) => {
                let rest: Vec<Chord> = chords_in(&table, h)
                    .into_iter()
                    .filter(|c| chord_key(*c) != chord_key(chord))
                    .collect();
                upsert(&mut o, h, rest);
            }
            None => {
                // Without the holder rule the table decides (clipboard, Escape, …).
                let without: Vec<Keybind> =
                    table.iter().copied().filter(|k| k.command != cmd).collect();
                if let Some(why) = refusal(&without, cmd, chord) {
                    return Err(why);
                }
            }
        }
        let mut chords = if add {
            chords_in(&table, cmd)
        } else {
            Vec::new()
        };
        if !chords.iter().any(|c| chord_key(*c) == chord_key(chord)) {
            chords.push(chord);
        }
        upsert(&mut o, cmd, chords);
        let problems = self.set_keybinds(o);
        match problems.into_iter().next() {
            Some(p) => Err(p),
            None => Ok(()),
        }
    }

    /// B1-0060: writes every shortcut in force to `path` ([`export_text`]).
    pub(super) fn export_keybinds_to(&mut self, path: &std::path::Path) -> Result<(), String> {
        std::fs::write(path, export_text()).map_err(|e| {
            format!(
                "The shortcuts could not be saved: {}.",
                crate::audio::engine::sentence(&e)
            )
        })
    }

    /// B1-0060: replaces the user's keybinds with the file's (commands it
    /// does not list keep their defaults). Returns the entries left out
    /// (sentences); nothing changes if the file is not a shortcuts file.
    pub(super) fn import_keybinds_from(
        &mut self,
        path: &std::path::Path,
    ) -> Result<Vec<String>, String> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            format!(
                "The shortcuts could not be read: {}.",
                crate::audio::engine::sentence(&e)
            )
        })?;
        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|_| "This file is not a PlumSpirit shortcuts file (JSON).".to_owned())?;
        let Some(k) = v.get("keybinds").filter(|k| k.is_object()) else {
            return Err("This file holds no PlumSpirit keyboard shortcuts.".to_owned());
        };
        let (o, mut problems) = overrides_from_json(k);
        problems.extend(self.set_keybinds(o));
        Ok(problems)
    }

    fn export_keybinds(&mut self, frame: &eframe::Frame) {
        if let Some(p) = super::files::pick_save(
            frame,
            super::files::FileKind::Keybinds,
            None,
            "PlumSpirit shortcuts.json",
        ) {
            self.keybind_edit.message = Some(match self.export_keybinds_to(&p) {
                Ok(()) => format!("Shortcuts saved to {}.", p.display()),
                Err(e) => e,
            });
        }
    }

    fn import_keybinds(&mut self, frame: &eframe::Frame) {
        if let Some(p) = super::files::pick_open(frame, super::files::FileKind::Keybinds, None) {
            self.keybind_edit.message = Some(match self.import_keybinds_from(&p) {
                Ok(left) if left.is_empty() => "Shortcuts imported.".to_owned(),
                Ok(left) => format!("Shortcuts imported; left out: {}", left.join(" ")),
                Err(e) => e,
            });
        }
    }

    /// B1-0060: `cmd` without any shortcut.
    pub(super) fn clear_keybind(&mut self, cmd: Command) {
        let mut o = self.settings.keybinds.clone();
        o.retain(|(c, _)| *c != cmd);
        o.push((cmd, Vec::new()));
        self.set_keybinds(o);
    }

    /// B1-0060: `cmd` back to its default shortcuts (its chords must be free:
    /// a command holding one of them loses it).
    pub(super) fn reset_keybind(&mut self, cmd: Command) {
        let mut o = self.settings.keybinds.clone();
        o.retain(|(c, _)| *c != cmd);
        let defaults = default_chords(cmd);
        for (_, chords) in o.iter_mut() {
            chords.retain(|c| !defaults.iter().any(|d| chord_key(*d) == chord_key(*c)));
        }
        self.set_keybinds(o);
    }

    /// Help → Keyboard Shortcuts… (F1): every shortcut of the table in
    /// force by group, with a filter, and the editor (B1-0060): Change… /
    /// Add… capture the next chord (Esc cancels), a chord another command
    /// holds asks before it moves, Clear, Reset, Reset All. Modeless.
    pub(super) fn keybinds_window_ui(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if !self.keybinds_window {
            self.keybind_edit.capture = None;
            return;
        }
        // A captured chord is applied before anything else reads the input.
        if let Some((cmd, add)) = self.keybind_edit.capture
            && let Some(ch) = ctx.input(|i| i.events.iter().find_map(captured_chord))
        {
            self.keybind_edit.capture = None;
            ctx.input_mut(|i| i.events.clear());
            self.keybind_edit.message = if ch.1 == Key::Escape && ch.0.is_none() {
                None
            } else {
                Some(match self.assign_chord(cmd, ch, add, false) {
                    Ok(()) => format!("{}: {}.", plain(cmd), shortcuts_of(cmd).join(", ")),
                    Err(e) => e,
                })
            };
        }
        enum Act {
            Capture(Command, bool),
            Clear(Command),
            Reset(Command),
            ResetAll,
            Replace,
            CancelConflict,
            Export,
            Import,
        }
        let mut acts: Vec<Act> = Vec::new();
        let mut open = true;
        let mut filter = std::mem::take(&mut self.keybinds_filter);
        let table = effective();
        let changed: Vec<Command> = self.settings.keybinds.iter().map(|(c, _)| *c).collect();
        let capture = self.keybind_edit.capture;
        let conflict = self.keybind_edit.conflict;
        let message = self.keybind_edit.message.clone();
        egui::Window::new("Keyboard Shortcuts")
            .id(egui::Id::new("plumspirit-keybinds"))
            .open(&mut open)
            .default_size([560.0, 560.0])
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut filter)
                            .hint_text("Find a command or key")
                            .desired_width(ui.available_width() - 230.0),
                    );
                    if ui
                        .button("Export…")
                        .on_hover_text("Saves every shortcut to a .json file (to keep or share).")
                        .clicked()
                    {
                        acts.push(Act::Export);
                    }
                    if ui
                        .button("Import…")
                        .on_hover_text("Uses the shortcuts of a .json file exported before; commands it does not list keep their defaults.")
                        .clicked()
                    {
                        acts.push(Act::Import);
                    }
                    if ui
                        .add_enabled(!changed.is_empty(), egui::Button::new("Reset All"))
                        .on_hover_text("Every shortcut back to PlumSpirit's defaults.")
                        .clicked()
                    {
                        acts.push(Act::ResetAll);
                    }
                });
                if let Some((cmd, add)) = capture {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!(
                            "Press the shortcut to {} {} (Esc cancels).",
                            if add { "add to" } else { "give" },
                            plain(cmd)
                        ),
                    );
                } else if let Some((cmd, chord, _, holder)) = conflict {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            format!(
                                "{} is already used by {}. Move it to {}?",
                                chord_text(chord.0, chord.1),
                                plain(holder),
                                plain(cmd)
                            ),
                        );
                        if ui.small_button("Move").clicked() {
                            acts.push(Act::Replace);
                        }
                        if ui.small_button("Cancel").clicked() {
                            acts.push(Act::CancelConflict);
                        }
                    });
                } else if let Some(m) = &message {
                    ui.label(egui::RichText::new(m).weak());
                }
                ui.add_space(4.0);
                let f = filter.trim().to_lowercase();
                let all = bindable();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // One grid for every group, so the columns line up.
                        egui::Grid::new("keys")
                            .num_columns(3)
                            .spacing([16.0, 2.0])
                            .show(ui, |ui| {
                        for g in GROUPS {
                            let cmds: Vec<(Command, String)> = all
                                .iter()
                                .copied()
                                .filter(|c| KEYBINDS.iter().any(|k| k.command == *c && k.group == *g))
                                .map(|c| {
                                    let chords: Vec<String> = table
                                        .iter()
                                        .filter(|k| k.command == c)
                                        .map(|k| chord_text(k.mods, k.key))
                                        .collect();
                                    (c, chords.join(", "))
                                })
                                .filter(|(c, t)| {
                                    f.is_empty()
                                        || t.to_lowercase().contains(&f)
                                        || c.label().to_lowercase().contains(&f)
                                })
                                .collect();
                            if cmds.is_empty() {
                                continue;
                            }
                            ui.end_row();
                            ui.label(egui::RichText::new(*g).strong());
                            ui.end_row();
                                    for (c, chords) in cmds {
                                        let shown = if chords.is_empty() { "—".to_owned() } else { chords };
                                        let mut t = egui::RichText::new(shown).monospace();
                                        if changed.contains(&c) {
                                            t = t.strong();
                                        }
                                        ui.label(t);
                                        match track_list_meaning(c) {
                                            Some(m) => ui.label(format!("{} — in the Track List: {m}", c.label())),
                                            None => ui.label(c.label()),
                                        };
                                        ui.horizontal(|ui| {
                                            if ui
                                                .small_button("Change…")
                                                .on_hover_text("Press the new shortcut; it replaces this command's shortcuts.")
                                                .clicked()
                                            {
                                                acts.push(Act::Capture(c, false));
                                            }
                                            if ui
                                                .small_button("Add…")
                                                .on_hover_text("Press another shortcut for this command.")
                                                .clicked()
                                            {
                                                acts.push(Act::Capture(c, true));
                                            }
                                            if ui.small_button("Clear").on_hover_text("No shortcut for this command.").clicked() {
                                                acts.push(Act::Clear(c));
                                            }
                                            if ui
                                                .add_enabled(changed.contains(&c), egui::Button::new("Reset").small())
                                                .on_hover_text("This command's default shortcuts.")
                                                .clicked()
                                            {
                                                acts.push(Act::Reset(c));
                                            }
                                        });
                                        ui.end_row();
                                    }
                        }
                            });
                        // Fixed shortcuts (clipboard events, mouse gestures,
                        // Exit): shown, not editable.
                        let extra: Vec<&(&str, &str, &str)> = EXTRA
                            .iter()
                            .filter(|e| {
                                f.is_empty()
                                    || e.1.to_lowercase().contains(&f)
                                    || e.2.to_lowercase().contains(&f)
                            })
                            .collect();
                        if !extra.is_empty() {
                            ui.add_space(8.0);
                            ui.label(egui::RichText::new("Fixed (clipboard, mouse, Exit)").strong());
                            egui::Grid::new("keys-fixed")
                                .num_columns(2)
                                .spacing([16.0, 2.0])
                                .show(ui, |ui| {
                                    for e in extra {
                                        ui.label(egui::RichText::new(e.1).monospace().weak());
                                        ui.label(format!("{} ({})", e.2, e.0));
                                        ui.end_row();
                                    }
                                });
                        }
                    });
            });
        self.keybinds_filter = filter;
        for a in acts {
            match a {
                Act::Capture(c, add) => {
                    self.keybind_edit.capture = Some((c, add));
                    self.keybind_edit.conflict = None;
                    self.keybind_edit.message = None;
                    ctx.memory_mut(|m| m.stop_text_input());
                }
                Act::Clear(c) => {
                    self.clear_keybind(c);
                    self.keybind_edit.message = Some(format!("{}: no shortcut.", plain(c)));
                }
                Act::Reset(c) => {
                    self.reset_keybind(c);
                    self.keybind_edit.message =
                        Some(format!("{}: {}.", plain(c), shortcuts_of(c).join(", ")));
                }
                Act::ResetAll => {
                    self.set_keybinds(Vec::new());
                    self.keybind_edit = EditState::default();
                    self.keybind_edit.message =
                        Some("Every shortcut is back to its default.".to_owned());
                }
                Act::Replace => {
                    if let Some((cmd, chord, add, _)) = self.keybind_edit.conflict.take() {
                        self.keybind_edit.message =
                            Some(match self.assign_chord(cmd, chord, add, true) {
                                Ok(()) => {
                                    format!("{}: {}.", plain(cmd), shortcuts_of(cmd).join(", "))
                                }
                                Err(e) => e,
                            });
                    }
                }
                Act::CancelConflict => self.keybind_edit.conflict = None,
                Act::Export => self.export_keybinds(frame),
                Act::Import => self.import_keybinds(frame),
            }
        }
        if !open {
            self.keybinds_window = false;
            self.keybind_edit.capture = None;
            self.keybind_edit.conflict = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// B1-0066: tooltip hints follow the user's keybinds — a rebound command
    /// shows its new key, an unbound one none (messages name the menu).
    #[test]
    fn tooltip_hints_follow_the_users_keybinds() {
        set_overrides(&vec![]);
        assert_eq!(hint(Command::Undo), " (Ctrl+Z)");
        assert_eq!(hint(Command::Redo), " (Ctrl+Y, Ctrl+Shift+Z)");
        assert_eq!(first_key(Command::Redo), "Ctrl+Y");
        set_overrides(&vec![(Command::Undo, vec![(CTRL, Key::U)])]);
        assert_eq!(hint(Command::Undo), " (Ctrl+U)");
        assert_eq!(key_or_menu(Command::Undo, "Edit → Undo"), "Ctrl+U");
        set_overrides(&vec![(Command::Undo, vec![])]);
        assert_eq!(hint(Command::Undo), "");
        assert_eq!(key_or_menu(Command::Undo, "Edit → Undo"), "Edit → Undo");
        set_overrides(&vec![]);
        assert_eq!(hint(Command::Undo), " (Ctrl+Z)");
    }

    /// v0.9 §197 audit: a row on a chord egui-winit turns into a clipboard
    /// event (on Linux or Windows) only works if the event handler runs the
    /// row's command; Copy / Paste chords are never rows.
    #[test]
    fn clipboard_chords_in_the_table_still_run_their_command_on_every_platform() {
        let mut cut_rows = 0;
        for windows in [false, true] {
            for k in KEYBINDS {
                match clipboard_chord(k.mods, k.key, windows) {
                    None => {}
                    Some(ClipboardChord::Cut) => {
                        cut_rows += 1;
                        assert_eq!(
                            cut_command(k.mods, windows),
                            k.command,
                            "{} (windows {windows})",
                            chord_text(k.mods, k.key)
                        );
                    }
                    Some(c) => panic!("{} is a {c:?} chord", chord_text(k.mods, k.key)),
                }
            }
        }
        // Ctrl+X on both platforms, Ctrl+Shift+Del on Windows.
        assert_eq!(cut_rows, 3);
        let table: Vec<String> = KEYBINDS.iter().map(|k| chord_text(k.mods, k.key)).collect();
        // Reserved Track List chords (§198) stay free and usable.
        for (mods, key, what) in RESERVED {
            let chord = chord_text(*mods, *key);
            assert!(!table.contains(&chord), "{chord} ({what}) is taken");
            for windows in [false, true] {
                assert_eq!(clipboard_chord(*mods, *key, windows), None, "{chord}");
            }
        }
        let ctrl_shift = Modifiers {
            shift: true,
            ..Modifiers::COMMAND
        };
        assert_eq!(cut_command(ctrl_shift, true), Command::DeleteSelectedTracks);
        assert_eq!(cut_command(Modifiers::SHIFT, true), Command::Delete);
        assert_eq!(cut_command(Modifiers::COMMAND, true), Command::Cut);
        assert_eq!(cut_command(ctrl_shift, false), Command::Cut);
        // The chords handled outside the table never repeat a table chord.
        for (_, chord, _) in EXTRA {
            assert!(
                !table.iter().any(|t| t == chord),
                "{chord} is also a table row"
            );
        }
    }

    #[test]
    fn no_two_keybinds_share_a_chord_and_every_one_is_listed() {
        let mut seen = std::collections::HashSet::new();
        for k in KEYBINDS {
            let chord = (
                k.mods.command || k.mods.ctrl,
                k.mods.shift,
                k.mods.alt,
                k.key,
            );
            assert!(
                seen.insert(chord),
                "duplicate chord {}",
                chord_text(k.mods, k.key)
            );
            assert!(GROUPS.contains(&k.group), "{}", k.group);
        }
        // Shift rows come first so Ctrl+S cannot swallow Ctrl+Shift+S.
        let v = ordered(false);
        let first_plain = v.iter().position(|k| !k.mods.shift).unwrap();
        assert!(v[first_plain..].iter().all(|k| !k.mods.shift));
        // Copy / Paste come from clipboard events: no table row may claim them.
        // Ctrl+X is listed once and routed through the Cut event.
        assert!(
            !KEYBINDS
                .iter()
                .any(|k| (k.mods.command) && matches!(k.key, Key::C | Key::V))
        );
        let x: Vec<_> = KEYBINDS
            .iter()
            .filter(|k| k.mods.command && k.key == Key::X)
            .collect();
        assert!(x.len() == 1 && x[0].command == Command::Cut && !x[0].mods.shift && !x[0].mods.alt);
        // Menus and tooltips show every chord of a command.
        assert_eq!(Command::Delete.shortcut(), "Del");
        assert_eq!(Command::Cut.shortcut(), "Ctrl+X");
        assert_eq!(Command::Copy.shortcut(), "Ctrl+C");
        // The commands added in v0.8 are reachable from the keyboard.
        for c in [
            Command::RepeatLastTool,
            Command::RepeatLastToolNewSeed,
            Command::ReopenLastTool,
            Command::MuteNotes,
            Command::UnmuteNotes,
            Command::SliceNotes,
        ] {
            assert!(!c.shortcut().is_empty(), "{}", c.label());
        }
        assert_eq!(Command::SliceNotes.shortcut(), "Ctrl+K");
        assert_eq!(Command::DuplicateOctaveUp.shortcut(), "Ctrl+Alt+↑");
        assert_eq!(Command::PaintCurrentSubchannel.shortcut(), "Ctrl+P");
        assert_eq!(
            Command::OpenTool("paint_color").shortcut(),
            "Ctrl+Shift+P",
            "Paint Color keeps its chord"
        );
        assert_eq!(Command::DuplicateOctaveDown.shortcut(), "Ctrl+Alt+↓");
        assert_eq!(
            Some(Command::UngroupNotes.shortcut()).as_deref(),
            Some("Ctrl+Shift+G")
        );
        assert_eq!(
            Some(Command::MoveTrackUp.shortcut()).as_deref(),
            Some("Alt+↑")
        );
        // Every tool shortcut names a registered tool.
        for k in KEYBINDS {
            if let Command::OpenTool(id) = k.command {
                assert!(plumspirit_engine::transform::tool(id).is_some(), "{id}");
            }
        }
    }

    /// V09-S10: Insert (reserved by V09-0142) adds a Piano Roll Clip at the
    /// edit cursor; it is a row of the table now, no longer reserved.
    #[test]
    fn insert_adds_a_piano_roll_clip_and_is_no_longer_reserved() {
        let k = KEYBINDS
            .iter()
            .find(|k| k.command == Command::AddClip)
            .expect("Insert row");
        assert_eq!((k.mods, k.key, k.group), (NONE, Key::Insert, "Track List"));
        assert!(!RESERVED.iter().any(|(_, key, _)| *key == Key::Insert));
        assert_eq!(Command::AddClip.label(), "Add Piano Roll Clip");
    }

    /// B1-0060: every default chord reads back from its own text, and the
    /// usual spellings are accepted; other text is refused with a sentence.
    #[test]
    fn chords_read_back_from_their_text() {
        for k in KEYBINDS {
            let text = chord_text(k.mods, k.key);
            let c = parse_chord(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(chord_key(c), chord_key((k.mods, k.key)), "{text}");
        }
        let ctrl_shift = Modifiers {
            shift: true,
            ..Modifiers::COMMAND
        };
        assert_eq!(parse_chord("ctrl+shift+g"), Ok((ctrl_shift, Key::G)));
        assert_eq!(
            parse_chord(" Control + Shift + G "),
            Ok((ctrl_shift, Key::G))
        );
        assert_eq!(parse_chord("Alt+Up"), Ok((ALT, Key::ArrowUp)));
        assert_eq!(parse_chord("Delete"), Ok((NONE, Key::Delete)));
        assert_eq!(parse_chord("F4"), Ok((NONE, Key::F4)));
        assert_eq!(parse_chord("Ctrl+7"), Ok((CTRL, Key::Num7)));
        assert_eq!(parse_chord("Tab"), Ok((NONE, Key::Tab)));
        for bad in ["", "Ctrl+", "Hyper+G", "Ctrl+Foo", "Ctrl+Shift"] {
            let e = parse_chord(bad).unwrap_err();
            assert!(e.ends_with('.'), "{bad}: {e}");
        }
    }

    /// B1-0060: command ids are unique and read back.
    #[test]
    fn command_ids_are_unique_and_read_back() {
        let all = bindable();
        let mut ids = std::collections::HashSet::new();
        for c in &all {
            assert!(ids.insert(command_id(*c)), "{c:?}");
            assert_eq!(command_from_id(&command_id(*c)), Some(*c));
        }
        assert_eq!(
            command_id(Command::OpenTool("humanize")),
            "OpenTool(\"humanize\")"
        );
        assert_eq!(command_from_id("NoSuchCommand"), None);
        assert!(all.len() > 60, "{}", all.len());
    }

    /// B1-0060: an override replaces the command's chords in dispatch and in
    /// the menus; its old chord becomes free; an empty list unbinds; a
    /// conflicting, clipboard or reserved chord is refused with a sentence
    /// (the rest applies); no overrides restore the defaults.
    #[test]
    fn overrides_change_dispatch_and_menus_and_refuse_conflicts() {
        let ctrl_alt = |k| (CTRL_ALT, k);
        let problems = set_overrides(&vec![
            (Command::Undo, vec![ctrl_alt(Key::Z)]),
            // Ctrl+Z is free now (Undo moved), Ctrl+Y is Redo's: refused.
            (Command::GroupNotes, vec![(CTRL, Key::Z), (CTRL, Key::Y)]),
            (Command::ToggleOnionOutlines, vec![]),
            (
                Command::ScrubTool,
                vec![(CTRL, Key::C), (ALT, Key::F4), (NONE, Key::Escape)],
            ),
        ]);
        assert_eq!(shortcuts_of(Command::Undo), vec!["Ctrl+Alt+Z"]);
        assert_eq!(Command::Undo.shortcut(), "Ctrl+Alt+Z", "menus follow");
        assert_eq!(shortcuts_of(Command::GroupNotes), vec!["Ctrl+Z"]);
        assert!(shortcuts_of(Command::ToggleOnionOutlines).is_empty());
        assert!(shortcuts_of(Command::ScrubTool).is_empty());
        assert_eq!(problems.len(), 4, "{problems:?}");
        assert_eq!(problems[0], "Group Notes: Ctrl+Y is already used by Redo.");
        assert!(
            problems[1].contains("Ctrl+C is a clipboard shortcut"),
            "{}",
            problems[1]
        );
        assert!(
            problems[2].contains("Alt+F4 is already used by Exit"),
            "{}",
            problems[2]
        );
        assert!(problems[3].contains("Escape is kept"), "{}", problems[3]);
        let rows = ordered(false);
        assert!(
            rows.iter()
                .any(|k| k.command == Command::Undo && k.mods == CTRL_ALT && k.key == Key::Z)
        );
        assert!(
            !rows
                .iter()
                .any(|k| k.command == Command::Undo && k.mods == CTRL)
        );
        // Shift rows still first.
        let first_plain = rows.iter().position(|k| !k.mods.shift).unwrap();
        assert!(rows[first_plain..].iter().all(|k| !k.mods.shift));
        // A transport command keeps its class.
        set_overrides(&vec![(Command::PlayPause, vec![(NONE, Key::F3)])]);
        assert!(
            ordered(true)
                .iter()
                .any(|k| k.command == Command::PlayPause && k.key == Key::F3)
        );
        assert!(
            !ordered(false)
                .iter()
                .any(|k| k.command == Command::PlayPause)
        );
        // Ctrl+X is reserved for the Cut event.
        let p = set_overrides(&vec![(Command::Quantize, vec![(CTRL, Key::X)])]);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(set_overrides(&vec![(Command::Cut, vec![(CTRL, Key::X)])]).is_empty());
        assert_eq!(shortcuts_of(Command::Cut), vec!["Ctrl+X"]);
        // Back to the defaults.
        assert!(set_overrides(&Vec::new()).is_empty());
        assert_eq!(shortcuts_of(Command::Undo), vec!["Ctrl+Z"]);
        assert_eq!(Command::Delete.shortcut(), "Del");
        assert_eq!(Command::Cut.shortcut(), "Ctrl+X");
        assert_eq!(effective().len(), KEYBINDS.len());
    }

    /// B1-0060: the app's dispatcher follows the user's keybinds, which are
    /// kept in the settings as they are in force; clearing them restores the
    /// default chord.
    #[test]
    fn the_dispatcher_follows_the_users_keybinds() {
        use crate::app::test_support::Harness;
        use plumspirit_engine::model::PlumDocument;
        let mut h = Harness::new(PlumDocument::new_project(
            "k".into(),
            Default::default(),
            480,
        ));
        let before = h.app.settings.selection_analytics;
        h.press(Key::F6);
        assert_ne!(h.app.settings.selection_analytics, before, "default F6");
        let p = h.app.set_keybinds(vec![(
            Command::ToggleSelectionAnalytics,
            vec![(NONE, Key::F4), (NONE, Key::F7)],
        )]);
        assert_eq!(
            p,
            vec!["Selection Analytics: F7 is already used by Ghost Outlines.".to_owned()]
        );
        assert_eq!(
            h.app.settings.keybinds,
            vec![(Command::ToggleSelectionAnalytics, vec![(NONE, Key::F4)])],
            "only what is in force is kept"
        );
        let s = h.app.settings.selection_analytics;
        h.press(Key::F6);
        assert_eq!(h.app.settings.selection_analytics, s, "F6 is free now");
        h.press(Key::F4);
        assert_ne!(h.app.settings.selection_analytics, s, "F4 runs it");
        assert!(h.app.set_keybinds(Vec::new()).is_empty());
        assert!(h.app.settings.keybinds.is_empty());
        assert_eq!(Command::ToggleSelectionAnalytics.shortcut(), "F6");
    }

    /// B1-0060: the editor's operations — Change replaces, Add adds, a chord
    /// another command holds is asked about and moves only when confirmed,
    /// Clear unbinds, Reset (one / all, and the Settings Keybinds section)
    /// restores; overrides equal to the defaults are not kept.
    #[test]
    fn the_editor_changes_adds_moves_clears_and_resets() {
        use crate::app::settings_window::Section;
        use crate::app::test_support::Harness;
        use plumspirit_engine::model::PlumDocument;
        let mut h = Harness::new(PlumDocument::new_project(
            "k".into(),
            Default::default(),
            480,
        ));
        let app = &mut h.app;
        assert_eq!(
            app.assign_chord(Command::Quantize, (NONE, Key::F4), false, false),
            Ok(())
        );
        assert_eq!(Command::Quantize.shortcut(), "F4");
        assert_eq!(
            app.assign_chord(Command::Quantize, (CTRL, Key::Q), true, false),
            Ok(())
        );
        assert_eq!(Command::Quantize.shortcut(), "F4, Ctrl+Q");
        // Ctrl+G is Group Notes': asked, nothing changes until confirmed.
        let e = app.assign_chord(Command::Quantize, (CTRL, Key::G), false, false);
        assert_eq!(e, Err("Ctrl+G is already used by Group Notes.".to_owned()));
        assert_eq!(Command::Quantize.shortcut(), "F4, Ctrl+Q");
        assert!(app.keybind_edit.conflict.is_some());
        assert_eq!(
            app.assign_chord(Command::Quantize, (CTRL, Key::G), false, true),
            Ok(())
        );
        assert_eq!(Command::Quantize.shortcut(), "Ctrl+G");
        assert_eq!(Command::GroupNotes.shortcut(), "", "moved away");
        // Clipboard / Escape are refused outright.
        assert!(
            app.assign_chord(Command::Quantize, (CTRL, Key::V), false, true)
                .is_err()
        );
        assert!(
            app.assign_chord(Command::Quantize, (NONE, Key::Escape), false, true)
                .is_err()
        );
        app.clear_keybind(Command::ToggleLoop);
        assert_eq!(Command::ToggleLoop.shortcut(), "");
        // Reset Quantize: its Ctrl+Q is free; Group Notes stays unbound.
        app.reset_keybind(Command::Quantize);
        assert_eq!(Command::Quantize.shortcut(), "Ctrl+Q");
        assert_eq!(Command::GroupNotes.shortcut(), "");
        app.reset_keybind(Command::GroupNotes);
        assert_eq!(Command::GroupNotes.shortcut(), "Ctrl+G");
        // Only ToggleLoop is still changed (defaults are not kept).
        assert_eq!(app.settings.keybinds, vec![(Command::ToggleLoop, vec![])]);
        // Changing back to the default chord drops the override.
        assert_eq!(
            app.assign_chord(Command::ToggleLoop, (CTRL, Key::L), false, false),
            Ok(())
        );
        assert!(app.settings.keybinds.is_empty());
        app.clear_keybind(Command::ToggleLoop);
        assert_eq!(
            h.settings_reset(Some(Section::Playback)),
            0,
            "not Playback's"
        );
        assert_eq!(h.settings_reset(Some(Section::Shortcuts)), 1);
        assert!(h.app.settings.keybinds.is_empty());
        assert_eq!(Command::ToggleLoop.shortcut(), "Ctrl+L");
    }

    /// B1-0060: in the Keyboard Shortcuts window Change… captures the next
    /// chord (no shortcut runs meanwhile), Esc cancels, a held chord asks.
    #[test]
    fn the_window_captures_the_next_chord() {
        use crate::app::test_support::Harness;
        use plumspirit_engine::model::PlumDocument;
        let mut h = Harness::new(PlumDocument::new_project(
            "k".into(),
            Default::default(),
            480,
        ));
        let frame =
            |h: &mut Harness, key: Key, modifiers: Modifiers| h.keybinds_frame(key, modifiers);
        h.app.keybinds_window = true;
        h.app.keybind_edit.capture = Some((Command::ToggleSelectionAnalytics, false));
        let before = h.app.settings.selection_analytics;
        frame(&mut h, Key::F7, NONE);
        assert_eq!(
            h.app.settings.selection_analytics, before,
            "no shortcut ran"
        );
        assert!(!h.app.keybind_edit.capturing());
        assert!(
            h.app.keybind_edit.conflict.is_some(),
            "F7 is Ghost Outlines'"
        );
        h.app.keybind_edit.conflict = None;
        h.app.keybind_edit.capture = Some((Command::ToggleSelectionAnalytics, false));
        frame(&mut h, Key::Escape, NONE);
        assert_eq!(
            Command::ToggleSelectionAnalytics.shortcut(),
            "F6",
            "Esc cancels"
        );
        h.app.keybind_edit.capture = Some((Command::ToggleSelectionAnalytics, false));
        frame(&mut h, Key::F4, ALT);
        // Alt+F4 is Exit's (fixed): refused with a sentence, nothing changed.
        assert_eq!(Command::ToggleSelectionAnalytics.shortcut(), "F6");
        assert!(
            h.app
                .keybind_edit
                .message
                .as_deref()
                .unwrap_or("")
                .contains("Exit")
        );
        h.app.keybind_edit.capture = Some((Command::ToggleSelectionAnalytics, true));
        frame(&mut h, Key::F4, NONE);
        assert_eq!(Command::ToggleSelectionAnalytics.shortcut(), "F6, F4");
        assert_eq!(
            h.app.keybind_edit.message.as_deref(),
            Some("Selection Analytics: F6, F4.")
        );
    }

    /// B1-0060: Export writes every shortcut in force; Import replaces the
    /// user's keybinds with the file's (bad entries left out, named), and a
    /// file that is not a shortcuts file changes nothing.
    #[test]
    fn shortcuts_export_and_import_through_a_file() {
        use crate::app::test_support::Harness;
        use plumspirit_engine::model::PlumDocument;
        let mut h = Harness::new(PlumDocument::new_project(
            "k".into(),
            Default::default(),
            480,
        ));
        let dir = std::env::temp_dir().join(format!("ps-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("keys.json");
        assert_eq!(
            h.app
                .assign_chord(Command::Save, (NONE, Key::F4), false, false),
            Ok(())
        );
        h.app.export_keybinds_to(&file).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("\"Save\": [\n      \"F4\"\n    ]"), "{text}");
        assert!(
            text.contains("\"Undo\": [\n      \"Ctrl+Z\"\n    ]"),
            "defaults too"
        );
        // Back to defaults, then import: Save is F4 again, nothing else kept.
        h.app.set_keybinds(Vec::new());
        assert_eq!(h.app.import_keybinds_from(&file), Ok(vec![]));
        assert_eq!(Command::Save.shortcut(), "F4");
        assert_eq!(
            h.app.settings.keybinds,
            vec![(Command::Save, vec![(NONE, Key::F4)])]
        );
        // Bad entries are named, the rest applies.
        std::fs::write(
            &file,
            r#"{"version": 1, "keybinds": {"Save": ["Hyper+S"], "NoSuch": [], "Quantize": ["Ctrl+Y"], "Undo": ["F3"]}}"#,
        )
        .unwrap();
        let left = h.app.import_keybinds_from(&file).unwrap();
        assert_eq!(left.len(), 3, "{left:?}");
        assert!(
            left.iter()
                .any(|l| l.contains("Ctrl+Y is already used by Redo")),
            "{left:?}"
        );
        assert_eq!(Command::Undo.shortcut(), "F3");
        assert_eq!(
            Command::Save.shortcut(),
            "",
            "its only chord was unreadable"
        );
        // Not a shortcuts file: refused, nothing changes.
        std::fs::write(&file, "{\"version\": 1}").unwrap();
        assert_eq!(
            h.app.import_keybinds_from(&file),
            Err("This file holds no PlumSpirit keyboard shortcuts.".to_owned())
        );
        std::fs::write(&file, "not json").unwrap();
        assert!(h.app.import_keybinds_from(&file).is_err());
        assert!(
            h.app
                .import_keybinds_from(&dir.join("missing.json"))
                .unwrap_err()
                .starts_with("The shortcuts could not be read: ")
        );
        assert_eq!(Command::Undo.shortcut(), "F3");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// B1-0061: Tab switches Track List ↔ Piano Roll and back (each view
    /// keeps its state: the open clip, the roll's view), not while a text
    /// field has the keyboard, and it can be rebound.
    #[test]
    fn tab_switches_the_main_view_and_can_be_rebound() {
        use crate::app::test_support::Harness;
        use crate::app::track_list::MainView;
        use plumspirit_engine::model::PlumDocument;
        let mut h = Harness::new(PlumDocument::new_project(
            "k".into(),
            Default::default(),
            480,
        ));
        let tab = |h: &mut Harness| h.transport_press(Key::Tab);
        assert_eq!(h.app.main_view, MainView::TrackList);
        h.app.roll.view.time.origin_tick = 4_321;
        tab(&mut h);
        assert_eq!(h.app.main_view, MainView::PianoRoll);
        assert_eq!(
            h.app.roll.view.time.origin_tick, 4_321,
            "the roll keeps its view"
        );
        tab(&mut h);
        assert_eq!(h.app.main_view, MainView::TrackList);
        assert_eq!(Command::SwitchView.shortcut(), "Tab");
        assert_eq!(
            Command::SwitchView.label(),
            "Switch Track List / Piano Roll"
        );
        // Rebound to F3: Tab does nothing, F3 switches.
        assert!(
            h.app
                .set_keybinds(vec![(Command::SwitchView, vec![(NONE, Key::F3)])])
                .is_empty()
        );
        tab(&mut h);
        assert_eq!(h.app.main_view, MainView::TrackList);
        h.app.set_keybinds(Vec::new());
        // Without a project there is nothing to switch.
        assert!(h.app.enabled(Command::SwitchView));
    }
}
