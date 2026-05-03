#!/usr/bin/env bash
# scripts/check-coverage-regression.test.sh — tests for
# check-coverage-regression.sh. Self-contained; no bats dependency.
#
# Exercises the gate decision logic with mock baselines and pre-captured
# status --json payloads.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$SCRIPT_DIR/check-coverage-regression.sh"

_pass=0
_fail=0

assert_exit_code() {
    local desc="$1" expected="$2"; shift 2
    local actual=0
    "$@" >/dev/null 2>&1 || actual=$?
    if [[ "$actual" -eq "$expected" ]]; then
        _pass=$(( _pass + 1 ))
        printf 'PASS: %s (exit %d)\n' "$desc" "$actual"
    else
        _fail=$(( _fail + 1 ))
        printf 'FAIL: %s — expected exit %d, got %d\n' "$desc" "$expected" "$actual"
    fi
}

assert_output_contains() {
    local desc="$1" needle="$2"; shift 2
    local out
    out="$("$@" 2>&1 || true)"
    if printf '%s' "$out" | grep -qFe "$needle"; then
        _pass=$(( _pass + 1 ))
        printf 'PASS: %s\n' "$desc"
    else
        _fail=$(( _fail + 1 ))
        printf 'FAIL: %s — combined output did not contain %q\n' "$desc" "$needle"
        printf '      output was: %s\n' "$out"
    fi
}

assert_output_not_contains() {
    local desc="$1" needle="$2"; shift 2
    local out
    out="$("$@" 2>&1 || true)"
    if printf '%s' "$out" | grep -qFe "$needle"; then
        _fail=$(( _fail + 1 ))
        printf 'FAIL: %s — combined output unexpectedly contained %q\n' "$desc" "$needle"
        printf '      output was: %s\n' "$out"
    else
        _pass=$(( _pass + 1 ))
        printf 'PASS: %s\n' "$desc"
    fi
}

finish() {
    printf '\n%d passed, %d failed\n' "$_pass" "$_fail"
    [[ "$_fail" -eq 0 ]]
}

# ---- scratch setup -----------------------------------------------------------

SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT

write_baseline() {
    local path="$1" unsupported="$2" supported="$3" total="$4"
    cat > "$path" <<EOF
{
  "schema_version": "1.0",
  "releases": [
    {
      "version": "test-fixture",
      "timestamp": "2026-05-03T00:00:00Z",
      "total_specs": 709,
      "fully_functional": 529,
      "requires_js_generators": $total,
      "native_providers": 12,
      "corrected_generators": 171,
      "hand_audit_required": 0,
      "requires_js_generators_total": $total,
      "requires_js_generators_supported": $supported,
      "requires_js_generators_unsupported": $unsupported,
      "commands_nonfunctional": 0
    }
  ]
}
EOF
}

write_status() {
    local path="$1" unsupported="$2" supported="$3" total="$4" nonfunctional="$5"
    cat > "$path" <<EOF
{
  "schema_version": "1.2",
  "spec_counts": {
    "total": 709,
    "fully_functional": 529,
    "partially_functional": 180,
    "embedded": 709,
    "filesystem_overrides": 709,
    "parse_errors": 0,
    "parse_error_details": [],
    "commands_addressable": 717,
    "commands_fully_functional": 529,
    "commands_partially_functional": 180,
    "commands_nonfunctional": $nonfunctional,
    "requires_js_generators_total": $total,
    "requires_js_generators_supported": $supported,
    "requires_js_generators_unsupported": $unsupported,
    "command_alias_conflicts": 0,
    "command_alias_conflict_details": [],
    "requires_js_generators_supported_by_kind": {
      "post_process": 0,
      "script_function": 0,
      "custom": 0
    }
  },
  "file_scan": {
    "spec_files_total": 709,
    "requires_js_generators_total": $total,
    "requires_js_generators_supported": $supported,
    "requires_js_generators_supported_post_process": 0,
    "requires_js_generators_supported_script_function": 0,
    "requires_js_generators_supported_custom": 0
  },
  "js_runtime": {"enabled": true},
  "coverage_trend": null
}
EOF
}

baseline_zero="$SCRATCH/baseline-zero.json"
write_baseline "$baseline_zero" 0 3641 3641

baseline_500="$SCRATCH/baseline-500.json"
write_baseline "$baseline_500" 500 3141 3641

# ---- flag / usage ------------------------------------------------------------

assert_exit_code "--help exits 0" 0 bash "$SCRIPT" --help
assert_output_contains "--help prints usage" "Usage:" bash "$SCRIPT" --help
assert_output_contains "--help mentions --tolerance" "--tolerance" bash "$SCRIPT" --help

assert_exit_code "unknown flag exits 2" 2 bash "$SCRIPT" --bogus
assert_output_contains "unknown flag names offending flag" "--bogus" \
    bash "$SCRIPT" --bogus

assert_exit_code "non-integer tolerance exits 2" 2 \
    bash "$SCRIPT" --tolerance abc --baseline "$baseline_zero" --status-json "$SCRATCH/dummy.json"

# ---- missing baseline → soft warning ----------------------------------------

assert_exit_code "missing baseline → exit 0 (skip)" 0 \
    bash "$SCRIPT" --baseline "$SCRATCH/nope.json" --status-json "$SCRATCH/dummy.json"
assert_output_contains "missing baseline → ::warning:: annotation" \
    "::warning::" bash "$SCRIPT" --baseline "$SCRATCH/nope.json" --status-json "$SCRATCH/dummy.json"

# ---- missing status-json → exit 2 -------------------------------------------

assert_exit_code "missing status-json file → exit 2" 2 \
    bash "$SCRIPT" --baseline "$baseline_zero" --status-json "$SCRATCH/no-such-status.json"

# ---- happy path: zero unsupported, baseline=0 -------------------------------

status_clean="$SCRATCH/status-clean.json"
write_status "$status_clean" 0 3641 3641 0
assert_exit_code "clean (current=0, baseline=0) → exit 0" 0 \
    bash "$SCRIPT" --baseline "$baseline_zero" --status-json "$status_clean"
assert_output_contains "clean → ok line" "ok: requires_js coverage holds" \
    bash "$SCRIPT" --baseline "$baseline_zero" --status-json "$status_clean"

# --quiet suppresses the ok line
assert_output_not_contains "clean + --quiet → no ok line" "ok: requires_js" \
    bash "$SCRIPT" --quiet --baseline "$baseline_zero" --status-json "$status_clean"

# ---- regression: current > baseline + tolerance -----------------------------

status_regressed="$SCRATCH/status-regressed.json"
write_status "$status_regressed" 5 3636 3641 0
assert_exit_code "regression beyond tolerance → exit 1" 1 \
    bash "$SCRIPT" --baseline "$baseline_zero" --status-json "$status_regressed"
assert_output_contains "regression names delta" "regressed by 5" \
    bash "$SCRIPT" --baseline "$baseline_zero" --status-json "$status_regressed"
assert_output_contains "regression points at baseline refresh" \
    "update docs/coverage-baseline.json" \
    bash "$SCRIPT" --baseline "$baseline_zero" --status-json "$status_regressed"

# ---- within-tolerance increase emits a warning but does not fail ------------

assert_exit_code "rise within tolerance → exit 0" 0 \
    bash "$SCRIPT" --baseline "$baseline_zero" --status-json "$status_regressed" --tolerance 5
assert_output_contains "rise within tolerance → ::warning::" \
    "::warning::" \
    bash "$SCRIPT" --baseline "$baseline_zero" --status-json "$status_regressed" --tolerance 5
assert_output_contains "rise within tolerance → ok line still printed" \
    "ok: requires_js coverage holds" \
    bash "$SCRIPT" --baseline "$baseline_zero" --status-json "$status_regressed" --tolerance 5

# ---- improvement (unsupported drops) is fine --------------------------------

status_improved="$SCRATCH/status-improved.json"
write_status "$status_improved" 0 3641 3641 0
assert_exit_code "improvement (current=0, baseline=500) → exit 0" 0 \
    bash "$SCRIPT" --baseline "$baseline_500" --status-json "$status_improved"
assert_output_not_contains "improvement → no ::warning::" \
    "::warning::" \
    bash "$SCRIPT" --baseline "$baseline_500" --status-json "$status_improved"

# ---- nonfunctional commands always fail -------------------------------------

status_nonfunctional="$SCRATCH/status-nonfunctional.json"
write_status "$status_nonfunctional" 0 3641 3641 3
assert_exit_code "any nonfunctional command → exit 1" 1 \
    bash "$SCRIPT" --baseline "$baseline_zero" --status-json "$status_nonfunctional"
assert_output_contains "nonfunctional → message names count" "3 command(s) are nonfunctional" \
    bash "$SCRIPT" --baseline "$baseline_zero" --status-json "$status_nonfunctional"

# ---- env var pathway --------------------------------------------------------

assert_exit_code "COVERAGE_REGRESSION_TOLERANCE=10 within tolerance → exit 0" 0 \
    env COVERAGE_REGRESSION_TOLERANCE=10 \
    bash "$SCRIPT" --baseline "$baseline_zero" --status-json "$status_regressed"

# ---- done --------------------------------------------------------------------

finish
