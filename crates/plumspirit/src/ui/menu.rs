//! Main menu: File / Edit / Tools / Song / View / Playback. Every visible item
//! is functional; items that cannot act right now are shown disabled.

use crate::app::commands::Command;
use egui::Button;
use plumspirit_engine::model::OnionMode;
use plumspirit_engine::model::project::{
    DensityBasis, MAX_ONION_OPACITY, MIN_ONION_OPACITY, OnionOrder,
};

/// Direct quick transforms; the same routes are used by both menus.
pub const SIMPLE_COMMANDS: [Command; 4] = [
    Command::FlipX,
    Command::FlipY,
    Command::ScaleX,
    Command::ScaleY,
];

/// The Tools menu's sections, in order (plugins have their own menu). v0.9
/// §087 / §213: the owner's Iterations section (Paint V2 and — B1-0099 —
/// Duplicate V2, also Edit → Paste Repeat… / Ctrl+B in the Piano Roll)
/// follows the other Piano Roll tool sections; v0.9 §151 (V09-S51): the
/// Generator section (Simple, then Advanced) after Experimental. The former
/// Paste Repeat section is gone: it held only Duplicate V2.
pub const TOOL_MENU_FAMILIES: [plumspirit_engine::transform::registry::Family; 7] = {
    use plumspirit_engine::transform::registry::Family;
    [
        Family::Fl,
        Family::Editor,
        Family::Interactive,
        Family::Geometry,
        Family::Iterations,
        Family::Experimental,
        Family::Generator,
    ]
};

/// One Tools-menu submenu: its label, whether it is a marked Legacy section
/// (v0.9 §113), and its tools (ids, menu order).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolSection {
    pub label: &'static str,
    pub legacy: bool,
    pub tools: Vec<&'static str>,
    /// Small headings inside the submenu: (index of the first tool under
    /// it, text) — the Generator section's "Simple" / "Advanced" (§151).
    pub headers: Vec<(usize, &'static str)>,
}

/// The weak line at the top of a Legacy submenu.
pub const LEGACY_NOTE: &str = "Replaced tools, kept so old presets keep working.";

/// The Tools menu's submenus with Legacy table `legacy`: the family sections
/// without moved tools, then the marked "Legacy" and "Legacy Experimental"
/// sections (v0.9 §109, §113: a tool moved to Legacy keeps its id, window
/// and presets; only where it is listed changes). Empty sections are left
/// out.
pub fn tool_menu_sections(
    legacy: &plumspirit_engine::transform::definition::LegacyTable,
) -> Vec<ToolSection> {
    use plumspirit_engine::transform::definition::Category;
    let tools: Vec<(&'static str, _, Category)> = plumspirit_engine::transform::registry()
        .iter()
        .map(|t| {
            let family = (t.spec)().family;
            (t.id, family, legacy.category(t.id, family))
        })
        .collect();
    let mut out: Vec<ToolSection> = TOOL_MENU_FAMILIES
        .iter()
        .map(|&fam| {
            let mut listed: Vec<(&'static str, Category)> = tools
                .iter()
                .filter(|(_, f, c)| *f == fam && !c.is_legacy())
                .map(|(id, _, c)| (*id, *c))
                .collect();
            // Generators: Simple first, then Advanced, each under a heading.
            listed.sort_by_key(|(_, c)| *c == Category::GeneratorAdvanced);
            let mut headers = Vec::new();
            for (k, (_, c)) in listed.iter().enumerate() {
                let h = match c {
                    Category::GeneratorSimple => "Simple",
                    Category::GeneratorAdvanced => "Advanced",
                    _ => continue,
                };
                if headers.last().is_none_or(|&(_, last)| last != h) {
                    headers.push((k, h));
                }
            }
            ToolSection {
                label: fam.label(),
                legacy: false,
                tools: listed.into_iter().map(|(id, _)| id).collect(),
                headers,
            }
        })
        .collect();
    for cat in [Category::Legacy, Category::LegacyExperimental] {
        out.push(ToolSection {
            label: cat.legacy_label().unwrap_or_default(),
            legacy: true,
            tools: tools
                .iter()
                .filter(|(_, _, c)| *c == cat)
                .map(|(id, _, _)| *id)
                .collect(),
            headers: Vec::new(),
        });
    }
    out.retain(|s| !s.tools.is_empty());
    out
}

/// [`tool_menu_sections`] of PlumSpirit's Legacy table (static: built once).
fn tool_sections() -> &'static [ToolSection] {
    static S: std::sync::OnceLock<Vec<ToolSection>> = std::sync::OnceLock::new();
    S.get_or_init(|| tool_menu_sections(&plumspirit_engine::transform::definition::LEGACY))
}

/// The menu bar's logo height (points).
pub const LOGO_SIZE: f32 = 16.0;

pub struct MenuInput<'a> {
    pub enabled: &'a dyn Fn(Command) -> bool,
    /// v0.9 (V09-S12): why a command cannot run now (`Some`: the entry is
    /// disabled and the sentence is its tooltip). Asked only while a menu is
    /// open; cheap (no note is visited).
    pub why_disabled: &'a dyn Fn(Command) -> Option<String>,
    /// B1-0175: the official logo, [`LOGO_SIZE`] points tall.
    pub logo: Option<egui::load::SizedTexture>,
    pub undo_label: Option<&'a str>,
    pub redo_label: Option<&'a str>,
    pub velocity_lane: bool,
    pub follow: bool,
    pub onion_mode: Option<OnionMode>,
    /// B1-0064: Onion Skin → Per-Track Settings.
    pub onion_per_track: bool,
    pub onion_opacity: u8,
    pub loop_on: bool,
    pub graph2: bool,
    pub event_list: bool,
    /// v0.9: MIDI export writes every status byte (File menu checkbox).
    pub explicit_status: bool,
    // v0.5
    pub onion_order: OnionOrder,
    pub onion_density: DensityBasis,
    // v0.6
    pub progressor: bool,
    /// v0.9 (V09-S17): the MIDI Player is shown.
    pub midi_player: bool,
    pub midi_inspector: bool,
    /// B1-0115 / B1-0116: the scale handles duplicate notes; Scale Y
    /// prevents the overlaps it creates.
    pub scale_y_duplicate: bool,
    pub scale_y_prevent: bool,
    /// B1-0117: the General Note Overlap Prevention settings.
    pub overlap: crate::settings::OverlapPrefs,
    /// B1-0091: Alt + corner drags rotate about the selection's center.
    pub rotate_about_center: bool,
    /// v0.7: scale handles fill rows when stretching vertically.
    pub onion_outlines: bool,
    /// v0.8: outline darkness (0 % white … 100 % black).
    pub onion_outline_darkness: u8,
    /// v0.9 (V09-S08): the view the central area shows.
    pub main_view: crate::app::track_list::MainView,
    pub skip_channel_10: bool,
    pub selection_analytics: bool,
    /// L-E01: the selection charts window is shown.
    pub selection_charts: bool,
    /// B1-0059: Piano Roll text overlays hidden.
    pub hide_text_overlays: bool,
    /// B1-0051: Help → Show the Quick Start on the Start Screen.
    pub show_orientation: bool,
    /// v0.8: Open File History and which entries are missing.
    pub recent: &'a [crate::recent::RecentEntry],
    pub recent_missing: &'a [bool],
    pub now: u64,
    /// v1.0-beta1 (B1-0080): the Edit / Anchor playhead and the Pause policy.
    pub anchor_playhead: bool,
    pub pause_rewinds: bool,
    /// B1-0038: every ready Lua plugin's commands (plugin id, name,
    /// [(command id, name, tooltip)]).
    pub lua_commands: &'a [crate::app::LuaMenuPlugin],
    /// B1-0045: ready plugins' custom tools (Tools → Custom).
    pub custom_tools: &'a [crate::app::LuaMenuPlugin],
}

#[derive(Default)]
pub struct MenuOutput {
    pub command: Option<Command>,
    pub onion_opacity: Option<u8>,
    pub onion_outline_darkness: Option<u8>,
    pub audio_settings: bool,
    /// B1-0080: Playback → After Pause was chosen.
    pub pause_rewinds: Option<bool>,
    /// L-F04: View → View Settings… (Settings, at Appearance).
    pub view_settings: bool,
    pub recent: Option<crate::recent::RecentAction>,
    /// B1-0038: a Lua plugin command was chosen (plugin id, command id).
    pub lua_command: Option<(String, String)>,
    /// B1-0045: a custom tool was chosen (plugin id, tool id).
    pub custom_tool: Option<(String, String)>,
}

/// B1-0064: what the Onion Skin items show (View → Onion Skin and the
/// toolbar's Onion menu draw the same items).
#[derive(Clone, Copy, Debug)]
pub struct OnionMenuState {
    pub mode: OnionMode,
    pub order: OnionOrder,
    pub density: DensityBasis,
    pub outlines: bool,
    pub outline_darkness: u8,
    pub opacity: u8,
    pub choose_enabled: bool,
    /// Per-Track Settings (each track keeps its own mode and list).
    pub per_track: bool,
}

/// What the Onion Skin items asked for.
#[derive(Clone, Copy, Debug, Default)]
pub struct OnionMenuOut {
    pub command: Option<Command>,
    pub opacity: Option<u8>,
    pub outline_darkness: Option<u8>,
}

/// The Onion Skin items: modes, Choose Tracks…, outlines, layer order,
/// density, ghost opacity.
pub fn onion_items(ui: &mut egui::Ui, st: &OnionMenuState, out: &mut OnionMenuOut) {
    for m in OnionMode::ALL {
        if ui.radio(st.mode == m, m.label()).clicked() {
            out.command = Some(Command::Onion(m));
        }
    }
    ui.separator();
    if ui
        .add_enabled(
            st.choose_enabled,
            egui::Button::new(Command::ChooseOnionTracks.label()),
        )
        .on_hover_text("Pick the ghost tracks from a list. In the track list, the color box left of a track's name shows and hides it as a ghost directly (drag over several).")
        .clicked()
    {
        out.command = Some(Command::ChooseOnionTracks);
        ui.close();
    }
    // L-G03: Replace / Add / Remove are three different things.
    for (cmd, tip) in [
        (
            Command::OnionShowOnlySelected,
            "The tracks selected in the track list become the only ghosts: every other ghost, including an earlier list, is hidden.",
        ),
        (
            Command::OnionAddSelected,
            "The tracks selected in the track list are shown as ghosts too; the ghosts shown now stay.",
        ),
        (
            Command::OnionRemoveSelected,
            "The tracks selected in the track list are no longer ghosts; the others stay.",
        ),
    ] {
        let text = cmd.label().trim_start_matches("Onion Skin: ");
        let shortcut = cmd.shortcut();
        let mut b = egui::Button::new(text);
        if !shortcut.is_empty() {
            b = b.shortcut_text(shortcut);
        }
        if ui
            .add_enabled(st.choose_enabled, b)
            .on_hover_text(tip)
            .clicked()
        {
            out.command = Some(cmd);
            ui.close();
        }
    }
    let mut pt = st.per_track;
    if ui
        .checkbox(&mut pt, Command::ToggleOnionPerTrack.label())
        .on_hover_text("Each track keeps its own Onion mode and ghost tracks: making a track active brings back what it last used (saved with the project).")
        .clicked()
    {
        out.command = Some(Command::ToggleOnionPerTrack);
    }
    if ui
        .add_enabled(
            st.per_track,
            egui::Button::new(
                Command::OnionUseForEveryTrack
                    .label()
                    .trim_start_matches("Onion Skin: "),
            ),
        )
        .on_hover_text("Per-Track Settings: the Onion Skin in force now becomes every track's — the tracks' own remembered settings are reset.")
        .clicked()
    {
        out.command = Some(Command::OnionUseForEveryTrack);
        ui.close();
    }
    let mut ol = st.outlines;
    if ui
        .checkbox(&mut ol, Command::ToggleOnionOutlines.label())
        .on_hover_text("Ghost notes of other tracks get an outline, so note art stays readable. Very small ghosts are drawn without outlines (zoomed out).")
        .clicked()
    {
        out.command = Some(Command::ToggleOnionOutlines);
    }
    ui.add_enabled_ui(st.outlines, |ui| {
        ui.horizontal(|ui| {
            ui.label("Outline darkness");
            let mut d = st.outline_darkness;
            if ui
                .add(egui::Slider::new(&mut d, 0..=100).suffix("%"))
                .on_hover_text("0 % white, 50 % the ghost color itself, 100 % black. Separate from the ghost opacity.")
                .changed()
            {
                out.outline_darkness = Some(d);
            }
        });
    });
    ui.separator();
    ui.label("Layer order");
    for o in OnionOrder::ALL {
        let tip = match o {
            OnionOrder::Project => "Ghost layers follow the track list.",
            OnionOrder::DenseFirst => {
                "The source with the most notes is drawn in front (render order only; the track list is not changed)."
            }
            OnionOrder::SparseFirst => "The source with the fewest notes is drawn in front.",
        };
        if ui
            .radio(st.order == o, o.label())
            .on_hover_text(tip)
            .clicked()
        {
            out.command = Some(Command::OnionOrder(o));
        }
    }
    ui.add_enabled_ui(st.order != OnionOrder::Project, |ui| {
        ui.menu_button("Density measured over", |ui| {
            for d in [DensityBasis::VisibleRange, DensityBasis::WholeTrack] {
                if ui.radio(st.density == d, d.label()).clicked() {
                    out.command = Some(Command::OnionDensity(d));
                }
            }
        });
    });
    ui.separator();
    ui.label("Ghost opacity");
    let mut v = st.opacity;
    if ui
        .add(egui::Slider::new(&mut v, MIN_ONION_OPACITY..=MAX_ONION_OPACITY).suffix("%"))
        .changed()
    {
        out.opacity = Some(v);
    }
}

/// The Plugins menu entries of one section, in registry order: every
/// registered built-in plugin of that kind (v0.9 §024).
pub fn plugin_menu_commands(kind: plumspirit_engine::plugins::PluginKind) -> Vec<Command> {
    plumspirit_engine::plugins::PLUGINS
        .iter()
        .filter(|p| p.kind == kind)
        .map(|p| Command::for_plugin(p.id))
        .collect()
}

fn item(ui: &mut egui::Ui, input: &MenuInput<'_>, cmd: Command, out: &mut MenuOutput) {
    item_labeled(ui, input, cmd, cmd.label(), out);
}

fn item_labeled(
    ui: &mut egui::Ui,
    input: &MenuInput<'_>,
    cmd: Command,
    label: &str,
    out: &mut MenuOutput,
) {
    let why = (input.why_disabled)(cmd);
    let enabled = (input.enabled)(cmd) && why.is_none();
    let mut resp = ui.add_enabled(enabled, Button::new(label).shortcut_text(cmd.shortcut()));
    if let Some(why) = why {
        resp = resp.on_disabled_hover_text(why);
    }
    if resp.clicked() {
        out.command = Some(cmd);
        ui.close();
    }
}

pub fn menu_bar(ui: &mut egui::Ui, input: &MenuInput<'_>) -> MenuOutput {
    let mut out = MenuOutput::default();
    // L-F05 (contract §012): a click inside a menu does not close it; a
    // check box or radio button toggles and the menu stays open (several in
    // a row), while commands, windows and navigation close it themselves.
    egui::MenuBar::new()
        .config(
            egui::containers::menu::MenuConfig::new()
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside),
        )
        .ui(ui, |ui| {
        if let Some(logo) = input.logo {
            ui.add(egui::Image::from_texture(logo).alt_text("PlumSpirit"))
                .on_hover_text(format!(
                    "{} (version {})",
                    crate::RELEASE_NAME,
                    env!("CARGO_PKG_VERSION")
                ));
        }
        ui.menu_button("File", |ui| {
            fit_menu(ui, |ui| {
            item(ui, input, Command::NewProject, &mut out);
            item(ui, input, Command::Open, &mut out);
            ui.menu_button("Recent Files", |ui| {
                if let Some(a) = crate::ui::recent_list::menu_items(
                    ui,
                    input.recent,
                    input.recent_missing,
                    input.now,
                ) {
                    out.recent = Some(a);
                }
            });
            ui.separator();
            item(ui, input, Command::Save, &mut out);
            item(ui, input, Command::SaveAs, &mut out);
            item(ui, input, Command::Export, &mut out);
            // v1.0-beta1 (B1-0123): shown as Running Status Compression (on =
            // the compact form); stored as `export_explicit_status` (off).
            let mut compress = !input.explicit_status;
            if ui
                .checkbox(&mut compress, Command::ToggleExplicitStatus.label())
                .on_hover_text("On: a MIDI message leaves out its status byte when it repeats the previous one (running status, the standard compact form). Off: every message carries its status byte (larger files, for players that mishandle running status). Both are valid MIDI with the same events; strict no-edit exports follow this setting too.")
                .clicked()
            {
                out.command = Some(Command::ToggleExplicitStatus);
            }
            ui.separator();
            item(ui, input, Command::ImportIntoTrack, &mut out);
            ui.separator();
            item(ui, input, Command::InsertScore, &mut out);
            item(ui, input, Command::SaveScoreAs, &mut out);
            item(ui, input, Command::SaveAutomationAs, &mut out);
            ui.separator();
            item(ui, input, Command::ProjectProperties, &mut out);
            ui.separator();
            item(ui, input, Command::CloseProject, &mut out);
            item(ui, input, Command::Exit, &mut out);
            });
        });
        ui.menu_button("Edit", |ui| {
            fit_menu(ui, |ui| {
            let undo = input
                .undo_label
                .map_or_else(|| "Undo".to_owned(), |l| format!("Undo {l}"));
            let redo = input
                .redo_label
                .map_or_else(|| "Redo".to_owned(), |l| format!("Redo {l}"));
            item_labeled(ui, input, Command::Undo, &undo, &mut out);
            item_labeled(ui, input, Command::Redo, &redo, &mut out);
            item(ui, input, Command::UndoHistory, &mut out);
            // L-F04: moved from View (a history window, not a view).
            item(ui, input, Command::RecentTasks, &mut out);
            ui.separator();
            item(ui, input, Command::Copy, &mut out);
            item(ui, input, Command::Cut, &mut out);
            item(ui, input, Command::Paste, &mut out);
            // B1-0099: Ctrl+B opens Duplicate V2 on the selected notes in the
            // Piano Roll and Paste Repeat on the selected clips in the Track
            // List; the entry says which.
            let repeat = if input.main_view == crate::app::track_list::MainView::TrackList {
                Command::PasteRepeat.label()
            } else {
                "Duplicate V2…"
            };
            item_labeled(ui, input, Command::PasteRepeat, repeat, &mut out);
            item(ui, input, Command::Delete, &mut out);
            ui.separator();
            // v0.9 §047 (V09-S12): the Track List's selected clips; a
            // disabled entry says why in its tooltip.
            {
                use plumspirit_engine::editor::clip_merge::MergeKind;
                item(ui, input, Command::MergeClips(MergeKind::Sequence), &mut out);
                item(ui, input, Command::MergeClips(MergeKind::Stack), &mut out);
            }
            ui.separator();
            item(ui, input, Command::SelectAll, &mut out);
            item(ui, input, Command::SelectRange, &mut out);
            item(ui, input, Command::MoveToSubchannel, &mut out);
            item(ui, input, Command::PaintCurrentSubchannel, &mut out);
            ui.separator();
            item(ui, input, Command::GroupNotes, &mut out);
            item(ui, input, Command::UngroupNotes, &mut out);
            item(ui, input, Command::MuteNotes, &mut out);
            item(ui, input, Command::UnmuteNotes, &mut out);
            item(ui, input, Command::ToggleMuteNotes, &mut out);
            item(ui, input, Command::SliceNotes, &mut out);
            item(ui, input, Command::DuplicateOctaveUp, &mut out);
            item(ui, input, Command::DuplicateOctaveDown, &mut out);
            ui.separator();
            item(ui, input, Command::RepeatLastTool, &mut out);
            item(ui, input, Command::RepeatLastToolNewSeed, &mut out);
            item(ui, input, Command::ReopenLastTool, &mut out);
            ui.separator();
            overlap_menu(ui, input, &mut out);
            ui.separator();
            item(ui, input, Command::OpenSettings, &mut out);
            });
        });
        ui.menu_button("Tools", |ui| {
            fit_menu(ui, |ui| {
        ui.menu_button("Simple", |ui| {
            fit_menu(ui, |ui| {
            for cmd in SIMPLE_COMMANDS { item(ui, input, cmd, &mut out); }
            ui.separator();
            let mut dup = input.scale_y_duplicate;
            if ui
                .checkbox(&mut dup, Command::ToggleScaleYDuplicate.label())
                .on_hover_text("While on, dragging a scale handle to stretch the rows duplicates the notes into the added rows (Scale Y → Duplicate Notes, with its last options). Alt on an edge handle reverses it for one drag (Alt on a corner rotates).")
                .clicked()
            {
                out.command = Some(Command::ToggleScaleYDuplicate);
            }
            let mut prevent = input.scale_y_prevent;
            if ui
                .checkbox(&mut prevent, Command::ToggleScaleYOverlap.label())
                .on_hover_text("Scale Y cleanup uses the last dialog choice: exact duplicates (default), or trim earlier overlapping gates. Destructively cleans selected same-key material on a shrink; Undo restores originals. Other layers stay.")
                .clicked()
            {
                out.command = Some(Command::ToggleScaleYOverlap);
            }
            ui.menu_button("Rotation Pivot", |ui| {
                for center in [true, false] {
                    let cmd = Command::RotatePivot(center);
                    if ui
                        .radio(input.rotate_about_center == center, cmd.label())
                        .on_hover_text("Where Alt + a corner handle drag turns the selection around.")
                        .clicked()
                    {
                        out.command = Some(cmd);
                    }
                }
            });
            let alt = if input.scale_y_duplicate {
                "Scale handles duplicate notes into stretched rows; hold Alt on an edge for N notes → N notes."
            } else {
                "Scale handles: N notes stay N notes. Hold Alt on an edge to duplicate notes into stretched rows."
            };
            ui.label(egui::RichText::new(format!("{alt}\nAlt+drag a corner: Rotate (Shift: 15° steps). Ctrl+drag a corner: Skew. Ctrl+Shift+drag a corner: Perspective.")).small().weak());
            });
        });
            ui.separator();
            let mut separated = false;
            for s in tool_sections() {
                if s.legacy && !separated {
                    ui.separator();
                    separated = true;
                }
                ui.menu_button(s.label, |ui| {
                    if s.legacy {
                        ui.label(egui::RichText::new(LEGACY_NOTE).small().weak());
                    }
                    for (k, &id) in s.tools.iter().enumerate() {
                        if let Some(&(_, h)) = s.headers.iter().find(|x| x.0 == k) {
                            if k > 0 {
                                ui.separator();
                            }
                            ui.label(egui::RichText::new(h).small().weak());
                        }
                        item(ui, input, Command::OpenTool(id), &mut out);
                    }
                });
            }
            // B1-0045: tools that enabled Lua plugins registered.
            ui.separator();
            ui.menu_button("Custom", |ui| {
                if input.custom_tools.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "No custom tools yet: enable a plugin that has some (Plugins → Lua Plugins…).",
                        )
                        .small()
                        .weak(),
                    );
                }
                for (plugin, name, tools) in input.custom_tools {
                    ui.label(egui::RichText::new(name.as_str()).small().weak());
                    for (id, label, tip) in tools {
                        let mut r = ui.button(label.as_str());
                        if !tip.is_empty() {
                            r = r.on_hover_text(tip.as_str());
                        }
                        if r.clicked() {
                            out.custom_tool = Some((plugin.clone(), id.clone()));
                            ui.close();
                        }
                    }
                }
                ui.separator();
                item(ui, input, Command::ShowLuaPlugins, &mut out);
            });
            // L-F04: moved from View (the formulas of Event Graph curves).
            item(ui, input, Command::CustomFunctions, &mut out);
            });
        });
        ui.menu_button("Plugins", |ui| {
            fit_menu(ui, |ui| {
            use plumspirit_engine::plugins::PluginKind;
            for kind in [PluginKind::PianoRoll, PluginKind::General] {
                ui.label(egui::RichText::new(kind.label()).weak());
                for c in plugin_menu_commands(kind) {
                    item(ui, input, c, &mut out);
                }
            }
            ui.separator();
            ui.label(egui::RichText::new("Lua Plugins").weak());
            for (plugin, name, commands) in input.lua_commands {
                ui.menu_button(name.as_str(), |ui| {
                    for (id, label, tip) in commands {
                        let mut r = ui.button(label.as_str());
                        if !tip.is_empty() {
                            r = r.on_hover_text(tip.as_str());
                        }
                        if r.clicked() {
                            out.lua_command = Some((plugin.clone(), id.clone()));
                            ui.close();
                        }
                    }
                });
            }
            item(ui, input, Command::ShowLuaPlugins, &mut out);
            ui.separator();
            item(ui, input, Command::ShowAbout, &mut out);
            });
        });
        ui.menu_button("Song", |ui| {
            fit_menu(ui, |ui| {
            item(ui, input, Command::SelectConductor, &mut out);
            ui.separator();
            item(ui, input, Command::AddTempoChange, &mut out);
            item(ui, input, Command::AddTimeSignature, &mut out);
            item(ui, input, Command::AddMarker, &mut out);
            ui.separator();
            item(ui, input, Command::AddTrack, &mut out);
            item(ui, input, Command::AddTracks, &mut out);
            item(ui, input, Command::AddClip, &mut out);
            ui.separator();
            item(ui, input, Command::TrackProperties, &mut out);
            item(ui, input, Command::FitTrackEnd, &mut out);
            ui.separator();
            item(ui, input, Command::AddSubtrack, &mut out);
            item(ui, input, Command::AddSubtracks, &mut out);
            ui.separator();
            item(ui, input, Command::MoveTrackUp, &mut out);
            item(ui, input, Command::MoveTrackDown, &mut out);
            item(ui, input, Command::IndentTrack, &mut out);
            item(ui, input, Command::OutdentTrack, &mut out);
            item(ui, input, Command::DeleteSelectedTracks, &mut out);
            ui.separator();
            item(ui, input, Command::AddSubchannel, &mut out);
            item(ui, input, Command::AddSubchannels, &mut out);
            item(ui, input, Command::Subchannels, &mut out);
            let mut skip = input.skip_channel_10;
            if ui
                .checkbox(&mut skip, Command::ToggleSkipChannel10.label())
                .on_hover_text("A new subchannel gets the MIDI channel after the previous one (1 → 2 → … → 16 → 1). With this on, channel 10 (General MIDI drums) is skipped: 9 → 11.")
                .clicked()
            {
                out.command = Some(Command::ToggleSkipChannel10);
            }
            ui.separator();
            item(ui, input, Command::PlayingTime, &mut out);
            });
        });
        // L-F04 (contract §011): View holds view actions only — the main
        // view, panels and readouts shown, Follow, the Onion Skin, zoom and
        // fit. Their configuration lives in Settings (one place each); the
        // last item opens it.
        ui.menu_button("View", |ui| {
            fit_menu(ui, |ui| {
            // v0.9 (V09-S08): the Track List is the primary window; the Piano
            // Roll edits the active track's notes.
            for v in [
                crate::app::track_list::MainView::TrackList,
                crate::app::track_list::MainView::PianoRoll,
            ] {
                let cmd = Command::MainView(v);
                if ui
                    .add_enabled(
                        (input.enabled)(cmd),
                        egui::RadioButton::new(input.main_view == v, cmd.label()),
                    )
                    .clicked()
                {
                    out.command = Some(cmd);
                    ui.close();
                }
            }
            // B1-0061: Tab by default.
            item(ui, input, Command::SwitchView, &mut out);
            ui.separator();
            // Panels.
            let mut lane = input.velocity_lane;
            if ui
                .checkbox(&mut lane, "Event Graphs")
                .on_hover_text("The graphs under the piano roll (Velocity, Pitch Bend, CC, Master Volume, BPM)")
                .clicked()
            {
                out.command = Some(Command::ToggleVelocityLane);
            }
            for (on, cmd) in [
                (input.graph2, Command::ToggleGraph2),
                (input.event_list, Command::ToggleEventList),
            ] {
                let mut v = on;
                if ui
                    .add_enabled((input.enabled)(cmd), egui::Checkbox::new(&mut v, cmd.label()))
                    .clicked()
                {
                    out.command = Some(cmd);
                }
            }
            ui.separator();
            // Readouts and analysis windows.
            for (on, cmd, tip) in [
                (input.progressor, Command::ToggleProgressor, "Notes, NPS, poly and PPS at the playhead or cursor, in a corner of the piano roll (exact at any position, without playing first)."),
                (input.midi_player, Command::ToggleMidiPlayer, "The exported file falling onto a keyboard, in place of the editor, with exact Time, NPS, polyphony, BPM, notes and passed notes at the playhead. Esc returns."),
                (input.midi_inspector, Command::ToggleMidiInspector, "What the exported file contains: total notes, exact maximum NPS and polyphony, duration through the tempo map, PPQN and tempo, with NPS and polyphony charts of the whole song. Updates as you edit."),
                (input.selection_analytics, Command::ToggleSelectionAnalytics, "For selected notes: a compact readout next to the selection (hold Alt over it for every line)."),
                (input.selection_charts, Command::ToggleSelectionCharts, "The selection's advanced section: exact NPS, polyphony, note counter and tempo charts over the selected notes only, with the playhead, hover readouts, zoom and Follow Playhead. Also the caret at the readout's top right."),
                (input.hide_text_overlays, Command::ToggleTextOverlays, "Hides the text drawn over the notes (the Progressor and Selection Analytics readouts) without turning them off; uncheck to show them again."),
            ] {
                let mut v = on;
                if ui.checkbox(&mut v, cmd.label()).on_hover_text(tip).clicked() {
                    out.command = Some(cmd);
                }
            }
            ui.separator();
            let mut follow = input.follow;
            if ui
                .add_enabled(
                    (input.enabled)(Command::ToggleFollow),
                    egui::Checkbox::new(&mut follow, "Follow Playhead"),
                )
                .clicked()
            {
                out.command = Some(Command::ToggleFollow);
            }
            ui.add_enabled_ui(input.onion_mode.is_some(), |ui| {
                ui.menu_button("Onion Skin", |ui| {
                    let st = OnionMenuState {
                        mode: input.onion_mode.unwrap_or_default(),
                        order: input.onion_order,
                        density: input.onion_density,
                        outlines: input.onion_outlines,
                        outline_darkness: input.onion_outline_darkness,
                        opacity: input.onion_opacity,
                        choose_enabled: (input.enabled)(Command::ChooseOnionTracks),
                        per_track: input.onion_per_track,
                    };
                    let mut o = OnionMenuOut::default();
                    onion_items(ui, &st, &mut o);
                    item(ui, input, Command::OnionOpacity, &mut out);
                    if o.command.is_some() {
                        out.command = o.command;
                    }
                    out.onion_opacity = out.onion_opacity.or(o.opacity);
                    out.onion_outline_darkness = out.onion_outline_darkness.or(o.outline_darkness);
                });
            });
            ui.separator();
            item(ui, input, Command::FitSong, &mut out);
            // B2-F29: the selected clips (Track List) or notes (Piano Roll).
            item(ui, input, Command::ZoomToSelection, &mut out);
            item(ui, input, Command::JumpToPlayhead, &mut out);
            item(ui, input, Command::FitKeys128, &mut out);
            item(ui, input, Command::FitAllKeys, &mut out);
            // L-F04: the step zooms and view history in one submenu, so the
            // menu fits 1366 × 768 (wheel: Ctrl = Zoom X, Ctrl+Shift = Y).
            ui.menu_button("Zoom", |ui| {
                item(ui, input, Command::ResetZoom, &mut out);
                item(ui, input, Command::ZoomInY, &mut out);
                item(ui, input, Command::ZoomOutY, &mut out);
                ui.separator();
                item(ui, input, Command::ZoomTool, &mut out);
                item(ui, input, Command::ZoomBack, &mut out);
                item(ui, input, Command::ZoomForward, &mut out);
                ui.separator();
                ui.label(
                    egui::RichText::new("Wheel: Ctrl = Zoom X, Ctrl+Shift = Zoom Y")
                        .small()
                        .weak(),
                );
            });
            item(ui, input, Command::ResetView, &mut out);
            ui.separator();
            // The contextual link: theme, UI scale, colors, Follow options,
            // fit lock, readout texts, keyboard colors, audition, renderer.
            if ui
                .button("View Settings…")
                .on_hover_text("Theme, UI scale, note colors, Smooth Follow and Follow Position, the fit lock, the readouts' texts, keyboard colors, audition and the renderer are in Settings.")
                .clicked()
            {
                out.view_settings = true;
                ui.close();
            }
            });
        });
        ui.menu_button("Playback", |ui| {
            fit_menu(ui, |ui| {
            item(ui, input, Command::PlayPause, &mut out);
            item(ui, input, Command::Stop, &mut out);
            item(ui, input, Command::ScrubTool, &mut out);
            ui.separator();
            // B1-0080: the Edit / Anchor playhead and what Pause does.
            let mut anchor = input.anchor_playhead;
            if ui
                .checkbox(&mut anchor, Command::ToggleAnchorPlayhead.label())
                .on_hover_text(format!(
                    "{}. A second, stationary playhead (dashed) that only you move — ruler click, Go to…, measure steps. Play starts there; Stop ({}) returns to it. Off: one playhead; Stop returns to where playback started.",
                    Command::ToggleAnchorPlayhead.shortcut(),
                    Command::Stop.shortcut()
                ))
                .clicked()
            {
                out.command = Some(Command::ToggleAnchorPlayhead);
            }
            ui.add_enabled_ui(input.anchor_playhead, |ui| {
                ui.menu_button("After Pause", |ui| {
                    if ui
                        .radio(!input.pause_rewinds, "Hold the place (Play resumes there)")
                        .clicked()
                    {
                        out.pause_rewinds = Some(false);
                    }
                    if ui
                        .radio(input.pause_rewinds, "Return to the anchor")
                        .clicked()
                    {
                        out.pause_rewinds = Some(true);
                    }
                });
            });
            ui.separator();
            item(ui, input, Command::GoToStart, &mut out);
            item(ui, input, Command::PreviousMeasure, &mut out);
            item(ui, input, Command::NextMeasure, &mut out);
            item(ui, input, Command::GoToEnd, &mut out);
            ui.separator();
            let mut l = input.loop_on;
            if ui
                .add_enabled(
                    (input.enabled)(Command::ToggleLoop),
                    egui::Checkbox::new(&mut l, "Loop"),
                )
                .on_hover_text(format!(
                    "{}. Shift+drag on the ruler sets the loop range.",
                    Command::ToggleLoop.shortcut()
                ))
                .clicked()
            {
                out.command = Some(Command::ToggleLoop);
            }
            item(ui, input, Command::LoopToSelection, &mut out);
            item(ui, input, Command::ClearLoop, &mut out);
            ui.separator();
            item(ui, input, Command::PlayOnlySelected, &mut out);
            item(ui, input, Command::RestoreAllTracks, &mut out);
            item(ui, input, Command::PlayAll, &mut out);
            item(ui, input, Command::MuteCurrentTrack, &mut out);
            item(ui, input, Command::SoloCurrentTrack, &mut out);
            item(ui, input, Command::PlayingTime, &mut out);
            ui.separator();
            item(ui, input, Command::ChooseSoundFont, &mut out);
            if ui
                .button("Audio Settings…")
                .on_hover_text("Output device, sample rate, buffer, Realtime / Pre-Render, voices, headroom, SoundFont list and diagnostics.")
                .clicked()
            {
                out.audio_settings = true;
                ui.close();
            }
            });
        });
        ui.menu_button("Help", |ui| {
            fit_menu(ui, |ui| {
            // B1-0050 / B1-0051.
            item(ui, input, Command::ShowHelp, &mut out);
            item(ui, input, Command::ShowQuickStart, &mut out);
            item(ui, input, Command::ShowKeybinds, &mut out);
            let mut orient = input.show_orientation;
            if ui
                .checkbox(&mut orient, Command::ToggleOrientation.label())
                .on_hover_text("The start screen shows a short quick-start card when no project is open; it is always in Help → Quick Start.")
                .clicked()
            {
                out.command = Some(Command::ToggleOrientation);
            }
            ui.separator();
            item(ui, input, Command::SaveDiagnostics, &mut out);
            ui.separator();
            item(ui, input, Command::ShowAbout, &mut out);
            });
        });
    });
    out
}

/// Runs a menu's contents in a scroll area as tall as the window allows, so a
/// menu taller than the window scrolls instead of running off the screen
/// (v0.8 Wave Q fix: on a 1366 × 768 screen the View menu's last entries —
/// Renderer and Show CPU / RAM — could not be reached).
fn fit_menu<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let room = (ui.ctx().content_rect().bottom() - ui.cursor().top() - 8.0).max(160.0);
    // A menu that needs scrolling uses all the room below the menu bar (egui's
    // popup alone would make it much shorter); a short menu keeps its size.
    egui::ScrollArea::vertical()
        .id_salt("menu-scroll")
        .max_height(room)
        .min_scrolled_height(room)
        .auto_shrink([true, true])
        .show(ui, add)
        .inner
}

/// B1-0117: Edit → Note Overlap Prevention (a submenu: no ComboBox in a
/// menu).
fn overlap_menu(ui: &mut egui::Ui, input: &MenuInput, out: &mut MenuOutput) {
    let o = input.overlap;
    let title = if o.on {
        "Note Overlap Prevention (On)"
    } else {
        "Note Overlap Prevention (Off)"
    };
    ui.menu_button(title, |ui| {
        let mut on = o.on;
        if ui
            .checkbox(&mut on, Command::ToggleOverlapPrevention.label())
            .on_hover_text("Off by default. On: tools, scale gestures, Scale X / Y, Flip, Quantize, Paste and Draw remove (or shorten) the note overlaps they would create, with the rule below. Moves, Undo / Redo and imported files are never changed.")
            .clicked()
        {
            out.command = Some(Command::ToggleOverlapPrevention);
        }
        ui.separator();
        ui.label(egui::RichText::new("What counts as an overlap").small().weak());
        let now = plumspirit_engine::transform::overlap::Mode::ALL
            .iter()
            .position(|m| *m == o.general.policy.mode)
            .unwrap_or(3);
        for m in 0..4u8 {
            let cmd = Command::OverlapMode(m);
            if ui.radio(now == usize::from(m), cmd.label()).clicked() {
                out.command = Some(cmd);
            }
        }
        ui.separator();
        for (v, cmd, tip) in [
            (
                o.general.policy.generated_only,
                Command::ToggleOverlapGeneratedOnly,
                "On (default): a note the operation left exactly as it was is the user's own and never goes.",
            ),
            (
                o.general.policy.across,
                Command::ToggleOverlapAcross,
                "On: notes of one key overlap even on different subchannels or MIDI channels. Off (default): each subchannel and channel is its own layer.",
            ),
            (
                o.general.existing,
                Command::ToggleOverlapExisting,
                "On (default): results are compared with the notes already in the track too (those notes are never changed). Off: results only with each other.",
            ),
        ] {
            let mut b = v;
            if ui.checkbox(&mut b, cmd.label()).on_hover_text(tip).clicked() {
                out.command = Some(cmd);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use plumspirit_engine::transform::registry::{Family, registry};

    /// Every tool except the plugins (own menu) is in a Tools menu section;
    /// Paint V2 is in Iterations, after Geometry and before Experimental.
    #[test]
    fn every_tool_has_a_tools_menu_section_and_paint_v2_is_in_iterations() {
        for d in registry() {
            let fam = (d.spec)().family;
            assert!(
                fam == Family::Plugin || TOOL_MENU_FAMILIES.contains(&fam),
                "{} ({fam:?}) is in no Tools menu section",
                d.id
            );
        }
        let paint = registry().iter().find(|d| d.id == "subpaint").unwrap();
        assert_eq!((paint.spec)().family, Family::Iterations);
        let at = |f: Family| TOOL_MENU_FAMILIES.iter().position(|x| *x == f).unwrap();
        assert_eq!(at(Family::Iterations), at(Family::Geometry) + 1);
        assert_eq!(at(Family::Experimental), at(Family::Iterations) + 1);
        // B1-0099: Duplicate V2 is in Iterations after Paint V2; the former
        // Paste Repeat section is gone.
        let s = tool_menu_sections(&plumspirit_engine::transform::definition::LEGACY);
        let it = s.iter().find(|x| x.label == "Iterations").unwrap();
        assert_eq!(it.tools, vec!["repeat", "subpaint", "paste_repeat_notes"]);
        assert!(s.iter().all(|x| x.label != "Paste Repeat"));
    }

    /// v0.9 §151 (V09-S51): the generators have their own Generator
    /// section after Experimental — Simple ones under a "Simple" heading,
    /// then Advanced ones under "Advanced".
    #[test]
    fn generators_are_listed_in_the_generator_section_simple_first() {
        use plumspirit_engine::transform::definition::{Category, LEGACY};
        use plumspirit_engine::transform::generator::is_advanced;
        let s = tool_menu_sections(&LEGACY);
        let at = |label: &str| s.iter().position(|x| x.label == label).unwrap();
        let g = &s[at("Generator")];
        assert_eq!(at("Generator"), at("Experimental") + 1);
        let gens: Vec<&str> = registry()
            .iter()
            .filter(|d| (d.spec)().family == Family::Generator)
            .map(|d| d.id)
            .collect();
        for id in [
            "gen.lines",
            "gen.rect",
            "gen.circles",
            "gen.arcs",
            "gen.polygons",
            "gen.custom",
            "gen.parametric",
            "gen.lsystem",
            "gen.spirograph",
            "gen.growth",
        ] {
            assert!(gens.contains(&id), "{id}");
        }
        assert_eq!(g.tools.len(), gens.len());
        let first_adv = g.tools.iter().position(|id| is_advanced(id));
        assert_eq!(g.headers[0], (0, "Simple"));
        match first_adv {
            Some(k) => {
                assert_eq!(g.headers[1], (k, "Advanced"));
                assert!(g.tools[k..].iter().all(|id| is_advanced(id)));
            }
            None => assert_eq!(g.headers.len(), 1),
        }
        assert!(
            g.tools
                .iter()
                .all(|id| LEGACY.category(id, Family::Generator) != Category::Experimental)
        );
    }

    /// v0.9 §109 / §113 (V09-S33): with nothing moved the Tools menu is the
    /// family sections, every non-plugin tool once; a tool moved through the
    /// Legacy table leaves its family section for the marked "Legacy" /
    /// "Legacy Experimental" submenu (after the others) and opens the same
    /// tool by the same id.
    #[test]
    fn moved_tools_are_listed_only_in_the_marked_legacy_submenus() {
        use plumspirit_engine::transform::definition::{LEGACY, LegacyTable};
        let today = tool_menu_sections(&LEGACY);
        // V09-S71: the formerly moved tools are retired completely.
        assert!(today.iter().all(|s| !s.legacy));
        let listed: Vec<&str> = today.iter().flat_map(|s| s.tools.clone()).collect();
        let tools: Vec<&str> = registry()
            .iter()
            .filter(|d| (d.spec)().family != Family::Plugin)
            .map(|d| d.id)
            .collect();
        assert_eq!(listed.len(), tools.len());
        assert!(tools.iter().all(|id| listed.contains(id)));
        assert_eq!(tool_sections(), &today[..]);

        static MOVED: LegacyTable = LegacyTable {
            tools: &["shear", "twirl", "gravity_well"],
            functions: &[],
        };
        let s = tool_menu_sections(&MOVED);
        let find = |id: &str| -> Vec<&str> {
            s.iter()
                .filter(|x| x.tools.contains(&id))
                .map(|x| x.label)
                .collect()
        };
        assert_eq!(find("shear"), vec!["Legacy"]);
        assert_eq!(find("twirl"), vec!["Legacy"]);
        assert_eq!(find("gravity_well"), vec!["Legacy Experimental"]);
        assert_eq!(find("wave"), vec!["Geometry"]);
        let legacy: Vec<&ToolSection> = s.iter().filter(|x| x.legacy).collect();
        assert_eq!(legacy.len(), 2);
        assert_eq!(legacy[0].tools, vec!["shear", "twirl"], "menu order");
        // The Legacy submenus come last; the listing is otherwise unchanged.
        assert!(s[s.len() - 2].legacy && s[s.len() - 1].legacy);
        let rest: Vec<&str> = s.iter().flat_map(|x| x.tools.clone()).collect();
        assert_eq!(rest.len(), listed.len());
        // The entry opens the same tool (same id, same name).
        assert_eq!(
            Command::OpenTool("shear").label(),
            Command::OpenTool(legacy[0].tools[0]).label()
        );
    }
}
