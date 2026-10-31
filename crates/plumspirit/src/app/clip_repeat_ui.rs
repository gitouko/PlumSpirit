//! Paste Repeat in the app (v0.9 V09-S13 / S15, contract §088 – §091,
//! §094, §198; `docs/PASTE_REPEAT.md`). **Ctrl+B** / Edit → *Paste
//! Repeat…*: in the Piano Roll it opens the `paste_repeat_notes` tool window
//! on the selected notes (the platform's Preview and undo); in the Track
//! List it opens this module's dialog on the selected clips — the owner's
//! reference fields (Number of times to paste, End to end / Even spacing,
//! Paste every + unit) plus Linked / Independent copies, the ten clip
//! functions (S15: an Advanced section drawn from
//! `editor::clip_repeat::params()` with the tool windows' rows), the plan's
//! numbers or its sentence, and dashed outlines of every new clip in the
//! Track List (muted ones dimmer) — then Apply as one undo step with the new
//! clips selected (Undo brings the old selection back). The fields, the
//! Linked choice and the functions are remembered for the next opening.

use super::PlumSpiritApp;
use super::dialogs::Dialog;
use crate::ui::status_bar::StatusKind;
use plumspirit_engine::editor::clip_repeat::{
    ClipRepeatPlan, TOOL_ID, apply_clip_repeat, params, plan_clip_repeat,
};
use plumspirit_engine::midi::group_digits;
use plumspirit_engine::model::project::KeyRange;
use plumspirit_engine::model::{ClipId, PlumDocument};
use plumspirit_engine::transform::definition::ParamDef;
use plumspirit_engine::transform::param::ParamSet;
use plumspirit_engine::transform::repeat::{Decimal, RepeatSpec, RepeatUnit, Spacing};
use std::sync::{Arc, OnceLock};

/// One new clip of the Preview: its track, song span and whether it is
/// muted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OutlineClip {
    pub track: u32,
    pub start: u64,
    pub end: u64,
    pub muted: bool,
}

/// What the new clips cover, for the Track List's outlines: sorted by
/// (track, start) so a frame finds a row's clips in view by binary search.
#[derive(Clone, Debug, PartialEq)]
pub struct RepeatOutline {
    pub clips: Arc<[OutlineClip]>,
    /// The longest clip (how far before the view a clip in view can start).
    pub longest: u64,
    end: u64,
}

impl RepeatOutline {
    pub fn new(mut clips: Vec<OutlineClip>) -> RepeatOutline {
        clips.sort_unstable();
        let longest = clips.iter().map(|c| c.end - c.start).max().unwrap_or(0);
        let end = clips.iter().map(|c| c.end).max().unwrap_or(0);
        RepeatOutline {
            clips: clips.into(),
            longest,
            end,
        }
    }

    /// The song tick where the last new clip ends: while the dialog is open
    /// the Track List lets the view zoom and scroll out to it, so every
    /// outline can be seen before OK (the copies usually go past the song's
    /// end).
    pub fn end(&self) -> u64 {
        self.end
    }

    /// The clips on track `track` that overlap song ticks `[t0, t1)`.
    pub fn in_view(&self, track: usize, t0: u64, t1: u64) -> &[OutlineClip] {
        let t = track as u32;
        let a = self.clips.partition_point(|c| c.track < t);
        let b = self.clips.partition_point(|c| c.track <= t);
        let row = &self.clips[a..b];
        let from = t0.saturating_sub(self.longest);
        let lo = row.partition_point(|c| c.start < from);
        let hi = row.partition_point(|c| c.start < t1);
        &row[lo..hi.max(lo)]
    }
}

/// The dialog's inputs (a change re-plans).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepeatInputs {
    pub count: u32,
    pub even: bool,
    pub every: String,
    pub unit: usize,
}

impl Default for RepeatInputs {
    fn default() -> Self {
        RepeatInputs {
            count: 3,
            even: false,
            every: "1".into(),
            unit: RepeatUnit::ALL
                .iter()
                .position(|&u| u == RepeatUnit::Seconds)
                .unwrap_or(0),
        }
    }
}

impl RepeatInputs {
    /// The spec, or the sentence why the fields do not make one.
    pub fn spec(&self) -> Result<RepeatSpec, String> {
        let spacing = if self.even {
            Spacing::Even
        } else {
            Spacing::EndToEnd
        };
        Ok(RepeatSpec {
            count: self.count,
            spacing,
            every: match spacing {
                Spacing::Even => Decimal::parse(&self.every)?,
                Spacing::EndToEnd => Decimal::whole(1),
            },
            unit: RepeatUnit::ALL
                .get(self.unit)
                .copied()
                .unwrap_or(RepeatUnit::Seconds),
        })
    }
}

/// What the dialog remembers between openings.
#[derive(Clone, Debug, PartialEq)]
pub struct RepeatMemory {
    pub inputs: RepeatInputs,
    pub linked: bool,
    pub fx: ParamSet,
}

impl Default for RepeatMemory {
    fn default() -> Self {
        RepeatMemory {
            inputs: RepeatInputs::default(),
            linked: true,
            fx: ParamSet::new(),
        }
    }
}

/// The functions' parameters with their editors (built once).
pub fn param_defs() -> &'static [ParamDef] {
    static DEFS: OnceLock<Vec<ParamDef>> = OnceLock::new();
    DEFS.get_or_init(|| {
        params()
            .into_iter()
            .map(|spec| ParamDef {
                editor: plumspirit_engine::transform::editors::editor(TOOL_ID, &spec),
                spec,
            })
            .collect()
    })
}

/// The Track List Paste Repeat dialog's state.
#[derive(Clone, Debug)]
pub struct RepeatDialog {
    /// The clips selected when the dialog opened (what Undo reselects).
    pub ids: Vec<ClipId>,
    pub inputs: RepeatInputs,
    /// Linked copies (the same content, ∞) or independent ones.
    pub linked: bool,
    /// The ten functions' values (schema `editor::clip_repeat::params`).
    pub fx: ParamSet,
    /// The Track List's snap unit (End to end rounds up to it).
    pub snap: Option<u64>,
    /// The project's key range and the first selected clip's subchannel
    /// ids (the editors' ranges and swatches).
    pub keys: KeyRange,
    pub sub_ids: Vec<u32>,
    /// What `plan` and `summary` were made for.
    pub planned: Option<RepeatMemory>,
    pub plan: Result<Arc<ClipRepeatPlan>, String>,
    /// The plan in words (or empty when it failed).
    pub summary: String,
    pub outline: Option<RepeatOutline>,
    /// Apply failed (the clips changed meanwhile).
    pub error: Option<String>,
}

impl RepeatDialog {
    fn memory(&self) -> RepeatMemory {
        RepeatMemory {
            inputs: self.inputs.clone(),
            linked: self.linked,
            fx: self.fx.clone(),
        }
    }

    /// Whether the fields, the Linked choice or a function changed since
    /// the last plan.
    pub fn stale(&self) -> bool {
        self.planned
            .as_ref()
            .is_none_or(|m| m.inputs != self.inputs || m.linked != self.linked || m.fx != self.fx)
    }
}

impl PlumSpiritApp {
    /// Why Paste Repeat cannot run now (`None`: it can, or it is off for
    /// another reason). The menus show it as the disabled entry's tooltip.
    pub(super) fn paste_repeat_unavailable(&self) -> Option<String> {
        self.open.as_ref()?;
        match self.main_view {
            super::track_list::MainView::TrackList if self.track_list.selected.is_empty() => {
                Some("Select the clips to paste again (or notes in the Piano Roll).".into())
            }
            super::track_list::MainView::PianoRoll if self.selected_count() == 0 => {
                Some("Select the notes to paste again (or clips in the Track List).".into())
            }
            _ => None,
        }
    }

    /// Paste Repeat (Ctrl+B, Edit menu, clip and note menus): the tool
    /// window for the selected notes, or the clip dialog.
    pub(super) fn start_paste_repeat(&mut self) {
        if let Some(why) = self.paste_repeat_unavailable() {
            self.set_status(why, StatusKind::Warning);
            return;
        }
        if self.main_view == super::track_list::MainView::PianoRoll {
            self.open_tool("paste_repeat_notes");
            return;
        }
        let ids: Vec<ClipId> = self.track_list.selected.iter().copied().collect();
        let m = self.clip_repeat_memory.clone();
        let (keys, sub_ids) = self.open.as_ref().map_or_else(
            || (KeyRange::Standard, Vec::new()),
            |o| {
                let subs = ids
                    .first()
                    .and_then(|&id| o.doc.clips().placement(id))
                    .and_then(|p| o.doc.track_by_id(p.track))
                    .map(|t| t.subchannels().entries().iter().map(|e| e.id.0).collect())
                    .unwrap_or_default();
                (o.doc.project().keys, subs)
            },
        );
        let mut d = RepeatDialog {
            ids,
            inputs: m.inputs,
            linked: m.linked,
            fx: m.fx,
            snap: self.track_list.snap,
            keys,
            sub_ids,
            planned: None,
            plan: Err(String::new()),
            summary: String::new(),
            outline: None,
            error: None,
        };
        if let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) {
            let meter = self.meter_map().cloned();
            replan(&doc, &mut d, meter.as_ref());
        }
        self.dialog = Some(Dialog::RepeatClips(Box::new(d)));
    }

    /// The dialog's fields changed: plan again.
    pub(super) fn replan_clip_repeat(&mut self) {
        let Some(doc) = self.open.as_ref().map(|o| o.doc.clone()) else {
            return;
        };
        let meter = self.meter_map().cloned();
        if let Some(Dialog::RepeatClips(d)) = self.dialog.as_mut()
            && d.stale()
        {
            replan(&doc, d, meter.as_ref());
            self.clip_repeat_memory = d.memory();
        }
    }

    /// Apply: one undo step; the new clips become the selection, the old
    /// selection is remembered for Undo. `Err`: the sentence for the dialog.
    pub(super) fn apply_clip_repeat(&mut self) -> Result<(), String> {
        self.replan_clip_repeat();
        let Some(Dialog::RepeatClips(d)) = self.dialog.as_ref() else {
            return Ok(());
        };
        let plan = d.plan.clone()?;
        let ids = d.ids.clone();
        let Some(o) = self.open.as_ref() else {
            return Ok(());
        };
        let doc = o.doc.clone();
        let before = o.history.current_state();
        let first = doc.clips().next_ids().0;
        let op = apply_clip_repeat(&doc, &plan).map_err(|e| e.0)?;
        drop(doc);
        let added = plan.clips.len();
        self.apply_op(op)
            .ok_or_else(|| "The clips were not pasted.".to_string())?;
        let new: Vec<ClipId> = (0..added as u32)
            .map(|k| ClipId::Placed(first + k))
            .collect();
        self.track_list.selected = new.iter().copied().collect();
        if let Some(o) = self.open.as_mut() {
            let after = o.history.current_state();
            super::clip_merge_ui::remember(&mut o.clip_selections, before, ids);
            super::clip_merge_ui::remember(&mut o.clip_selections, after, new);
        }
        let n = plan.base.offsets.len() as u64;
        self.set_status(
            format!(
                "Pasted {} {} ({} new clips).",
                group_digits(n),
                if n == 1 { "time" } else { "times" },
                group_digits(added as u64)
            ),
            StatusKind::Info,
        );
        Ok(())
    }
}

/// The plan in words.
fn summary(
    d: &RepeatDialog,
    p: &ClipRepeatPlan,
    meter: Option<&plumspirit_engine::view::grid::MeterMap>,
) -> String {
    let pos = |t: u64| {
        meter.map_or_else(
            || format!("tick {}", group_digits(t)),
            |m| m.format_position(t),
        )
    };
    let n = p.base.offsets.len() as u64;
    let sources = p.sources.len() as u64;
    let plural = |n: u64, one: &'static str, many: &'static str| if n == 1 { one } else { many };
    let first = p.clips.iter().map(|c| c.start).min();
    let mut s = format!(
        "{} {} of {} {}: {} new {}{}.",
        group_digits(n),
        plural(n, "copy", "copies"),
        group_digits(sources),
        plural(sources, "clip", "clips"),
        group_digits(p.clips.len() as u64),
        plural(p.clips.len() as u64, "clip", "clips"),
        if d.fx.get_raw("a4") == Some(&plumspirit_engine::transform::ParamValue::Bool(true)) {
            ", linked or independent per generation"
        } else if d.linked {
            ", linked (no notes copied)"
        } else {
            ", independent"
        },
    );
    if let Some(f) = first {
        s.push_str(&format!(
            " The first starts at {}; the last ends at {}.",
            pos(f),
            pos(p.end())
        ));
    }
    // Functions 2, 3 and 10 reshape the base series' spacing (GUI journey
    // M2, S15: "One every 3,840 ticks" was shown for a recursive pattern).
    let on =
        |k: &str| d.fx.get_raw(k) == Some(&plumspirit_engine::transform::ParamValue::Bool(true));
    if on("a2") || on("a3") || on("a10") {
        s.push_str(" The spacing changes from copy to copy (Advanced functions).");
    } else {
        match p.base.period {
            Some(t) => s.push_str(&format!(" One every {} ticks.", group_digits(t))),
            None => {
                s.push_str(" The spacing in ticks differs from copy to copy (bars or real time).")
            }
        }
    }
    if !p.keys.is_empty() {
        s.push_str(&format!(
            " {} transposed or remapped {} ({} notes; the source is not changed, later edits of it do not reach them).",
            group_digits(p.keys.len() as u64),
            plural(p.keys.len() as u64, "content", "contents"),
            group_digits(p.derived_notes)
        ));
    }
    if p.left_out_notes > 0 || p.empty_copies > 0 {
        s.push_str(&format!(
            " {} {} outside the keys and {} left out{}.",
            group_digits(p.left_out_notes),
            plural(p.left_out_notes, "note falls", "notes fall"),
            plural(p.left_out_notes, "is", "are"),
            if p.empty_copies > 0 {
                format!(
                    "; {} {} be empty and {} not made",
                    group_digits(p.empty_copies),
                    plural(p.empty_copies, "copy would", "copies would"),
                    plural(p.empty_copies, "is", "are")
                )
            } else {
                String::new()
            }
        ));
    }
    if p.omitted > 0 || p.mutated > 0 {
        s.push_str(&format!(
            " {} {} omitted, {} mutated.",
            group_digits(u64::from(p.omitted)),
            plural(u64::from(p.omitted), "generation", "generations"),
            group_digits(u64::from(p.mutated))
        ));
    }
    s
}

/// Plans `d` for its current inputs: the plan, its summary and outline.
fn replan(
    doc: &PlumDocument,
    d: &mut RepeatDialog,
    meter: Option<&plumspirit_engine::view::grid::MeterMap>,
) {
    d.planned = Some(d.memory());
    d.error = None;
    d.plan = d
        .inputs
        .spec()
        .and_then(|s| plan_clip_repeat(doc, &d.ids, &s, d.snap, d.linked, &d.fx).map_err(|e| e.0))
        .map(Arc::new);
    match &d.plan {
        Ok(p) => {
            d.summary = summary(d, p, meter);
            d.outline = Some(RepeatOutline::new(
                p.clips
                    .iter()
                    .map(|c| {
                        let s = &p.sources[c.source as usize];
                        OutlineClip {
                            track: c.row,
                            start: c.start,
                            end: c.start + (s.span.1 - s.span.0).max(1),
                            muted: c.muted,
                        }
                    })
                    .collect(),
            ));
        }
        Err(_) => {
            d.summary.clear();
            d.outline = None;
        }
    }
}

#[cfg(test)]
mod tests {
    //! The clip dialog on the app harness: opens on the Track List
    //! selection, plans as the fields change, Apply is one undo step with
    //! the new clips selected, Undo restores the clips and the old
    //! selection, Cancel changes nothing; in the Piano Roll Ctrl+B opens the
    //! notes tool; with nothing selected a sentence.
    use super::*;
    use crate::app::commands::Command;
    use crate::app::dialogs::DialogOutcome;
    use crate::app::test_support::Harness;
    use crate::app::track_list::MainView;
    use plumspirit_engine::model::{Bpm, ContentRef, NoteAttrs, NoteOffKind, NoteRecord};

    fn song() -> PlumDocument {
        let mut d = PlumDocument::new_project("repeat".into(), Bpm::DEFAULT, 960);
        d.add_track().unwrap();
        for i in 0..2 {
            let recs: Vec<NoteRecord> = (0..4u64)
                .map(|k| {
                    NoteRecord::new(
                        k * 960,
                        900,
                        NoteAttrs::new(60 + i as u8, 100, 0, 0, NoteOffKind::NoteOff).unwrap(),
                    )
                })
                .collect();
            d.track_mut(i)
                .unwrap()
                .notes
                .insert_sorted_batch(&recs, false)
                .unwrap();
        }
        d
    }

    fn dialog(h: &Harness) -> &RepeatDialog {
        match h.app.dialog.as_ref() {
            Some(Dialog::RepeatClips(d)) => d,
            _ => panic!("the dialog is open: {}", h.status()),
        }
    }

    #[test]
    fn ctrl_b_repeats_the_selected_clips_in_one_undo_step() {
        let mut h = Harness::new(song());
        let ids: Vec<ClipId> = (0..2).map(|i| ClipId::Primary(h.id(i))).collect();
        h.set_clips(&ids);
        h.app.track_list.snap = Some(240);
        let fp = h.doc().state_fingerprint();
        h.chord(Command::PasteRepeat);
        let d = dialog(&h);
        // Both clips end at 3,780: copies every 3,840 (snap 240).
        assert_eq!(
            d.plan.as_ref().unwrap().base.offsets,
            vec![3840, 7680, 11_520]
        );
        assert!(
            d.summary
                .starts_with("3 copies of 2 clips: 6 new clips, linked"),
            "{}",
            d.summary
        );
        // Every new clip is outlined: 3 copies × 2 clips.
        assert_eq!(d.outline.as_ref().unwrap().clips.len(), 6);
        // Only the Linked choice changes: the summary follows it (S13 GUI
        // journey: it kept saying "linked" after Independent was chosen).
        if let Some(Dialog::RepeatClips(d)) = h.app.dialog.as_mut() {
            d.linked = false;
        }
        h.dialog_outcome(DialogOutcome::Open);
        let s = &dialog(&h).summary;
        assert!(
            s.starts_with("3 copies of 2 clips: 6 new clips, independent.")
                && !s.contains("linked"),
            "{s}"
        );
        if let Some(Dialog::RepeatClips(d)) = h.app.dialog.as_mut() {
            d.linked = true;
        }
        h.dialog_outcome(DialogOutcome::Open);
        assert!(dialog(&h).summary.contains(", linked (no notes copied)."));
        // Cancel: nothing.
        h.dialog_outcome(DialogOutcome::Closed);
        assert!(h.app.dialog.is_none());
        assert_eq!(h.doc().state_fingerprint(), fp);
        // Again, Even spacing every 2 bars, 2 times, independent.
        h.chord(Command::PasteRepeat);
        if let Some(Dialog::RepeatClips(d)) = h.app.dialog.as_mut() {
            d.inputs.count = 2;
            d.inputs.even = true;
            d.inputs.every = "2".into();
            d.inputs.unit = 0; // Bars
            d.linked = false;
        }
        h.dialog_outcome(DialogOutcome::Open);
        assert_eq!(
            dialog(&h).plan.as_ref().unwrap().base.offsets,
            vec![7680, 15_360]
        );
        let next = h.doc().clips().next_ids().0;
        h.dialog_outcome(DialogOutcome::ClipRepeatApply);
        assert!(h.app.dialog.is_none(), "{}", h.status());
        let new: Vec<ClipId> = (0..4).map(|k| ClipId::Placed(next + k)).collect();
        assert_eq!(
            h.app
                .track_list
                .selected
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            new
        );
        for id in &new {
            let p = h.doc().clips().placement(*id).unwrap();
            assert!(matches!(p.content, ContentRef::Owned(_)), "independent");
        }
        assert_eq!(
            h.app.open.as_ref().unwrap().history.undo_label(),
            Some("Paste Repeat Clips")
        );
        assert!(h.command(Command::Undo));
        assert_eq!(h.doc().state_fingerprint(), fp);
        assert_eq!(
            h.app
                .track_list
                .selected
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            ids
        );
        assert!(h.command(Command::Redo));
        assert_eq!(
            h.app
                .track_list
                .selected
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            new
        );
        // The settings are remembered for the next time.
        h.set_clips(&ids);
        h.chord(Command::PasteRepeat);
        assert_eq!(dialog(&h).inputs.count, 2);
    }

    /// V09-S15 (§094): the Advanced functions plan with the fields — the
    /// outline is every planned clip (on the tracks the functions choose),
    /// the summary names the derived contents — and Apply is one undo step;
    /// the functions are remembered for the next opening.
    #[test]
    fn the_advanced_functions_plan_preview_and_apply_in_one_undo_step() {
        use plumspirit_engine::transform::ParamValue;
        let mut h = Harness::new(song());
        let ids = [ClipId::Primary(h.id(0))];
        h.set_clips(&ids);
        let fp = h.doc().state_fingerprint();
        h.chord(Command::PasteRepeat);
        if let Some(Dialog::RepeatClips(d)) = h.app.dialog.as_mut() {
            d.inputs.count = 2;
            // Lane offset 1 (wrap: two tracks), transpose +12 per generation,
            // the source's mute on every picked generation (all of them).
            d.fx.set("a1", ParamValue::Bool(true));
            d.fx.set("a1.edge", ParamValue::Choice(1));
            d.fx.set("a5", ParamValue::Bool(true));
            d.fx.set("a9", ParamValue::Bool(true));
            d.fx.set("a9.mode", ParamValue::Choice(1));
            d.fx.set(
                "a9.curve",
                ParamValue::Curve(plumspirit_engine::transform::param::Curve::new(vec![
                    (0.0, 1.0),
                    (1.0, 1.0),
                ])),
            );
        }
        h.dialog_outcome(DialogOutcome::Open);
        let d = dialog(&h);
        let plan = d.plan.as_ref().unwrap().clone();
        assert_eq!(
            plan.clips
                .iter()
                .map(|c| (c.row, c.start, c.muted))
                .collect::<Vec<_>>(),
            vec![(1, 3780, true), (0, 7560, true)]
        );
        let o = d.outline.as_ref().unwrap();
        assert_eq!(o.clips.len(), 2);
        assert!(o.clips.iter().all(|c| c.muted));
        assert_eq!(o.in_view(1, 0, 5000).len(), 1);
        assert_eq!(o.in_view(0, 0, 5000).len(), 0);
        assert!(
            d.summary.contains("2 transposed or remapped contents")
                && d.summary.contains("2 mutated"),
            "{}",
            d.summary
        );
        let contents = h.doc().clips().contents().len();
        h.dialog_outcome(DialogOutcome::ClipRepeatApply);
        assert!(h.app.dialog.is_none(), "{}", h.status());
        assert_eq!(h.doc().clips().contents().len(), contents + 2);
        assert_eq!(h.app.track_list.selected.len(), 2);
        assert!(h.command(Command::Undo));
        assert_eq!(h.doc().state_fingerprint(), fp);
        // Remembered: the next dialog opens with the functions on.
        h.set_clips(&ids);
        h.chord(Command::PasteRepeat);
        let d = dialog(&h);
        assert_eq!(d.fx.get_raw("a5"), Some(&ParamValue::Bool(true)));
        assert_eq!(d.inputs.count, 2);
        // A function the clips cannot follow says why; OK stays off.
        if let Some(Dialog::RepeatClips(d)) = h.app.dialog.as_mut() {
            d.fx.set("a1.edge", ParamValue::Choice(0));
            d.fx.set("a1.lanes", ParamValue::Int(5));
        }
        h.dialog_outcome(DialogOutcome::Open);
        let why = dialog(&h).plan.as_ref().unwrap_err().clone();
        assert!(why.contains("past the last track"), "{why}");
        assert!(dialog(&h).outline.is_none());
        // GUI journey M2 (S15): with a recursive pattern the copies are not
        // evenly spaced, so no "One every … ticks"; one omitted generation is
        // "1 generation".
        if let Some(Dialog::RepeatClips(d)) = h.app.dialog.as_mut() {
            d.fx.set("a1", ParamValue::Bool(false));
            d.fx.set("a5", ParamValue::Bool(false));
            d.fx.set("a9.mode", ParamValue::Choice(0));
            d.fx.set(
                "a9.curve",
                ParamValue::Curve(plumspirit_engine::transform::param::Curve::new(vec![
                    (0.0, 1.0),
                    (0.4, 1.0),
                    (0.6, 0.0),
                    (1.0, 0.0),
                ])),
            );
        }
        h.dialog_outcome(DialogOutcome::Open);
        let s = dialog(&h).summary.clone();
        assert!(s.contains(" One every "), "{s}");
        assert!(s.contains("1 generation omitted, 0 mutated."), "{s}");
        if let Some(Dialog::RepeatClips(d)) = h.app.dialog.as_mut() {
            d.fx.set("a10", ParamValue::Bool(true));
        }
        h.dialog_outcome(DialogOutcome::Open);
        let s = dialog(&h).summary.clone();
        assert!(!s.contains(" One every "), "{s}");
        assert!(s.contains("The spacing changes from copy to copy"), "{s}");
    }

    /// GUI journey M2 (V09-S15): Enter while a field is being typed in
    /// commits the field only; the next Enter applies. Drives the real
    /// window headless: click the count field (edit mode), Enter, Enter.
    #[test]
    fn enter_in_a_field_commits_the_field_and_the_next_enter_applies() {
        let mut h = Harness::new(song());
        let ids: Vec<ClipId> = (0..2).map(|i| ClipId::Primary(h.id(i))).collect();
        h.set_clips(&ids);
        h.chord(Command::PasteRepeat);
        let mut d = h.app.dialog.take().expect("the dialog opens");
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 900.0));
        let mut frame = |events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events,
                ..Default::default()
            };
            let mut outcome = DialogOutcome::Closed;
            let mut o = ctx.run_ui(input, |ui| {
                outcome = crate::app::dialogs::show(&mut d, ui.ctx(), true);
            });
            o.textures_delta.clear();
            let mut texts = Vec::new();
            fn collect(s: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
                match s {
                    egui::Shape::Text(t) => out.push((
                        t.galley.text().to_string(),
                        t.galley.rect.translate(t.pos.to_vec2()),
                    )),
                    egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, out)),
                    _ => {}
                }
            }
            o.shapes.iter().for_each(|c| collect(&c.shape, &mut texts));
            (outcome, texts)
        };
        let (_, texts) = frame(vec![]);
        let (_, texts) = if texts.is_empty() {
            frame(vec![])
        } else {
            (DialogOutcome::Open, texts)
        };
        let label = texts
            .iter()
            .find(|t| t.0 == "Number of times to paste:")
            .expect("the count label")
            .1;
        let field = texts
            .iter()
            .find(|t| {
                t.0 == "3"
                    && t.1.center().y > label.top()
                    && t.1.center().y < label.bottom()
                    && t.1.left() > label.right()
            })
            .expect("the count field")
            .1
            .center();
        let click = |pressed| egui::Event::PointerButton {
            pos: field,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(vec![egui::Event::PointerMoved(field)]);
        frame(vec![click(true)]);
        frame(vec![click(false)]);
        frame(vec![]);
        assert!(
            ctx.egui_wants_keyboard_input(),
            "the count field is being edited"
        );
        let enter = || egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let (o, _) = frame(vec![enter()]);
        assert!(
            matches!(o, DialogOutcome::Open),
            "the first Enter only commits the field"
        );
        frame(vec![]);
        assert!(!ctx.egui_wants_keyboard_input());
        let (o, _) = frame(vec![enter()]);
        assert!(
            matches!(o, DialogOutcome::ClipRepeatApply),
            "the next Enter applies"
        );
    }

    #[test]
    fn a_field_that_makes_no_plan_shows_its_sentence_and_apply_changes_nothing() {
        let mut h = Harness::new(song());
        h.set_clips(&[ClipId::Primary(h.id(0))]);
        let fp = h.doc().state_fingerprint();
        h.chord(Command::PasteRepeat);
        if let Some(Dialog::RepeatClips(d)) = h.app.dialog.as_mut() {
            d.inputs.even = true;
            d.inputs.every = "1.5".into();
            d.inputs.unit = 0;
        }
        h.dialog_outcome(DialogOutcome::Open);
        assert!(
            dialog(&h)
                .plan
                .as_ref()
                .unwrap_err()
                .starts_with("Bars take whole numbers")
        );
        assert!(dialog(&h).outline.is_none());
        h.dialog_outcome(DialogOutcome::ClipRepeatApply);
        assert!(h.app.dialog.is_some(), "stays open");
        assert_eq!(h.doc().state_fingerprint(), fp);
    }

    #[test]
    fn in_the_piano_roll_ctrl_b_opens_the_notes_tool_and_nothing_selected_says_why() {
        let mut h = Harness::new(song());
        h.set_nothing();
        h.chord(Command::PasteRepeat);
        assert!(h.app.dialog.is_none());
        assert!(
            h.status().contains("Select the clips to paste again"),
            "{}",
            h.status()
        );
        h.set_notes(0);
        assert_eq!(h.app.main_view, MainView::PianoRoll);
        h.chord(Command::PasteRepeat);
        assert_eq!(
            h.app.tool.as_ref().map(|t| t.def.id),
            Some("paste_repeat_notes")
        );
        assert!(h.app.dialog.is_none());
    }
}
