"""Development-only reference oracle for the Spiderweb port (contract v0.8 §029, §235).

Runs the ORIGINAL Spiderweb note engine (MIT, Kanade Tachibana; clone of
https://github.com/UnPrioritized/Spiderweb at f7fd766, path given as argv[1]) on
the shapes in crates/plumspirit_engine/tests/fixtures/spiderweb_cases.json and
writes the notes it makes to spiderweb_golden.json next to it, exactly as the
Spiderweb app would render them (notes_tracks per shape, the 15-colour cap,
render with the case's channel mode / split and tracks_apart).

Usage: NPY_DISABLE_CPU_FEATURES=X86_V4 python3 oracle.py /path/to/Spiderweb   (needs NumPy; not shipped)

NPY_DISABLE_CPU_FEATURES=X86_V4: on CPUs with AVX-512, NumPy computes array powers / exp / log with Intel SVML,
which differs from the C library in the last bit for a few percent of values (so Spiderweb's geometry depends on
the CPU it runs on). Without AVX-512 dispatch NumPy uses the C library, as on most PCs; PlumSpirit reproduces that.
The script refuses to run with AVX-512 dispatch active.
"""
import json
import os
import sys

up = os.path.join(sys.argv[1], "scripts")
sys.path.insert(0, up)
import numpy as np  # noqa: E402

if np._core._multiarray_umath.__cpu_features__.get("AVX512F") and "X86_V4" not in os.environ.get(
        "NPY_DISABLE_CPU_FEATURES", ""):
    sys.exit("Run with NPY_DISABLE_CPU_FEATURES=X86_V4 (see the top).")
from notes.engine import clean_shape, render, shape_notes_tracks, shape_strokes  # noqa: E402
import struct  # noqa: E402
from notes.custom import capped_colours, tracks_apart  # noqa: E402
from notes.areas import COLOURS, _maps  # noqa: E402

here = os.path.dirname(os.path.abspath(__file__))
fx = os.path.join(here, "..", "..", "..", "crates", "plumspirit_engine", "tests", "fixtures")
cases = json.load(open(os.path.join(fx, "spiderweb_cases.json")))


def fnv(path):
    """FNV-1a (64-bit) over the IEEE bits of every coordinate: the geometry, bit for bit."""
    h = 0xcbf29ce484222325
    for q in path:
        for v in q:
            h = ((h ^ struct.unpack("<Q", struct.pack("<d", float(v)))[0]) * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return "%016x" % h


out = []
for c in cases:
    ppq, keys = c.get("ppq", 960), c.get("keys", 128)
    shapes = [clean_shape(s) for s in c["shapes"]]
    res = {"name": c["name"], "clean": [s is not None for s in shapes], "shapes": []}
    shapes = [s for s in shapes if s is not None]
    got = []
    for sh in shapes:
        # areas.py remembers area maps by the strokes alone (not where the shape is): start each shape afresh so
        # every result is the shape's own, not one left from an earlier case
        _maps.clear()
        got.append(shape_notes_tracks(sh, ppq, keys))
    for sh, (n, t) in zip(shapes, got):
        res["shapes"].append({"notes": np.asarray(n).tolist(),
                              "tracks": None if t is None else np.asarray(t).tolist(),
                              "strokes": [[len(p), fnv(p)] for p in shape_strokes(sh)]})
    wanted = [0 if t is None or "notes" in sh else len(np.unique(t)) for sh, (_, t) in zip(shapes, got)]
    got = [(n, capped_colours(t) if w > COLOURS else t) for (n, t), w in zip(got, wanted)]
    rendered, count = render([n for n, _ in got], c.get("mode", "single"), c.get("split", "key"),
                             [t for _, t in got], [tracks_apart(sh) for sh in shapes])
    res["rendered"] = np.asarray(rendered).tolist()
    res["count"] = count
    out.append(res)
json.dump(out, open(os.path.join(fx, "spiderweb_golden.json"), "w"), separators=(",", ":"))
print(len(out), "cases")
