#!/usr/bin/env python3
"""B1-0012 / contract §029: machine-checkable acceptance of v1.0-beta1.

python3 tools/acceptance.py            report every acceptance family; exit 1 on a FAIL
python3 tools/acceptance.py --quiet    only the summary and the failures\npython3 tools/acceptance.py --beta2    also audit current beta2 requirement evidence

For every family (API-01 ... REL-02) it checks, from the files in the repository:

* requirements: each listed requirement exists in REQ_GRAPH.toml and is DONE with a
  passing evidence entry; a requirement that needs the owner's Windows PC may instead
  be NEEDS_VERIFICATION with a `verification` note (the family is then OWNER, never PASS);
* test references: every `file.rs::test`, `cargo test ... <filter>`, `--test` / `--example`
  target and `review:` file a requirement names still exists in the source;
* files and counts: the family's documents, assets and minimum counts;
* version labels and registered context actions (CONTEXT_MATRIX.md test names).

Not a QA application: it reads files and runs no build or test.
"""
import glob
import json
import os
import re
import sys
import tomllib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PLAN = os.path.join(ROOT, "docs", "v1_0_beta1")

# family: (title, requirements, owner-only requirements, files / globs with minimum counts)
FAMILIES = {
    "API-01": ("Lua runtime, versioned host API, no crash on bad scripts",
               ["B1-0020", "B1-0021", "B1-0022", "B1-0023", "B1-0024"], [], {"docs/LUA_API.md": 1}),
    "API-02": ("Notes, Track / Subtrack / Clip and CC APIs functional",
               ["B1-0026", "B1-0027", "B1-0028", "B1-0029", "B1-0030", "B1-0031", "B1-0032"], [],
               {"crates/plumspirit/src/lua/*_tests.rs": 10}),
    "API-03": ("Custom tool under Tools > Custom, Preview / Undo / presets, lifecycle",
               ["B1-0033", "B1-0034", "B1-0035", "B1-0045", "B1-0046", "B1-0047", "B1-0048", "B1-0049", "B1-0037",
                "B1-0038"], [],
               {"crates/plumspirit/examples-lua/*/plum-plugin.toml": 2, "crates/plumspirit/examples-lua/*/main.lua": 2}),
    "API-04": ("Lua permissions / budgets, cancellation / rollback",
               ["B1-0021", "B1-0025", "B1-0036", "B1-0039", "B1-0040", "B1-0041"], [],
               {"docs/v1_0_beta1/LUA_TEST_MATRIX.md": 1}),
    "DOC-01": ("Searchable human-written Help covering every legacy family",
               ["B1-0050", "B1-0051", "B1-0052"], [], {"assets/help/*.md": 20}),
    "CFG-01": ("Unified Settings, keybind overrides, 30 - 175 % UI scale",
               ["B1-0058", "B1-0059", "B1-0060", "B1-0062"], [], {"docs/v1_0_beta1/SETTINGS_MAP.md": 1}),
    "NPRO-01": ("Expressions for tick / gate / key / velocity / channel / subchannel",
                ["B1-0075", "B1-0076", "B1-0077"], [], {}),
    "NPRO-02": ("Multi-ranges with editable graphs, correct composition", ["B1-0078", "B1-0112"], [], {}),
    "UX-01": ("Toolbar / settings / onion / numeric input / Tab / dropdowns understandable",
              ["B1-0055", "B1-0056", "B1-0057", "B1-0061", "B1-0063", "B1-0064", "B1-0068", "B1-0069", "B1-0086"], [], {}),
    "UX-02": ("Overlays never hide notes; two independent playheads", ["B1-0065", "B1-0080", "B1-0081"], [], {}),
    "UX-03": ("128 / 256 fit and lock, optional Y beyond range, validated zoom",
              ["B1-0087", "B1-0088", "B1-0089", "B1-0090"], [], {}),
    "UX-04": ("Tooltips and American English", ["B1-0066", "B1-0067"], [], {"tools/us_english.py": 1}),
    "CLIP-01": ("Clip moved across Track / Subtrack keeps its count", ["B1-0095"], [], {}),
    "CLIP-02": ("Merge shows the merged data", ["B1-0096"], [], {}),
    "CLIP-03": ("Slice / Delete / Copy / Paste in the Track List", ["B1-0097", "B1-0010", "B1-0180", "B1-0181"], [],
                {"docs/v1_0_beta1/CONTEXT_MATRIX.md": 1}),
    "CLIP-04": ("Square colored header, transparent body, no clipped notes", ["B1-0098"], [], {}),
    "EDIT-01": ("Alt free rotation on all four corners", ["B1-0091"], [], {}),
    "EDIT-02": ("Scale Y duplication and its overlap cleanup", ["B1-0115", "B1-0116"], [], {}),
    "EDIT-03": ("General overlap prevention off by default, explicit policies", ["B1-0117"], [], {}),
    "EDIT-04": ("Duplicate V2 under Iterations with a stable id", ["B1-0099"], [], {}),
    "PPQN-01": ("MIDI PPQN 1 - 32,767; extended project PPQN honest", ["B1-0100", "B1-0102"], [], {}),
    "PPQN-02": ("BPM up to 60,000,000, tempo conversion valid or warned", ["B1-0101"], [], {}),
    "PPQN-03": ("Change PPQN converts every time element", ["B1-0103", "B1-0104"], [], {}),
    "GRAPH-01": ("Per-point smoothness, overshoot", ["B1-0112", "B1-0113"], [],
                 {"docs/references/v1_0_beta1/OVERSHOOT_AUDIT.md": 1}),
    "ANAL-01": ("Gate-range selection metric", ["B1-0105"], [], {}),
    "ANAL-02": ("Mute / Solo-sensitive Progressor on by default", ["B1-0106"], [], {}),
    "ANAL-03": ("Progressor and Selection Analytics in the Track List", ["B1-0107"], [], {}),
    "ANAL-04": ("NPS / Poly zoom and scroll, Tempo chart", ["B1-0108", "B1-0109"], [], {}),
    "COLOR-01": ("One color per keyboard key in both visualizers", ["B1-0110", "B1-0111"], [], {}),
    "MIDI-01": ("RSB ON / OFF keeps the ordered decoded events", ["B1-0123", "B1-0125"], [],
                {"docs/v1_0_beta1/MIDI_EXPORT_CERTIFICATION.md": 1}),
    "MIDI-02": ("tau duplicate tempo kept at track 0 tick 480", ["B1-0120", "B1-0126"], [], {}),
    "MIDI-03": ("160 RDR tempo on track 1 and same-tick order kept", ["B1-0127"], [], {}),
    "MIDI-04": ("Every representable track / event / velocity / CC preserved",
                ["B1-0121", "B1-0122", "B1-0124", "B1-0128", "B1-0129", "B1-0130"], [], {}),
    "AUDIO-01": ("Installed synths and KDMAPI selectable, internal fallback",
                 ["B1-0135", "B1-0136", "B1-0137"], ["B1-0139"], {"docs/v1_0_beta1/AUDIO_MATRIX.md": 1}),
    "AUDIO-02": ("Held-note pause / resume correct for the transport policy",
                 ["B1-0082", "B1-0083", "B1-0084", "B1-0138"], ["B1-0139"], {}),
    "REND-01": ("Player matches PFA imagery by the recorded tolerance",
                ["B1-0140", "B1-0141", "B1-0142", "B1-0143", "B1-0144", "B1-0145"], [],
                {"docs/PLAYER.md": 1}),
    "REND-02": ("Piano Roll stable layer priority, no shadows / gradients",
                ["B1-0092", "B1-0150", "B1-0153", "B1-0154", "B1-0155"], ["B1-0157"], {"docs/v1_0_beta1/RENDERER_AUDIT.md": 1}),
    "REND-03": ("Dense tiles fill the viewport, no flicker / outline / color collapse",
                ["B1-0151", "B1-0152", "B1-0156"], ["B1-0157"], {}),
    "PERF-01": ("1M / 10M / 100M / 250M benchmarks with results or not-run reasons", ["B1-0160"], [],
                {"docs/v1_0_beta1/BENCH_RESULTS.csv": 1}),
    "PERF-02": ("No unjustified regression, safe shutdown",
                ["B1-0161", "B1-0162", "B1-0163", "B1-0164", "B1-0165", "B1-0166", "B1-0170", "B1-0195"], [], {}),
    "BRAND-01": ("Official logo everywhere, placeholders gone", ["B1-0175", "B1-0176", "B1-0009"], ["B1-0190"],
                 {"assets/brand/*.png": 8, "assets/icon/plumspirit.ico": 1}),
    "REL-01": ("Canonical source sync, source / EXE hashes, recovery", ["B1-0006", "B1-0007", "B1-0194"], ["B1-0190"],
               {"docs/v1_0_beta1/CONTINUE.txt": 1, "tools/sync.py": 1}),
    "REL-02": ("Unfinished external tests and limitations stated honestly",
               ["B1-0001", "B1-0005", "B1-0008", "B1-0012", "B1-0185", "B1-0191", "B1-0192", "B1-0193"],
               ["B1-0186", "B1-0187"], {"docs/v1_0_beta1/KNOWN_LIMITATIONS.md": 1}),
}

# Requirements that may be accepted only on the owner's Windows PC.
OWNER_OK = {"B1-0139", "B1-0157", "B1-0186", "B1-0187", "B1-0190"}

VALUE_FLAGS = {"-p", "--package", "--test", "--example", "--bin", "--target", "--features",
               "--dir", "-Z", "--registry", "--mode", "--names"}


def read(p):
    with open(os.path.join(ROOT, p), encoding="utf-8") as f:
        return f.read()


class Source:
    """Names of test functions, files and modules in the workspace (for reference checks)."""

    def __init__(self):
        self.fns, self.mods, self.files = set(), set(), {}
        for p in glob.glob(os.path.join(ROOT, "crates", "**", "*.rs"), recursive=True):
            rel = os.path.relpath(p, ROOT).replace(os.sep, "/")
            if "/target/" in rel:
                continue
            text = open(p, encoding="utf-8", errors="replace").read()
            self.files[rel] = text
            stem = os.path.splitext(os.path.basename(p))[0]
            self.mods.add(stem)
            if stem == "mod":
                self.mods.add(os.path.basename(os.path.dirname(p)))
            self.fns.update(re.findall(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)", text))
            self.mods.update(re.findall(r"\bmod\s+([A-Za-z_][A-Za-z0-9_]*)", text))

    def has_filter(self, flt):
        segs = [s for s in flt.split("::") if s]
        if not segs:
            return True
        *heads, last = segs
        if any(h not in self.mods for h in heads):
            return False
        return last in self.mods or any(last in f for f in self.fns)

    def target(self, kind, name):
        return any(f.endswith(f"/{kind}/{name}.rs") or f.endswith(f"/{kind}/{name}/main.rs") for f in self.files)


def check_test_ref(src, t):
    """Problems with one `tests` entry of a requirement (empty list = fine)."""
    t = t.strip()
    if t.startswith("review:"):
        bad = []
        for part in re.split(r",\s*", t[len("review:"):].strip()):
            part = part.strip().split(" ")[0].strip("`")
            if not part or "<" in part:
                continue
            cands = [part, os.path.join("docs/references/v1_0_beta1", part), os.path.join("docs/v1_0_beta1", part)]
            if "{" in part or not any(os.path.exists(os.path.join(ROOT, c)) for c in cands):
                if "{" not in part:
                    bad.append(f"review file missing: {part}")
        return bad
    m = re.match(r"^(crates/[^\s:]+\.rs)::([A-Za-z0-9_:]+)", t)
    if m:
        path, name = m.group(1), m.group(2).split("::")[-1]
        if path not in src.files:
            return [f"file missing: {path}"]
        if not re.search(r"\bfn\s+" + re.escape(name) + r"\b", src.files[path]) and \
                not re.search(r"\bmod\s+" + re.escape(name) + r"\b", src.files[path]):
            return [f"test missing: {path}::{name}"]
        return []
    if re.match(r"^(env\s+\S+=\S+\s+)?cargo\s+(test|run|build|clippy)\b", t):
        toks = t.split()
        bad, i, after_dd = [], 0, False
        while i < len(toks):
            tok = toks[i]
            if tok in ("env", "cargo", "test", "run", "build", "clippy") or "=" in tok and not after_dd:
                i += 1
                continue
            if tok == "--":
                after_dd = True
            elif tok in ("--test", "--example") and i + 1 < len(toks):
                kind = "tests" if tok == "--test" else "examples"
                if not src.target(kind, toks[i + 1]):
                    bad.append(f"{tok} target missing: {toks[i + 1]}")
                i += 1
            elif tok in VALUE_FLAGS:
                i += 1
            elif tok.startswith("-") or tok.startswith("<") or tok.startswith(">") or tok in ("&&", "|"):
                if tok in (">", "&&", "|"):
                    break
            elif after_dd or re.match(r"^[A-Za-z_][A-Za-z0-9_:]*$", tok):
                if not after_dd or re.match(r"^[A-Za-z_][A-Za-z0-9_:]*$", tok):
                    if not src.has_filter(tok):
                        bad.append(f"test filter matches nothing: {tok}")
            i += 1
        return bad
    return []  # gates, scripts, manual steps: checked elsewhere


def version_labels():
    errs = []
    ws = tomllib.loads(read("Cargo.toml"))
    ver = ws.get("workspace", {}).get("package", {}).get("version") or ws.get("package", {}).get("version")
    # v1.0-Launch identity (L-S40): version 1.0.0 shown as "PlumSpirit v1.0-Launch".
    if ver != "1.0.0":
        errs.append(f"workspace version is {ver!r}, not 1.0.0")
    for p, needle in [("crates/plumspirit/src/main.rs", '"v1.0-Launch"'),
                      ("crates/plumspirit/windows/plumspirit.rc", "PlumSpirit v1.0-Launch"),
                      ("crates/plumspirit/windows/plumspirit.manifest", "PlumSpirit v1.0-Launch"),
                      ("README.md", "v1.0-Launch"), ("CHANGELOG.md", "v1.0-Launch (1.0.0)")]:
        if needle not in read(p):
            errs.append(f"{p}: no {needle}")
    rc = read("crates/plumspirit/windows/plumspirit.rc")
    if not re.search(r"FILEVERSION\s+1\s*,\s*0\s*,\s*0", rc):
        errs.append("plumspirit.rc: FILEVERSION is not 1,0,0,x")
    for p in ("assets/plum_placeholder.png", "assets/generated"):
        if os.path.exists(os.path.join(ROOT, p)) and subprocess_tracked(p):
            errs.append(f"placeholder branding still tracked: {p}")
    return errs


def subprocess_tracked(p):
    import subprocess
    out = subprocess.run(["git", "-C", ROOT, "ls-files", p], capture_output=True, text=True).stdout
    return bool(out.strip())


def context_actions(src):
    """Every test the context matrix names exists."""
    errs, names = [], set()
    for row in read("docs/v1_0_beta1/CONTEXT_MATRIX.md").splitlines():
        if not row.startswith("|") or row.startswith("|---"):
            continue
        last = row.rstrip("|").split("|")[-1]
        for code in re.findall(r"`([^`]+)`", last):
            for tok in re.findall(r"[A-Za-z_][A-Za-z0-9_:]*", code):
                if "_" in tok and not tok.startswith("_") and tok.count("::") <= 3 and not tok.endswith(".rs"):
                    names.add(tok.rstrip(":"))
    for n in sorted(names):
        last = n.split("::")[-1]
        if last.endswith("_") or last == "":
            continue
        if last not in src.fns and last not in src.mods and not any(f.startswith(last) for f in src.fns) \
                and not any(re.search(r"\b" + re.escape(last) + r"\b", t) for t in src.files.values()):
            errs.append(f"CONTEXT_MATRIX names a missing test: {n}")
    return errs, len(names)


def beta2_acceptance(src):
    """Audit current beta2 records; owner-only hardware remains explicitly OWNER."""
    plan = tomllib.loads(read("docs/v1_0_beta2/REQ_GRAPH.toml"))
    evidence = [json.loads(l) for l in read("docs/v1_0_beta2/EVIDENCE.jsonl").splitlines() if l.strip()]
    passing = {r for e in evidence if e.get("result") == "pass" for r in e.get("requirement_ids", [])}
    counts = {"PASS": 0, "OWNER": 0, "FAIL": 0}
    for rid, r in plan["req"].items():
        problems = []
        if r.get("status") == "DONE":
            if rid not in passing:
                problems.append("DONE without passing evidence")
            for t in r.get("tests", []):
                problems += check_test_ref(src, t)
            state = "FAIL" if problems else "PASS"
        elif rid == "B2-R03" and r.get("status") == "NEEDS_VERIFICATION" and r.get("verification"):
            state = "OWNER"
        else:
            state = "FAIL"
            problems.append("status " + str(r.get("status")))
        counts[state] += 1
        if state != "PASS":
            print(f"{state:<5} beta2 {rid}: " + ("; ".join(problems) or r.get("verification", "")))
    print("beta2 requirements: " + ", ".join(f"{n} {st}" for st, n in counts.items()))
    return counts["FAIL"]


def beta2_owner_signoff():
    plan = tomllib.loads(read("docs/v1_0_beta2/REQ_GRAPH.toml"))
    if plan["req"].get("B2-R03", {}).get("status") != "DONE":
        return False
    return any(e.get("kind") == "manual" and e.get("result") == "pass"
               and "B2-R03" in e.get("requirement_ids", [])
               for e in (json.loads(l) for l in read("docs/v1_0_beta2/EVIDENCE.jsonl").splitlines() if l.strip()))


def main(argv):
    quiet = "--quiet" in argv
    carried_owner_signed = "--beta2" in argv and beta2_owner_signoff()
    plan = tomllib.loads(read("docs/v1_0_beta1/REQ_GRAPH.toml"))
    reqs = plan["req"]
    evidence = [json.loads(l) for l in read("docs/v1_0_beta1/EVIDENCE.jsonl").splitlines() if l.strip()]
    passing = {}
    for e in evidence:
        if e.get("result") == "pass":
            for r in e.get("requirement_ids", []):
                passing.setdefault(r, []).append(e["id"])
    src = Source()

    global_errs = version_labels()
    ctx_errs, ctx_n = context_actions(src)
    global_errs += ctx_errs
    ref_errs = {}
    for rid, r in reqs.items():
        for t in r.get("tests", []):
            for p in check_test_ref(src, t):
                ref_errs.setdefault(rid, []).append(p)

    covered = set()
    results = {"PASS": [], "OWNER": [], "FAIL": []}
    for fam, (title, need, owner, files) in FAMILIES.items():
        problems, waiting = [], []
        for rid in need + owner:
            covered.add(rid)
            r = reqs.get(rid)
            if r is None:
                problems.append(f"{rid} not in REQ_GRAPH")
                continue
            st = r.get("status")
            if st == "DONE":
                if rid not in passing:
                    problems.append(f"{rid} DONE without passing evidence")
            elif rid in owner and rid in OWNER_OK and st == "NEEDS_VERIFICATION" and r.get("verification"):
                if not carried_owner_signed:
                    waiting.append(rid)
            else:
                problems.append(f"{rid} is {st}")
            problems += [f"{rid}: {p}" for p in ref_errs.get(rid, [])]
        for pat, n in files.items():
            got = len(glob.glob(os.path.join(ROOT, pat)))
            if got < n:
                problems.append(f"{pat}: {got} file(s), need {n}")
        state = "FAIL" if problems else ("OWNER" if waiting else "PASS")
        results[state].append(fam)
        if not quiet or state == "FAIL":
            extra = f" — waiting for the owner's Windows check: {', '.join(waiting)}" if waiting else ""
            print(f"{state:<5} {fam:<9} {title}{extra}")
            for p in problems:
                print(f"        {p}")

    # Every requirement belongs to some family or is a process requirement listed here.
    process = {"B1-0002", "B1-0003", "B1-0004", "B1-0011", "B1-0013", "B1-0196"}
    loose = sorted(set(reqs) - covered - process)
    if loose:
        global_errs.append("requirements in no acceptance family: " + ", ".join(loose))

    # Benchmark ladder: every rung has a row (a result or a NOT RUN reason).
    bench = read("docs/v1_0_beta1/BENCH_RESULTS.csv")
    for rung in ("1,000,000", "10,000,000", "100,000,000", "250,000,000"):
        if not any("ladder" in l and rung + " notes" in l for l in bench.splitlines()):
            global_errs.append(f"BENCH_RESULTS.csv has no ladder row of {rung} notes")

    for e in global_errs:
        print("FAIL  global   ", e)
    print(f"\nfamilies: {len(results['PASS'])} PASS, {len(results['OWNER'])} OWNER (Windows playtest), "
          f"{len(results['FAIL'])} FAIL of {len(FAMILIES)}; context-matrix tests checked: {ctx_n}; "
          f"requirement test references checked: {sum(len(r.get('tests', [])) for r in reqs.values())}")
    if carried_owner_signed:
        print("Carried owner checks accepted on beta2 under B2-R03; historical beta1 records retained.")
    beta2_failures = beta2_acceptance(src) if "--beta2" in argv else 0
    return 1 if results["FAIL"] or global_errs or beta2_failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
