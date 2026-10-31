#!/usr/bin/env python3
"""Fixed PlumSpirit benchmark suite (v0.9 contract §204, §291, §292).

bench.py OUT.csv [GROUP ...]     run the suite (or the named groups) and append rows
bench.py --list                  show the groups and their commands

The same commands run for the v0.8 baseline (docs/v0_9/PERFORMANCE_BASELINE.csv)
and for v0.9 (docs/v0_9/BENCH_RESULTS.csv), on the same machine, so rows compare
directly by `operation`. Every number is parsed from the program's own output
("label: value unit", "... run 12.3 ms, apply 4.5 ms", Rust Duration debug
"364.04ms"); multi-run groups report the median. Raw logs go to
$BENCH_LOG_DIR (default /tmp/plumspirit-bench). Linux; the GUI group needs Xvfb.
Environment: PLUMSPIRIT_TEST_SF2 (SoundFont for the audio group).
"""
import csv
import datetime
import os
import platform
import re
import statistics
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HEADER = ["date", "commit", "machine", "operation", "fixture", "notes", "elapsed_ms",
          "peak_rss_mb", "cpu", "fps", "underruns", "output_count", "command"]
CR = "cargo run -q --release --offline -p plumspirit_engine --example perf_report --"
TE = "cargo test -q --release --offline -p plumspirit_engine --test"
TA = "cargo test -q --release --offline -p plumspirit --bin PlumSpirit"
CE = "cargo run -q --release --offline -p plumspirit_engine --example"
SUITE = {
    # group: (reps, fixture, [commands])
    "engine": (3, "generated Dense / v0x fixtures, 1,000,000 notes", [
        f"{CR} scenario 1000000", f"{CR} v02 1000000", f"{CR} v05 1000000",
        f"{CR} v06 1000000", f"{CR} v07 1000000", f"{CR} v09 1000000"]),
    "stress": (1, "stress suites (generated, up to 5M notes)", [
        f"{TE} stress -- --ignored --nocapture --test-threads=1",
        f"{TE} stress_v03 -- --ignored --nocapture --test-threads=1",
        f"{TE} stress_v08 -- --ignored --nocapture --test-threads=1",
        f"{TE} v08_history journal_cost -- --ignored --nocapture --test-threads=1"]),
    "frames": (1, "headless frame-cost fixtures (app)", [
        f"{TA} frame_cost -- --ignored --nocapture --test-threads=1",
        f"{TA} dense_ten_million_notes_are_never_blank -- --ignored --nocapture",
        f"{CE} tile_cost"]),
    "audio": (1, "GeneralUser GS, generated walls / bursts", [
        f"{TA} synth_cost -- --ignored --nocapture --test-threads=1",
        f"{TA} audio_profile_v08 -- --ignored --nocapture --test-threads=1"]),
    "gui": (3, "release app under Xvfb, fresh config; empty / 1M-note MIDI", ["@gui"]),
}
NUM = r"([-+]?\d[\d,]*\.?\d*)\s*(ms|µs|us|ns|s|MB|MiB|KB|KiB|GB)\b"
PAIR = re.compile(r"([A-Za-z][\w+/\-.()]*(?: [A-Za-z][\w+/\-.()]*){0,3})?\s*[:=]?\s*" + NUM)


def to_ms(v, u):
    return v * {"ms": 1.0, "µs": 1e-3, "us": 1e-3, "ns": 1e-6, "s": 1000.0}[u]


def to_mb(v, u):
    return v * {"MB": 1.0, "MiB": 1.048576, "KB": 1e-3, "KiB": 1.024e-3, "GB": 1000.0}[u]


def parse(text):
    """{metric: (kind, value)} from a program's output."""
    out = {}
    for line in text.splitlines():
        s = line.strip().lstrip("-").strip()
        if not s or s.startswith(("test ", "running ", "Running ", "Finished", "Compiling", "warning")):
            continue
        label, _, rest = s.partition(":")
        if not rest:
            label, rest = "", s
        label = " ".join(label.split())[:90]
        m = re.search(r"(\d[\d,]*) out\b", rest)
        if m:
            out[f"{label} / output"] = ("count", float(m.group(1).replace(",", "")))
        for i, pm in enumerate(PAIR.finditer(rest)):
            word = (pm.group(1) or "").strip()
            v = float(pm.group(2).replace(",", ""))
            u = pm.group(3)
            key = label + (f" / {word}" if word else (f" / #{i}" if i else ""))
            kind = "mb" if u in ("MB", "MiB", "KB", "KiB", "GB") else "ms"
            out.setdefault(key, (kind, to_mb(v, u) if kind == "mb" else to_ms(v, u)))
    return out


def gui_run(binary, midi):
    """One launch: start time (s), RSS (MB) after 6 s idle, CPU ticks over 10 s idle."""
    cfg = tempfile.mkdtemp(prefix="psbench")
    env = dict(os.environ, DISPLAY=os.environ.get("DISPLAY", ":99"), PLUMSPIRIT_CONFIG_DIR=cfg,
               LIBGL_ALWAYS_SOFTWARE="1")
    args = [binary] + ([midi] if midi else [])
    p = subprocess.Popen(args, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    log = os.path.join(cfg, "plumspirit.log")
    start = None
    for _ in range(300):
        time.sleep(0.1)
        if os.path.exists(log):
            m = re.search(r"^\[\s*([0-9.]+)s\b.*theme preference", open(log, errors="replace").read(), re.M)
            if m:
                start = float(m.group(1))
                break
    time.sleep(6)
    imp = None
    if os.path.exists(log):
        m = re.search(r"imported .*?in ([0-9.]+) ms", open(log, errors="replace").read())
        imp = float(m.group(1)) if m else None
    rss = int(open(f"/proc/{p.pid}/status").read().split("VmRSS:")[1].split()[0]) / 1024
    st = open(f"/proc/{p.pid}/stat").read().rsplit(")", 1)[1].split()
    a = int(st[11]) + int(st[12])
    time.sleep(10)
    st = open(f"/proc/{p.pid}/stat").read().rsplit(")", 1)[1].split()
    b = int(st[11]) + int(st[12])
    p.kill()
    p.wait()
    return start, rss, b - a, imp


def fixture_midi(path, notes=1_000_000):
    """Deterministic 1M-note Format-1 MIDI (8 tracks, 960 PPQN, 16th-note walls)."""
    if os.path.exists(path):
        return path
    per = notes // 8
    def vlq(n):
        b = [n & 0x7F]
        n >>= 7
        while n:
            b.append((n & 0x7F) | 0x80)
            n >>= 7
        return bytes(reversed(b))

    out = bytearray(b"MThd" + (6).to_bytes(4, "big") + (1).to_bytes(2, "big") + (8).to_bytes(2, "big") + (960).to_bytes(2, "big"))
    for t in range(8):
        ev = bytearray()
        x = 12345 + t
        for g in range(per // 8):
            ps = []
            for _ in range(8):
                x = (x * 1103515245 + 12345) & 0x7FFFFFFF
                ps.append(24 + (x >> 8) % 80)
            for k, pch in enumerate(ps):
                ev += vlq(0 if k else 120) + bytes([0x90 | t, pch, 100])
            for k, pch in enumerate(ps):
                ev += vlq(120 if k == 0 else 0) + bytes([0x80 | t, pch, 0])
        ev += b"\x00\xff\x2f\x00"
        out += b"MTrk" + len(ev).to_bytes(4, "big") + ev
    with open(path, "wb") as f:
        f.write(out)
    return path


def run(cmd, logdir, tag):
    t0 = time.time()
    p = subprocess.run(cmd, shell=True, cwd=ROOT, capture_output=True, text=True)
    with open(os.path.join(logdir, f"{tag}.log"), "w") as f:
        f.write(f"$ {cmd}\n# exit {p.returncode}, {time.time() - t0:.1f} s\n{p.stdout}\n{p.stderr}")
    if p.returncode != 0:
        print(f"FAILED ({p.returncode}): {cmd}", file=sys.stderr)
    return p.stdout + "\n" + p.stderr


def main():
    if "--list" in sys.argv:
        for g, (reps, fx, cmds) in SUITE.items():
            print(f"{g} (x{reps}; {fx})")
            for c in cmds:
                print("   ", c)
        return 0
    out_csv, groups = sys.argv[1], sys.argv[2:] or list(SUITE)
    logdir = os.environ.get("BENCH_LOG_DIR", "/tmp/plumspirit-bench")
    os.makedirs(logdir, exist_ok=True)
    commit = subprocess.run(["git", "-C", ROOT, "rev-parse", "--short", "HEAD"], capture_output=True, text=True).stdout.strip()
    dirty = bool(subprocess.run(["git", "-C", ROOT, "status", "--porcelain", "--untracked-files=no"], capture_output=True, text=True).stdout.strip())
    cpu = re.search(r"model name\s*:\s*(.*)", open("/proc/cpuinfo").read())
    machine = f"{platform.system()} {os.cpu_count()} vCPU {cpu.group(1) if cpu else '?'}"
    date = datetime.date.today().isoformat()
    rows = []
    for g in groups:
        reps, fixture, cmds = SUITE[g]
        for ci, cmd in enumerate(cmds):
            if cmd == "@gui":
                subprocess.run("cargo build -q --release --offline -p plumspirit", shell=True, cwd=ROOT, check=True)
                binary = os.path.join(ROOT, "target/release/PlumSpirit")
                if not os.environ.get("DISPLAY"):
                    subprocess.Popen(["Xvfb", ":99", "-screen", "0", "1600x900x24"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                    os.environ["DISPLAY"] = ":99"
                    time.sleep(1.5)
                midi = fixture_midi(os.path.join(logdir, "bench_1m.mid"))
                for name, m in (("empty project", None), ("1M-note MIDI (8 tracks)", midi)):
                    res = [gui_run(binary, m) for _ in range(reps)]
                    med = lambda i: statistics.median([r[i] for r in res if r[i] is not None]) if any(r[i] is not None for r in res) else None
                    st, rss, ticks, imp = med(0), med(1), med(2), med(3)
                    rows.append([date, commit + ("+dirty" if dirty else ""), machine, f"gui / {name} / start to ready", fixture, f"median of {reps}", f"{st * 1000:.1f}" if st else "", "", "", "", "", "", "PlumSpirit " + ("" if m is None else "bench_1m.mid")])
                    rows.append([date, commit + ("+dirty" if dirty else ""), machine, f"gui / {name} / idle", fixture, f"median of {reps}; CPU ticks over 10 s idle", "", f"{rss:.1f}", f"{ticks}", "", "", "", "/proc VmRSS after 6 s"])
                    if imp:
                        rows.append([date, commit + ("+dirty" if dirty else ""), machine, f"gui / {name} / import", fixture, f"median of {reps}", f"{imp:.1f}", "", "", "", "", "", "log 'imported … in N ms'"])
                continue
            runs = [parse(run(cmd, logdir, f"{g}{ci}_r{r}")) for r in range(reps)]
            keys = [k for k in runs[0] if all(k in x for x in runs)]
            tag = cmd.replace(CR, "perf_report").replace(TE, "test").replace(TA, "app-test").replace(CE, "example").split(" --")[0].strip()
            for k in keys:
                kind = runs[0][k][0]
                v = statistics.median([x[k][1] for x in runs])
                row = [date, commit + ("+dirty" if dirty else ""), machine, f"{g} / {tag} / {k}", fixture,
                       f"median of {reps}" if reps > 1 else "1 run", "", "", "", "", "", "", cmd]
                if kind == "ms":
                    row[6] = f"{v:.3f}"
                elif kind == "mb":
                    row[7] = f"{v:.2f}"
                else:
                    row[11] = f"{int(v)}"
                rows.append(row)
            print(f"{g}: {tag}: {len(keys)} metrics", file=sys.stderr)
    new = not os.path.exists(out_csv) or os.path.getsize(out_csv) == 0
    with open(out_csv, "a", newline="", encoding="utf-8") as f:
        w = csv.writer(f, lineterminator="\n")
        if new:
            w.writerow(HEADER)
        w.writerows(rows)
    print(f"{len(rows)} rows → {out_csv}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
