#!/bin/bash
# L-S41 release gate (contract §033, TEST_IMPACT.toml [gates.release]), one command at a time.
# Logs: /tmp/claude-0/gate-rel/<name>.log, summary in /tmp/claude-0/gate-rel/summary.log
cd /home/claude/ps
G=/tmp/claude-0/gate-rel
mkdir -p $G
TC=/home/claude/tc/llvm-mingw-20261020-ucrt-ubuntu-22.04-x86_64/bin
WIN="env PATH=$TC:$PATH CC_x86_64_pc_windows_gnullvm=x86_64-w64-mingw32-clang CXX_x86_64_pc_windows_gnullvm=x86_64-w64-mingw32-clang++ AR_x86_64_pc_windows_gnullvm=llvm-ar CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER=x86_64-w64-mingw32-clang CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_RUSTFLAGS=-Ctarget-feature=+crt-static CARGO_TARGET_DIR=/home/claude/target-win"
run() { name=$1; shift; echo "=== $name: $*" >> $G/summary.log; s=$(date +%s); bash -c "$*" > $G/$name.log 2>&1; rc=$?; echo "=== $name exit $rc in $(( $(date +%s) - s )) s" >> $G/summary.log; }
# fmt / clippy (host, Windows) / capsule passed at 422f27d in run 2 (gate-rel-run2)
run tests_debug_rest "env CARGO_PROFILE_DEV_DEBUG=0 cargo test -p plumspirit_engine --no-fail-fast --offline --test v07_tracks --test v08_editing --test v08_editors --test v08_folders --test v08_history --test v08_metrics --test v08_paint --test v08_plugins --test v08_reference --test v08_spiderweb --test v08_spiderweb_edit --test v08_visual --test v09_clip_merge --test v09_clip_migration --test v09_clip_ops --test v09_clip_repeat --test v09_content_edit --test v09_driver_dimensions --test v09_experimental_review --test v09_export_certification --test v09_export_keys --test v09_export_matrix --test v09_export_plan --test v09_generators --test v09_gestures --test v09_inspector --test v09_keys --test v09_legacy_presets --test v09_paste_repeat --test v09_player --test v09_selection_peaks --test v09_tool_definitions --test v09_tool_expansion "
run tests_debug_doc "env CARGO_PROFILE_DEV_DEBUG=0 cargo test --workspace --doc --offline"
run tests_release "cargo test --release --workspace --no-fail-fast --offline"
for s in stress stress_v02 stress_v03 stress_v05 stress_v06 stress_v07 stress_v08; do
  run $s "cargo test --release --offline -p plumspirit_engine --test $s -- --ignored --nocapture --test-threads=1"
done
run v08_history_paint "cargo test --release --offline -p plumspirit_engine --test v08_history --test v08_paint -- --ignored --nocapture --test-threads=1"
run dense_10m_never_blank "cargo test --release --offline -p plumspirit dense_ten_million_notes_are_never_blank -- --ignored --nocapture"
run tool_catalog "cargo run -q --release --offline -p plumspirit_engine --example tool_catalog > /tmp/claude-0/plumspirit-tools.tsv && python3 tools/capsule.py --dir docs/v0_9 catalog --registry /tmp/claude-0/plumspirit-tools.tsv"
run catalog_gate "python3 tools/capsule.py --dir docs/v0_9 catalog --gate"
run acceptance "python3 tools/acceptance.py --quiet"
run harness_300 "env PLUMSPIRIT_HARNESS_DRAWS=300 cargo test --release --offline -p plumspirit_engine --test v07_tools -- --include-ignored --test-threads=1"
echo "=== GATE DONE" >> $G/summary.log
