#!/usr/bin/env python3
"""Adds new-function designs to docs/v0_9/TOOL_CATALOG.toml (V09-S32 design
gate, docs/v0_9/TOOL_DESIGN.md).

catalog_design.py [--replace] DESIGNS.py
                               DESIGNS.py defines D = {tool_id: [design, ...]}
                               with design = (name, summary, controls, sweeps,
                               growth, preview, purpose, shared, rubric[, order])
                               and rubric = (distinct, black_midi, depth,
                               experiment, interaction, perf, preview, test);
                               optionally P = {tool_id: {function_id: purpose}}
                               for the tool's existing functions.

Each design becomes a `[[tool.function]]` block (id n01…, state PROPOSED,
origin new, order 110, 120, … after the tool's existing new functions, or
the design's own order — 500–790 for output shaping), written after the
tool's last function, one field per line (the Rust catalog test reads the
file line by line). P adds `purpose = …` after the existing function's
`params` (or `order`) line so the duplicate-purpose test sees every active
function. Names already present in the tool are refused. With --replace
the tool's PROPOSED new functions are replaced by the designs: a design
keeps the id of the function with its name (or with the old name in the
optional RENAME = {tool_id: {old_name: new_name}}), the ids of dropped
designs go to the tool's `retired` list and are never handed out again. Run
`python3 tools/capsule.py check` afterwards.
"""
import os
import re
import runpy
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CATALOG = os.path.join(ROOT, "docs", "v0_9", "TOOL_CATALOG.toml")
RUBRIC = ("distinct", "black_midi", "depth", "experiment", "interaction", "perf", "preview", "test")


def q(s):
    return '"' + str(s).replace("\\", "\\\\").replace('"', '\\"') + '"'


def block(fid, order, d):
    name, summary, controls, sweeps, growth, preview, purpose, shared, rubric = d[:9]
    r = ", ".join(f"{k} = {v}" for k, v in zip(RUBRIC, rubric))
    return "\n".join([
        "[[tool.function]]",
        f"id = {q(fid)}",
        f"name = {q(name)}",
        'status = "active"',
        'state = "PROPOSED"',
        'origin = "new"',
        f"order = {order}",
        f"params = {len(controls)}",
        f"summary = {q(summary)}",
        "controls = [" + ", ".join(q(c) for c in controls) + "]",
        "sweeps = [" + ", ".join(q(c) for c in sweeps) + "]",
        f"growth = {q(growth)}",
        f"preview = {q(preview)}",
        f"purpose = {q(purpose)}",
        "shared = [" + ", ".join(q(c) for c in shared) + "]",
        f"rubric = {{ {r} }}",
    ]) + "\n"


def set_purposes(text, tid, purposes):
    """`purpose = …` for existing functions of one tool (replaced if present)."""
    start = re.search(r'^\[\[tool\]\]\nid = "' + re.escape(tid) + r'"\n', text, re.M)
    if not start:
        raise SystemExit(f"{tid}: not in the catalog")
    nxt = re.search(r"^\[\[tool\]\]\n", text[start.end():], re.M)
    end = start.end() + nxt.start() if nxt else len(text)
    body = text[start.start():end]
    for fid, purpose in purposes.items():
        m = re.search(r'^\[\[tool\.function\]\]\nid = "' + re.escape(fid) + r'"\n(?:(?!\n\n|\n\[).)*', body, re.M | re.S)
        if not m:
            raise SystemExit(f"{tid}/{fid}: not in the catalog")
        fb = re.sub(r'^purpose = .*\n?', "", m.group(0), flags=re.M).rstrip("\n")
        body = body[:m.start()] + fb + f"\npurpose = {q(purpose)}" + body[m.end():]
    print(f"{tid}: {len(purposes)} purposes set on existing functions")
    return text[:start.start()] + body + text[end:]


NEW_BLOCK = re.compile(r'^\[\[tool\.function\]\]\nid = "(n\d+)"\nname = "((?:[^"\\\n]|\\.)*)"\n(?:(?!\n\n|\n\[).)*', re.M | re.S)


def strip_new(body, tid):
    """Removes the tool's new (PROPOSED) function blocks: (body, name → id, retired ids)."""
    known, retired = {}, re.findall(r'"(n\d+)"', (re.search(r"^retired = \[(.*)\]$", body, re.M) or [None, ""])[1])
    for m in list(NEW_BLOCK.finditer(body))[::-1]:
        if 'origin = "new"' not in m.group(0):
            continue
        if 'state = "PROPOSED"' not in m.group(0):
            raise SystemExit(f"{tid}/{m.group(1)}: not PROPOSED, --replace refused")
        known[m.group(2)] = m.group(1)
        body = body[:m.start()].rstrip("\n") + "\n" + body[m.end():].lstrip("\n")
    return body.rstrip("\n") + "\n", known, retired


def main():
    argv = sys.argv[1:]
    replace = "--replace" in argv
    argv = [a for a in argv if a != "--replace"]
    spec = runpy.run_path(argv[0])
    designs = spec.get("D", {})
    with open(CATALOG, encoding="utf-8") as f:
        text = f.read()
    for tid, purposes in spec.get("P", {}).items():
        text = set_purposes(text, tid, purposes)
    for tid, ds in designs.items():
        start = re.search(r'^\[\[tool\]\]\nid = "' + re.escape(tid) + r'"\n', text, re.M)
        if not start:
            raise SystemExit(f"{tid}: not in the catalog")
        nxt = re.search(r"^\[\[tool\]\]\n", text[start.end():], re.M)
        end = start.end() + nxt.start() if nxt else len(text)
        body = text[start.start():end]
        known, retired = {}, []
        if replace:
            body, known, retired = strip_new(body, tid)
            for was, now in spec.get("RENAME", {}).get(tid, {}).items():
                if was in known:
                    known[now] = known.pop(was)
            gone = sorted(set(known.values()) - {known[d[0]] for d in ds if d[0] in known})
            retired = sorted(set(retired) | set(gone))
            line = "retired = [" + ", ".join(q(r) for r in retired) + "]"
            if re.search(r"^retired = ", body, re.M):
                body = re.sub(r"^retired = .*$", line, body, count=1, flags=re.M)
            elif retired:
                body = re.sub(r'^(rule = .*)$', lambda m: m.group(1) + "\n" + line, body, count=1, flags=re.M)
        names = set(re.findall(r'^name = "(.*)"$', body, re.M))
        ids = [int(m) for m in re.findall(r'^id = "n(\d+)"$', body, re.M)]
        ids += [int(i[1:]) for i in list(known.values()) + retired]
        orders = [int(m.group(2)) for m in re.finditer(
            r'^origin = "(new)"\norder = (\d+)$', body, re.M) if int(m.group(2)) < 500]
        k, order = (max(ids) if ids else 0), (max(orders) if orders else 100)
        out = []
        for d in ds:
            if d[0] in names:
                raise SystemExit(f"{tid}: a function named {d[0]!r} exists")
            if d[0] in known:
                fid = known[d[0]]
            else:
                k += 1
                fid = f"n{k:02d}"
            if len(d) > 9:
                o = d[9]
            else:
                order += 10
                o = order
            out.append(block(fid, o, d))
        insert = body.rstrip("\n") + "\n\n" + "\n".join(out) + "\n"
        text = text[:start.start()] + insert + text[end:]
        print(f"{tid}: {len(ds)} designs {'replaced' if replace else 'added'}"
              + (f", retired {', '.join(retired)}" if retired else ""))
    with open(CATALOG, "w", encoding="utf-8") as f:
        f.write(text)


if __name__ == "__main__":
    main()
