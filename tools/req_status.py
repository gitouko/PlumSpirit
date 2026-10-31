#!/usr/bin/env python3
"""Update docs/v0_7/REQ_MATRIX.md rows: req_status.py STATE "verification" ID [ID ...]
STATE: DONE | IN_PROGRESS | NOT_STARTED | BLOCKED | NEEDS_VERIFICATION. IDs may be ranges (V07-0021..V07-0030)."""
import re, sys
STATES = {"DONE": "[DONE]", "IN_PROGRESS": "[IN PROGRESS]", "NOT_STARTED": "[NOT STARTED]",
          "BLOCKED": "[BLOCKED]", "NEEDS_VERIFICATION": "[NEEDS VERIFICATION]"}
state, verif, ids = STATES[sys.argv[1]], sys.argv[2], []
for a in sys.argv[3:]:
    if ".." in a:
        lo, hi = (int(x.split("-")[1]) for x in a.split(".."))
        ids += [f"V07-{i:04d}" for i in range(lo, hi + 1)]
    else:
        ids.append(a)
p = "docs/v0_7/REQ_MATRIX.md"
lines = open(p).read().split("\n")
n = 0
for i, l in enumerate(lines):
    cells = l.split(" | ")
    if len(cells) >= 8 and cells[0].lstrip("| ") in ids:
        cells[3] = state
        cells[7] = verif + " |"
        lines[i] = " | ".join(cells)
        n += 1
open(p, "w").write("\n".join(lines))
print(f"updated {n} row(s)")
