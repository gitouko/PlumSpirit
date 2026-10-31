#!/bin/sh
# Targeted tests for one tool-expansion step (V09-S37+; the full engine suite
# and the all-tool harness stay at the group gates).
#
#   tools/tool_step.sh <tool id> <test prefix> [log dir]
#   e.g. tools/tool_step.sh skew skew_
#
# Runs, stopping at the first failure:
#   1. transform unit tests (lib, debug)
#   2. the tool's section of tests/v09_tool_expansion.rs (<prefix>*)
#   3. tests/v09_tool_definitions.rs (catalog agreement, declared policies,
#      presets) and tests/v09_legacy_presets.rs
#   4. tests/v07_tools.rs for the tool only, 300 draws: far_end / spanning /
#      random_parameters / 256_key — optimized but with overflow checks and
#      debug assertions on (what a debug build checks, at release speed; its
#      own target dir, target-checked/). V09-S55: a generator's random draws
#      can legitimately make millions of notes; in a debug build the four
#      tests took hours.
#   5. tests/v07_tools.rs in release for the tool only, 300 draws, incl.
#      ignored (the rest of the v0.7 harness for that tool: the six tests of
#      stage 4 are skipped — stage 4 ran them with the same code and more
#      checks; V09-S50)
#   6. app check + clippy -D warnings (engine + app), fmt check
# Each stage's log goes to <log dir>/<id>-<n>.log; one PASS / FAIL line per
# stage is printed with its wall time. Long: run in the background.
set -u
id=$1
prefix=$2
logs=${3:-/tmp/plumspirit-tool-step}
mkdir -p "$logs"
cd "$(dirname "$0")/.." || exit 1
stage() {
    n=$1
    shift
    s=$(date +%s)
    if "$@" > "$logs/$id-$n.log" 2>&1; then
        r=PASS
    else
        r=FAIL
    fi
    line=$(grep -E "^test result" "$logs/$id-$n.log" | tail -1)
    echo "$r stage $n ($(( $(date +%s) - s )) s): $* :: $line"
    [ "$r" = PASS ] || exit 1
}
E="-p plumspirit_engine --offline"
stage 1 cargo test $E --lib transform::
stage 2 cargo test $E --test v09_tool_expansion "$prefix"
stage 3a cargo test $E --test v09_tool_definitions
stage 3b cargo test $E --test v09_legacy_presets
stage 4 env PLUMSPIRIT_HARNESS_TOOLS="$id" PLUMSPIRIT_HARNESS_DRAWS=300 \
    CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS=true CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true \
    CARGO_TARGET_DIR=target-checked \
    cargo test --release $E --test v07_tools -- --include-ignored far_end spanning random_parameters 256_key
stage 5 env PLUMSPIRIT_HARNESS_TOOLS="$id" PLUMSPIRIT_HARNESS_DRAWS=300 \
    cargo test --release $E --test v07_tools -- --include-ignored --test-threads=1 \
    --skip far_end --skip spanning --skip random_parameters --skip 256_key
stage 6a cargo check -p plumspirit --all-targets --offline
stage 6b cargo clippy -p plumspirit_engine -p plumspirit --all-targets --offline -- -D warnings
stage 6c cargo fmt --all -- --check
echo "ALL PASS: $id"
