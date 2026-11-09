//! Explicit format actions and review of private imported transactions.
use super::PlumSpiritApp;
use crate::lua::{Capability, formats::Mode, plugins::Status, reactive::Fence, tx::Commits};
use crate::ui::status_bar::StatusKind;
use plumspirit_engine::editor::NoteTarget;
use plumspirit_engine::model::{NoteStore, PlumDocument};
use std::sync::Arc;

const ACTION: &str = "@format/";
#[path = "lua_format_browser.rs"]
mod browser;
struct Pending {
    plugin: String,
    format: String,
    mode: Mode,
    generation: u64,
    revision: u64,
    lifecycle: Arc<Fence>,
}
pub(super) struct Review {
    pending: Pending,
    name: String,
    commits: Commits,
    lifecycle: Arc<Fence>,
    empty: NoteStore,
    rev: u64,
    error: Option<String>,
    browser: Option<Box<browser::Browser>>,
    #[cfg(test)]
    inspect_button: Option<egui::Rect>,
}
#[derive(Default)]
pub(super) struct State {
    pending: Option<Pending>,
    review: Option<Review>,
}
impl State {
    pub fn reviewing(&self) -> bool {
        self.review.is_some()
    }
    fn busy(&self) -> bool {
        self.pending.is_some() || self.reviewing()
    }
}
pub(super) fn action(mode: Mode, id: &str) -> String {
    format!("{ACTION}{}/{id}", mode.name())
}
pub(super) fn parse_action(id: &str) -> Option<(Mode, &str)> {
    let (mode, id) = id.strip_prefix(ACTION)?.split_once('/')?;
    Some((
        match mode {
            "import" => Mode::Import,
            "export" => Mode::Export,
            _ => return None,
        },
        id,
    ))
}
pub(super) fn preview<'a>(
    state: &'a State,
    source: &Arc<PlumDocument>,
    target: NoteTarget,
) -> Option<crate::piano_roll::ToolPreview<'a>> {
    let r = state.review.as_ref()?;
    if !r.lifecycle.active() || !r.commits.source_matches(source) {
        return None;
    }
    let result = r.commits.result();
    let id = source.track(target.host_track())?.id();
    let mapped = result.index_of(id).map(|host| match target {
        NoteTarget::Track(_) => NoteTarget::Track(host),
        NoteTarget::Content { content, .. } => NoteTarget::Content { host, content },
    });
    let notes = mapped
        .and_then(|target| plumspirit_engine::editor::note_view(result, target).ok())
        .map_or(&r.empty, |v| v.notes);
    Some(crate::piano_roll::ToolPreview::Store {
        notes,
        all: true,
        rev: r.rev,
    })
}
impl PlumSpiritApp {
    fn format_permitted(&self, plugin: &str, mode: Mode) -> bool {
        let prefs = &self.settings.lua_plugins;
        prefs.run_plugins
            && prefs.installed.get(plugin).is_some_and(|s| s.enabled)
            && self
                .lua_plugins
                .as_ref()
                .and_then(|m| m.plugin(plugin))
                .is_some_and(|p| {
                    let caps = p.effective(prefs.installed.get(plugin));
                    p.status == Status::Ready
                        && caps.contains(&Capability::ReadProject)
                        && caps.contains(&if mode == Mode::Import {
                            Capability::ReadUserFile
                        } else {
                            Capability::WriteUserFile
                        })
                        && (mode != Mode::Import || caps.contains(&Capability::EditNotes))
                })
    }
    pub(super) fn start_lua_format(&mut self, plugin: &str, id: &str, mode: Mode) {
        if self.lua_formats.busy()
            || self.job.is_some()
            || self.dialog.is_some()
            || self.tool.is_some()
            || self.custom_tool.is_some()
        {
            self.set_status(
                "Finish or close the current editing tool or file action first.",
                StatusKind::Warning,
            );
            return;
        }
        if !self.format_permitted(plugin, mode) {
            self.set_status(
                "Enable this plugin and grant the requested project and file permissions first.",
                StatusKind::Warning,
            );
            return;
        }
        let Some(o) = self.open.as_ref() else {
            self.set_status(
                "Open a project before using a format adapter.",
                StatusKind::Warning,
            );
            return;
        };
        let Some(lifecycle) = self
            .lua_plugins
            .as_ref()
            .and_then(|m| m.plugin(plugin))
            .and_then(|p| p.lifecycle())
        else {
            return;
        };
        let pending = Pending {
            plugin: plugin.into(),
            format: id.into(),
            mode,
            generation: o.generation,
            revision: o.history.current_state(),
            lifecycle,
        };
        let world = self.lua_world(self.dark_mode);
        match self.lua_plugins().run_format(plugin, id, mode, world) {
            Ok(()) => self.lua_formats.pending = Some(pending),
            Err(e) => self.set_status(e, StatusKind::Warning),
        }
    }
    pub(super) fn lua_format_result(
        &mut self,
        plugin: &str,
        id: &str,
        mode: Mode,
        selected: bool,
        result: Result<Option<Commits>, String>,
        lifecycle: Arc<Fence>,
    ) {
        let Some(p) = self.lua_formats.pending.as_ref() else {
            super::project::reap(result);
            return;
        };
        if p.plugin != plugin
            || p.format != id
            || p.mode != mode
            || !Arc::ptr_eq(&p.lifecycle, &lifecycle)
        {
            super::project::reap(result);
            return;
        }
        let pending = self.lua_formats.pending.take().unwrap();
        if !lifecycle.active()
            || !self.format_permitted(plugin, mode)
            || self
                .open
                .as_ref()
                .is_none_or(|o| o.generation != pending.generation)
        {
            super::project::reap(result);
            return;
        }
        let commits = match result {
            Err(e) => {
                self.set_status(e, StatusKind::Error);
                return;
            }
            Ok(c) => c,
        };
        if !selected {
            self.set_status("File selection canceled.", StatusKind::Info);
            return;
        }
        if mode == Mode::Export {
            super::project::reap(commits);
            self.set_status("Export finished.", StatusKind::Info);
            return;
        }
        let Some(commits) = commits else {
            self.set_status("The importer changed nothing.", StatusKind::Info);
            return;
        };
        if self.open.as_ref().is_none_or(|o| {
            o.history.current_state() != pending.revision || !commits.source_matches(&o.doc)
        }) {
            super::project::reap(commits);
            self.set_status(
                "The project changed during import. Import again to review the current project.",
                StatusKind::Warning,
            );
            return;
        }
        static NEXT: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0x464d_5400_0000_0000);
        let name = self
            .lua_plugins
            .as_ref()
            .and_then(|m| m.plugin(plugin))
            .and_then(|p| p.decls.formats.iter().find(|f| f.id == id))
            .map_or_else(|| id.into(), |d| d.name.clone());
        self.lua_formats.review = Some(Review {
            pending,
            name,
            commits,
            lifecycle,
            empty: NoteStore::default(),
            rev: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            error: None,
            browser: None,
            #[cfg(test)]
            inspect_button: None,
        });
    }
    fn valid_format_review(&self, r: &Review) -> bool {
        r.lifecycle.active()
            && self.format_permitted(&r.pending.plugin, Mode::Import)
            && self.open.as_ref().is_some_and(|o| {
                o.generation == r.pending.generation
                    && o.history.current_state() == r.pending.revision
                    && r.commits.source_matches(&o.doc)
            })
    }
    pub(super) fn maintain_lua_format(&mut self) {
        if self
            .lua_formats
            .review
            .as_ref()
            .is_some_and(|r| !self.valid_format_review(r))
        {
            super::project::reap(self.lua_formats.review.take());
            self.set_status(
                "The import preview expired because its project or plugin changed.",
                StatusKind::Warning,
            );
        }
        let expired = self.lua_formats.pending.as_ref().is_some_and(|p| {
            !p.lifecycle.active()
                || self
                    .open
                    .as_ref()
                    .is_none_or(|o| o.generation != p.generation)
                || !self.format_permitted(&p.plugin, p.mode)
                || self
                    .lua_plugins
                    .as_ref()
                    .and_then(|m| m.plugin(&p.plugin))
                    .is_none_or(|p| !p.user_job_pending())
        });
        if expired {
            self.cancel_pending_lua_format();
        }
    }
    fn cancel_pending_lua_format(&mut self) {
        if let Some(p) = self.lua_formats.pending.take()
            && p.lifecycle.active()
            && self
                .lua_plugins
                .as_ref()
                .and_then(|m| m.plugin(&p.plugin))
                .is_some_and(|p| p.user_job_pending())
        {
            self.lua_plugins().cancel(&p.plugin);
        }
    }
    pub(super) fn cancel_lua_format(&mut self) {
        self.cancel_pending_lua_format();
        super::project::reap(self.lua_formats.review.take());
    }
    pub(super) fn apply_lua_format(&mut self) {
        self.release_preview_worker();
        self.release_tool_worker();
        self.maintain_lua_format();
        let Some(r) = self.lua_formats.review.as_mut() else {
            return;
        };
        if r.commits.needs_confirmation() {
            r.error=Some("This import removes at least one million notes. Import a smaller region before applying.".into());
            return;
        }
        if self.job.is_some()
            || self
                .open
                .as_ref()
                .is_none_or(|o| Arc::strong_count(&o.doc) > 2)
        {
            r.error = Some(
                "The project is still processing another task. Try Apply when it finishes.".into(),
            );
            return;
        }
        let mut r = self.lua_formats.review.take().unwrap();
        let o = self.open.as_mut().unwrap();
        let result = r.commits.apply(&mut o.history, &mut o.doc);
        super::project::reap(r);
        match result {
            Ok(_) => self.set_status(
                "Import applied. Undo restores the previous project.",
                StatusKind::Info,
            ),
            Err(e) => self.set_status(e.user_message(), StatusKind::Warning),
        }
    }
    pub(super) fn lua_format_ui(&mut self, ctx: &egui::Context) {
        self.maintain_lua_format();
        let Some(r) = self.lua_formats.review.as_mut() else {
            return;
        };
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        let mut show = false;
        let mut inspect = None;
        egui::Window::new(format!("Review Import — {}",r.name))
            .id(egui::Id::new("lua-format-review"))
            .open(&mut open).default_width(520.).resizable(true).collapsible(false).vscroll(true)
            .show(ctx,|ui| {
                ui.label("The project has not changed. Review the staged result, then Apply or Cancel.");
                ui.label("The Piano Roll previews the currently opened Track or Clip. Resulting Tracks are listed below.");
                let d=r.commits.result();
                ui.horizontal_wrapped(|ui| {
                    apply=ui.button("Apply").clicked();
                    cancel=ui.button("Cancel").clicked();
                    show=ui.button("Show Piano Roll").clicked();
                    let response=ui.button("Inspect staged result");
                    #[cfg(test)]{r.inspect_button=Some(response.rect);}
                    if response.clicked(){inspect=Some(0);}
                });
                if let Some(e)=&r.error {ui.colored_label(egui::Color32::YELLOW,e);}
                if let Some(report)=r.commits.format_review() {review_summary(ui,report);}
                ui.label("Musical changes use one Undo step.");
                egui::ScrollArea::vertical().max_height(220.).show_rows(ui,22.,d.track_count(),|ui,range| {
                    for i in range {
                        if let Some(t)=d.track(i) {
                            let name=t.name().unwrap_or("Track").chars().take(80).map(|c|if c.is_control(){' '}else{c}).collect::<String>();
                            let label=format!("{name}: {} notes",t.notes.len());
                            if ui.add(egui::Button::selectable(false,&label).frame(false).truncate()).on_hover_text(label).clicked(){inspect=Some(i);}
                        }
                    }
                });
            });
        if let Some(i) = inspect {
            let b = r
                .browser
                .get_or_insert_with(|| Box::new(browser::Browser::default()));
            b.select_track(r.commits.result(), i);
            b.open = true;
        }
        if let Some(b) = r.browser.as_mut() {
            b.show(ctx, r.commits.result(), r.rev, self.dark_mode);
        }
        if r.browser.as_ref().is_some_and(|b| !b.open) {
            super::project::reap(r.browser.take());
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            cancel = true;
        }
        if show {
            self.main_view = super::track_list::MainView::PianoRoll;
        }
        if cancel || !open {
            self.cancel_lua_format();
        } else if apply {
            self.apply_lua_format();
        }
    }
}

fn review_summary(ui: &mut egui::Ui, r: &crate::lua::format_review::Report) {
    let a = r.before;
    let b = r.after;
    let c = r.changes;
    egui::Grid::new("format-review-counts")
        .num_columns(3)
        .striped(true)
        .show(ui, |ui| {
            ui.strong("Stored data");
            ui.strong("Before");
            ui.strong("After");
            ui.end_row();
            for (label, a, b) in [
                (
                    "Tracks (including Subtracks)",
                    a.tracks as u64,
                    b.tracks as u64,
                ),
                ("Subtracks", a.subtracks as u64, b.subtracks as u64),
                ("Notes in Track content", a.track_notes, b.track_notes),
                ("Non-note Track events", a.track_events, b.track_events),
                ("Owned Clip contents", a.contents as u64, b.contents as u64),
                ("Notes in owned Clip content", a.clip_notes, b.clip_notes),
                ("Non-note owned Clip events", a.clip_events, b.clip_events),
                (
                    "Explicit Clip placements",
                    a.placements as u64,
                    b.placements as u64,
                ),
            ] {
                ui.label(label);
                ui.label(a.to_string());
                ui.label(b.to_string());
                ui.end_row();
            }
        });
    ui.small("Stored contents are counted once. Linked placements share their notes; unchanged primary placements are implicit.");
    ui.label(format!("Track changes: {} added, {} removed; {} existing with changed notes, {} with changed events, {} with changed settings.",c.added,c.removed,c.notes,c.events,c.settings));
    ui.collapsing("Conductor, organization and project data", |ui| {
        egui::Grid::new("format-review-domains")
            .num_columns(4)
            .striped(true)
            .show(ui, |ui| {
                for (label, a, b, changed) in [
                    ("Tempo points", a.tempos, b.tempos, c.tempo),
                    ("Time signatures", a.signatures, b.signatures, c.meter),
                    ("Markers / chapters", a.markers, b.markers, c.markers),
                    ("Master Volume points", a.master, b.master, c.master),
                    ("Groups", a.groups, b.groups, c.groups),
                    (
                        "Plugin attribute records",
                        a.attributes,
                        b.attributes,
                        c.attributes,
                    ),
                ] {
                    ui.label(label);
                    ui.label(a.to_string());
                    ui.label(format!("→ {b}"));
                    ui.label(if changed { "Changed" } else { "Unchanged" });
                    ui.end_row();
                }
            });
        for (label, changed) in [
            ("Track order or hierarchy", c.order),
            ("Clip layout or owned content", c.clips),
            ("Project settings", c.project),
        ] {
            ui.label(format!(
                "{label}: {}",
                if changed { "changed" } else { "unchanged" }
            ));
        }
    });
    if !r.samples.is_empty() {
        ui.collapsing(
            format!(
                "Changed Track samples ({} of {})",
                r.samples.len(),
                r.sampled_tracks
            ),
            |ui| {
                for s in &r.samples {
                    ui.label(format!(
                        "{} [id {}]: {}{}{}{}",
                        s.label,
                        s.track.0,
                        s.kind,
                        if s.notes { " · notes" } else { "" },
                        if s.events { " · events" } else { "" },
                        if s.settings { " · settings" } else { "" }
                    ));
                }
            },
        );
    }
}

#[cfg(test)]
#[path = "lua_formats_tests.rs"]
mod tests;
