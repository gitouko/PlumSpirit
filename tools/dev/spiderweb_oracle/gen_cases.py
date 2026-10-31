"""Writes crates/plumspirit_engine/tests/fixtures/spiderweb_cases.json: hand-made shapes plus seeded random ones
(deterministic). Development-only (see oracle.py)."""
import json
import os
import random

R = random.Random(20261003)
cases = []


def case(name, shapes, **kw):
    c = {"name": name, "shapes": shapes}
    c.update(kw)
    cases.append(c)


# ---------------------------------------------------------------- lines and friends
case("line_up", [{"kind": "line", "pts": [[0, 60], [2, 64]]}])
case("line_down_end_dot", [{"kind": "line", "pts": [[0, 72], [1.5, 60]], "end_dot": True}])
case("line_flat", [{"kind": "line", "pts": [[0, 60], [4, 60.3]]}])
case("line_vertical", [{"kind": "line", "pts": [[1, 50], [1, 70]]}])
case("line_right_to_left", [{"kind": "line", "pts": [[3, 40], [0.25, 45]]}])
case("poly_zigzag", [{"kind": "poly", "pts": [[0, 60], [1, 66], [2, 58], [3, 70], [3, 50]]}])
case("poly_end_dot", [{"kind": "poly", "pts": [[0, 60], [1, 66], [2, 58], [3, 70]], "end_dot": True}])
case("poly_square", [{"kind": "poly", "pts": [[0, 60], [0, 64], [2, 64], [2, 60], [0, 60]]}])
case("free_scribble", [{"kind": "free", "pts": [[i * 0.05, 60 + 8 * ((i * 7) % 13) / 13.0] for i in range(60)]}])
case("curve_s", [{"kind": "curve", "pts": [[0, 50], [1, 50], [1, 70], [2, 70]]}])
case("curve_two_segments", [{"kind": "curve", "pts": [[0, 50], [1, 60], [2, 40], [3, 55], [4, 70], [5, 30], [6, 60]]}])
case("arc_up", [{"kind": "arc", "pts": [[0, 60], [1, 66], [2, 60]]}])
case("arc_k", [{"kind": "arc", "pts": [[0, 60], [4, 66], [8, 60]], "k": 4}])
case("arc_full_circle", [{"kind": "arc", "pts": [[0, 60], [2, 60], [0, 60]], "k": 0.25}])
case("arc_bends_back", [{"kind": "arc", "pts": [[0, 60], [-1, 70], [0.5, 80]], "end_dot": True}])
case("arc_straight", [{"kind": "arc", "pts": [[0, 60], [1, 61], [2, 62]]}])
case("vel_ramp", [{"kind": "line", "pts": [[0, 40], [4, 80]], "vel0": 10, "vel1": 120}])
case("vel_env", [{"kind": "line", "pts": [[0, 40], [4, 80]],
                  "vel_env": [[0, 20], [0.5, 100], [0.5, 30], [1, 127]]}])
case("vel_half", [{"kind": "line", "pts": [[0, 40], [4, 44]], "vel0": 64.5, "vel1": 64.5}])
case("keys_256", [{"kind": "line", "pts": [[0, 120], [2, 140]]}], keys=256)
case("keys_128_cut", [{"kind": "line", "pts": [[0, 120], [2, 140]]}])
case("negative_time", [{"kind": "line", "pts": [[-1, 60], [1, 64]]}])
case("ppq_96", [{"kind": "curve", "pts": [[0, 50], [1, 60], [2, 40], [3, 55]]}], ppq=96)
case("ppq_32767", [{"kind": "line", "pts": [[0, 0], [1, 127]]}], ppq=32767)
case("bad_kind", [{"kind": "spiral", "pts": [[0, 0]]}, {"kind": "line", "pts": [[0, 60], [1, 62]]}])
case("two_overlap_single", [{"kind": "line", "pts": [[0, 60], [2, 64]]},
                             {"kind": "line", "pts": [[1, 60], [3, 64]], "vel0": 90, "vel1": 90}], mode="single")
case("two_overlap_raw", [{"kind": "line", "pts": [[0, 60], [2, 64]]},
                          {"kind": "line", "pts": [[1, 60], [3, 64]]}], mode="raw")
case("multi_key", [{"kind": "line", "pts": [[0, 60], [2, 64]]}, {"kind": "line", "pts": [[1, 60], [3, 64]]},
                   {"kind": "line", "pts": [[5, 60], [6, 64]]}, {"kind": "line", "pts": [[0, 80], [2, 84]]}],
     mode="auto")
case("multi_time", [{"kind": "line", "pts": [[0, 60], [2, 64]]}, {"kind": "line", "pts": [[1, 70], [3, 74]]},
                    {"kind": "line", "pts": [[5, 60], [6, 64]]}], mode="auto", split="time")

# ---------------------------------------------------------------- seeded random line kinds


def rp():
    return [round(R.uniform(-0.5, 8), 3), round(R.uniform(20, 100), 3)]


for i in range(160):
    kind = R.choice(["line", "poly", "free", "curve", "arc"])
    n = {"line": 2, "arc": 3, "curve": 3 * R.randint(1, 4) + 1}.get(kind, R.randint(2, 12))
    sh = {"kind": kind, "pts": [rp() for _ in range(n)]}
    if R.random() < 0.3:
        sh["end_dot"] = True
    if R.random() < 0.3:
        sh["vel0"], sh["vel1"] = R.randint(1, 127), R.randint(1, 127)
    if kind == "arc" and R.random() < 0.5:
        sh["k"] = R.choice([0.25, 0.5, 2, 3.7])
    if kind == "free" and R.random() < 0.3:
        sh["pts"] = [[round(j * 0.03, 4), round(60 + 10 * R.random(), 3)] for j in range(R.randint(3, 80))]
    shapes = [sh]
    if R.random() < 0.4:
        shapes.append({"kind": "line", "pts": [rp(), rp()]})
    case(f"rand_line_{i}", shapes, mode=R.choice(["raw", "single", "auto"]), split=R.choice(["key", "time"]),
         ppq=R.choice([96, 480, 960, 1920]))

# ---------------------------------------------------------------- claw, strum, colours
for i in range(120):
    shapes = []
    for _ in range(R.randint(1, 3)):
        kind = R.choice(["line", "poly", "curve", "arc"])
        n = {"line": 2, "arc": 3, "curve": 4}.get(kind, R.randint(2, 6))
        sh = {"kind": kind, "pts": [rp() for _ in range(n)], "vel0": R.randint(1, 127), "vel1": R.randint(1, 127)}
        r = R.random()
        if r < 0.35:
            mode = R.choice(["time", "notes", "keys", "chords", "random"])
            sh["claw"] = {"mode": mode, "period": R.choice([1, 2, 4, 8, 16, 3]),
                          "trash": R.choice([[1, 4], [2, 4], [3, 3], [2, 2], None, [5, 4]]),
                          "dist": R.choice([0, 37.5, -80, 100, -100]), "stretch": R.random() < 0.5,
                          "short": R.random() < 0.5, "keep": R.randint(0, 4), "skip": R.randint(0, 3),
                          "pct": R.choice([10, 50, 99.5, 100]), "seed": R.randint(0, 99),
                          "shorten": R.random() < 0.5, "cut": R.choice([1, 25, 60, 99])}
        elif r < 0.7:
            sh["strum"] = {"time": R.choice([0, 0.25, -0.5, 1 / 3]), "tension": R.choice([0, 50, -100, 100]),
                           "vel": R.choice([0, 10, -30, 127]), "vel_tension": R.choice([0, 25, -60]),
                           "end_time": R.choice([0, 0.125, -0.25]), "end_tension": R.choice([0, 40, -40]),
                           "start": R.random() < 0.8, "preserve": R.random() < 0.3, "ahead": R.random() < 0.3,
                           "end": R.random() < 0.7, "chop": R.random() < 0.4, "alternate": R.random() < 0.4}
        if R.random() < 0.3:
            by = R.choice(["step", "key", "time"])
            sh["cycle"] = {"by": by, "n": R.randint(1, 17),
                           "every": [R.randint(1, 3), R.choice([1, 4, 8, 16])] if by == "time" else R.randint(1, 4)}
        shapes.append(sh)
    case(f"rand_after_{i}", shapes, mode=R.choice(["raw", "single", "auto"]), split=R.choice(["key", "time"]),
         ppq=R.choice([96, 480, 960]))

# ---------------------------------------------------------------- freehand made perfect
import math as _m
for i in range(60):
    kind = R.choice(["wobble", "loop", "square", "tri", "zig"])
    n = R.randint(20, 160)
    if kind == "wobble":
        pts = [[j * 0.05, 60 + 6 * _m.sin(j * 0.2) + R.uniform(-0.3, 0.3)] for j in range(n)]
    elif kind == "loop":
        a, b = R.uniform(0.5, 3), R.uniform(2, 12)
        pts = [[2 + a * _m.cos(2 * _m.pi * j / n) + R.uniform(-0.05, 0.05),
                60 + b * _m.sin(2 * _m.pi * j / n) + R.uniform(-0.2, 0.2)] for j in range(n + 1)]
    elif kind == "square":
        w, h = R.uniform(0.5, 3), R.uniform(3, 15)
        side = [(0, 0), (w, 0), (w, h), (0, h), (0, 0)]
        pts = []
        for (x0, y0), (x1, y1) in zip(side, side[1:]):
            for j in range(n // 4):
                t = j / (n // 4)
                pts.append([x0 + (x1 - x0) * t + R.uniform(-0.02, 0.02), 60 + y0 + (y1 - y0) * t + R.uniform(-0.1, 0.1)])
        pts.append(pts[0])
    elif kind == "tri":
        side = [(0, 0), (2, 10), (4, 0), (0, 0)]
        pts = []
        for (x0, y0), (x1, y1) in zip(side, side[1:]):
            for j in range(n // 3):
                t = j / (n // 3)
                pts.append([x0 + (x1 - x0) * t + R.uniform(-0.03, 0.03), 50 + y0 + (y1 - y0) * t + R.uniform(-0.1, 0.1)])
        pts.append(pts[0])
    else:
        pts = [[j * 0.1, 60 + (8 if (j // 10) % 2 else 0) + R.uniform(-0.2, 0.2)] for j in range(n)]
    sh = {"kind": "free", "pts": [[round(x, 4), round(y, 4)] for x, y in pts],
          "smooth": R.choice([0, 5, 20, 45, 70, 100, "60", 33.5]), "k": R.choice([1, 0.5, 0.25, 2])}
    case(f"smooth_{kind}_{i}", [sh], ppq=R.choice([480, 960]))

# ---------------------------------------------------------------- tumours
for i in range(120):
    kind = R.choice(["line", "poly", "curve", "arc", "free"])
    n = {"line": 2, "arc": 3, "curve": 4}.get(kind, R.randint(3, 8))
    pts = [rp() for _ in range(n)]
    if kind == "arc" and R.random() < 0.2:
        pts = [[0, 60], [2, 60], [0, 60]]
    tm = {"shape": R.choice(["triangle", "square", "circle", "parabola", "blob"]),
          "size": R.choice([3, -2, 0.5, 7, 0]), "length": R.choice([0.125, 0.25, 0, 0.5, 0.01]),
          "dist": R.choice([0.125, 0.25, 0.4, 1, 0.07]), "side": R.choice(["alt", "left", "right", "random"]),
          "wrap": R.choice(["simple", "wrap"]), "start": R.choice([0, 0.1, 0.3]), "end": R.choice([1, 0.9, 0.6]),
          "ease": R.choice([0, 0, 0.3, 1]), "rot": R.choice([0, 0, 30, -90, 180]),
          "slant": R.choice([0, 0.5, -0.5, 1]), "fit": R.random() < 0.4, "seed": R.randint(0, 50),
          "mirror": R.random() < 0.3, "k": R.choice([0.25, 0.5, 1, 0.1])}
    if R.random() < 0.3:
        tm["on"] = R.choice([False, True, 0])
    if R.random() < 0.35:
        key = R.choice(["size", "length", "dist", "rot", "slant"])
        tm["graphs"] = {key: [[0, R.uniform(0.2, 2)], [R.uniform(0.2, 0.8), R.uniform(-1, 3)], [1, R.uniform(0.1, 2)]]}
    case(f"tumour_{i}", [{"kind": kind, "pts": pts, "tumour": tm, "end_dot": R.random() < 0.3}],
         ppq=R.choice([96, 480, 960]))

# ---------------------------------------------------------------- formulas along paths (pattern.py) and joined curves
PAT = [("wave", "height * sin(x * 2 * pi)", {"height": 4.0}),
       ("zigzag", "height * 2 / pi * asin(sin(x * 2 * pi))", {"height": 4.0}),
       ("bounce", "height * abs(sin(x * pi))", {"height": 4.0}),
       ("square", "height * max(-1, min(1, steep * sin(x * 2 * pi)))", {"height": 4.0, "steep": 8.0}),
       ("saw", "height * 2 * (x - floor(x + 0.5))", {"height": 4.0}),
       ("teeth", "height * (1 - abs(2 * x - 1))", {"height": 4.0}),
       ("spikes", "height * (1 - abs(2 * x - 1))^sharp", {"height": 4.0, "sharp": 3.0}),
       ("bumps", "height * sqrt(max(0, 1 - (2 * x - 1)^2))", {"height": 4.0}),
       ("stairs", "height * min(steps, floor((1 - abs(2 * x - 1)) * (steps + 1))) / steps",
        {"height": 4.0, "steps": 3.0}),
       ("swell", "height * sin(x * pi) * sin(x * waves * 2 * pi)", {"height": 4.0, "waves": 4.0}),
       ("fade", "height * (1 - x) * sin(x * waves * 2 * pi)", {"height": 4.0, "waves": 4.0}),
       ("curls", "height * (1 - cos(x * 2 * pi)) / 2", {"height": 4.0, "curl": 2.0}),
       ("", "x^2 * 6 - 3 * x", {}), ("", "height * sin(x * 2 * pi) + t", {"height": 2}),
       ("", "1 / (x - 0.5)", {}), ("", "sqrt(x - 0.5)", {}), ("", "log(x) / 4", {}), ("", "sin(", {}),
       ("", "foo(x)", {}), ("", "round(x * 4) - 2 * x", {}), ("", "pow(-1, floor(x * 4)) * 3", {}),
       ("", "abs((-1) ** x) * 2", {}), ("", "x // 0.25 - (x % 0.25) * 8", {})]
SHP = [("circle", "cos(t * 2 * pi)", "sin(t * 2 * pi)", {}),
       ("spiral", "t * cos(t * turns * 2 * pi)", "t * sin(t * turns * 2 * pi)", {"turns": 3.0}),
       ("flower", "(1 + depth * cos(petals * t * 2 * pi)) * cos(t * 2 * pi)",
        "(1 + depth * cos(petals * t * 2 * pi)) * sin(t * 2 * pi)", {"petals": 5.0, "depth": 0.3}),
       ("heart", "16 * sin(t * 2 * pi)^3",
        "13 * cos(t * 2 * pi) - 5 * cos(4 * pi * t) - 2 * cos(6 * pi * t) - cos(8 * pi * t)", {}),
       ("figure8", "sin(t * 2 * pi)", "sin(t * 2 * pi) * cos(t * 2 * pi)", {}),
       ("slow", "t", "t^bend", {"bend": 2.0}), ("fast", "t", "1 - (1 - t)^bend", {"bend": 2.0}),
       ("scurve", "t", "t^steep / (t^steep + (1 - t)^steep)", {"steep": 2.0}),
       ("reverse_s", "t^steep / (t^steep + (1 - t)^steep)", "t", {"steep": 2.0}),
       ("quarter", "sin(t * pi / 2)", "1 - cos(t * pi / 2)", {}),
       ("exponential", "t", "(exp(steep * t) - 1) / (exp(steep) - 1)", {"steep": 5.0}),
       ("logarithmic", "t", "ln(1 + steep * t) / ln(1 + steep)", {"steep": 20.0}),
       ("", "0", "0", {}), ("", "t", "1 / (t - 0.5)", {}), ("", "t * 3", "x * t", {"x": 2}),
       ("", "cos(t * 4 * pi)", "sin(t * 2 * pi)", {}), ("", "t^2", "t^3 - t", {})]
HAND = [{"pts": [[0, 0], [0.3, 3], [0.6, -3], [1, 0]]},
        {"pts": [[0, 0], [0.2, 2], [0.3, 2], [0.5, 0], [0.7, -2], [0.8, -2], [1, 0]], "sharp": [1], "sym": "turn"},
        {"pts": [[0, 0], [0.1, 1], [0.2, 1]]}]


def rpat():
    pid, text, vs = R.choice(PAT)
    p = {"preset": pid, "formula": text, "vars": dict(vs), "loops": R.choice([1, 2, 4, 6.5, 0.5, 12, 30, 3]),
         "each": R.random() < 0.4, "k": R.choice([1, 0.5, 0.25, 0.125, 2]), "mirror": R.random() < 0.3,
         "scale": R.choice([1, 1, 0.5, 2, -1, 1.5])}
    if pid == "curls":
        p["along"] = "curl * sin(x * 2 * pi) / (2 * pi)"
        if R.random() < 0.5:
            p["vars"]["curl"] = R.choice([0.5, 1, 3])
    if R.random() < 0.3:
        p["sym"] = R.choice(["mirror", "turn", "flip"])
    if R.random() < 0.3 and p["vars"]:
        p["vars"][R.choice(list(p["vars"]))] = R.choice([1, 2.5, 6, -3, "2"])
    if R.random() < 0.15:
        p["loop"] = dict(R.choice(HAND))
    return p


def rshape():
    sid, x, y, vs = R.choice(SHP)
    p = {"preset": sid, "x": x, "y": y, "vars": dict(vs), "k": R.choice([1, 0.5, 0.25, 2]),
         "mirror": R.random() < 0.3}
    if R.random() < 0.3:
        p["sym"] = R.choice(["mirror", "turn"])
    if R.random() < 0.3 and p["vars"]:
        p["vars"][R.choice(list(p["vars"]))] = R.choice([1, 2.5, 6, 0.5])
    if R.random() < 0.1:
        p["loop"] = dict(R.choice(HAND))
    return p


def rtm():
    return {"shape": R.choice(["triangle", "square", "circle", "parabola", "blob"]),
            "size": R.choice([3, -2, 0.5, 7]), "length": R.choice([0.125, 0.25, 0.5]),
            "dist": R.choice([0.125, 0.25, 0.4, 1]), "side": R.choice(["alt", "left", "right", "random"]),
            "wrap": R.choice(["simple", "wrap"]), "seed": R.randint(0, 50), "k": R.choice([0.25, 0.5, 1]),
            "on": R.random() < 0.85}


def rline_kind():
    kind = R.choice(["line", "poly", "curve", "arc"])
    n = {"line": 2, "arc": 3}.get(kind, R.choice([4, 7, 10]) if kind == "curve" else R.randint(3, 6))
    return kind, [rp() for _ in range(n)]


for i in range(160):
    kind, pts = rline_kind()
    sh = {"kind": kind, "pts": pts, "end_dot": R.random() < 0.3}
    r = R.random()
    if r < 0.45:
        sh["pattern"] = rpat()
    elif r < 0.75:
        sh["shape"] = rshape()
    else:
        sh["pattern"], sh["shape"] = rpat(), rshape()
    if R.random() < 0.15:
        sh["tumour"] = rtm()
    if kind == "curve" and R.random() < 0.3:
        sh["sym"] = R.choice(["mirror", "turn"])
    if R.random() < 0.2:
        sh["vel0"], sh["vel1"] = R.randint(1, 127), R.randint(1, 127)
    case(f"formula_{kind}_{i}", [sh], ppq=R.choice([96, 480, 960]), mode=R.choice(["single", "auto"]))

# odd files: values Python reads (or refuses) in its own way
odd = [
    {"preset": "wave", "formula": "height * sin(x * 2 * pi)", "vars": [["height", "3"]], "loops": "4", "k": 0.5},
    {"formula": "height * sin(x * 2 * pi)", "loops": 0},
    {"formula": "height * sin(x * 2 * pi)", "k": -1},
    {"formula": "height * sin(x * 2 * pi)", "scale": "inf"},
    {"formula": "height * sin(x * 2 * pi)", "vars": None},
    {"formula": "height * sin(x * 2 * pi)", "vars": {"height": "x"}},
    {"formula": "height * sin(x * 2 * pi)", "vars": {"height": True}, "name": "My wave", "each": 1},
    {"formula": "", "loop": {"pts": [[0, 0], [0.3, 3], [0.6, -3], [1, 0]], "sharp": [0, 1, 5]}},
    {"formula": "sin(", "loop": {"pts": [[0, 0], [0.3, 3], [0.6, -3], [1, Infinity]] if False else [[0, 0], [0.3, 3], [0.6, -3], [1, 0]]}},
    {"formula": "sin(", "loop": {"pts": [[0, 0], [0.3, 3], [0.6, -3]]}},
    {"formula": 5, "loops": 2},
    {"formula": True, "loops": 2, "scale": 3},
    {"formula": "x", "along": "  ", "loops": 1},
    {"formula": "x", "along": "x * 3", "loops": 1, "sym": "mirror"},
    "not a dict",
]
for j, p in enumerate(odd):
    case(f"odd_pattern_{j}", [{"kind": "line", "pts": [[0, 60], [4, 64]], "pattern": p}])
case("odd_shape_bad_k", [{"kind": "line", "pts": [[0, 60], [4, 64]], "shape": {"x": "t", "y": "t^2", "k": 0}}])
case("odd_shape_dot", [{"kind": "poly", "pts": [[0, 60], [2, 64], [4, 60]], "shape": {"x": "0", "y": "0"}}])
case("odd_shape_names", [{"kind": "arc", "pts": [[0, 60], [2, 66], [4, 60]],
                          "shape": {"x": "t * a", "y": "sin(t * pi) * b", "vars": {"b": 3}}}])
case("formula_on_free_ignored", [{"kind": "free", "pts": [[0, 60], [1, 62], [2, 61]],
                                  "pattern": {"formula": "4 * sin(x * 2 * pi)", "loops": 3}}])
case("pattern_many_loops", [{"kind": "line", "pts": [[0, 40], [16, 80]],
                             "pattern": {"formula": "2 * sin(x * 2 * pi)", "loops": 5000, "k": 0.25}}])
case("pattern_huge_loops", [{"kind": "line", "pts": [[0, 40], [16, 80]],
                             "pattern": {"formula": "2 * sin(x * 2 * pi)", "loops": 1e9, "k": 0.25}}])
case("pattern_tiny_loops", [{"kind": "poly", "pts": [[0, 40], [4, 80], [8, 40]],
                             "pattern": {"formula": "2 * sin(x * 2 * pi)", "loops": 1e-6}}])
case("pattern_degenerate", [{"kind": "poly", "pts": [[1, 40], [1, 40], [1, 40]],
                             "pattern": {"formula": "2 * sin(x * 2 * pi)", "loops": 3}}])

# joined curves: pieces (gaps), sections with their own tumours (splits), formulas across pieces
for i in range(50):
    segs = R.randint(2, 5)
    pts = [rp()]
    for _ in range(segs):
        pts += [rp(), rp(), rp()]
    last = segs
    sh = {"kind": "curve", "pts": pts}
    gaps = sorted(R.sample(range(0, last + 1), R.randint(0, 2)))
    if R.random() < 0.2:
        gaps.append(R.choice(["x", 1.7, -1]))
    sh["gaps"] = gaps
    splits = sorted(R.sample(range(0, last + 1), R.randint(0, 2)))
    if splits:
        sh["splits"] = splits
    r = R.random()
    if r < 0.5:
        # one tumour setting per section (the count must fit or they're dropped)
        good = [g for g in sorted({int(g) for g in gaps if not isinstance(g, str)})]
        sh["tumours"] = [rtm() if R.random() < 0.7 else None for _ in range(R.randint(1, 5))]
    elif r < 0.7:
        sh["tumour"] = rtm()
    if R.random() < 0.35:
        sh["pattern"] = rpat()
    if R.random() < 0.15:
        sh["shape"] = rshape()
    if R.random() < 0.2:
        sh["sym"] = "mirror"
    case(f"joined_{i}", [sh], ppq=R.choice([96, 480, 960]), mode=R.choice(["single", "auto", "raw"]))
# exact fits (sections = splits + gaps + 1)
case("joined_sections_fit", [{"kind": "curve", "pts": [[0, 50], [1, 60], [2, 40], [3, 55], [4, 70], [5, 30], [6, 60],
                                                        [7, 50], [8, 52], [9, 58], [10, 60], [11, 50], [12, 55]],
                              "gaps": [2], "splits": [1], "tumours": [
                                  {"shape": "triangle", "size": 3, "length": 0.25, "dist": 0.5, "on": True},
                                  None, {"shape": "circle", "size": -2, "length": 0.125, "dist": 0.25, "on": True}]}])
case("joined_gaps_only", [{"kind": "curve", "pts": [[0, 50], [1, 60], [2, 40], [3, 55], [4, 70], [5, 30], [6, 60],
                                                     [7, 50], [8, 52], [9, 58], [10, 60]], "gaps": [1, 2],
                           "pattern": {"formula": "3 * sin(x * 2 * pi)", "loops": 5, "each": False}}])

# ---------------------------------------------------------------- custom shapes (custom.py, areas.py, shrink.py, polygon.py)
SQUARE = {"kind": "poly", "pts": [[0, 0], [1, 0], [1, 1], [0, 1], [0, 0]]}
CIRCLE = {"kind": "ellipse", "box": [0, 0, 1, 1]}
UPRIGHT = [[0, 60], [4, 60], [0, 72]]


def cust(strokes, pts=UPRIGHT, **kw):
    sh = {"kind": "custom", "pts": pts, "strokes": strokes}
    sh.update(kw)
    return sh


case("custom_square_outline", [cust([SQUARE])])
case("custom_circle_fill", [cust([CIRCLE], fill="fill")])
for e in ("round", "keep", "drop", "min", "stretch"):
    for a in ("auto", "aligned", "centred"):
        case(f"custom_circle_spam_{e}_{a}", [cust([CIRCLE], pts=[[0.3, 50], [2.3, 50], [0.3, 70]], fill="spam",
                                                  gate=R.choice([0.0625, 0.1, 0.03, 0.25]), ends=e, align=a)],
             ppq=R.choice([96, 480, 960]))
case("custom_circle_outline_spam", [cust([CIRCLE], fill="outline_spam", gate=0.125, ends="keep")])
case("custom_triangle_lines", [cust([{"kind": "poly", "pts": [[0, 0], [1, 0]]}, {"kind": "poly", "pts": [[1, 0], [0.5, 1]]},
                                     {"kind": "poly", "pts": [[0.5, 1], [0, 0]]}], fill="fill")])
case("custom_u_curve_fill", [cust([{"kind": "curve", "pts": [[0, 1], [0, -0.3], [1, -0.3], [1, 1]]}], fill="fill")])
case("custom_u_curve_spam", [cust([{"kind": "curve", "pts": [[0, 1], [0, -0.3], [1, -0.3], [1, 1]]}], fill="spam",
                                  gate=0.05)])
case("custom_near_gap", [cust([{"kind": "poly", "pts": [[0, 0], [1, 0], [1, 1], [0.004, 1], [0.001, 0.05]]}], fill="fill")])
case("custom_flat_line_fill", [cust([{"kind": "poly", "pts": [[0, 0.5], [1, 0.52]]}, CIRCLE], fill="fill")])
case("custom_flat_line_spam", [cust([{"kind": "poly", "pts": [[0, 0.5], [1, 0.5]]}], fill="spam", gate=0.1)])
two = [{"kind": "ellipse", "box": [0, 0, 0.6, 1]}, {"kind": "ellipse", "box": [0.4, 0, 1, 1]}]
case("custom_overlap_evenodd", [cust(two, fill="fill")])
case("custom_overlap_union", [cust(two, fill="fill", union=True)])
case("custom_overlap_union_spam", [cust(two, fill="spam", union=True, gate=0.1)], ppq=480)
for fill in ("fill", "spam"):
    case(f"custom_apart_{fill}", [cust([CIRCLE], fill=fill, apart=True, gate=0.0625)], mode="auto", ppq=480)
    case(f"custom_apart_band_{fill}", [cust([CIRCLE], fill=fill, apart=True, edge=0.25, gate=0.0625)], mode="auto",
         ppq=480)
    case(f"custom_apart_sideways_{fill}", [cust(two, fill=fill, apart=True, edge=0.25, edge_mode="sideways",
                                               gate=0.0625)], mode="auto", ppq=480)
case("custom_empty_band", [cust([CIRCLE, {"kind": "poly", "pts": [[0.2, 0.2], [0.8, 0.8]]}], edge=0.3)], ppq=480)
case("custom_empty_sideways", [cust([CIRCLE], edge=0.3, edge_mode="sideways")], ppq=480)
case("custom_outline_spam_band", [cust([SQUARE, CIRCLE], fill="outline_spam", edge=0.2, gate=0.0625)], ppq=480)
case("custom_roles", [cust([SQUARE, {"kind": "poly", "pts": [[0, 0.5], [1, 0.5]], "role": "cut"},
                            {"kind": "poly", "pts": [[0.2, 0.2], [0.8, 0.9]], "role": "edge", "colour": 3}],
                           fill="fill")], mode="auto")
case("custom_roles_spam", [cust([SQUARE, {"kind": "poly", "pts": [[0, 0.5], [1, 0.5]], "role": "cut"},
                                 {"kind": "poly", "pts": [[0.2, 0.2], [0.8, 0.9]], "role": "edge", "colour": 3}],
                                fill="spam", gate=0.1, apart=True)], mode="auto", ppq=480)
case("custom_stroke_colours", [cust([dict(SQUARE, colour=2), dict(CIRCLE, colour=5), {"kind": "poly", "pts": [[0, 0],
                                                                                                           [1, 1]]}])],
     mode="auto")
case("custom_src_groups", [cust([dict(SQUARE, src=0), dict(CIRCLE, src=1), dict(CIRCLE, src=1, colour=4)],
                                edge=0.2)], mode="auto", ppq=480)
# areas coloured by hand
quad = [SQUARE, {"kind": "poly", "pts": [[0, 0.5], [1, 0.5]], "role": "cut"},
        {"kind": "poly", "pts": [[0.5, 0], [0.5, 1]], "role": "cut"}]
case("custom_areas_fill", [cust(quad, fill="fill", areas=[[0.25, 0.25, 2], [0.75, 0.75, 0], [0.75, 0.25, 7]])],
     mode="auto")
case("custom_areas_spam", [cust(quad, fill="spam", gate=0.1, areas=[[0.25, 0.25, 2], [0.75, 0.75, 0], [0.7, 0.2, 7]])],
     mode="auto", ppq=480)
case("custom_areas_apart_borders", [cust(quad, fill="fill", apart=True, borders=True,
                                         areas=[[0.25, 0.25, 2], [0.75, 0.75, 3], [0.75, 0.25, 7]])], mode="auto")
case("custom_areas_spam_apart_borders", [cust(quad, fill="spam", gate=0.1, apart=True, borders=True, edge=0.15,
                                              areas=[[0.25, 0.25, 2], [0.75, 0.75, 3], [0.75, 0.25, 7]])],
     mode="auto", ppq=480)
case("custom_areas_band", [cust(quad, fill="fill", apart=True, edge=0.2, areas=[[0.25, 0.25, 2], [0.75, 0.75, 0]])],
     mode="auto", ppq=480)
case("custom_areas_slanted", [cust([CIRCLE, {"kind": "poly", "pts": [[0, 0.1], [1, 0.8]], "role": "cut"},
                                    {"kind": "curve", "pts": [[0.2, 0], [0.4, 0.6], [0.6, 0.3], [0.9, 1]], "role": "cut"}],
                                   fill="fill", areas=[[0.2, 0.7, 4], [0.8, 0.2, 9], [0.5, 0.5, 0]])], mode="auto")
case("custom_areas_union", [cust(two + [{"kind": "poly", "pts": [[0.5, 0], [0.5, 1]], "role": "cut"}], fill="spam",
                                 union=True, gate=0.0625, areas=[[0.2, 0.5, 6], [0.8, 0.5, 11]])], mode="auto", ppq=480)
# frames turned, flipped, sheared
for i, pts in enumerate([[[0, 60], [3, 64], [-1, 70]], [[4, 60], [0, 60], [4, 72]], [[0, 72], [4, 72], [0, 60]],
                         [[1, 50], [2, 70], [3, 50]], [[0, 60], [0, 60.5], [5, 60]]]):
    case(f"custom_frame_{i}", [cust([CIRCLE, {"kind": "poly", "pts": [[0, 0], [1, 1]]}], pts=pts, fill="spam",
                                    gate=0.0625)], ppq=480)
    case(f"custom_frame_fill_{i}", [cust(quad, pts=pts, fill="fill", areas=[[0.25, 0.25, 3]])], mode="auto")
# polygons and stars (polygon.py)
for i, pg in enumerate([{"points": 5}, {"points": 6, "style": "star", "inner": 40}, {"points": 7, "style": "cross", "skip": 3},
                        {"points": 6, "style": "cross", "skip": 2}, {"points": 4, "style": "polygon",
                                                                     "pattern": {"formula": "0.1 * sin(x * 2 * pi)", "loops": 2}},
                        {"points": 3, "shape": {"preset": "arch", "x": "x", "y": "0.2 * sin(x * pi)"}}]):
    case(f"custom_polygon_{i}", [cust([SQUARE], polygon=pg, fill=R.choice(["empty", "fill", "spam"]), gate=0.0625)],
         ppq=480)
# formulas and other stroke kinds inside custom shapes
case("custom_formulas", [cust([{"kind": "curve", "pts": [[0, 0], [0.3, 1], [0.7, 1], [1, 0]],
                                "pattern": {"formula": "0.05 * sin(x * 2 * pi)", "loops": 6}},
                               {"kind": "arc", "pts": [[0, 0], [0.5, 0.4], [1, 0]], "k": 0.5},
                               {"kind": "poly", "pts": [[0, 0], [1, 0]], "pattern": {"formula": "0.1 * x", "loops": 3}},
                               {"kind": "poly", "free": True, "smooth": 0.5, "pts": [[0, 0.5], [0.3, 0.6], [0.6, 0.4],
                                                                                    [1, 0.5]]}], fill="fill")])
# "Colours" on custom shapes
for by, every in (("step", 2), ("key", 3), ("time", [1, 8])):
    for fill in ("empty", "spam", "fill"):
        case(f"custom_cycle_{by}_{fill}", [cust([CIRCLE], fill=fill, gate=0.1, align="aligned",
                                                cycle={"by": by, "n": 3, "every": every})], mode="auto", ppq=480)
case("custom_cycle_apart", [cust([CIRCLE], fill="spam", gate=0.1, apart=True, cycle={"by": "key", "n": 4, "every": 1})],
     mode="auto", ppq=480)


# pasted notes (pack_notes): own velocities / tracks, the box moved, stretched and turned
def packed(rows):
    import base64, zlib, struct
    raw = b"".join(struct.pack("<5i", *r) for r in rows)
    return "t:" + base64.b64encode(zlib.compress(raw, 1)).decode("ascii")


rows = [[0, 240, 0, 100, 0], [240, 480, 2, 90, 1], [480, 960, 5, 80, 0], [0, 960, 3, 70, 2], [100, 140, 1, 60, 1]]
for i, pts in enumerate([[[1, 59.5], [2, 59.5], [1, 65.5]], [[1, 59.5], [3, 59.5], [1, 62.5]],
                         [[2, 59.5], [1, 59.5], [2, 65.5]], [[0, 60], [0.5, 66], [-1, 61]]]):
    case(f"custom_pasted_{i}", [cust([SQUARE], pts=pts, notes=packed(rows), own_vel=(i % 2 == 0))], mode="auto")
case("custom_pasted_old_format", [cust([SQUARE], notes=__import__("base64").b64encode(__import__("zlib").compress(
    b"".join(__import__("struct").pack("<4i", *r[:4]) for r in rows))).decode())])
case("custom_pasted_bad", [cust([SQUARE], notes="t:abc"), cust([SQUARE], notes="t:" + "A" * 8), cust([SQUARE], notes=5)])
# odd files
case("custom_odd_strokes", [cust([{"kind": "curve", "pts": [[0, 0], [1, 1]], "src": 3, "role": "edge", "colour": 4},
                                  {"kind": "ellipse", "box": [0, 0, 1]}, {"kind": "arc", "pts": [[0, 0], [1, 1]]},
                                  {"kind": "line", "pts": [[0, 0], [1, 1]], "colour": 99, "role": "nope"},
                                  {"kind": "poly", "pts": []}, 7, {"pts": [[0, 0], [0.5, 1], [1, 0], [0, 0]],
                                                                  "src": True, "colour": 2}], fill="fill",
                                 gate="0.1", edge="0.2", ends="weird", align=3, union=1, apart="yes")])
case("custom_bad", [cust([]), cust([SQUARE], pts=[[0, 0], [1, 0]]), cust([7])])


def rstroke():
    r = R.random()
    if r < 0.2:
        a, b = sorted([R.random(), R.random()])
        c, d = sorted([R.random(), R.random()])
        st = {"kind": "ellipse", "box": [a, c, b + 0.05, d + 0.05]}
    elif r < 0.45:
        n = R.choice([4, 7, 10])
        st = {"kind": "curve", "pts": [[R.random(), R.random()] for _ in range(n)]}
        if R.random() < 0.3:
            st["pts"][-1] = list(st["pts"][0])
    elif r < 0.55:
        st = {"kind": "arc", "pts": [[R.random(), R.random()] for _ in range(3)], "k": R.choice([1, 0.5, 2])}
    elif r < 0.65:
        st = {"kind": "poly", "free": True, "smooth": R.choice([0, 0.3, 1]),
              "pts": [[i / 9, R.random()] for i in range(10)]}
    else:
        n = R.randint(2, 6)
        st = {"kind": "poly", "pts": [[R.random(), R.random()] for _ in range(n)]}
        if R.random() < 0.5 and n >= 3:
            st["pts"].append(list(st["pts"][0]))
    if R.random() < 0.15:
        st["role"] = R.choice(["edge", "cut"])
    if R.random() < 0.15:
        st["colour"] = R.randint(1, 15)
    if R.random() < 0.1:
        st["src"] = R.randint(0, 2)
    return st


for i in range(120):
    w, h = R.choice([0.5, 1, 2, 4]), R.choice([3, 8, 16, 30])
    b0, p0 = R.choice([0, 0.25, 1.5, -0.5]), R.randint(30, 80)
    if R.random() < 0.7:
        pts = [[b0, p0], [b0 + w, p0], [b0, p0 + h]]
    else:
        pts = [[b0 + R.uniform(-1, 1), p0 + R.uniform(-3, 3)] for _ in range(3)]
    sh = cust([rstroke() for _ in range(R.randint(1, 4))], pts=pts,
              fill=R.choice(["empty", "fill", "spam", "outline_spam"]),
              gate=R.choice([0.0625, 0.125, 0.03, 0.1, 0.25]), ends=R.choice(["round", "keep", "drop", "min", "stretch"]),
              align=R.choice(["auto", "aligned", "centred"]))
    for f in ("union", "apart", "borders"):
        if R.random() < 0.3:
            sh[f] = True
    if R.random() < 0.3:
        sh["edge"] = R.choice([0.0625, 0.2, 0.5])
        if R.random() < 0.3:
            sh["edge_mode"] = "sideways"
    if R.random() < 0.3:
        sh["areas"] = [[R.random(), R.random(), R.randint(0, 15)] for _ in range(R.randint(1, 4))]
    if R.random() < 0.15:
        sh["cycle"] = {"by": R.choice(["step", "key", "time"]), "n": R.randint(2, 5), "every": R.choice([1, 2])}
    if R.random() < 0.2:
        sh["vel0"], sh["vel1"] = R.randint(1, 127), R.randint(1, 127)
    case(f"custom_random_{i}", [sh], ppq=R.choice([96, 480]), mode=R.choice(["single", "raw", "auto"]))

# ---------------------------------------------------------------- Hz bass (hzbass.py)
HZBOX = [[0, 30], [2, 30], [0, 42]]
for i, hz in enumerate([{"key": 33, "bpm": 120}, {"key": 40, "cents": 25, "bpm": 140, "fixed": True},
                        {"key": 28, "bpm": 97.5, "auto": 3}, {"key": 45, "bpm": 180, "auto": 40},
                        {"key": 300, "bpm": 120}, {"key": 33, "bpm": 0}, {"key": 33, "bpm": 120, "cents": 2000}]):
    g = 120 / (60 * 440 * 2 ** ((hz["key"] - 69 + hz.get("cents", 0) / 100) / 12)) if hz["key"] < 256 and hz["bpm"] else 0.1
    case(f"hz_plain_{i}", [cust([CIRCLE], pts=HZBOX, fill="spam", gate=g, hz=hz)], ppq=R.choice([96, 480, 960]))


def tone(t, ln, key, **kw):
    d = {"t": t, "len": ln, "key": key}
    d.update(kw)
    return d


TONES = [tone(0, 0.5, 33, id=1, to=[{"id": 2, "out": 0.1, "in": 0.1}]), tone(0.5, 0.5, 36, id=2), tone(1.25, 0.5, 31, id=3,
                                                                                                   cents=20, auto=10),
         tone(0.25, 0.5, 40, id=4)]
case("hz_tones", [cust([SQUARE], pts=HZBOX, fill="spam", gate=0.01, hz={"key": 33, "bpm": 120, "tones": TONES})], ppq=480)
case("hz_tones_auto", [cust([SQUARE], pts=HZBOX, fill="spam", hz={"key": 33, "bpm": 130, "auto": 5, "tones": TONES})],
     ppq=960)
case("hz_tones_fixed", [cust([SQUARE], pts=HZBOX, fill="spam", hz={"key": 33, "bpm": 130, "fixed": True, "tones": TONES})],
     ppq=96)
case("hz_old_leads", [cust([SQUARE], pts=HZBOX, fill="spam", hz={"key": 33, "bpm": 120, "tones": [
    tone(0, 0.5, 33, out=0.1), tone(0.5, 0.5, 38, **{"in": 0.2}), tone(1, 0.5, 30, out=0.05), tone(1.6, 0.3, 35)]})], ppq=480)
case("hz_gap_slide", [cust([SQUARE], pts=HZBOX, fill="spam", hz={"key": 33, "bpm": 120, "tones": [
    tone(0, 0.5, 33, id=5, to=[{"id": 6, "out": 0.2, "in": 0.2}]), tone(0.9, 0.5, 45, id=6)]})], ppq=480)
FXS = {"slant": [[0, 0], [2, 1]], "groups": [[0, 0.2], [2, 1]], "offpitch": [[0, 0.5]], "noisy": [[0, 0.3]],
       "vibrato": [[0, 0.5], [1, 1, 0.5]], "pitch": [[0, 0.5], [1, 0.8, "hold"], [1.5, 0.3]], "volume": [[0, 1], [2, 0.4]],
       "sweep": [[0, 0], [2, 1]], "wah": [[0, 0.5]], "tremolo": [[0, 0.3]], "octave": [[0, 0.6]], "sine": [[0, 0.8]],
       "square": [[0, 0.5]], "saw": [[0, 1]], "triangle": [[0, 0.4]]}
for name, pts in FXS.items():
    case(f"hz_fx_{name}", [cust([SQUARE], pts=HZBOX, fill="spam", hz={"key": 33, "bpm": 120, "tones": TONES[:3],
                                                                       "fx": {name: pts}})], ppq=480)
case("hz_fx_loop_amount", [cust([SQUARE], pts=HZBOX, fill="spam", hz={
    "key": 33, "bpm": 120, "tones": TONES[:3], "fx": {"slant": [[0, 0, 0.5], [0.25, 1, -0.5], [0.5, 0]], "volume": [[0, 1], [0.5, 0.2]]},
    "loop": {"slant": 0.5, "volume": 0.25, "nope": 1}, "amount": {"slant": [[0, 0], [2, 1]], "groups": [[0, 1]]}})], ppq=480)
case("hz_fx_off", [cust([SQUARE], pts=HZBOX, fill="spam", hz={
    "key": 33, "bpm": 120, "tones": TONES[:3], "fx": {"slant": [[0, 1]], "wah": [[0, 0.7]], "noisy": [[0, 0.2]]},
    "off": ["wah", "noisy", "zzz"]})], ppq=480)
case("hz_fx_all", [cust([CIRCLE], pts=HZBOX, fill="spam", hz={"key": 33, "bpm": 120, "tones": TONES, "fx": dict(FXS)})],
     ppq=480)
case("hz_old_fx", [cust([SQUARE], pts=HZBOX, fill="spam", hz={"key": 33, "bpm": 120, "tones": [
    tone(0, 0.5, 33, id=1, fx={"slant": [[0, 0], [1, 1]]}), tone(0.6, 0.5, 35, id=2, fx={"slant": [[0, 1]], "wah": [[0.5, 0.5]]})]})],
     ppq=480)
case("hz_bad_points", [cust([SQUARE], pts=HZBOX, fill="spam", hz={"key": 33, "bpm": 120, "tones": TONES[:2], "fx": {
    "slant": [[0, 0], ["x", 1]], "wah": [[0, 0.5, "hold"], [1, 2, 5], [float("inf"), 1]], "groups": 7}})], ppq=480)
for i in range(40):
    tones = []
    for k in range(R.randint(1, 5)):
        tones.append(tone(round(R.uniform(0, 1.5), 3), round(R.uniform(0.05, 0.6), 3), R.randint(24, 50), id=k + 1,
                          cents=R.choice([0, 0, 15, -30])))
    for a in tones:
        if R.random() < 0.4:
            b = R.choice(tones)
            a["to"] = [{"id": b["id"], "out": R.choice([0, 0.05, 0.2]), "in": R.choice([0, 0.05, 0.2])}]
    hz = {"key": R.randint(24, 50), "bpm": R.choice([90, 120, 140.5, 174]), "tones": tones}
    if R.random() < 0.3:
        hz["auto"] = R.choice([1, 5, 20])
    elif R.random() < 0.2:
        hz["fixed"] = True
    if R.random() < 0.6:
        hz["fx"] = {n: FXS[n] for n in R.sample(sorted(FXS), R.randint(1, 4))}
    w = R.choice([1, 2])
    case(f"hz_random_{i}", [cust([R.choice([SQUARE, CIRCLE])], pts=[[0.5, 28], [0.5 + w, 28], [0.5, 28 + R.randint(3, 14)]],
                                 fill=R.choice(["spam", "spam", "outline_spam"]), gate=0.01, hz=hz,
                                 cycle=R.choice([None, {"by": "step", "n": 3, "every": 1}]) or {})],
         ppq=R.choice([96, 480, 960]))

# ---------------------------------------------------------------- text (text.py: letters as they're saved)


def bez_poly(pts):
    """A closed polyline as curve points (anchor, handle, handle, anchor ...): straight pieces."""
    out = [list(pts[0])]
    for a, b in zip(pts, pts[1:]):
        out += [[a[0] + (b[0] - a[0]) / 3, a[1] + (b[1] - a[1]) / 3], [a[0] + (b[0] - a[0]) * 2 / 3, a[1] + (b[1] - a[1]) * 2 / 3],
                list(b)]
    return out


def bez_ring(cx, cy, r, rev=False):
    k = 0.5523 * r
    pts = [[cx + r, cy], [cx + r, cy + k], [cx + k, cy + r], [cx, cy + r], [cx - k, cy + r], [cx - r, cy + k], [cx - r, cy],
           [cx - r, cy - k], [cx - k, cy - r], [cx, cy - r], [cx + k, cy - r], [cx + r, cy - k], [cx + r, cy]]
    return pts[::-1] if rev else pts


LETTERS = [{"kind": "curve", "pts": bez_ring(0.25, 0.5, 0.25)}, {"kind": "curve", "pts": bez_ring(0.25, 0.5, 0.15, True)},
           {"kind": "curve", "pts": bez_poly([[0.6, 0], [1, 0], [1, 0.15], [0.72, 0.15], [0.72, 1], [0.6, 1], [0.6, 0]])}]
for i, (th, grow, holes) in enumerate([(50, 0, [1]), (10, 0, [1]), (90, 0, [1]), (50, 0.3, [1]), (50, -0.2, [1]),
                                       (50, 0.5, []), (0, 0, [1]), (100, 0, [1])]):
    tx = {"text": "OL", "font": "Arial", "size": 24, "threshold": th, "grow": grow, "k": 0.25, "holes": holes,
          "bbox": [0, -0.2, 1.4, 0.75], "cap": 0.72}
    for fill in ("empty", "fill", "spam"):
        case(f"text_{i}_{fill}", [cust(LETTERS, pts=[[0, 40], [6, 40], [0, 64]], fill=fill, gate=0.0625, text=tx)],
             ppq=R.choice([96, 480]))
case("text_turned", [cust(LETTERS, pts=[[0, 40], [5, 45], [-2, 60]], fill="spam", gate=0.0625, edge=0.2,
                          text={"text": "OL", "bbox": [0, 0, 1, 1], "grow": 0.2, "k": 0.5, "holes": [1]})], ppq=480)
case("text_apart", [cust(LETTERS, pts=[[0, 40], [6, 40], [0, 64]], fill="fill", apart=True, edge=0.15,
                         text={"text": "OL", "bbox": [0, 0, 1, 1], "threshold": 30, "holes": [1]})], mode="auto", ppq=480)
case("text_bad", [cust(LETTERS, text={"text": "x"}), cust(LETTERS, text={"text": "x", "bbox": [0, 0, 1]}),
                  cust(LETTERS, text={"bbox": [0, 0, 1, 1], "size": "big"}), cust(LETTERS, text="hello")])

# ---------------------------------------------------------------- funnels (funnel.py)
CURVE = {"pts": [[0, 0], [0.7, 0.06], [0.94, 0.3], [1, 1]], "sharp": []}
S_CURVE = {"pts": [[0, 0], [0.3, 0.0], [0.2, 0.5], [0.5, 0.5], [0.8, 0.5], [0.7, 1], [1, 1]], "sharp": [1]}


def fun(pts, starts, **kw):
    sh = {"kind": "funnel", "pts": pts, "starts": starts}
    sh.update(kw)
    return sh


BASIC = [[0, 60], [4, 60], [4, 72], [4, 48]]
both = [{"line": 0, "at": 0, "ends": [CURVE, CURVE]}]
for fill in ("spam", "long"):
    for wall in ("in", "past"):
        case(f"funnel_{fill}_{wall}", [fun(BASIC, both, fill=fill, wall=wall, gate0=0.0625)], ppq=480)
for change in ("steps", "smooth"):
    for follow in ("time", "curve"):
        case(f"funnel_vary_{change}_{follow}", [fun(BASIC, both, vary=True, gate0=0.25, gate1=0.03125, change=change,
                                                    follow=follow)], ppq=480)
case("funnel_reverse", [fun([[4, 60], [0, 60], [0, 70], [0, 50]], both, gate0=0.1, gate1=0.02, vary=True)], ppq=960)
case("funnel_half", [fun([[0, 60], [3, 60], [3, 66], [3, 60]], [{"line": 0, "at": 0.2, "ends": [S_CURVE, None]}])])
case("funnel_two_lines", [fun([[0, 60], [4, 60], [4, 72], [4, 48], [0, 66], [4, 66], [8, 62], [4, 62]],
                              [{"line": 0, "at": 0, "ends": [CURVE, CURVE]}, {"line": 1, "at": 0.5, "ends": [None, CURVE]},
                               {"line": 2, "at": 0.1, "ends": [CURVE, dict(CURVE, link=1, flip=True)]}], wall="past",
                              gate0=0.05)], ppq=480)
case("funnel_turned", [fun([[0, 60], [3, 66], [5, 62], [1, 70]], both, gate0=0.0625, fill="spam")], ppq=480)
case("funnel_upright", [fun([[2, 60], [2, 60], [2, 70], [2, 50]], both, gate1=0.125)], ppq=480)
case("funnel_formulas", [fun(BASIC, [{"line": 0, "at": 0, "ends": [
    dict(CURVE, pattern={"formula": "0.05 * sin(x * 2 * pi)", "loops": 4}), dict(CURVE, shape={"preset": "arch", "x": "x",
                                                                                        "y": "0.2 * sin(x * pi)"},
                                                                         rev=True)]}], gate0=0.0625)], ppq=480)
case("funnel_old", [{"kind": "funnel", "pts": [[0, 60], [4, 70]], "bend": [0.6, 0.3]},
                    {"kind": "funnel", "pts": [[0, 60], [4, 66]], "sides": "one", "gate0": 0.1, "gate1": 0.05},
                    {"kind": "funnel", "pts": [[0, 60], [2, 64]], "starts": [{"line": 0, "at": 0, "ends": [[[0.5, 0.5], [0.2, 0.7]],
                                                                                                      [[0.3, 0.1], [0.6, 0.5], [0.9, 0.2]]]}]}],
     ppq=480)
case("funnel_bad", [fun([[0, 60], [4, 60], [4, 72]], both), fun(BASIC, [{"line": 5, "ends": [CURVE]}]),
                    fun(BASIC, [{"line": 0, "ends": [{"pts": [[0, 0], [1, 1]]}, None]}]),
                    fun(BASIC, "nope"), fun(BASIC, both, gate0="x")], ppq=480)
for i in range(40):
    b0, p0 = R.uniform(0, 2), R.randint(40, 80)
    ln = R.choice([1, 2, 4])
    wall_b = b0 + ln * R.choice([1, -1])
    h = R.randint(2, 14)
    pts = [[b0, p0], [wall_b, p0 + R.choice([0, 0, 2, -3])], [wall_b, p0 + h], [wall_b, p0 - R.randint(0, h)]]
    if R.random() < 0.3:
        pts += [[b0 + R.uniform(-0.5, 0.5), p0 + R.randint(-5, 5)], [wall_b, p0 + R.randint(-2, 2)]]
    nl = len(pts) // 2 - 1
    starts = []
    for _ in range(R.randint(1, 3)):
        e = [R.choice([CURVE, S_CURVE, {"pts": [[0, 0], [R.random(), R.random()], [R.random(), R.random()], [1, 1]]}, None])
             for _ in range(2)]
        starts.append({"line": R.randrange(nl), "at": round(R.random(), 3), "ends": e})
    sh = fun(pts, starts, fill=R.choice(["spam", "long"]), wall=R.choice(["in", "past"]), change=R.choice(["steps", "smooth"]),
             follow=R.choice(["time", "curve"]), gate0=R.choice([0.0625, 0.125, 0.03]), gate1=R.choice([0.0625, 0.02, 0.25]))
    if R.random() < 0.2:
        sh["cycle"] = {"by": R.choice(["step", "key"]), "n": 3, "every": 1}
    case(f"funnel_random_{i}", [sh], ppq=R.choice([96, 480, 960]), mode=R.choice(["single", "auto"]))

fx = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "..", "crates", "plumspirit_engine",
                  "tests", "fixtures")
json.dump(cases, open(os.path.join(fx, "spiderweb_cases.json"), "w"), indent=None)
print(len(cases), "cases")
