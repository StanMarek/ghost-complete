#!/usr/bin/env bash
# scripts/check-coverage-baseline-drift.test.sh — tests for
# check-coverage-baseline-drift.sh. Self-contained; no bats dependency.
#
# The script-under-test is a non-blocking CI gate: it never exits non-zero
# except for usage errors. These tests exercise:
#   - flag parsing (--help, unknown flag, --threshold, --baseline, --quiet)
#   - missing baseline -> warning, exit 0 (non-blocking by design)
#   - empty releases array -> warning, exit 0
#   - future timestamp -> clamped to 0 drift (no spurious warning)
#   - old timestamp past threshold -> warning with correct day count
#   - fresh timestamp under threshold -> quiet vs ok-line behaviour
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$SCRIPT_DIR/check-coverage-baseline-drift.sh"

# ---- tiny test harness -------------------------------------------------------

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

assert_stdout_contains() {
    local desc="$1" needle="$2"; shift 2
    local out
    out="$("$@" 2>/dev/null || true)"
    if printf '%s' "$out" | grep -qFe "$needle"; then
        _pass=$(( _pass + 1 ))
        printf 'PASS: %s\n' "$desc"
    else
        _fail=$(( _fail + 1 ))
        printf 'FAIL: %s — stdout did not contain %q\n' "$desc" "$needle"
        printf '      stdout was: %s\n' "$out"
    fi
}

assert_stdout_not_contains() {
    local desc="$1" needle="$2"; shift 2
    local out
    out="$("$@" 2>/dev/null || true)"
    if printf '%s' "$out" | grep -qFe "$needle"; then
        _fail=$(( _fail + 1 ))
        printf 'FAIL: %s — stdout unexpectedly contained %q\n' "$desc" "$needle"
        printf '      stdout was: %s\n' "$out"
    else
        _pass=$(( _pass + 1 ))
        printf 'PASS: %s\n' "$desc"
    fi
}

assert_stderr_contains() {
    local desc="$1" needle="$2"; shift 2
    local err
    err="$("$@" 2>&1 >/dev/null || true)"
    if printf '%s' "$err" | grep -qFe "$needle"; then
        _pass=$(( _pass + 1 ))
        printf 'PASS: %s\n' "$desc"
    else
        _fail=$(( _fail + 1 ))
        printf 'FAIL: %s — stderr did not contain %q\n' "$desc" "$needle"
        printf '      stderr was: %s\n' "$err"
    fi
}

# Combined stdout+stderr needle check. The script prints
# `::warning::` lines to stdout (GitHub Actions convention) and "ok:" / usage
# lines also go to stdout, while `die` writes to stderr. A single helper that
# looks at both keeps assertions stable regardless of where a message lands.
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

# Write a baseline JSON fixture with a single release whose timestamp is
# <days-ago> UTC days before "now". Portable GNU + BSD date.
#   $1 path, $2 days-ago (int; may be negative for a future timestamp)
write_baseline_days_ago() {
    local path="$1" days="$2"
    local iso
    if date -u -d "@0" >/dev/null 2>&1; then
        # GNU date
        iso="$(date -u -d "${days} days ago" +'%Y-%m-%dT%H:%M:%SZ')"
    else
        # BSD / macOS date: -v takes a signed offset. `days ago` maps to
        # a negative offset; a negative `days` (future) maps to a positive
        # offset.
        local offset="$days"
        # BSD: -v-Nd subtracts N days, -v+Nd adds.
        if (( days >= 0 )); then
            iso="$(date -u -v-"${offset}"d +'%Y-%m-%dT%H:%M:%SZ')"
        else
            local pos=$(( -days ))
            iso="$(date -u -v+"${pos}"d +'%Y-%m-%dT%H:%M:%SZ')"
        fi
    fi
    cat > "$path" <<EOF
{
  "schema_version": "1.0",
  "releases": [
    {
      "version": "0.9.1",
      "timestamp": "$iso",
      "total_specs": 709,
      "fully_functional": 526,
      "requires_js_generators": 1889,
      "native_providers": 12,
      "corrected_generators": 139,
      "hand_audit_required": 866
    }
  ]
}
EOF
}

# ---- flag / usage smoke ------------------------------------------------------

assert_exit_code "--help exits 0"  0  bash "$SCRIPT" --help
assert_stdout_contains "--help prints usage" "Usage:" bash "$SCRIPT" --help
assert_stdout_contains "--help mentions --threshold" "--threshold" bash "$SCRIPT" --help

# -h is a documented alias for --help.
assert_exit_code "-h exits 0" 0 bash "$SCRIPT" -h

assert_exit_code "unknown flag → exit 2" 2 bash "$SCRIPT" --bogus
assert_stderr_contains "unknown flag → stderr names offending flag" \
    "--bogus" bash "$SCRIPT" --bogus

# Flags that take arguments but receive none must fail loudly (non-zero).
# --threshold/--baseline both use `${2:?...}` which trips BASH `-u` + `:?` to
# print a diagnostic on stderr and exit 1. (die() would exit 2, but the
# guard here is the parameter-expansion form — still non-zero, still loud.)
missing_value_exit() {
    local code=0
    bash "$SCRIPT" "$@" >/dev/null 2>&1 || code=$?
    if (( code != 0 )); then
        _pass=$(( _pass + 1 ))
        printf 'PASS: %s → exit %d (non-zero)\n' "$*" "$code"
    else
        _fail=$(( _fail + 1 ))
        printf 'FAIL: %s → unexpected exit 0\n' "$*"
    fi
}
missing_value_exit --threshold
missing_value_exit --baseline
assert_stderr_contains "--threshold missing value → stderr explains why" \
    "requires a value" bash "$SCRIPT" --threshold
assert_stderr_contains "--baseline missing value → stderr explains why" \
    "requires a value" bash "$SCRIPT" --baseline

# ---- missing-baseline is non-blocking ----------------------------------------

assert_exit_code "missing baseline → exit 0" 0 \
    bash "$SCRIPT" --baseline "$SCRATCH/nope.json"
assert_output_contains "missing baseline → ::warning:: annotation" \
    "::warning::" bash "$SCRIPT" --baseline "$SCRATCH/nope.json"
assert_output_contains "missing baseline → message names the path" \
    "$SCRATCH/nope.json" bash "$SCRIPT" --baseline "$SCRATCH/nope.json"

# ---- empty-releases array is a distinct warning ------------------------------

empty_baseline="$SCRATCH/empty.json"
printf '%s\n' '{"schema_version":"1.0","releases":[]}' > "$empty_baseline"
assert_exit_code "empty releases → exit 0" 0 \
    bash "$SCRIPT" --baseline "$empty_baseline"
assert_output_contains "empty releases → ::warning:: annotation" \
    "::warning::" bash "$SCRIPT" --baseline "$empty_baseline"
assert_output_contains "empty releases → message says 'no releases'" \
    "no releases" bash "$SCRIPT" --baseline "$empty_baseline"

# ---- fresh baseline under threshold -------------------------------------------

fresh="$SCRATCH/fresh.json"
write_baseline_days_ago "$fresh" 5
assert_exit_code "fresh baseline (5 days) → exit 0" 0 \
    bash "$SCRIPT" --baseline "$fresh"
# Without --quiet we expect an "ok:" line.
assert_output_contains "fresh baseline → prints ok: line" \
    "ok: coverage baseline is" bash "$SCRIPT" --baseline "$fresh"
# No drift warning should appear.
assert_output_not_contains "fresh baseline → no ::warning::" \
    "::warning::" bash "$SCRIPT" --baseline "$fresh"

# With --quiet the "ok:" line is suppressed; still exit 0.
assert_exit_code "fresh baseline + --quiet → exit 0" 0 \
    bash "$SCRIPT" --quiet --baseline "$fresh"
assert_output_not_contains "fresh baseline + --quiet → no ok: line" \
    "ok: coverage baseline" bash "$SCRIPT" --quiet --baseline "$fresh"

# ---- future-timestamp clamp (negative delta -> 0 days) -----------------------

future="$SCRATCH/future.json"
write_baseline_days_ago "$future" -30  # 30 days in the future
assert_exit_code "future timestamp → exit 0" 0 \
    bash "$SCRIPT" --baseline "$future"
# Delta is clamped to 0 days — must NOT emit the drift warning, even with a
# tiny threshold. Guards against a regression where unsigned arithmetic
# wraps and produces a giant "days old" number.
assert_output_not_contains "future timestamp + --threshold 1 → no ::warning::" \
    "::warning::" bash "$SCRIPT" --threshold 1 --baseline "$future"
# ok: line should show 0 days.
assert_output_contains "future timestamp → ok: 0 days" \
    "0 days old" bash "$SCRIPT" --baseline "$future"

# ---- old baseline past threshold emits drift warning -------------------------

old="$SCRATCH/old.json"
write_baseline_days_ago "$old" 200  # well past default 120-day threshold

assert_exit_code "old baseline → exit 0 (non-blocking)" 0 \
    bash "$SCRIPT" --baseline "$old"
assert_output_contains "old baseline → ::warning:: annotation" \
    "::warning::" bash "$SCRIPT" --baseline "$old"
assert_output_contains "old baseline → warning names day count" \
    "200 days old" bash "$SCRIPT" --baseline "$old"

# Lifting the threshold above the age makes the warning go away.
assert_output_not_contains "old baseline + --threshold 365 → no ::warning::" \
    "::warning::" bash "$SCRIPT" --threshold 365 --baseline "$old"
assert_output_contains "old baseline + --threshold 365 → ok: line" \
    "ok: coverage baseline is" bash "$SCRIPT" --threshold 365 --baseline "$old"

# --threshold also reachable via the DRIFT_THRESHOLD_DAYS env var.
assert_output_not_contains "old baseline + DRIFT_THRESHOLD_DAYS=365 → no ::warning::" \
    "::warning::" env DRIFT_THRESHOLD_DAYS=365 bash "$SCRIPT" --baseline "$old"

# Shrinking the threshold below the age still produces only a warning, never
# a failing exit code — the gate is non-blocking by design.
assert_exit_code "old baseline + --threshold 1 → still exit 0" 0 \
    bash "$SCRIPT" --threshold 1 --baseline "$old"
assert_output_contains "old baseline + --threshold 1 → ::warning::" \
    "::warning::" bash "$SCRIPT" --threshold 1 --baseline "$old"

# ---- done --------------------------------------------------------------------

finish
