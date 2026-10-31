#!/usr/bin/env python3
"""Tests for tools/capsule.py: python3 tools/test_capsule.py"""
import contextlib
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import capsule  # noqa: E402

GRAPH = '''
[meta]
version = "9.9"
active_slice = "S01"
canonical_repo = "PC folder"
phase = "testing"
owner_todo = ["import the bundle"]

[slice.S00]
title = "done slice"
status = "DONE"

[slice.S01]
title = "active slice"
status = "ACTIVE"
goal = "make undo generic"
files = ["crates/plumspirit_engine/src/history.rs"]

[req.T-0001]
name = "base"
status = "DONE"
slice = "S00"
text = "base requirement"
tests = ["cargo test base"]

[req.T-0002]
name = "generic undo"
status = "IN_PROGRESS"
slice = "S01"
dependencies = ["T-0001"]
perf = true
source_modules = ["history.rs"]
text = "one undo engine for every tool"
tests = ["cargo test -p plumspirit_engine --lib history::"]

[req.T-0003]
name = "later"
status = "NOT_STARTED"
slice = "S01"
dependencies = ["T-0002"]
text = "after T-0002"
'''

IMPACT = '''
fallback = "everything"

[[rule]]
name = "history"
paths = ["crates/plumspirit_engine/src/history"]
fast = ["cargo test -p plumspirit_engine --lib history::", "cargo check -p plumspirit"]
heavy = ["cargo test --release --test stress_v08 -- --ignored"]
bench = ["cargo run --release --example perf_report"]

[[rule]]
name = "engine"
paths = ["crates/plumspirit_engine/"]
fast = ["cargo check -p plumspirit"]

[[rule]]
name = "everything"
paths = ["<fallback>"]
fast = ["cargo test --workspace"]

[[rule]]
name = "docs"
paths = ["docs/"]
none = true

[gates.integration]
cmds = ["cargo test --workspace"]

[gates.release]
cmds = ["cargo test --workspace --release"]
'''

CATALOG = '''
experimental_active_count = 1

[[tool]]
id = "tool_a"
name = "Tool A"
family = "fl"
category = "piano-roll"
rule = "25"
[[tool.function]]
id = "f1"
name = "One"
status = "active"
state = "IMPLEMENTED"
[[tool.function]]
id = "f0"
name = "Old"
status = "legacy"

[[tool]]
id = "mute"
name = "Mute"
family = "fl"
category = "piano-roll"
rule = "one-click"

[[tool]]
id = "exp_a"
name = "Exp A"
family = "experimental"
category = "experimental"
rule = "25"
'''

SLICE = "# CURRENT SLICE\n\n## Current objective\nSlice: S01\n\n## NEXT EXACT ACTION\nWrite the history adapter.\n"
EVID = json.dumps({"id": "E0001", "timestamp": "2026-10-05T12:00Z", "requirement_ids": ["T-0001"],
                   "kind": "test", "result": "pass", "summary": "base ok"}) + "\n"
HEADER = ",".join(capsule.BENCH_HEADER) + "\n"


class Fixture(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = self.tmp.name
        self.d = os.path.join(self.root, "docs", "v9")
        os.makedirs(self.d)
        self.write("REQ_GRAPH.toml", GRAPH)
        self.write("TEST_IMPACT.toml", IMPACT)
        self.write("CURRENT_SLICE.md", SLICE)
        self.write("EVIDENCE.jsonl", EVID)
        with open(os.path.join(self.root, "Cargo.toml"), "w") as f:
            f.write('[workspace.package]\nversion = "0.9.0"\n')
        subprocess.run(["git", "init", "-q", self.root], check=True)

    def tearDown(self):
        self.tmp.cleanup()

    def write(self, name, text):
        with open(os.path.join(self.d, name), "w", encoding="utf-8") as f:
            f.write(text)

    def plan(self):
        return capsule.Plan(self.root, "docs/v9")

    def run_cli(self, *argv):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = capsule.main(["--dir", "docs/v9", *argv], root=self.root)
        return rc, out.getvalue()


class CheckTests(Fixture):
    def test_valid_plan_passes(self):
        self.assertEqual(self.plan().check(), [])

    def completed_plan(self):
        plan = self.plan()
        for s in plan.slices.values():
            s["status"] = "DONE"
        for r in plan.reqs.values():
            r["status"] = "DONE"
            r["tests"] = ["cargo test base"]
        plan.evidence.append({"id": "E0002", "requirement_ids": list(plan.reqs),
                              "kind": "bench", "result": "pass", "summary": "final gate"})
        return plan

    def test_completed_plan_keeps_the_final_slice_as_a_handoff_anchor(self):
        self.assertEqual(self.completed_plan().check(), [])

    def test_done_anchor_cannot_hide_an_unfinished_requirement_or_slice(self):
        plan = self.completed_plan()
        plan.reqs["T-0003"]["status"] = "NOT_STARTED"
        self.assertTrue(any("active slice" in e for e in plan.check()))
        plan = self.completed_plan()
        plan.slices["S00"]["status"] = "PLANNED"
        self.assertTrue(any("active slice" in e for e in plan.check()))

    def test_contract_statuses_only(self):
        self.write("REQ_GRAPH.toml", GRAPH.replace('status = "NOT_STARTED"', 'status = "TODO"'))
        self.assertTrue(any("status 'TODO'" in e for e in self.plan().check()))

    def test_unknown_dependency_and_cycle_are_reported(self):
        self.write("REQ_GRAPH.toml", GRAPH.replace('dependencies = ["T-0001"]', 'dependencies = ["T-0003", "T-9999"]'))
        errs = self.plan().check()
        self.assertTrue(any("T-9999 unknown" in e for e in errs), errs)
        self.assertTrue(any("cycle" in e for e in errs), errs)

    def test_done_requires_passing_evidence_tests_and_bench_when_perf(self):
        g = GRAPH.replace('name = "generic undo"\nstatus = "IN_PROGRESS"', 'name = "generic undo"\nstatus = "DONE"')
        self.write("REQ_GRAPH.toml", g)
        errs = self.plan().check()
        self.assertIn("T-0002: DONE without passing evidence", errs)
        self.assertIn("T-0002: performance-sensitive and DONE without bench evidence", errs)
        self.write("EVIDENCE.jsonl", EVID + json.dumps(
            {"id": "E0002", "requirement_ids": ["T-0002"], "kind": "bench", "result": "pass", "summary": "x"}) + "\n")
        self.assertEqual(self.plan().check(), [])

    def test_old_evidence_key_is_still_read(self):
        g = GRAPH.replace('name = "generic undo"\nstatus = "IN_PROGRESS"', 'name = "generic undo"\nstatus = "DONE"')
        self.write("REQ_GRAPH.toml", g)
        self.write("EVIDENCE.jsonl", EVID + json.dumps(
            {"id": "E0002", "reqs": ["T-0002"], "kind": "bench", "result": "pass", "summary": "x"}) + "\n")
        self.assertEqual(self.plan().check(), [])

    def test_done_with_unfinished_dependency_is_an_error(self):
        g = GRAPH.replace('name = "later"\nstatus = "NOT_STARTED"', 'name = "later"\nstatus = "DONE"')
        g = g.replace('text = "after T-0002"', 'text = "after T-0002"\nevidence_waived = "test"')
        self.write("REQ_GRAPH.toml", g)
        self.assertIn("T-0003: DONE but dependency T-0002 is not", self.plan().check())

    def test_blocked_needs_a_reason_and_acceptance_evidence_must_exist(self):
        g = GRAPH.replace('name = "later"\nstatus = "NOT_STARTED"', 'name = "later"\nstatus = "BLOCKED"\nacceptance_evidence = ["E0099"]')
        self.write("REQ_GRAPH.toml", g)
        errs = self.plan().check()
        self.assertIn("T-0003: BLOCKED without a `blocked` reason", errs)
        self.assertIn("T-0003: acceptance evidence E0099 unknown", errs)

    def test_slice_file_must_match_active_slice(self):
        self.write("CURRENT_SLICE.md", SLICE.replace("Slice: S01", "Slice: S00"))
        self.assertTrue(any("CURRENT_SLICE.md is slice S00" in e for e in self.plan().check()))

    def test_a_done_slice_needs_every_requirement_done(self):
        self.write("REQ_GRAPH.toml", GRAPH.replace('name = "base"\nstatus = "DONE"', 'name = "base"\nstatus = "NOT_STARTED"'))
        self.assertIn("slice S00 is DONE but T-0001 is NOT_STARTED", self.plan().check())

    def test_only_one_active_slice(self):
        self.write("REQ_GRAPH.toml", GRAPH.replace('title = "done slice"\nstatus = "DONE"',
                                                   'title = "done slice"\nstatus = "ACTIVE"'))
        self.assertTrue(any("more than one ACTIVE" in e for e in self.plan().check()))

    def test_bad_evidence_lines_are_reported(self):
        self.write("EVIDENCE.jsonl", EVID + "{not json\n" + json.dumps(
            {"id": "E0001", "requirement_ids": ["T-7"], "kind": "guess", "result": "ok"}) + "\n")
        errs = self.plan().check()
        for want in ("line 2", "duplicate id", "kind 'guess'", "result 'ok'", "unknown requirement T-7"):
            self.assertTrue(any(want in e for e in errs), (want, errs))

    def test_gates_are_required(self):
        self.write("TEST_IMPACT.toml", IMPACT.split("[gates.integration]")[0])
        self.assertIn("TEST_IMPACT gates.integration.cmds missing", self.plan().check())

    def test_a_rule_needs_commands_or_none(self):
        self.write("TEST_IMPACT.toml", IMPACT + '\n[[rule]]\nname = "empty"\npaths = ["x/"]\n')
        self.assertTrue(any("rule 4 (empty)" in e for e in self.plan().check()))
        self.write("TEST_IMPACT.toml", IMPACT + '\n[[rule]]\nname = "sweep"\npaths = ["x/"]\nheavy = ["sweep"]\n')
        self.assertEqual(self.plan().check(), [])

    def test_bench_csv_header_and_width(self):
        self.write("BENCH_RESULTS.csv", HEADER + "a,b\n")
        self.assertTrue(any("BENCH_RESULTS.csv line 2: 2 fields" in e for e in self.plan().check()))
        self.write("BENCH_RESULTS.csv", "x,y\n")
        self.assertTrue(any("BENCH_RESULTS.csv: header" in e for e in self.plan().check()))


class CatalogTests(Fixture):
    def test_catalog_schema_and_gate_counts(self):
        self.write("TOOL_CATALOG.toml", CATALOG)
        self.assertEqual(self.plan().check(), [])
        rows, errs = self.plan().catalog_counts()
        self.assertIn(("tool_a", "piano-roll", "25", 1, 1), rows)
        self.assertIn("tool_a: 1 active advanced functions, need 25", errs)
        self.assertIn("exp_a: 0 active advanced functions, need 25", errs)
        self.assertIn("0 generator-simple tools, need 5", errs)
        self.assertFalse(any(e.startswith("mute") for e in errs))
        self.assertEqual(self.run_cli("catalog", "--gate")[0], 1)
        self.assertEqual(self.run_cli("catalog")[0], 0)

    def test_registry_dump_must_match_the_v08_functions(self):
        self.write("TOOL_CATALOG.toml", CATALOG.replace('origin_marker', ''))
        cat = CATALOG.replace('name = "One"\nstatus = "active"\nstate = "IMPLEMENTED"', 'name = "One"\nstatus = "active"\nstate = "IMPLEMENTED"\norigin = "v0.8"').replace('id = "f1"', 'id = "a01"')
        self.write("TOOL_CATALOG.toml", cat)
        ok = "TOOL\ttool_a\tTool A\tFl\t3\nADV\t1\tOne\t2\nTOOL\tmute\tMute\tFl\t0\nTOOL\texp_a\tExp A\tExperimental\t0\n"
        self.assertEqual(self.plan().catalog_vs_registry(ok), [])
        bad = ok.replace("ADV\t1\tOne", "ADV\t1\tRenamed") + "TOOL\tnew_tool\tNew\tFl\t0\n"
        errs = self.plan().catalog_vs_registry(bad)
        self.assertIn("tool_a/a01: registry 'Renamed', catalog 'One'", errs)
        self.assertIn("registry tool new_tool missing from TOOL_CATALOG", errs)
        self.assertIn("catalog tool tool_a not in the registry", self.plan().catalog_vs_registry("TOOL\tmute\tMute\tFl\t0\n"))
        # beta1 B1-S05: a v0.9 tool whose registry functions are contract
        # functions (Paste Repeat (notes)) is compared by name too.
        self.write("TOOL_CATALOG.toml", cat.replace('origin = "v0.8"', 'origin = "contract"'))
        self.assertEqual(self.plan().catalog_vs_registry(ok), [])
        self.assertIn("tool_a/a01: registry 'Renamed', catalog 'One'", self.plan().catalog_vs_registry(bad))

    def test_fixed_count_rule(self):
        self.write("TOOL_CATALOG.toml", CATALOG + '\n[[tool]]\nid = "pr"\nname = "PR"\nfamily = "PasteRepeat"\ncategory = "paste-repeat"\nrule = "fixed"\nexpected = 2\n[[tool.function]]\nid = "x"\nname = "X"\nstatus = "active"\n')
        self.assertIn("pr: 1 active functions, need 2", self.plan().catalog_counts()[1])

    def test_duplicate_function_ids_and_bad_status(self):
        bad = CATALOG.replace('id = "f0"', 'id = "f1"').replace('status = "legacy"', 'status = "old"')
        self.write("TOOL_CATALOG.toml", bad)
        errs = self.plan().check()
        self.assertTrue(any("function id 'f1' missing or duplicate" in e for e in errs), errs)
        self.assertTrue(any("status must be active or legacy" in e for e in errs), errs)

    def test_legacy_map_must_point_at_catalog_tools(self):
        self.write("TOOL_CATALOG.toml", CATALOG)
        self.write("LEGACY_MAP.toml", '[[legacy]]\ntool = "ghost"\n')
        self.assertTrue(any("legacy tool 'ghost'" in e for e in self.plan().check()))

    def test_legacy_map_entries_agree_with_catalog_categories_and_statuses(self):
        cat = CATALOG.replace('category = "piano-roll"\nrule = "one-click"',
                              'category = "one-click"\nrule = "one-click"')
        cat = cat.replace('category = "experimental"', 'category = "legacy-experimental"')
        self.write("TOOL_CATALOG.toml", cat)
        good = ('[[legacy]]\ntool = "mute"\n\n[[legacy]]\ntool = "exp_a"\n\n'
                '[[legacy]]\ntool = "tool_a"\nfunction = "f0"\n')
        self.write("LEGACY_MAP.toml", good)
        errs = [e for e in self.plan().check() if "LEGACY" in e or "legacy" in e]
        self.assertEqual(errs, [])
        bad = ('[[legacy]]\ntool = "tool_a"\n\n[[legacy]]\ntool = "mute"\n\n'
               '[[legacy]]\ntool = "tool_a"\nfunction = "f1"\n')
        self.write("LEGACY_MAP.toml", bad)
        errs = self.plan().check()
        for want in ("legacy tool tool_a has category 'piano-roll'",
                     "legacy function tool_a/f1 is not a legacy function",
                     "exp_a: category legacy-experimental without a LEGACY_MAP"):
            self.assertTrue(any(want in e for e in errs), (want, errs))


DESIGN = '''[[tool.function]]
id = "n01"
name = "Gate"
status = "active"
state = "PROPOSED"
origin = "new"
order = 110
params = 2
summary = "Thins steps."
controls = ["Steps (Int 1–64)", "Rest (Choice: silence / hold)"]
sweeps = ["Steps"]
growth = "Thin"
preview = "Viewport"
purpose = "count:euclid-mask"
shared = ["euclid"]
rubric = { distinct = 3, black_midi = 3, depth = 2, experiment = 2, interaction = 2, perf = 3, preview = 3, test = 3 }
'''


class DesignTests(Fixture):
    """V09-S32 design fields of new functions (docs/v0_9/TOOL_DESIGN.md)."""

    def cat(self, *designs):
        return CATALOG.replace('[[tool.function]]\nid = "f0"', "\n".join(designs) + '[[tool.function]]\nid = "f0"')

    def test_a_complete_design_passes(self):
        self.write("TOOL_CATALOG.toml", self.cat(DESIGN))
        self.assertEqual(self.plan().check(), [])

    def test_bad_design_fields_are_named(self):
        bad = (DESIGN.replace('"Rest (Choice: silence / hold)"', '"Rest"')
               .replace('sweeps = ["Steps"]', 'sweeps = ["Rest", "Ghost"]')
               .replace('order = 110', 'order = 950')
               .replace('growth = "Thin"', 'growth = "Grow"')
               .replace('distinct = 3', 'distinct = 1'))
        self.write("TOOL_CATALOG.toml", self.cat(bad))
        errs = "\n".join(self.plan().check())
        for want in ("control 'Rest' is not 'Name (Type …)'", "sweep target 'Ghost'", "order 950 outside",
                     "growth 'Grow'", "distinct 1"):
            self.assertIn(want, errs)

    def test_missing_field_reserved_and_random_purposes(self):
        self.write("TOOL_CATALOG.toml", self.cat(DESIGN.replace('preview = "Viewport"\n', '')))
        self.assertTrue(any("design field preview missing" in e for e in self.plan().check()))
        self.write("TOOL_CATALOG.toml", self.cat(DESIGN.replace("count:euclid-mask", "control:sweep")))
        self.assertTrue(any("belongs to a mandatory service" in e for e in self.plan().check()))
        self.write("TOOL_CATALOG.toml", self.cat(DESIGN.replace("count:euclid-mask", "pitch:scatter-random")))
        self.assertTrue(any("generic random purpose outside the Randomizer" in e for e in self.plan().check()))

    def test_purposes_and_orders_must_differ_in_a_tool(self):
        twin = DESIGN.replace('"n01"', '"n02"').replace('"Gate"', '"Gate two"')
        self.write("TOOL_CATALOG.toml", self.cat(DESIGN, twin))
        errs = "\n".join(self.plan().check())
        self.assertIn("functions share a purpose ['count:euclid-mask']", errs)
        self.assertIn("new functions share a composition order [110]", errs)
        old = CATALOG.replace('state = "IMPLEMENTED"', 'state = "IMPLEMENTED"\npurpose = "count:euclid-mask"')
        self.write("TOOL_CATALOG.toml", old.replace('[[tool.function]]\nid = "f0"', DESIGN + '[[tool.function]]\nid = "f0"'))
        self.assertTrue(any("share a purpose" in e for e in self.plan().check()))
        self.write("TOOL_CATALOG.toml", CATALOG.replace('state = "IMPLEMENTED"', 'state = "IMPLEMENTED"\npurpose = "nothing"'))
        self.assertTrue(any("tool_a/f1: purpose 'nothing'" in e for e in self.plan().check()))

    def test_rubric_interaction_and_preview_follow_the_fields(self):
        self.write("TOOL_CATALOG.toml", self.cat(DESIGN.replace('sweeps = ["Steps"]', 'sweeps = []')))
        self.assertTrue(any("interaction 2 with 0 sweep target(s)" in e for e in self.plan().check()))
        self.write("TOOL_CATALOG.toml", self.cat(DESIGN.replace('preview = "Viewport"', 'preview = "Worker"')))
        self.assertTrue(any("preview score 3 does not match preview Worker" in e for e in self.plan().check()))

    def test_retired_ids_are_never_reused(self):
        cat = self.cat(DESIGN).replace('rule = "25"\n', 'rule = "25"\nretired = ["n01"]\n', 1)
        self.write("TOOL_CATALOG.toml", cat)
        self.assertTrue(any("retired function ids reused ['n01']" in e for e in self.plan().check()))

    def test_review_lists_same_names_and_cross_family_purposes(self):
        exp = ('\n[[tool]]\nid = "exp_b"\nname = "Exp B"\nfamily = "experimental"\ncategory = "experimental"\nrule = "25"\n'
               + DESIGN.replace('"Gate"', '"One"'))
        self.write("TOOL_CATALOG.toml", self.cat(DESIGN) + exp)
        lines = self.plan().catalog_review()
        self.assertIn("SAME NAME 'one': tool_a, exp_b", lines)
        self.assertIn("SHARED-X experimental/fl count:euclid-mask: exp_b, tool_a", lines)
        self.assertTrue(any(line.startswith("tool_a") and "1 new" in line for line in lines))

    def run_design(self, body, *flags):
        import catalog_design
        spec = os.path.join(self.root, "designs.py")
        with open(spec, "w", encoding="utf-8") as f:
            f.write(body)
        old = catalog_design.CATALOG
        catalog_design.CATALOG = os.path.join(self.d, "TOOL_CATALOG.toml")
        argv = sys.argv
        try:
            sys.argv = ["catalog_design.py", *flags, spec]
            with contextlib.redirect_stdout(io.StringIO()):
                catalog_design.main()
        finally:
            sys.argv, catalog_design.CATALOG = argv, old
        return {f["id"]: f for f in self.plan().catalog["tool"][0]["function"]}

    def test_replace_keeps_ids_by_name_and_retires_dropped_ones(self):
        self.write("TOOL_CATALOG.toml", CATALOG)
        r = 'R = (3, 3, 2, 2, 2, 3, 3, 3)\n'
        g = '("Gate", "Thins.", ["Steps (Int 1–64)"], ["Steps"], "Thin", "Viewport", "count:mask", [], R)'
        w = '("Walk", "Walks.", ["Stride (Int 1–9)"], ["Stride"], "Map", "Viewport", "order:walk", [], R)'
        c = '("Cap", "Caps.", ["Most (Int 1–9)"], ["Most"], "Thin", "Viewport", "count:cap", [], R)'
        self.run_design(r + f'D = {{"tool_a": [{g}, {w}]}}\n')
        fs = self.run_design(r + f'D = {{"tool_a": [{c}, {g}]}}\n', "--replace")
        self.assertEqual(self.plan().check(), [])
        self.assertEqual((fs["n01"]["name"], fs["n03"]["name"]), ("Gate", "Cap"))
        self.assertNotIn("n02", fs)
        self.assertEqual(self.plan().catalog["tool"][0]["retired"], ["n02"])
        self.assertEqual([fs["n03"]["order"], fs["n01"]["order"]], [110, 120])
        fs = self.run_design(r + f'D = {{"tool_a": [{c}, {g}, {w}]}}\n', "--replace")
        self.assertEqual(fs["n04"]["name"], "Walk")
        self.assertEqual(self.plan().check(), [])

    def test_catalog_design_writes_blocks_and_purposes_that_pass(self):
        import catalog_design
        spec = os.path.join(self.root, "designs.py")
        with open(spec, "w", encoding="utf-8") as f:
            f.write('P = {"tool_a": {"f1": "time:one"}}\n'
                    'R = (3, 3, 2, 2, 2, 3, 3, 3)\n'
                    'W = (3, 3, 2, 2, 0, 3, 1, 3)\n'
                    'D = {"tool_a": [("Gate", "Thins.", ["Steps (Int 1–64)"], ["Steps"], "Thin", "Viewport", "count:mask", [], R),\n'
                    '                ("Cap", "Caps.", ["Most (Int 1–9)"], [], "Thin", "Worker", "count:cap", [], W, 700),\n'
                    '                ("Walk", "Walks.", ["Stride (Int 1–9)"], ["Stride"], "Map", "Viewport", "order:walk", [], R)]}\n')
        self.write("TOOL_CATALOG.toml", CATALOG)
        old = catalog_design.CATALOG
        catalog_design.CATALOG = os.path.join(self.d, "TOOL_CATALOG.toml")
        argv = sys.argv
        try:
            sys.argv = ["catalog_design.py", spec]
            with contextlib.redirect_stdout(io.StringIO()):
                catalog_design.main()
        finally:
            sys.argv, catalog_design.CATALOG = argv, old
        self.assertEqual(self.plan().check(), [])
        fs = {f["id"]: f for f in self.plan().catalog["tool"][0]["function"]}
        self.assertEqual(fs["f1"]["purpose"], "time:one")
        self.assertEqual([fs[k]["order"] for k in ("n01", "n02", "n03")], [110, 700, 120])
        self.assertEqual(fs["n02"]["state"], "PROPOSED")


class ImpactTests(Fixture):
    def test_rules_are_unioned_in_order_without_duplicates(self):
        fast, heavy, bench, unmapped = self.plan().impact_for(["crates/plumspirit_engine/src/history/journal.rs"])
        self.assertEqual(fast, ["cargo test -p plumspirit_engine --lib history::", "cargo check -p plumspirit"])
        self.assertEqual(heavy, ["cargo test --release --test stress_v08 -- --ignored"])
        self.assertEqual(bench, ["cargo run --release --example perf_report"])
        self.assertEqual(unmapped, [])

    def test_unmapped_code_uses_the_fallback_and_docs_need_nothing(self):
        fast, _, _, unmapped = self.plan().impact_for(["build.rs", "docs/X.md", "README.md"])
        self.assertEqual(fast, ["cargo test --workspace"])
        self.assertEqual(unmapped, ["build.rs"])

    def test_windows_separators(self):
        fast, _, _, _ = self.plan().impact_for(["crates\\plumspirit_engine\\src\\model\\mod.rs"])
        self.assertEqual(fast, ["cargo check -p plumspirit"])


class CliTests(Fixture):
    def test_status_edits_only_the_named_requirement(self):
        rc, _ = self.run_cli("status", "BLOCKED", "T-0003")
        self.assertEqual(rc, 0)
        p = self.plan()
        self.assertEqual(p.reqs["T-0003"]["status"], "BLOCKED")
        self.assertEqual(p.reqs["T-0002"]["status"], "IN_PROGRESS")
        with self.assertRaises(SystemExit):
            self.run_cli("status", "DONE", "T-4242")

    def test_evidence_gets_the_next_id_and_contract_fields(self):
        rc, out = self.run_cli("evidence", "--reqs", "T-0002", "--kind", "bench", "--result", "pass",
                               "--summary", "1M notes", "--num", "ms=12.5", "--num", "notes=1000000",
                               "--test", "cargo bench", "--fixture", "fx.mid")
        self.assertEqual((rc, out.strip()), (0, "recorded E0002"))
        e = self.plan().evidence[-1]
        self.assertEqual((e["requirement_ids"], e["benchmark"], e["test_name"], e["reference_fixture"]),
                         (["T-0002"], {"ms": 12.5, "notes": 1000000}, "cargo bench", "fx.mid"))
        self.assertIn("timestamp", e)
        self.assertIn("commit", e)
        with self.assertRaises(SystemExit):
            self.run_cli("evidence", "--reqs", "NOPE", "--kind", "test", "--result", "pass", "--summary", "x")

    def test_field_sets_or_replaces_one_line(self):
        self.assertEqual(self.run_cli("field", "T-0003", "tests", '["cargo test later"]')[0], 0)
        self.assertEqual(self.plan().reqs["T-0003"]["tests"], ["cargo test later"])
        self.run_cli("field", "T-0003", "tests", '["a", "b"]')
        self.run_cli("field", "T-0002", "implementation_commit", '"abc1234"')
        p = self.plan()
        self.assertEqual(p.reqs["T-0003"]["tests"], ["a", "b"])
        self.assertEqual(p.reqs["T-0002"]["implementation_commit"], "abc1234")
        self.assertEqual(p.reqs["T-0002"]["tests"], ["cargo test -p plumspirit_engine --lib history::"])
        with self.assertRaises(SystemExit):
            self.run_cli("field", "T-0003", "Bad Key", "1")

    def test_ready_lists_not_started_with_done_dependencies_only(self):
        self.assertEqual(self.plan().ready(), [])
        self.run_cli("status", "DONE", "T-0002")
        self.assertEqual(self.plan().ready(), ["T-0003"])

    def test_slice_and_show_print_full_entries(self):
        rc, out = self.run_cli("slice")
        self.assertEqual(rc, 0)
        self.assertIn("[T-0002] generic undo — IN_PROGRESS (slice S01, perf)", out)
        self.assertIn("one undo engine for every tool", out)
        self.assertIn("[T-0003]", out)
        self.assertNotIn("[T-0001]", out)
        rc, out = self.run_cli("show", "T-0001")
        self.assertIn("base requirement", out)
        self.assertIn("evidence: E0001 test pass", out)

    def test_capsule_holds_the_contract_sections_and_stays_short(self):
        self.write("TOOL_CATALOG.toml", CATALOG)
        self.write("BENCH_RESULTS.csv", HEADER)
        rc, _ = self.run_cli("capsule")
        self.assertEqual(rc, 0)
        text = capsule.read(os.path.join(self.d, "SESSION_CAPSULE.md"))
        for want in ("App version 0.9.0", "## Dirty files at generation", "Current slice S01 — active slice",
                     "make undo generic", "T-0002 [IN_PROGRESS] generic undo", "Write the history adapter.",
                     "`cargo test -p plumspirit_engine --lib history::`", "Benchmarks:", "## Completed slices",
                     "S00 (done slice)", "## Known blockers", "## Test status", "## Benchmark status",
                     "BENCH_RESULTS.csv: 0 rows", "## Tool catalog", "import the bundle", "Requirements: NOT_STARTED 1"):
            self.assertIn(want, text)
        self.assertNotIn("one undo engine for every tool", text)  # full entries come from `slice`
        self.assertLess(len(text.splitlines()), 80)
        self.assertEqual(self.run_cli("capsule")[0], 0)
        self.assertEqual(text, capsule.read(os.path.join(self.d, "SESSION_CAPSULE.md")))

    def test_capsule_refuses_an_invalid_plan(self):
        self.write("CURRENT_SLICE.md", "no slice line")
        rc, out = self.run_cli("capsule")
        self.assertEqual(rc, 1)
        self.assertIn("ERROR", out)
        self.assertFalse(os.path.exists(os.path.join(self.d, "SESSION_CAPSULE.md")))


if __name__ == "__main__":
    unittest.main(verbosity=1)
