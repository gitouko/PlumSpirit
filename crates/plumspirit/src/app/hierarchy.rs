//! Track tree (v0.5): subtracks, the collapsible track list, subtree deletion and
//! the Onion Skin layer order.
//!
//! The document stores tracks depth first with parent ids (see the engine's
//! `PlumDocument::tree_rows`); the track list shows a flattened, virtualized
//! view of it. Collapsing is view state only: it never mutes, hides from
//! playback or export, or changes the document.

use super::PlumSpiritApp;
use super::dialogs::Dialog;
use crate::piano_roll::render::OnionSource;
use crate::ui::status_bar::StatusKind;
use plumspirit_engine::history::{Operation, Outcome};
use plumspirit_engine::midi::group_digits;
use plumspirit_engine::model::project::{DensityBasis, OnionOrder, OnionRanking};
use plumspirit_engine::model::{
    ContentId, ContentRef, NoteStore, OnionConfig, PlumDocument, TrackId,
};
use plumspirit_engine::view::tiles::SourcePlace;
use std::collections::HashSet;

/// One visible row of the track list: a track, or (v0.8) a folder header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeRow {
    /// Track index in the document. A folder header: its first member (an
    /// empty folder: the track it is shown before, or the track count).
    pub index: usize,
    pub depth: u32,
    pub has_children: bool,
    pub collapsed: bool,
    /// 1-based position among its siblings (labels of unnamed tracks).
    pub ordinal: u32,
    /// v0.8: a folder header row (`folder` is its id; `ordinal` its number of
    /// top-level tracks).
    pub header: bool,
    /// v0.8: the folder this row belongs to (0: none).
    pub folder: u32,
}

/// The visible rows, rebuilt only when the tree or the collapse state changes
/// (never per frame).
#[derive(Default)]
pub struct TreeCache {
    key: Option<(u64, u64, u64)>,
    rows: Vec<TreeRow>,
    /// Every track's sibling ordinal (not only the visible ones).
    ordinals: Vec<u32>,
    /// Bumped whenever the collapse state changes.
    pub collapse_revision: u64,
}

impl TreeCache {
    /// Visible rows for `doc` with the given collapsed tracks. O(tracks) when
    /// rebuilt, free otherwise.
    pub fn rows(
        &mut self,
        doc: &PlumDocument,
        generation: u64,
        collapsed: &HashSet<TrackId>,
    ) -> &[TreeRow] {
        let key = (
            generation,
            doc.structure_revision() ^ (doc.layout_revision() << 32),
            self.collapse_revision,
        );
        if self.key != Some(key) {
            self.ordinals = doc.track_ordinals();
            self.rows = visible_rows(doc, collapsed, &self.ordinals);
            self.key = Some(key);
        }
        &self.rows
    }
}

/// Notes each track plays through its clips (B1-0095), rebuilt only when a
/// placement or a placed content changes (`analysis_revision`), never per
/// frame.
#[derive(Default)]
pub struct NoteCountCache {
    key: Option<(u64, u64)>,
    counts: Vec<usize>,
}

impl NoteCountCache {
    pub fn counts(&mut self, doc: &PlumDocument, generation: u64) -> &[usize] {
        let key = (generation, doc.analysis_revision());
        if self.key != Some(key) {
            self.counts = doc.placed_note_counts();
            self.key = Some(key);
        }
        &self.counts
    }
}

/// Flattened visible rows: children of a collapsed track are skipped
/// (iterative). v0.8: a folder header precedes each run of its top-level
/// tracks (hidden while the folder is collapsed); empty folders are shown
/// before their anchor track or at the end.
pub fn visible_rows(
    doc: &PlumDocument,
    collapsed: &HashSet<TrackId>,
    ordinals: &[u32],
) -> Vec<TreeRow> {
    let tree = doc.tree_rows();
    let folders = &doc.project().folders;
    let folded = &doc.project().folders_collapsed;
    let mut out = Vec::with_capacity(tree.len());
    // Top-level members per folder (headers show the count).
    let mut counts: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    if !folders.is_empty() {
        for (t, &(depth, _)) in doc.tracks().iter().zip(&tree) {
            let f = folders.folder_of(t.id());
            if depth == 0 && f != 0 {
                *counts.entry(f).or_default() += 1;
            }
        }
    }
    let header = |id: u32, index: usize| TreeRow {
        index,
        depth: 0,
        has_children: true,
        collapsed: folded.contains(&id),
        ordinal: counts.get(&id).copied().unwrap_or(0),
        header: true,
        folder: id,
    };
    let empty: Vec<&plumspirit_engine::model::FolderInfo> = folders
        .folders()
        .iter()
        .filter(|f| !counts.contains_key(&f.id))
        .collect();
    let mut shown_empty: HashSet<u32> = HashSet::new();
    // Depth below which rows are hidden (a collapsed ancestor's depth).
    let mut hide_below: Option<u32> = None;
    let mut current = 0u32;
    let mut hide_folder = false;
    for (i, &(depth, has_children)) in tree.iter().enumerate() {
        let id = doc.tracks()[i].id();
        if depth == 0 && !folders.is_empty() {
            for f in empty.iter().filter(|f| f.anchor == Some(id)) {
                out.push(header(f.id, i));
                shown_empty.insert(f.id);
                current = 0;
            }
            let f = folders.folder_of(id);
            if f != current {
                current = f;
                if f != 0 {
                    out.push(header(f, i));
                }
            }
            hide_folder = f != 0 && folded.contains(&f);
            hide_below = None;
        }
        if hide_folder {
            continue;
        }
        if let Some(d) = hide_below {
            if depth > d {
                continue;
            }
            hide_below = None;
        }
        let is_collapsed = has_children && collapsed.contains(&id);
        if is_collapsed {
            hide_below = Some(depth);
        }
        out.push(TreeRow {
            index: i,
            depth,
            has_children,
            collapsed: is_collapsed,
            ordinal: ordinals.get(i).copied().unwrap_or(0),
            header: false,
            folder: current,
        });
    }
    for f in empty {
        if !shown_empty.contains(&f.id) {
            out.push(header(f.id, tree.len()));
        }
    }
    out
}

/// Onion Skin layer order, cached per (sources, view range, note revisions);
/// ranked with hysteresis (`OnionRanking`, v0.8 §116 – §118) so the front
/// layer never flickers. Since v0.9 V09-S11 also where each Onion track's
/// clips play ([`OnionOrderCache::places`]), resolved once per clip layout.
///
/// B1-0151: a moving view keeps the order it had until the view rests
/// ([`VIEW_REST`]): each new order is a new Onion picture whose tiles are
/// rebuilt, and re-ranking every few frames of a pan or zoom over dense
/// notes kept the Onion under the loading placeholder (owner's Hypernova).
#[derive(Default)]
pub struct OnionOrderCache {
    /// What the order was ranked for, without the view range.
    key: Option<u64>,
    /// The view range (1/64 of its width) the order was ranked for, the
    /// range seen last and since when.
    ranked_range: Option<(u64, u64)>,
    seen_range: Option<((u64, u64), std::time::Instant)>,
    order: Vec<usize>,
    ranking: OnionRanking,
    places: Option<(PlacesKey, Vec<Vec<OnionPlace>>)>,
}

/// A view that has not moved for this long is at rest (B1-0151: the Onion
/// re-ranks and the dense tiles build the shown zoom level).
pub use crate::piano_roll::dense::VIEW_REST;

/// What the cached placements were resolved for: the project, its clip
/// table and its track list (any change to a placement, an owned content's
/// existence or a track index changes one of them).
type PlacesKey = (u64, u64, u64);

/// One clip of an Onion track (V09-S11): its content and where it plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnionPlace {
    pub content: OnionContent,
    pub place: SourcePlace,
}

/// A clip's content: a track's own notes (by index, valid for the cached
/// track list) or an owned content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnionContent {
    Track(usize),
    Owned(ContentId),
}

impl OnionContent {
    pub fn notes(self, doc: &PlumDocument) -> Option<&NoteStore> {
        match self {
            OnionContent::Track(i) => doc.track(i).map(|t| &t.notes),
            OnionContent::Owned(c) => doc.clips().content(c).map(|o| &o.notes),
        }
    }
}

impl OnionOrderCache {
    /// Every track's clips as Onion sources (`placed_contents_by_track`),
    /// rebuilt only when the clip layout, the track list or the project
    /// changes — never per frame (V09-S11: one pass over 10,000 clips
    /// costs about 0.7 ms).
    pub fn places(&mut self, doc: &PlumDocument, generation: u64) -> &[Vec<OnionPlace>] {
        let key = (generation, doc.clips().revision(), doc.structure_revision());
        if self.places.as_ref().is_none_or(|(k, _)| *k != key) {
            let index: std::collections::HashMap<TrackId, usize> = doc
                .tracks()
                .iter()
                .enumerate()
                .map(|(i, t)| (t.id(), i))
                .collect();
            let by_track = doc
                .placed_contents_by_track()
                .into_iter()
                .map(|pcs| {
                    pcs.into_iter()
                        .filter_map(|pc| {
                            let content = match pc.placement.content {
                                ContentRef::Track(t) => OnionContent::Track(*index.get(&t)?),
                                ContentRef::Owned(c) => OnionContent::Owned(c),
                            };
                            Some(OnionPlace {
                                content,
                                place: SourcePlace::of(&pc.placement),
                            })
                        })
                        .collect()
                })
                .collect();
            self.places = Some((key, by_track));
        }
        self.places.as_ref().map_or(&[], |(_, v)| v.as_slice())
    }

    /// Ghost sources in drawing order (back to front). Density uses the notes
    /// that start in the visible range (two binary searches per chunk, no note
    /// visited) or the whole track, per the project's setting; small density
    /// changes keep the shown order, ties go by TrackId. Project order
    /// returns `sources` unchanged. `t0 ..= t1` is song time; with clips
    /// (`placed[k]`: the clips of `sources[k]`, V09-S11) a track's density
    /// is that of the notes its clips play there, else of its own notes.
    pub fn order(
        &mut self,
        doc: &PlumDocument,
        sources: &[usize],
        cfg: &OnionConfig,
        (t0, t1): (u64, u64),
        placed: Option<&[&[OnionSource<'_>]]>,
    ) -> Vec<usize> {
        if cfg.order == OnionOrder::Project || sources.len() < 2 {
            return sources.to_vec();
        }
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        cfg.order.hash(&mut h);
        cfg.density.hash(&mut h);
        // A view moved by less than 1/64 of its width keeps the ranking.
        let range = (cfg.density == DensityBasis::VisibleRange).then(|| {
            let q = (t1.saturating_sub(t0) / 64).max(1);
            (t0 / q, t1 / q)
        });
        let placed_of = |k: usize| placed.and_then(|p| p.get(k)).copied();
        for (k, &i) in sources.iter().enumerate() {
            if let Some(t) = doc.track(i) {
                (i, t.id(), t.notes.content_revision()).hash(&mut h);
            }
            for s in placed_of(k).unwrap_or_default() {
                (s.store.content_revision(), s.place).hash(&mut h);
            }
        }
        let key = h.finish();
        let now = std::time::Instant::now();
        // Only the view moved: hold the order until the view rests there.
        let rank = self.key != Some(key)
            || match range {
                Some(r) if self.ranked_range != Some(r) => {
                    if self.seen_range.is_none_or(|(s, _)| s != r) {
                        self.seen_range = Some((r, now));
                    }
                    self.seen_range
                        .is_some_and(|(_, since)| now.duration_since(since) >= VIEW_REST)
                }
                _ => false,
            };
        if rank {
            let counts: Vec<usize> = sources
                .iter()
                .enumerate()
                .map(|(k, &i)| match placed_of(k) {
                    Some(srcs) => srcs
                        .iter()
                        .map(|s| match cfg.density {
                            DensityBasis::VisibleRange => {
                                s.place.count_starting_in(s.store, t0, t1)
                            }
                            DensityBasis::WholeTrack => {
                                s.place.count_starting_in(s.store, 0, u64::MAX)
                            }
                        })
                        .sum(),
                    None => doc.track(i).map_or(0, |t| match cfg.density {
                        DensityBasis::VisibleRange => t.notes.count_starting_in(t0, t1),
                        DensityBasis::WholeTrack => t.notes.len(),
                    }),
                })
                .collect();
            let ids: Vec<TrackId> = sources
                .iter()
                .map(|&i| doc.track(i).map_or(TrackId(u32::MAX), |t| t.id()))
                .collect();
            self.order = self.ranking.order(cfg.order, sources, &ids, &counts);
            self.key = Some(key);
            self.ranked_range = range;
            self.seen_range = range.map(|r| (r, now));
        }
        self.order.clone()
    }

    /// When a held order is due to be re-ranked (the view moved and has not
    /// rested yet): the caller repaints then, so a view that stopped gets
    /// its order without another input.
    pub fn rerank_due(&self) -> Option<std::time::Instant> {
        match (self.ranked_range, self.seen_range) {
            (Some(r), Some((s, since))) if r != s => Some(since + VIEW_REST),
            _ => None,
        }
    }
}

impl PlumSpiritApp {
    pub(super) fn toggle_collapsed(&mut self, id: TrackId) {
        if !self.collapsed.remove(&id) {
            self.collapsed.insert(id);
        }
        self.tree_rows.collapse_revision += 1;
    }

    /// Adds `count` subtracks to the track at `parent` (one undo step) and makes
    /// the first new one active.
    pub(super) fn add_subtracks(&mut self, parent: usize, count: usize) {
        let Some(doc) = self.doc() else {
            return;
        };
        if count == 0 {
            return;
        }
        let room = usize::from(u16::MAX).saturating_sub(doc.track_count());
        if count > room {
            self.show_error(
                "Too many subtracks.",
                format!(
                    "A project can hold at most 65,535 tracks and subtracks (the limit of a MIDI file); {} more can be added.",
                    group_digits(room as u64)
                ),
            );
            return;
        }
        let pid = doc.track(parent).map(|t| t.id());
        let started = std::time::Instant::now();
        if let Some(Outcome::Track(i)) = self.apply_op(Operation::AddSubtracks { parent, count }) {
            if let Some(p) = pid
                && self.collapsed.remove(&p)
            {
                self.tree_rows.collapse_revision += 1;
            }
            self.select_track(i);
            let label = if count == 1 {
                "Added a subtrack".to_owned()
            } else {
                format!("Added {} subtracks", group_digits(count as u64))
            };
            self.tasks
                .finish(label.clone(), started, super::tasks::TaskOutcome::Done);
            self.set_status(format!("{label}."), StatusKind::Success);
        }
    }

    /// The name the track list shows for track `i` ("Track 3", "Subtrack 2" or
    /// its own name).
    pub(super) fn track_display_name(&self, i: usize) -> String {
        self.doc().map(|d| d.track_label(i)).unwrap_or_default()
    }

    /// Makes track `i` active (clears the gesture and the graph selection).
    pub(super) fn select_track(&mut self, i: usize) {
        self.select_conductor(false);
        if let Some(o) = self.open.as_mut()
            && o.active_track != i
        {
            o.active_track = i;
            self.roll.cancel_gesture();
            self.graphs.cancel();
            self.graphs.clear_point_selection();
        }
    }

    /// Delete confirmation that summarises the whole subtrees of `roots`.
    pub(super) fn delete_tracks_dialog(&self, roots: &[usize]) -> Option<Dialog> {
        let doc = self.doc()?;
        let (mut notes, mut events, mut tracks) = (0usize, 0usize, 0usize);
        for &index in roots {
            doc.track(index)?;
            let end = doc.subtree_end(index);
            for k in index..end {
                notes += doc.tracks()[k].notes.len();
                events += doc.tracks()[k].events.len();
            }
            tracks += end - index;
        }
        let name = if roots.len() == 1 {
            doc.track_label(roots[0])
        } else {
            format!("{} tracks", roots.len())
        };
        Some(Dialog::DeleteTrack {
            roots: roots.to_vec(),
            name,
            notes,
            events,
            subtracks: tracks - roots.len(),
            folder: 0,
        })
    }

    /// Mute (or unmute) a track together with all of its subtracks (explicit
    /// subtree action; ordinary Mute acts on one track's own notes only).
    pub(super) fn mute_subtree(&mut self, index: usize, on: bool) {
        let Some(d) = self.doc_mut() else {
            return;
        };
        let end = d.subtree_end(index);
        for k in index..end {
            if let Some(t) = d.track_mut(k) {
                t.mute = on;
            }
        }
    }

    /// Solo a track together with all of its subtracks (every other track is
    /// un-soloed).
    pub(super) fn solo_subtree(&mut self, index: usize) {
        let Some(d) = self.doc_mut() else {
            return;
        };
        let end = d.subtree_end(index);
        for k in 0..d.track_count() {
            if let Some(t) = d.track_mut(k) {
                t.solo = (index..end).contains(&k);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plumspirit_engine::history::History;
    use plumspirit_engine::model::Bpm;

    fn tree() -> PlumDocument {
        let mut d = PlumDocument::new_project("t".into(), Bpm::DEFAULT, 960);
        let mut h = History::default();
        h.apply(&mut d, Operation::AddTrack).unwrap();
        h.apply(
            &mut d,
            Operation::AddSubtracks {
                parent: 0,
                count: 3,
            },
        )
        .unwrap();
        h.apply(
            &mut d,
            Operation::AddSubtracks {
                parent: 1,
                count: 2,
            },
        )
        .unwrap();
        d
    }

    #[test]
    fn collapsing_hides_only_descendants_and_is_cached() {
        let d = tree();
        // 0: Track 1 [1: Sub 1 [2, 3], 4: Sub 2, 5: Sub 3], 6: Track 2
        let all = visible_rows(&d, &HashSet::new(), &d.track_ordinals());
        assert_eq!(all.len(), 7);
        assert_eq!(all[2].depth, 2);
        let sub1 = d.track(1).unwrap().id();
        let top = d.track(0).unwrap().id();
        let rows = visible_rows(&d, &HashSet::from([sub1]), &d.track_ordinals());
        let idx: Vec<usize> = rows.iter().map(|r| r.index).collect();
        assert_eq!(idx, vec![0, 1, 4, 5, 6]);
        assert!(rows[1].collapsed);
        let rows = visible_rows(&d, &HashSet::from([top]), &d.track_ordinals());
        let idx: Vec<usize> = rows.iter().map(|r| r.index).collect();
        assert_eq!(idx, vec![0, 6]);
        // Collapsing a track without children changes nothing.
        let leaf = d.track(6).unwrap().id();
        assert_eq!(
            visible_rows(&d, &HashSet::from([leaf]), &d.track_ordinals()).len(),
            7
        );
        let mut cache = TreeCache::default();
        let c = HashSet::new();
        assert_eq!(cache.rows(&d, 1, &c).len(), 7);
        assert_eq!(cache.rows(&d, 1, &c).len(), 7);
    }

    #[test]
    fn folder_headers_precede_their_tracks_and_collapse_hides_them() {
        use plumspirit_engine::editor::folders::plan_create;
        let mut d = tree();
        // 0: Track 1 [1: Sub 1 [2, 3], 4: Sub 2, 5: Sub 3], 6: Track 2
        let mut h = History::default();
        let (op, f) = plan_create(&d, &[6], "Lead", None).unwrap();
        h.apply(&mut d, op).unwrap();
        let anchor = d.track(0).map(|t| t.id());
        let (op, e) = plan_create(&d, &[], "Empty", anchor).unwrap();
        h.apply(&mut d, op).unwrap();
        let rows = visible_rows(&d, &HashSet::new(), &d.track_ordinals());
        let shape: Vec<(bool, usize, u32)> =
            rows.iter().map(|r| (r.header, r.index, r.folder)).collect();
        assert_eq!(
            shape,
            vec![
                (true, 0, e),
                (false, 0, 0),
                (false, 1, 0),
                (false, 2, 0),
                (false, 3, 0),
                (false, 4, 0),
                (false, 5, 0),
                (true, 6, f),
                (false, 6, f),
            ]
        );
        assert_eq!(rows[7].ordinal, 1, "one track in the folder");
        assert_eq!(rows[0].ordinal, 0, "the empty folder");
        // Collapsing the folder hides its tracks, not its header.
        d.project_mut().folders_collapsed.insert(f);
        let rows = visible_rows(&d, &HashSet::new(), &d.track_ordinals());
        assert_eq!(rows.len(), 8);
        assert!(rows[7].header && rows[7].collapsed);
        // Subtracks of a folder member are in the folder too.
        let (op, g) = plan_create(&d, &[0], "Big", None).unwrap();
        h.apply(&mut d, op).unwrap();
        let rows = visible_rows(&d, &HashSet::new(), &d.track_ordinals());
        assert!(
            rows.iter()
                .filter(|r| !r.header && r.index <= 5)
                .all(|r| r.folder == g)
        );
    }

    #[test]
    fn a_thousand_subtracks_build_rows_quickly() {
        let mut d = PlumDocument::new_project("t".into(), Bpm::DEFAULT, 960);
        let mut h = History::default();
        h.apply(
            &mut d,
            Operation::AddSubtracks {
                parent: 0,
                count: 1000,
            },
        )
        .unwrap();
        let t = std::time::Instant::now();
        let rows = visible_rows(&d, &HashSet::new(), &d.track_ordinals());
        assert_eq!(rows.len(), 1001);
        assert!(t.elapsed() < std::time::Duration::from_millis(50));
    }

    /// §225 through the app's cache: tracks with 1,000 / 5,000 / 10,000 notes
    /// in view; Dense First keeps C in front and Sparse First A, over hundreds
    /// of frames of small pans (visible counts change every frame).
    #[test]
    fn onion_order_cache_keeps_the_front_layer_while_panning() {
        use plumspirit_engine::model::project::OnionOrder;
        use plumspirit_engine::model::{NoteAttrs, NoteOffKind, NoteRecord, SubchannelId};
        let mut d = PlumDocument::new_project("o".into(), Bpm::DEFAULT, 960);
        d.add_track().unwrap();
        d.add_track().unwrap();
        d.add_track().unwrap();
        for (i, n) in [(1usize, 1_000u64), (2, 5_000), (3, 10_000)] {
            let step = 400_000 / n;
            let v: Vec<NoteRecord> = (0..n)
                .map(|k| NoteRecord {
                    start_tick: k * step,
                    duration_ticks: step.max(2) / 2,
                    attrs: NoteAttrs::new((k % 90) as u8, 100, 0, 64, NoteOffKind::NoteOff)
                        .unwrap(),
                    sub: SubchannelId(0),
                    group: 0,
                })
                .collect();
            d.track_mut(i)
                .unwrap()
                .notes
                .insert_sorted_batch(&v, false)
                .unwrap();
        }
        let sources = [1usize, 2, 3];
        for (order, front) in [
            (OnionOrder::DenseFirst, 3usize),
            (OnionOrder::SparseFirst, 1),
        ] {
            let cfg = OnionConfig {
                order,
                ..OnionConfig::default()
            };
            let mut cache = OnionOrderCache::default();
            let mut shown: Option<Vec<usize>> = None;
            for frame in 0..400u64 {
                let t0 = 10_000 + (frame % 9) * 997 + frame * 13;
                let o = cache.order(&d, &sources, &cfg, (t0, t0 + 300_000), None);
                assert_eq!(*o.last().unwrap(), front, "{order:?} frame {frame}");
                if let Some(p) = &shown {
                    assert_eq!(p, &o, "{order:?} frame {frame}");
                }
                shown = Some(o);
            }
        }
    }

    /// B1-0151: a moving view keeps its Onion order (each new order is a new
    /// Onion picture to rebuild); once the view rests for [`VIEW_REST`] the
    /// order follows what is in view, and `rerank_due` tells the app when to
    /// repaint for it.
    #[test]
    fn a_moving_view_holds_the_onion_order_until_it_rests() {
        use plumspirit_engine::model::project::OnionOrder;
        use plumspirit_engine::model::{NoteAttrs, NoteOffKind, NoteRecord};
        let mut d = PlumDocument::new_project("o".into(), Bpm::DEFAULT, 960);
        for _ in 0..3 {
            d.add_track().unwrap();
        }
        // Track 1 dense over 0 – 100,000, track 3 over 200,000 – 300,000.
        for (i, from) in [(1usize, 0u64), (3, 200_000)] {
            let v: Vec<NoteRecord> = (0..5_000u64)
                .map(|k| {
                    NoteRecord::new(
                        from + k * 20,
                        10,
                        NoteAttrs::new((k % 90) as u8, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
                    )
                })
                .collect();
            d.track_mut(i)
                .unwrap()
                .notes
                .insert_sorted_batch(&v, false)
                .unwrap();
        }
        let sources = [1usize, 2, 3];
        let cfg = OnionConfig {
            order: OnionOrder::DenseFirst,
            density: DensityBasis::VisibleRange,
            ..OnionConfig::default()
        };
        let mut cache = OnionOrderCache::default();
        let front = |o: Vec<usize>| *o.last().unwrap();
        assert_eq!(
            front(cache.order(&d, &sources, &cfg, (0, 100_000), None)),
            1
        );
        assert_eq!(cache.rerank_due(), None);
        // Panned to track 3's notes: held while moving.
        for t0 in [150_000u64, 180_000, 200_000] {
            let o = cache.order(&d, &sources, &cfg, (t0, t0 + 100_000), None);
            assert_eq!(front(o), 1, "held at {t0}");
            assert!(cache.rerank_due().is_some());
        }
        std::thread::sleep(VIEW_REST + std::time::Duration::from_millis(30));
        let o = cache.order(&d, &sources, &cfg, (200_000, 300_000), None);
        assert_eq!(front(o), 3, "re-ranked at rest");
        assert_eq!(cache.rerank_due(), None);
    }

    /// B1-0095: the panel counts follow a clip moved to a subtrack, its undo
    /// and a note edit inside the moved content — the cache never shows the
    /// stale own-content count.
    #[test]
    fn note_counts_follow_clips_between_tracks_and_refresh_on_every_change() {
        use plumspirit_engine::editor::clips::move_clips;
        use plumspirit_engine::model::{ClipId, NoteAttrs, NoteOffKind, NoteRecord};
        let mut d = tree();
        let mut h = History::default();
        let note = |t: u64| {
            NoteRecord::new(
                t,
                60,
                NoteAttrs::new(60, 90, 0, 0, NoteOffKind::NoteOff).unwrap(),
            )
        };
        let recs: Vec<NoteRecord> = (0..50u64).map(|k| note(k * 120)).collect();
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&recs, false)
            .unwrap();
        let mut cache = NoteCountCache::default();
        assert_eq!(cache.counts(&d, 1)[..2], [50, 0]);
        let t0 = d.track(0).unwrap().id();
        let op = move_clips(&d, &[(ClipId::Primary(t0), 1, 0)]).unwrap();
        h.apply(&mut d, op).unwrap();
        assert_eq!(cache.counts(&d, 1)[..2], [0, 50]);
        d.track_mut(0)
            .unwrap()
            .notes
            .insert_sorted_batch(&[note(99_999)], false)
            .unwrap();
        assert_eq!(cache.counts(&d, 1)[..2], [0, 51]);
        h.undo(&mut d).unwrap();
        assert_eq!(cache.counts(&d, 1)[..2], [51, 0]);
    }
}
