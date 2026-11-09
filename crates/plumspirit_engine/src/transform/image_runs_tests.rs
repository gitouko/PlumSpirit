use super::*;
use crate::transform::image_midi as im;
use std::cell::Cell;

fn reference(image: &Rgba, s: &Settings) -> Vec<Run> {
    let map = |p: [u8; 4]| {
        if p[3] < s.alpha_min || s.palette.is_empty() {
            return u32::MAX;
        }
        let mut best = (u32::MAX, 0);
        for (i, c) in s.palette.iter().enumerate() {
            let d = (0..3)
                .map(|k| (i32::from(p[k]) - i32::from(c[k])).pow(2) as u32)
                .sum();
            if d < best.0 {
                best = (d, i);
            }
        }
        if s.ignored.get(best.1).copied().unwrap_or(false) {
            u32::MAX
        } else {
            best.1 as u32
        }
    };
    let (lines, len) = match s.orientation {
        Orientation::PianoRoll => (image.h, image.w),
        Orientation::Upstream => (image.w, image.h),
    };
    let mut out = Vec::new();
    for line in 0..lines {
        let cells = (0..len)
            .map(|t| match s.orientation {
                Orientation::PianoRoll => map(image.at(t, line)),
                Orientation::Upstream => map(image.at(line, image.h - 1 - t)),
            })
            .collect::<Vec<_>>();
        let mut start = 0;
        while start < len {
            let color = cells[start as usize];
            if color == u32::MAX {
                start += 1;
                continue;
            }
            let end = (start + 1..len)
                .find(|t| cells[*t as usize] != color)
                .unwrap_or(len);
            let mut first = start;
            while first < end {
                let stop = if s.pixel_notes {
                    first + 1
                } else if let Some(block) = first.checked_div(s.max_len) {
                    match s.split {
                        SplitFrom::ImageStart => ((block + 1) * s.max_len).min(end),
                        SplitFrom::NoteStart => first.saturating_add(s.max_len).min(end),
                    }
                } else {
                    end
                };
                out.push(Run {
                    line,
                    start: first,
                    length: stop - first,
                    color,
                });
                first = stop;
            }
            start = end;
        }
    }
    out
}
#[test]
fn indexing_preflights_exact_bytes_preserves_ties_and_ignored_nearest_and_cancels_inside() {
    let image = Rgba::new(3, 1, vec![[100, 0, 0, 255], [100, 0, 0, 0], [0, 0, 0, 255]]).unwrap();
    let s = Settings {
        palette: vec![[0, 0, 0], [200, 0, 0]],
        ignored: vec![true, false],
        alpha_min: 1,
        ..Default::default()
    };
    let bytes = Cell::new(0);
    assert_eq!(
        index_controlled(
            &image,
            &s,
            |n| {
                bytes.set(n);
                Ok(())
            },
            || Ok(())
        )
        .unwrap(),
        vec![u32::MAX; 3]
    );
    assert_eq!(bytes.get(), 12);
    let before = Cell::new(false);
    assert_eq!(
        index_controlled(
            &image,
            &s,
            |_| {
                before.set(true);
                Err("preflight refusal".into())
            },
            || Ok(())
        )
        .unwrap_err(),
        "preflight refusal"
    );
    assert!(before.get());
    let large = Rgba::new(128, 128, vec![[255, 0, 0, 255]; 16384]).unwrap();
    let checks = Cell::new(0);
    assert_eq!(
        index_controlled(
            &large,
            &s,
            |_| Ok(()),
            || {
                checks.set(checks.get() + 1);
                if checks.get() == 4 {
                    Err("cancel inside indexing".into())
                } else {
                    Ok(())
                }
            }
        )
        .unwrap_err(),
        "cancel inside indexing"
    );
    assert_eq!(checks.get(), 4);
}
#[test]
fn cursor_matches_independent_run_partition_and_native_note_color_sort_for_seeded_images() {
    use crate::transform::paste::{ColorInfo, ColorNote, Generated};
    let mut random = 0x13579bdu64;
    for seed in 0..96u64 {
        let (w, h) = (1 + (seed % 31) as u32, 1 + (seed % 13) as u32);
        let mut pixels = Vec::new();
        for _ in 0..w * h {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            pixels.push([
                random as u8,
                (random >> 8) as u8,
                (random >> 16) as u8,
                if random.is_multiple_of(7) { 0 } else { 255 },
            ]);
        }
        let image = Rgba::new(w, h, pixels).unwrap();
        for orientation in [Orientation::PianoRoll, Orientation::Upstream] {
            for pixel_notes in [false, true] {
                for split in [SplitFrom::ImageStart, SplitFrom::NoteStart] {
                    let lines = if orientation == Orientation::PianoRoll {
                        h
                    } else {
                        w
                    };
                    let settings = Settings {
                        keys: if seed % 2 == 0 {
                            crate::model::project::KeyRange::Standard
                        } else {
                            crate::model::project::KeyRange::Extended256
                        },
                        first_key: if seed % 2 == 0 { 10 } else { 180 },
                        last_key: (if seed % 2 == 0 { 10 } else { 180 }) + lines as u8 - 1,
                        orientation,
                        alpha_min: if seed % 3 == 0 { 0 } else { 128 },
                        palette: vec![
                            [0, 0, 0],
                            [255, 0, 0],
                            [0, 255, 0],
                            [0, 0, 255],
                            [255, 255, 255],
                        ],
                        ignored: vec![seed % 3 == 0, false, false, false, seed % 5 == 0],
                        max_len: if seed % 4 == 0 {
                            0
                        } else {
                            1 + (seed % 7) as u32
                        },
                        split,
                        pixel_notes,
                        ticks_per_pixel: if seed % 5 == 0 { 0 } else { 7 },
                        velocity: if seed % 3 == 0 { 0 } else { 211 },
                        random_colors: Some(seed),
                        ..Default::default()
                    };
                    let expected = reference(&image, &settings);
                    let idx = index_controlled(&image, &settings, |_| Ok(()), || Ok(())).unwrap();
                    for page in [1, 7, MAX_BATCH] {
                        let mut cursor = Cursor::new(&idx, w, h, &settings).unwrap();
                        let mut actual = Vec::new();
                        while cursor
                            .visit_controlled(
                                &idx,
                                page,
                                |r| {
                                    actual.push(r);
                                    Ok(())
                                },
                                || Ok(()),
                            )
                            .unwrap()
                            != 0
                        {}
                        assert_eq!(
                            actual, expected,
                            "seed{seed} {orientation:?} {split:?} pixel{pixel_notes} page{page}"
                        );
                        assert_eq!(cursor.position(), (lines, 0));
                    }
                    let mut legacy = Generated {
                        notes: expected
                            .iter()
                            .map(|run| ColorNote {
                                start: u64::from(run.start) * settings.ticks_per_pixel.max(1),
                                dur: u64::from(run.length) * settings.ticks_per_pixel.max(1),
                                pitch: if orientation == Orientation::PianoRoll {
                                    settings.last_key - run.line as u8
                                } else {
                                    settings.first_key + run.line as u8
                                },
                                vel: settings.velocity.clamp(1, 127),
                                color: run.color,
                            })
                            .collect(),
                        colors: settings
                            .palette
                            .iter()
                            .enumerate()
                            .map(|(i, c)| ColorInfo {
                                rgb: im::display_color(&settings, i),
                                name: format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]),
                            })
                            .collect(),
                    };
                    legacy.sort();
                    assert_eq!(im::convert(&image, &settings, 10000).unwrap(), legacy);
                    if !legacy.notes.is_empty() {
                        assert!(
                            im::convert(&image, &settings, legacy.notes.len() - 1)
                                .unwrap_err()
                                .contains("more than")
                        );
                    }
                }
            }
        }
    }
}
#[test]
fn transparent_uniform_and_output_failure_cancel_with_atomic_cursor_and_bounded_work() {
    for value in [u32::MAX, 0] {
        let idx = vec![value; 16384];
        let mut cursor = Cursor::new(&idx, 128, 128, &Settings::default()).unwrap();
        let original = cursor.position();
        let checks = Cell::new(0);
        let mut output = Vec::new();
        assert_eq!(
            cursor
                .visit_controlled(
                    &idx,
                    MAX_BATCH,
                    |r| {
                        output.push(r);
                        Ok(())
                    },
                    || {
                        checks.set(checks.get() + 1);
                        if checks.get() == 4 {
                            Err("canceled scan".into())
                        } else {
                            Ok(())
                        }
                    }
                )
                .unwrap_err(),
            "canceled scan"
        );
        assert_eq!(cursor.position(), original);
        assert_eq!(cursor.inspected(), 0);
        output.clear();
        let count = cursor
            .visit_controlled(
                &idx,
                MAX_BATCH,
                |r| {
                    output.push(r);
                    Ok(())
                },
                || Ok(()),
            )
            .unwrap();
        assert_eq!(count, if value == u32::MAX { 0 } else { 128 });
        assert!(cursor.inspected() <= 2 * idx.len() as u64);
    }
    let idx = vec![0; 12];
    let settings = Settings {
        pixel_notes: true,
        ..Default::default()
    };
    let mut cursor = Cursor::new(&idx, 12, 1, &settings).unwrap();
    let mut emitted = 0;
    assert_eq!(
        cursor
            .visit_controlled(
                &idx,
                12,
                |_| {
                    emitted += 1;
                    if emitted == 5 {
                        Err("output refused".into())
                    } else {
                        Ok(())
                    }
                },
                || Ok(())
            )
            .unwrap_err(),
        "output refused"
    );
    assert_eq!(cursor.position(), (0, 0));
    let mut output = Vec::new();
    assert_eq!(
        cursor
            .visit_controlled(
                &idx,
                12,
                |r| {
                    output.push(r);
                    Ok(())
                },
                || Ok(())
            )
            .unwrap(),
        12
    );
    assert_eq!(output[0].start, 0);
}
#[test]
fn invalid_data_and_batches_refuse_before_callbacks_and_processed_time_axis_remains_supported() {
    let bad = Rgba {
        w: 5,
        h: 2,
        px: vec![[0; 4]; 9],
    };
    let before = Cell::new(false);
    assert!(
        index_controlled(
            &bad,
            &Settings::default(),
            |_| {
                before.set(true);
                Ok(())
            },
            || Ok(())
        )
        .is_err()
    );
    assert!(!before.get());
    assert!(Cursor::new(&[], 0, 1, &Settings::default()).is_err());
    assert!(Cursor::new(&[0], 1, 2, &Settings::default()).is_err());
    let idx = vec![0; 32768];
    let mut c = Cursor::new(&idx, 32768, 1, &Settings::default()).unwrap();
    for size in [0, MAX_BATCH + 1] {
        assert!(
            c.visit_controlled(&idx, size, |_| panic!("emit"), || panic!("check"))
                .is_err()
        );
    }
    let mut out = Vec::new();
    assert_eq!(
        c.visit_controlled(
            &idx,
            1,
            |r| {
                out.push(r);
                Ok(())
            },
            || Ok(())
        )
        .unwrap(),
        1
    );
    assert_eq!(out[0].length, 32768);
    assert!(
        c.visit_controlled(&[], 1, |_| panic!("emit"), || panic!("check"))
            .is_err()
    );
    let image = Rgba {
        w: 32768,
        h: 1,
        px: vec![[1, 2, 3, 255]; 32768],
    };
    let settings = Settings {
        palette: vec![[1, 2, 3]],
        ..Default::default()
    };
    assert_eq!(
        index_controlled(
            &image,
            &settings,
            |n| {
                assert_eq!(n, 131072);
                Ok(())
            },
            || Ok(())
        )
        .unwrap()
        .len(),
        32768
    );
    let settings = Settings {
        palette: vec![[0; 3]; MAX_PALETTE + 1],
        ..Default::default()
    };
    assert!(
        index_controlled(
            &image,
            &settings,
            |_| panic!("oversized palette preflight"),
            || Ok(())
        )
        .is_err()
    );
}
