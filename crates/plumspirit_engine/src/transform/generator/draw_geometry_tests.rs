use super::*;

fn reference(mut p: [P2; 4], t: f64) -> P2 {
    for n in (1..4).rev() {
        for i in 0..n {
            p[i] = (
                (1.0 - t) * p[i].0 + t * p[i + 1].0,
                (1.0 - t) * p[i].1 + t * p[i + 1].1,
            );
        }
    }
    p[0]
}
fn reference_distance(a: P2, b: P2, p: P2) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let along = (p.0 - a.0) * dx + (p.1 - a.1) * dy;
    if along <= 0.0 {
        return (p.0 - a.0).hypot(p.1 - a.1);
    }
    if along >= dx * dx + dy * dy {
        return (p.0 - b.0).hypot(p.1 - b.1);
    }
    ((p.0 - a.0) * dy - (p.1 - a.1) * dx).abs() / dx.hypot(dy)
}
fn flatten(p: [P2; 4], tolerance: f64) -> Vec<Pt> {
    flatten_cubic(p, tolerance, 4096, 24, || Ok::<_, ()>(())).unwrap()
}
#[test]
fn adaptive_control_hulls_bound_inflections_loops_backtracking_and_degeneracy() {
    for p in [
        [(0., 0.), (1., 2.), (2., -2.), (3., 0.)], // midpoint lies on chord
        [(0., 0.), (2., 3.), (-2., 3.), (0., 0.)], // coincident endpoints
        [(0., 0.), (10., 0.), (-10., 0.), (1., 0.)], // collinear overshoot
        [(-7., 20.), (30., -13.), (-40., 60.), (12., -4.)],
        [(3., -4.); 4],
    ] {
        let out = flatten(p, 0.001);
        assert_eq!((out[0].x, out[0].y, out[0].s), (p[0].0, p[0].1, 0.));
        let last = out.last().unwrap();
        assert_eq!((last.x, last.y, last.s), (p[3].0, p[3].1, 1.));
        for w in out.windows(2) {
            assert!(w[0].s < w[1].s);
            for j in 0..=64 {
                let t = w[0].s + (w[1].s - w[0].s) * j as f64 / 64.;
                let d = reference_distance((w[0].x, w[0].y), (w[1].x, w[1].y), reference(p, t));
                assert!(d <= 0.001 + 1e-12, "error={d} t={t}");
            }
        }
        assert_eq!(out, flatten(p, 0.001));
    }
    assert!(flatten([(0., 0.), (1., 2.), (2., -2.), (3., 0.)], 0.001).len() > 2);
}
#[test]
fn adaptive_caps_precision_validation_and_cancel_never_return_coarse_success() {
    let p = [(0., 0.), (1., 2.), (2., -2.), (3., 0.)];
    let call = |tol, points, depth| flatten_cubic(p, tol, points, depth, || Ok::<_, ()>(()));
    assert_eq!(call(0.001, 4096, 0), Err(FlattenError::Depth));
    assert_eq!(call(0.001, 2, 24), Err(FlattenError::Points));
    assert_eq!(call(1e-20, 4096, 24), Err(FlattenError::Precision));
    for tol in [0., -1., f64::NAN, f64::INFINITY, 1e13] {
        assert_eq!(call(tol, 4096, 24), Err(FlattenError::Invalid));
    }
    assert_eq!(call(0.1, 4097, 24), Err(FlattenError::Invalid));
    assert_eq!(call(0.1, 4096, 25), Err(FlattenError::Invalid));
    assert_eq!(
        flatten_cubic([(f64::NAN, 0.); 4], 0.1, 4096, 24, || Ok::<_, ()>(())),
        Err(FlattenError::Invalid)
    );
    let mut checks = 0;
    let result = flatten_cubic(p, 0.000001, 4096, 24, || {
        checks += 1;
        if checks == 3 { Err("canceled") } else { Ok(()) }
    });
    assert_eq!(result, Err(FlattenError::Stopped("canceled")));
    assert_eq!(checks, 3);
}
#[test]
fn translated_tiny_and_large_coordinates_have_explicit_precision_and_exact_endpoints() {
    for scale in [1e-100, 1., 1e10] {
        let p = [
            (scale, scale),
            (2. * scale, 3. * scale),
            (-scale, 2. * scale),
            (scale, -scale),
        ];
        let out = flatten(p, scale * 0.001);
        assert_eq!(out[0].x, scale);
        assert_eq!(out.last().unwrap().y, -scale);
        for w in out.windows(2) {
            for i in 0..=32 {
                let t = w[0].s + (w[1].s - w[0].s) * i as f64 / 32.;
                assert!(
                    reference_distance((w[0].x, w[0].y), (w[1].x, w[1].y), reference(p, t))
                        <= scale * 0.0011
                );
            }
        }
    }
    let p = [(1e12, 1e12); 4];
    assert_eq!(
        flatten_cubic(p, 0.001, 4096, 24, || Ok::<_, ()>(())),
        Err(FlattenError::Precision)
    );
    let p = [
        (1e10 - 7., 1e10 + 20.),
        (1e10 + 30., 1e10 - 13.),
        (1e10 - 40., 1e10 + 60.),
        (1e10 + 12., 1e10 - 4.),
    ];
    let out = flatten(p, 0.01);
    for w in out.windows(2) {
        for i in 0..=32 {
            let t = w[0].s + (w[1].s - w[0].s) * i as f64 / 32.;
            assert!(
                reference_distance((w[0].x, w[0].y), (w[1].x, w[1].y), reference(p, t)) <= 0.01
            );
        }
    }
}
#[test]
fn seeded_cubic_reference_sweep_respects_tolerance_and_order() {
    let mut seed = 0x6a09e667f3bcc909u64;
    for _ in 0..64 {
        let mut p = [(0., 0.); 4];
        for point in &mut p {
            let mut next = || {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                ((seed >> 32) as i32) as f64 / 2147483648. * 100.
            };
            *point = (next(), next());
        }
        let out = flatten(p, 0.002);
        for w in out.windows(2) {
            assert!(w[0].s < w[1].s);
            for i in 0..=32 {
                let t = w[0].s + (w[1].s - w[0].s) * i as f64 / 32.;
                assert!(
                    reference_distance((w[0].x, w[0].y), (w[1].x, w[1].y), reference(p, t))
                        <= 0.002 + 1e-11
                );
            }
        }
    }
}
#[test]
fn finite_segment_projection_handles_endpoints_zero_length_and_tiny_lengths() {
    assert_eq!(
        nearest_segment((0., 0.), (4., 0.), (2., 3.)),
        ((2., 0.), 0.5, 3.)
    );
    assert_eq!(
        nearest_segment((0., 0.), (4., 0.), (-1., 0.)),
        ((0., 0.), 0., 1.)
    );
    assert_eq!(
        nearest_segment((2., 2.), (2., 2.), (5., 6.)),
        ((2., 2.), 0., 5.)
    );
    let (q, t, d) = nearest_segment((0., 0.), (1e-200, 0.), (0.5e-200, 1e-200));
    assert_eq!(q, (0.5e-200, 0.));
    assert_eq!(t, 0.5);
    assert_eq!(d, 1e-200);
}
