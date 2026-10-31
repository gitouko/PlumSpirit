//! v0.8 Wave K visual stability (TEST_MATRIX T8-K-*): the stable Onion layer
//! order (contract §116 – §118, test §225) and the dense tiles' layer
//! priority (§116 / §122).

use plumspirit_engine::model::NoteView;
use plumspirit_engine::model::project::{OnionOrder, OnionRanking};
use plumspirit_engine::model::{
    NoteAttrs, NoteOffKind, NoteRecord, NoteStore, SubchannelId, TrackId,
};
use plumspirit_engine::view::tiles::{self, Outline, Scratch, TileColors, TileOptions};

fn rec(start: u64, dur: u64, pitch: u8) -> NoteRecord {
    NoteRecord {
        start_tick: start,
        duration_ticks: dur,
        attrs: NoteAttrs::new(pitch, 100, 0, 64, NoteOffKind::NoteOff).unwrap(),
        sub: SubchannelId(0),
        group: 0,
    }
}

/// `n` notes spread evenly over ticks 0 … 1,000,000.
fn spread(n: u64) -> NoteStore {
    let step = 1_000_000 / n;
    let v: Vec<NoteRecord> = (0..n)
        .map(|k| rec(k * step, step.max(2) / 2, (k % 100) as u8))
        .collect();
    NoteStore::from_sorted_records(&v, false).unwrap()
}

/// §225: A = 1,000, B = 5,000, C = 10,000 visible notes. Dense First keeps C in
/// front, Sparse First keeps A in front, over hundreds of frames while the view
/// pans a little (the visible counts change by a few notes every frame).
#[test]
fn dense_order_test_a_b_c_never_flickers_while_panning() {
    // Over the whole song: A 1,000, B 5,000, C 10,000; the view shows half of
    // it, so the visible counts are about 500 / 2,500 / 5,000 and change by a
    // few notes per pan step.
    let stores = [spread(1_000), spread(5_000), spread(10_000)];
    let ids = [TrackId(4), TrackId(2), TrackId(9)];
    let sources = [0usize, 1, 2];
    for (order, front) in [
        (OnionOrder::DenseFirst, 2usize),
        (OnionOrder::SparseFirst, 0),
    ] {
        let mut r = OnionRanking::default();
        let mut shown = None;
        for frame in 0..600u64 {
            // Small pans back and forth (±3 % of the view) plus a slow drift.
            let wobble = [0u64, 7_000, 15_000, 7_000][(frame % 4) as usize];
            let t0 = 100_000 + frame * 50 + wobble;
            let t1 = t0 + 500_000;
            let counts: Vec<usize> = stores.iter().map(|s| s.count_starting_in(t0, t1)).collect();
            let o = r.order(order, &sources, &ids, &counts);
            assert_eq!(
                *o.last().unwrap(),
                front,
                "{order:?} frame {frame}: {o:?} {counts:?}"
            );
            if let Some(prev) = &shown {
                assert_eq!(prev, &o, "{order:?} order changed at frame {frame}");
            }
            shown = Some(o);
        }
    }
    // Whole-track density gives the same fronts.
    let counts: Vec<usize> = stores.iter().map(NoteStore::len).collect();
    assert_eq!(counts, vec![1_000, 5_000, 10_000]);
    let mut r = OnionRanking::default();
    assert_eq!(
        r.order(OnionOrder::DenseFirst, &sources, &ids, &counts),
        vec![0, 1, 2]
    );
    assert_eq!(
        r.order(OnionOrder::SparseFirst, &sources, &ids, &counts),
        vec![2, 1, 0]
    );
    assert_eq!(
        r.order(OnionOrder::Project, &sources, &ids, &counts),
        vec![0, 1, 2]
    );
}

/// §117: two sources whose densities differ by a handful of notes, the larger
/// one alternating every frame (a view boundary crossing notes), keep one order;
/// a real change reorders at once and then stays.
#[test]
fn near_equal_densities_do_not_alternate_and_real_changes_reorder() {
    let ids = [TrackId(1), TrackId(2)];
    let sources = [10usize, 11];
    for order in [OnionOrder::DenseFirst, OnionOrder::SparseFirst] {
        let mut r = OnionRanking::default();
        let first = r.order(order, &sources, &ids, &[1_000, 1_003]);
        for frame in 0..400usize {
            let counts = if frame % 2 == 0 {
                [1_004, 1_000]
            } else {
                [1_000, 1_005]
            };
            assert_eq!(
                r.order(order, &sources, &ids, &counts),
                first,
                "{order:?} {frame}"
            );
        }
        // Track 2 becomes clearly denser (+50 %): it moves at once …
        let o = r.order(order, &sources, &ids, &[1_000, 1_500]);
        let dense_front = *o.last().unwrap() == 11;
        assert_eq!(dense_front, order == OnionOrder::DenseFirst, "{order:?}");
        // … and stays while the counts wobble again.
        for frame in 0..100usize {
            let c = [1_000 + frame % 5, 1_500 - frame % 7];
            assert_eq!(r.order(order, &sources, &ids, &c), o);
        }
    }
    // Equal densities: the lower TrackId in front, whatever the input order.
    let mut r = OnionRanking::default();
    assert_eq!(
        r.order(
            OnionOrder::DenseFirst,
            &[5, 6, 7],
            &[TrackId(30), TrackId(10), TrackId(20)],
            &[9, 9, 9]
        ),
        vec![5, 7, 6]
    );
    let mut r = OnionRanking::default();
    assert_eq!(
        r.order(
            OnionOrder::DenseFirst,
            &[7, 5, 6],
            &[TrackId(20), TrackId(30), TrackId(10)],
            &[9, 9, 9]
        ),
        vec![5, 7, 6]
    );
}

/// Many sources (an "All Other Tracks" Onion of a big project): ranking stays
/// a deterministic total order and is fast.
#[test]
fn many_sources_rank_deterministically() {
    let n = 2_000usize;
    let sources: Vec<usize> = (0..n).collect();
    let ids: Vec<TrackId> = (0..n as u32).map(|i| TrackId(n as u32 - i)).collect();
    let counts: Vec<usize> = (0..n).map(|i| (i * 7919) % 5_000).collect();
    let mut a = OnionRanking::default();
    let started = std::time::Instant::now();
    let oa = a.order(OnionOrder::DenseFirst, &sources, &ids, &counts);
    assert!(started.elapsed().as_millis() < 200);
    let mut sorted = oa.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, sources, "a permutation");
    for w in oa.windows(2) {
        assert!(counts[w[0]] <= counts[w[1]], "dense last");
    }
    // Reversed input → the same drawing order of the same tracks.
    let rs: Vec<usize> = sources.iter().rev().copied().collect();
    let ri: Vec<TrackId> = ids.iter().rev().copied().collect();
    let rc: Vec<usize> = counts.iter().rev().copied().collect();
    let mut b = OnionRanking::default();
    assert_eq!(b.order(OnionOrder::DenseFirst, &rs, &ri, &rc), oa);
}

/// Onion layers in dense tiles: the front source owns a shared texel outright
/// (its full colour — v0.9 §030: no blend can defeat the layer order) even
/// when a back source has many more notes there; same input → same pixels.
#[test]
fn dense_tiles_put_the_front_layer_first() {
    struct Layers;
    impl TileColors for Layers {
        fn color(&self, source: usize, _n: &NoteView) -> ([u8; 4], bool) {
            ([if source == 0 { 0 } else { 240 }, 0, 0, 255], false)
        }
        fn rank(&self, source: usize) -> u16 {
            source as u16
        }
    }
    struct OneLayer;
    impl TileColors for OneLayer {
        fn color(&self, source: usize, _n: &NoteView) -> ([u8; 4], bool) {
            ([if source == 0 { 0 } else { 240 }, 0, 0, 255], false)
        }
    }
    // Back source: 30 notes on one texel; front source: 1 note.
    let back =
        NoteStore::from_sorted_records(&(0..30).map(|_| rec(0, 1, 60)).collect::<Vec<_>>(), false)
            .unwrap();
    let front = NoteStore::from_sorted_records(&[rec(0, 1, 60)], false).unwrap();
    let mut sc = Scratch::default();
    let opt = TileOptions::default();
    let t = tiles::build_tile(&[&back, &front], &Layers, 0, 0, opt, &mut sc, &|| false).unwrap();
    let red = t.pixels[(tiles::ROWS - 1 - 60) * tiles::TILE_COLS][0];
    assert_eq!(red, 240, "the front layer owns the texel");
    let t2 = tiles::build_tile(&[&back, &front], &Layers, 0, 0, opt, &mut sc, &|| false).unwrap();
    assert_eq!(t.hash(), t2.hash());
    // Without ranks the sources are drawn in their order, like the exact
    // renderer: the note count never decides (v0.8 blended by count).
    let t = tiles::build_tile(&[&back, &front], &OneLayer, 0, 0, opt, &mut sc, &|| false).unwrap();
    let red = t.pixels[(tiles::ROWS - 1 - 60) * tiles::TILE_COLS][0];
    assert_eq!(red, 240, "the one note drawn last, not the 30 below it");
}

/// §121 / §122: dense edge texels take the outline colour (white at 0 %,
/// black at 100 %, the note colour at 50 %), interiors keep their colour;
/// outlines are deterministic.
#[test]
fn dense_outlines_follow_the_darkness() {
    struct C;
    impl TileColors for C {
        fn color(&self, _s: usize, _n: &NoteView) -> ([u8; 4], bool) {
            ([100, 160, 220, 200], false)
        }
    }
    // A block 20 columns × 5 pitches: its middle is interior.
    let v: Vec<NoteRecord> = (60..65).map(|p| rec(10, 20, p)).collect();
    let mut v = v;
    v.sort_by_key(|r| r.start_tick);
    let s = NoteStore::from_sorted_records(&v, false).unwrap();
    let px = |t: &tiles::TileOut, col: usize, pitch: u8| {
        t.pixels[(tiles::ROWS - 1 - usize::from(pitch)) * tiles::TILE_COLS + col]
    };
    let mut sc = Scratch::default();
    let build = |d: u8, sc: &mut Scratch| {
        let opt = TileOptions {
            outline: Some(Outline::new(230, d)),
            ..TileOptions::default()
        };
        tiles::build_tile(&[&s], &C, 0, 0, opt, sc, &|| false).unwrap()
    };
    let black = build(100, &mut sc);
    let white = build(0, &mut sc);
    let same = build(50, &mut sc);
    // Edges: black / white, opaque at least 230.
    assert_eq!(px(&black, 10, 62)[..3], [0, 0, 0]);
    assert_eq!(px(&white, 10, 62)[..3], [255, 255, 255]);
    assert_eq!(px(&same, 10, 62)[..3], [100, 160, 220]);
    assert!(px(&black, 10, 62)[3] >= 230);
    // Interior untouched.
    assert_eq!(px(&black, 20, 62)[..3], [100, 160, 220]);
    assert!(px(&black, 20, 62)[3] < 230);
    // Top and bottom rows of the block are edges too.
    assert_eq!(px(&black, 20, 64)[..3], [0, 0, 0]);
    assert_eq!(build(100, &mut sc).hash(), black.hash());
}
