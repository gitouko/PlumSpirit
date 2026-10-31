#!/usr/bin/env python3
"""A/B two builds of the engine perf harness on the same machine (alternating runs,
medians of three), printing a Markdown table.

    python3 tools/perf_ab.py [--mode scenario|v02] [--names OLD,NEW] OLD_PERF_REPORT NEW_PERF_REPORT [notes ...]

`--mode scenario` (default) compares the v0.1 measurements (import, export, queries,
edits); `--mode v02` the v0.2 ones (.lup files, playback scheduling, undo). Both
builds must have the mode.

Build the old harness from its own commit into a separate target directory, e.g.
    git worktree add /tmp/v02 c6c816d
    (cd /tmp/v02 && CARGO_TARGET_DIR=/tmp/v02-target cargo build --release \\
        -p plumspirit_engine --example perf_report)
"""
import os
import re, subprocess, statistics, sys

KEYS = {
    "scenario": [
        ("generate", r"generate document: ([\d.]+) ms"),
        ("heap B/note", r"\(([\d.]+) bytes/note incl"),
        ("export write", r"export \(write ([\d.]+) ms"),
        ("export validate", r"\+ validate ([\d.]+) ms\)"),
        ("import", r"- import: ([\d.]+) ms"),
        ("import RSS MB", r"- import: .*→ RSS ([\d.]+) MB"),
        ("viewport query", r"viewport query \(4 bars\): ([\d.]+) ms"),
        ("full-range query", r"full-range query .*: ([\d.]+) ms"),
        ("density raster (whole song)", r"whole song in 1600 columns: ([\d.]+) ms"),
        ("box select 1 bar", r"box select 1 bar: ([\d.]+) ms"),
        ("paste", r"paste selection: ([\d.]+) ms"),
        ("insert one note", r"insert one note \(mid-track\): ([\d.]+) ms"),
        ("delete one note", r"delete one note: ([\d.]+) ms"),
        ("Flip Y 10%", r"Flip Y on \d+ notes: ([\d.]+) ms"),
        ("Flip X 10% prepare", r"Flip X on \d+ notes: prepare ([\d.]+) ms"),
        ("Flip X 10% commit", r"Flip X on \d+ notes: prepare [\d.]+ ms \+ commit ([\d.]+) ms"),
        ("Scale X x2 prepare", r"Scale X ×2 on all \d+ notes: prepare ([\d.]+) ms"),
        ("Scale X x2 commit", r"Scale X ×2 on all \d+ notes: prepare [\d.]+ ms \+ commit ([\d.]+) ms"),
        ("Scale Y x-1", r"Scale Y ×-1 on all notes: ([\d.]+) ms"),
        ("delete all", r"delete all \d+ notes \(batch compaction\): ([\d.]+) ms"),
        ("end RSS MB", r"- end: RSS ([\d.]+) MB"),
        ("peak RSS MB", r"- end: RSS [\d.]+ MB \(peak ([\d.]+) MB\)"),
    ],
    "v02": [
        ("generate", r"generate document: ([\d.]+) ms"),
        (".lup save total", r"\.lup save \(write .*: total ([\d.]+) ms"),
        (".lup save write", r"\.lup save \(write ([\d.]+) ms"),
        (".lup save verify", r"\.lup save \(write [\d.]+ ms \+ verify ([\d.]+) ms"),
        (".lup save again", r"\.lup save again .*: ([\d.]+) ms"),
        (".lup file MB", r"; file ([\d.]+) MB \("),
        (".lup load", r"- \.lup load: ([\d.]+) ms"),
        (".lup load RSS MB", r"- \.lup load: .*→ RSS ([\d.]+) MB"),
        ("playback index build", r"playback index build: ([\d.]+) ms"),
        ("schedule whole song", r"scheduling the whole song .*?: ([\d.]+) ms ="),
        ("seek avg", r"seek \(100 random positions\): ([\d.]+) ms avg"),
        ("velocity edit all", r"velocity edit on all \d+ notes: ([\d.]+) ms"),
        ("undo velocity", r"undo velocity edit: ([\d.]+) ms"),
        ("redo velocity", r"redo velocity edit: ([\d.]+) ms"),
        ("move all prepare", r"move all notes .*: prepare ([\d.]+) ms"),
        ("move all apply", r"move all notes .*: prepare [\d.]+ ms \+ apply ([\d.]+) ms"),
        ("undo move", r"undo move: ([\d.]+) ms"),
        ("delete all", r"- delete all notes: ([\d.]+) ms"),
        ("undo delete", r"undo delete \(all notes back\): ([\d.]+) ms"),
        ("end RSS MB", r"- end: RSS ([\d.]+) MB"),
        ("peak RSS MB", r"- end: RSS [\d.]+ MB \(peak ([\d.]+) MB\)"),
    ],
}

args = sys.argv[1:]
mode, names = "scenario", ("old", "new")
while args and args[0].startswith("--"):
    opt = args.pop(0)
    if opt == "--mode":
        mode = args.pop(0)
    elif opt == "--names":
        names = tuple(args.pop(0).split(",", 1))
    else:
        sys.exit(f"unknown option {opt}")
BIN = {names[0]: args[0], names[1]: args[1]}
keys = KEYS[mode]


def run(ver, n):
    out = subprocess.run([BIN[ver], mode, str(n)], capture_output=True, text=True).stdout
    return {k: float(m.group(1)) for k, rx in keys if (m := re.search(rx, out))}


sizes = [int(x) for x in args[2:]] or [100_000, 1_000_000, 5_000_000]
reps = int(os.environ.get("PERF_AB_REPS", "3"))
for n in sizes:
    res = {v: [] for v in names}
    for _ in range(reps):
        for v in names:
            res[v].append(run(v, n))
    print(f"\n### {mode}: {n:,} notes (median of {reps} alternating runs)\n")
    print(f"| measurement | {names[0]} | {names[1]} | change |")
    print("|---|---:|---:|---:|")
    for k, _ in keys:
        a = [r[k] for r in res[names[0]] if k in r]
        b = [r[k] for r in res[names[1]] if k in r]
        if not a or not b:
            continue
        ma, mb = statistics.median(a), statistics.median(b)
        unit = "" if ("MB" in k or "B/note" in k) else " ms"
        ch = f"{(mb / ma - 1) * 100:+.0f}%" if ma > 0 else "n/a"
        print(f"| {k} | {ma:.3f}{unit} | {mb:.3f}{unit} | {ch} |")
