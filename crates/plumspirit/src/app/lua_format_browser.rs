//! Read-only native view of private import data. No Lua or project actions.
use crate::piano_roll::{PianoRoll, RollInput};
use plumspirit_engine::model::events::{EventStore, RawKind};
use plumspirit_engine::model::metadata::Value;
use plumspirit_engine::model::{ContentId, ContentRef, Division, PlumDocument};
use plumspirit_engine::view::{TimeAxis, grid::MeterMap};
use std::fmt::{self, Write};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Notes,
    Events,
    Settings,
    Clips,
    Conductor,
    Groups,
    Attributes,
}
pub(super) struct Browser {
    pub open: bool,
    tab: Tab,
    track: usize,
    content: Option<ContentId>,
    shift: i128,
    placement: Option<usize>,
    roll: PianoRoll,
    // Constant-size tick grid; no full Conductor conversion or clone on UI.
    meter: MeterMap,
    tick: String,
    error: Option<String>,
    focus: Option<(u64, u8)>,
    generation: u64,
    attribute: usize,
    attribute_page: usize,
    #[cfg(test)]
    rows: usize,
}
impl Default for Browser {
    fn default() -> Self {
        Self {
            open: true,
            tab: Tab::Notes,
            track: 0,
            content: None,
            shift: 0,
            placement: None,
            roll: PianoRoll::default(),
            meter: MeterMap::new(Division::ppqn(0), &[]),
            tick: "0".into(),
            error: None,
            focus: None,
            generation: 1,
            attribute: 0,
            attribute_page: 0,
            #[cfg(test)]
            rows: 0,
        }
    }
}
impl Browser {
    pub fn select_track(&mut self, doc: &PlumDocument, index: usize) {
        if doc.track(index).is_none() {
            return;
        }
        self.track = index;
        self.content = None;
        self.shift = 0;
        self.placement = None;
        self.tab = Tab::Notes;
        self.changed(doc);
    }
    fn select_clip(&mut self, doc: &PlumDocument, index: usize) {
        let Some(p) = doc.clips().explicit_placements().get(index) else {
            return;
        };
        let (track, content) = match p.content {
            ContentRef::Track(id) => (doc.index_of(id), None),
            ContentRef::Owned(id) => (doc.index_of(p.track), Some(id)),
        };
        let Some(track) = track else {
            return;
        };
        self.track = track;
        self.content = content;
        self.shift = i128::from(p.start) - i128::from(p.offset);
        self.placement = Some(index);
        self.tab = Tab::Notes;
        self.changed(doc);
    }
    fn target(&self) -> plumspirit_engine::editor::NoteTarget {
        self.content.map_or(
            plumspirit_engine::editor::NoteTarget::Track(self.track),
            |content| plumspirit_engine::editor::NoteTarget::Content {
                host: self.track,
                content,
            },
        )
    }
    fn changed(&mut self, doc: &PlumDocument) {
        self.generation = self.generation.wrapping_add(1);
        self.error = None;
        self.focus = plumspirit_engine::editor::note_view(doc, self.target())
            .ok()
            .and_then(|v| {
                v.notes
                    .iter()
                    .next()
                    .map(|n| (n.start_tick, n.attrs.pitch()))
            });
        self.tick = self.focus.map_or(0, |n| n.0).to_string();
        self.roll.request_reset_view();
    }
    pub fn show(&mut self, ctx: &egui::Context, doc: &PlumDocument, revision: u64, dark: bool) {
        if !self.open {
            return;
        }
        #[cfg(test)]
        {
            self.rows = 0;
        }
        let mut open = true;
        egui::Window::new("Staged import data — read only").id(egui::Id::new("lua-format-browser")).open(&mut open).default_size(egui::vec2(900.,620.)).min_size(egui::vec2(400.,300.)).resizable(true).collapsible(false).show(ctx,|ui| {
            ui.label("Private imported result. Apply or Cancel in Review Import. This view does not edit or audition notes.");
            ui.horizontal_wrapped(|ui| {
                for (tab,label) in [(Tab::Notes,"Notes"),(Tab::Events,"Events"),(Tab::Settings,"Track settings"),(Tab::Clips,"Clips"),(Tab::Conductor,"Conductor"),(Tab::Groups,"Groups"),(Tab::Attributes,"Attributes")] {ui.selectable_value(&mut self.tab,tab,label);}
            });
            ui.horizontal(|ui| {
                ui.label("Track / Subtrack");let mut index=self.track+1;
                if ui.add(egui::DragValue::new(&mut index).range(1..=doc.track_count().max(1))).changed(){let tab=self.tab;self.select_track(doc,index-1);self.tab=tab;}
                if let Some(t)=doc.track(self.track){ui.label(short(t.name().unwrap_or("Track"),80));ui.label(format!("id {}",t.id().0));}
            });
            if let Some(i)=self.placement&&let Some(p)=doc.clips().explicit_placements().get(i) {
                ui.label(format!("Clip {:?} · host Track id {} · content {:?} · song start {} · offset {} · length {:?} · muted {}",p.id,p.track.0,p.content,p.start,p.offset,p.length,p.muted));
                ui.small("Notes and events show the complete shared content in local ticks; the ruler is shifted to song time. The placement window above controls which notes play.");
            }
            match self.tab {
                Tab::Notes=>self.notes(ui,doc,revision,dark),
                Tab::Events=>{
                    if let Ok(view)=plumspirit_engine::editor::note_view(doc,self.target()) {let rows=events(ui,view.events());#[cfg(test)]{self.rows+=rows;}let _=rows;}
                }
                Tab::Settings=>{
                    if let Some(t)=doc.track(self.track) {
                        let mut props=Excerpt::new(2048);let _=write!(props,"{:?}",t.props());
                        ui.label(format!("Parent {:?} · mute {} · solo {} · explicit end {:?}",t.parent(),t.mute,t.solo,t.end_of_track_tick));ui.monospace(props.text);
                        ui.label("Subchannels (native IDs and physical MIDI channels)");
                        let rows=table(ui,"staged-subchannels",t.subchannels().len(),|ui,i|{let s=&t.subchannels().entries()[i];line(ui,format!("id {} · MIDI channel {} · {}",s.id.number(),s.midi_channel+1,short(s.name.as_deref().unwrap_or(""),80)));});
                        #[cfg(test)]{self.rows+=rows;}let _=rows;
                    }
                }
                Tab::Clips=>self.clips(ui,doc),
                Tab::Conductor=>self.conductor(ui,doc),
                Tab::Groups=>{
                    let entries=doc.project().groups.entries();
                    let rows=table(ui,"staged-groups",entries.len(),|ui,i|{let g=&entries[i];line(ui,format!("id {} · parent {} · link {} · locked {} · temporary {} · color {:?} · {}",g.id,g.parent,g.link,g.locked,g.temporary,g.color,short(g.name.as_deref().unwrap_or(""),80)));});
                    #[cfg(test)]{self.rows+=rows;}let _=rows;
                }
                Tab::Attributes=>self.attributes(ui,doc),
            }
        });
        self.open = open;
    }
    fn notes(&mut self, ui: &mut egui::Ui, doc: &PlumDocument, revision: u64, dark: bool) {
        let Ok(view) = plumspirit_engine::editor::note_view(doc, self.target()) else {
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.label("Content tick");
            ui.add(
                egui::TextEdit::singleline(&mut self.tick)
                    .char_limit(20)
                    .desired_width(170.),
            );
            if ui.button("Go").clicked() {
                match self.tick.parse::<u64>() {
                    Ok(t) => {
                        self.focus = Some((
                            t,
                            self.roll
                                .view
                                .pitch
                                .y_to_pitch(self.roll.view_height() / 2.)
                                .unwrap_or(60),
                        ));
                        self.error = None;
                    }
                    Err(_) => {
                        self.error = Some(
                            "Enter an exact non-negative integer tick up to 18446744073709551615."
                                .into(),
                        )
                    }
                }
            }
            if ui.button("First note").clicked() {
                self.focus = view
                    .notes
                    .iter()
                    .next()
                    .map(|n| (n.start_tick, n.attrs.pitch()));
            }
            if ui.button("Reset zoom").clicked() {
                self.roll.view.time.pixels_per_tick =
                    crate::piano_roll::viewport::reference_ppt(doc.division());
                let _ = self.roll.set_zoom_y_percent(100.);
            }
        });
        ui.small(format!("{} notes · {} non-note events · {} keys · native Track/Subchannel colors · tick grid (Conductor maps are listed separately)",view.notes.len(),view.events().len(),doc.project().keys.keys()));
        if let Some(e) = &self.error {
            ui.colored_label(egui::Color32::YELLOW, e);
        }
        let pal = if dark {
            &crate::ui::theme::DARK
        } else {
            &crate::ui::theme::LIGHT
        };
        let input = RollInput {
            doc,
            track_index: self.track,
            content: self.content,
            shift: self.shift,
            title: Some("Staged content — read only"),
            locked: None,
            parts: &[],
            meter: &self.meter,
            palette: pal,
            dark,
            editable: false,
            keyboard: false,
            onion: &[],
            onion_alpha: 0,
            onion_outlines: None,
            capture: None,
            stroke_overlay: &[],
            playhead: None,
            conductor_active: false,
            new_note_channel: 0,
            new_note_gate: plumspirit_engine::editor::gate::DefaultGate::Grid,
            new_note_sub: Default::default(),
            preview: None,
            tool_preview: None,
            added: None,
            loop_range: None,
            loop_enabled: false,
            corrections_note: None,
            range: None,
            key_colors: None,
            audition: false,
            key_guide: false,
            note_menu: None,
            overlay_controls: [None; 3],
        };
        let extent = view
            .notes
            .max_end()
            .unwrap_or(0)
            .max(view.events().last_tick().unwrap_or(0));
        self.roll
            .show_staged(ui, input, revision.wrapping_add(self.generation), extent);
        if let Some((tick, key)) = self.focus.take() {
            self.roll.view.time = TimeAxis::new(tick, self.roll.view.time.pixels_per_tick);
            self.roll
                .view
                .center_pitch(f64::from(key), self.roll.view_height());
            self.roll.cursor_tick = tick;
            ui.ctx().request_repaint();
        }
    }
    fn clips(&mut self, ui: &mut egui::Ui, doc: &PlumDocument) {
        ui.label("Explicit placements. Unchanged primary placements are available through their Track. Click a row to inspect its shared content.");
        let placements = doc.clips().explicit_placements();
        let mut picked = None;
        let rows = table(ui, "staged-clips", placements.len(), |ui, i| {
            let p = &placements[i];
            if line(
                ui,
                format!(
                    "{:?} · host {} · {:?} · start {} · offset {} · length {:?} · muted {} · {}",
                    p.id,
                    p.track.0,
                    p.content,
                    p.start,
                    p.offset,
                    p.length,
                    p.muted,
                    short(p.name.as_deref().unwrap_or(""), 80)
                ),
            )
            .clicked()
            {
                picked = Some(i);
            }
        });
        #[cfg(test)]
        {
            self.rows += rows;
        }
        let _ = rows;
        if let Some(i) = picked {
            self.select_clip(doc, i);
        }
    }
    fn conductor(&mut self, ui: &mut egui::Ui, doc: &PlumDocument) {
        ui.label(format!(
            "{} · {:?} · {:?}",
            short(&doc.project().name, 80),
            doc.source_format(),
            doc.division()
        ));
        let c = doc.conductor();
        egui::ScrollArea::vertical()
            .id_salt("staged-conductor")
            .max_height(ui.available_height().max(80.))
            .show(ui, |ui| {
                for (kind, label, n) in [
                    (0, "Tempo points", c.tempo_points().len()),
                    (1, "Time signatures", c.time_signatures().len()),
                    (2, "Markers / chapters", c.markers().len()),
                    (3, "Master Volume points", c.master_volume().len()),
                ] {
                    egui::CollapsingHeader::new(format!("{label} ({n})"))
                        .default_open(true)
                        .show(ui, |ui| {
                            let rows = table(ui, kind, n, |ui, i| {
                                let text = match kind {
                                    0 => {
                                        let p = &c.tempo_points()[i];
                                        format!(
                                            "id {} · tick {} · {} BPM · {:?}",
                                            p.id,
                                            p.tick,
                                            plumspirit_engine::model::conductor::format_tempo(
                                                p.value
                                            ),
                                            p.interp
                                        )
                                    }
                                    1 => {
                                        let p = &c.time_signatures()[i];
                                        format!(
                                            "id {} · tick {} · {} · clocks {} · 32nds {}",
                                            p.id,
                                            p.tick,
                                            p.label(),
                                            p.clocks,
                                            p.thirty_seconds
                                        )
                                    }
                                    2 => {
                                        let p = &c.markers()[i];
                                        format!(
                                            "id {} · start {} · end {:?} · cue {} · {}",
                                            p.id,
                                            p.start,
                                            p.end,
                                            p.cue,
                                            short(&p.title, 500)
                                        )
                                    }
                                    _ => {
                                        let p = &c.master_volume()[i];
                                        format!("id {} · tick {} · value {}", p.id, p.tick, p.value)
                                    }
                                };
                                line(ui, text);
                            });
                            #[cfg(test)]
                            {
                                self.rows += rows;
                            }
                            let _ = rows;
                        });
                }
            });
    }
    fn attributes(&mut self, ui: &mut egui::Ui, doc: &PlumDocument) {
        let metadata = &doc.project().metadata;
        ui.label("Plugin-owned native attributes, in namespace/target order. Select a record to inspect its typed value.");
        let rows = table_height(ui, "staged-attributes", metadata.len(), 120., |ui, i| {
            if let Some((owner, target, r)) = metadata.records().nth(i)
                && line(ui, format!("{owner} · {target:?} · version {}", r.version)).clicked()
            {
                self.attribute = i;
                self.attribute_page = 0;
            }
        });
        #[cfg(test)]
        {
            self.rows += rows;
        }
        let _ = rows;
        if let Some((owner, target, r)) = metadata.records().nth(self.attribute) {
            ui.label(format!("{owner} · {target:?} · version {}", r.version));
            let current_page = self.attribute_page;
            let mut text = Excerpt::page(8192, current_page * 8192);
            let complete = value(&mut text, &r.value).is_ok();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        self.attribute_page > 0,
                        egui::Button::new("Previous value page"),
                    )
                    .clicked()
                {
                    self.attribute_page -= 1;
                }
                ui.label(format!(
                    "Value page {} (up to 8192 characters)",
                    current_page + 1
                ));
                if ui
                    .add_enabled(
                        !complete && self.attribute_page < 48,
                        egui::Button::new("Next value page"),
                    )
                    .clicked()
                {
                    self.attribute_page += 1;
                }
            });
            egui::ScrollArea::vertical()
                .id_salt("staged-attribute-value")
                .max_height(180.)
                .show(ui, |ui| {
                    ui.monospace(text.text);
                });
        }
    }
}
fn table(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    count: usize,
    row: impl FnMut(&mut egui::Ui, usize),
) -> usize {
    table_height(ui, id, count, 260., row)
}
fn table_height(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    count: usize,
    height: f32,
    mut row: impl FnMut(&mut egui::Ui, usize),
) -> usize {
    let mut visible = 0;
    egui::ScrollArea::vertical()
        .id_salt(id)
        .max_height(height)
        .show_rows(ui, 22., count, |ui, range| {
            visible = range.len();
            for i in range {
                row(ui, i);
            }
        });
    visible
}
fn line(ui: &mut egui::Ui, text: String) -> egui::Response {
    ui.add(
        egui::Label::new(&text)
            .truncate()
            .sense(egui::Sense::click()),
    )
    .on_hover_text(text)
}
fn short(text: &str, n: usize) -> String {
    let mut s = text
        .chars()
        .take(n)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>();
    if text.chars().nth(n).is_some() {
        s.push('…');
    }
    s
}
fn events(ui: &mut egui::Ui, events: &EventStore) -> usize {
    ui.small("Exact content-local ticks and stored bytes. Variable payload excerpts show at most 32 bytes; complete bytes remain in the native event store.");
    table(ui, "staged-events", events.len(), |ui, i| {
        let e = &events.events()[i];
        let data = match e.kind {
            RawKind::Channel { status, data } => {
                let interpretation = match status & 0xf0 {
                    0xb0 => format!("CC {} = {}", data[0], data[1]),
                    0xe0 => format!(
                        "Pitch Bend {}",
                        i32::from(data[0]) + (i32::from(data[1]) << 7) - 8192
                    ),
                    0xc0 => format!("Program {}", data[0]),
                    0xd0 => format!("Channel Pressure {}", data[0]),
                    0xa0 => format!("Poly Pressure key {} = {}", data[0], data[1]),
                    0x90 => format!("Unpaired Note On key {} velocity {}", data[0], data[1]),
                    0x80 => format!("Unpaired Note Off key {} velocity {}", data[0], data[1]),
                    _ => "Preserved channel event".into(),
                };
                format!(
                    "channel {} · {interpretation} · status {status:02X} · bytes {:02X} {:02X}",
                    1 + (status & 15),
                    data[0],
                    data[1]
                )
            }
            RawKind::System { status, data } => {
                format!("system {status:02X} · data {:02X} {:02X}", data[0], data[1])
            }
            RawKind::Meta { meta_type, payload } => format!(
                "meta {meta_type:02X} · {} bytes · {:02X?}",
                payload.len,
                &events.payload(payload)[..(payload.len as usize).min(32)]
            ),
            RawKind::SysEx { payload } | RawKind::SysExEscape { payload } => format!(
                "{:?} · {} bytes · {:02X?}",
                if matches!(e.kind, RawKind::SysEx { .. }) {
                    "SysEx"
                } else {
                    "SysEx escape"
                },
                payload.len,
                &events.payload(payload)[..(payload.len as usize).min(32)]
            ),
        };
        line(
            ui,
            format!(
                "#{} · tick {} · anchor {} · {data}",
                i + 1,
                e.tick,
                e.anchor
            ),
        );
    })
}
struct Excerpt {
    text: String,
    left: usize,
    skip: usize,
}
impl Excerpt {
    fn new(limit: usize) -> Self {
        Self {
            text: String::new(),
            left: limit,
            skip: 0,
        }
    }
    fn page(limit: usize, skip: usize) -> Self {
        Self {
            skip,
            ..Self::new(limit)
        }
    }
}
impl Write for Excerpt {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let mut chars = s.chars();
        while self.skip > 0 {
            if chars.next().is_none() {
                return Ok(());
            }
            self.skip -= 1;
        }
        for c in chars.by_ref().take(self.left) {
            self.text.push(c);
            self.left -= 1;
        }
        if chars.next().is_some() {
            Err(fmt::Error)
        } else {
            Ok(())
        }
    }
}
fn value(w: &mut Excerpt, v: &Value) -> fmt::Result {
    match v {
        Value::Null => w.write_str("null"),
        Value::Bool(v) => write!(w, "{v}"),
        Value::Integer(v) => write!(w, "{v}"),
        Value::Number(v) => write!(w, "{:?}", f64::from_bits(*v)),
        Value::Text(v) => write!(w, "{v:?}"),
        Value::Array(a) => {
            w.write_char('[')?;
            for (i, v) in a.iter().enumerate() {
                if i > 0 {
                    w.write_str(", ")?;
                }
                value(w, v)?;
            }
            w.write_char(']')
        }
        Value::Object(a) => {
            w.write_char('{')?;
            for (i, (k, v)) in a.iter().enumerate() {
                if i > 0 {
                    w.write_str(", ")?;
                }
                write!(w, "{k:?}: ")?;
                value(w, v)?;
            }
            w.write_char('}')
        }
    }
}

#[cfg(test)]
#[path = "lua_format_browser_tests.rs"]
mod tests;
