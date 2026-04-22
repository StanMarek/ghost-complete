#!/usr/bin/env bash
# scripts/check-bench.test.sh — tests for check-bench.sh
# Self-contained; no bats dependency.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$SCRIPT_DIR/check-bench.sh"

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

finish() {
    printf '\n%d passed, %d failed\n' "$_pass" "$_fail"
    [[ "$_fail" -eq 0 ]]
}

# ---- scratch setup -----------------------------------------------------------

SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT

# Build a patched copy pointing at our scratch tree.  We rewrite REPO_ROOT and
# keep the `source lib/common.sh` pointing at the real one.
PATCHED="$SCRATCH/check-bench-patched.sh"
sed \
    -e "s|REPO_ROOT=\"\$(cd \"\$SCRIPT_DIR/..\" && pwd)\"|REPO_ROOT=\"$SCRATCH\"|" \
    "$SCRIPT" > "$PATCHED"
sed -i.bak "s|source \"\$SCRIPT_DIR/lib/common.sh\"|source \"$SCRIPT_DIR/lib/common.sh\"|" "$PATCHED"
chmod +x "$PATCHED"

run_patched() { bash "$PATCHED" "$@"; }

# Helpers for building a synthetic criterion tree.
write_estimate() {
    # $1 group, $2 bench, $3 median_ns
    local group="$1" bench="$2" ns="$3"
    local dir="$SCRATCH/target/criterion/$group/$bench/new"
    mkdir -p "$dir"
    cat > "$dir/estimates.json" <<EOF
{
  "mean":    {"confidence_interval":{"confidence_level":0.95,"lower_bound":$ns,"upper_bound":$ns},"point_estimate":$ns,"standard_error":0},
  "median":  {"confidence_interval":{"confidence_level":0.95,"lower_bound":$ns,"upper_bound":$ns},"point_estimate":$ns,"standard_error":0},
  "median_abs_dev": {"confidence_interval":{"confidence_level":0.95,"lower_bound":0,"upper_bound":0},"point_estimate":0,"standard_error":0},
  "slope":   {"confidence_interval":{"confidence_level":0.95,"lower_bound":$ns,"upper_bound":$ns},"point_estimate":$ns,"standard_error":0},
  "std_dev": {"confidence_interval":{"confidence_level":0.95,"lower_bound":0,"upper_bound":0},"point_estimate":0,"standard_error":0}
}
EOF
}

write_baseline() {
    # single-argument: baseline JSON body
    mkdir -p "$SCRATCH/benchmarks"
    printf '%s\n' "$1" > "$SCRATCH/benchmarks/baseline-pre-js-port.json"
}

reset_scratch() {
    rm -rf "$SCRATCH/benchmarks" "$SCRATCH/target"
}

# ---- flag / usage smoke ------------------------------------------------------

assert_exit_code "--help exits 0"  0  bash "$SCRIPT" --help
assert_stdout_contains "--help prints usage" "Usage:" bash "$SCRIPT" --help

assert_exit_code "unknown flag → exit 2" 2  bash "$SCRIPT" --bogus

assert_exit_code "--threshold bad value → exit 2" 2  bash "$SCRIPT" --threshold abc
assert_exit_code "--threshold missing value → exit 2" 2  bash "$SCRIPT" --threshold

# ---- missing-baseline + missing-data defaults ---------------------------------

reset_scratch
# Missing baseline file → exit 0 with warning
assert_exit_code "missing baseline → exit 0" 0 run_patched
assert_stderr_contains "missing baseline → stderr warning" "warning" run_patched

# Baseline present but no target/criterion → exit 0 with warning
write_baseline '{"schema_version":"1.0","groups":{"g":{"b":{"median_ns":1000}}}}'
assert_exit_code "no criterion dir → exit 0" 0 run_patched
assert_stderr_contains "no criterion dir → stderr warning" "warning" run_patched

# Baseline present but empty-groups → skip with warning
reset_scratch
write_baseline '{"schema_version":"1.0","groups":{}}'
mkdir -p "$SCRATCH/target/criterion/g/b/new"
write_estimate g b 1000
assert_exit_code "empty-groups baseline → exit 0" 0 run_patched
assert_stderr_contains "empty-groups baseline → warning" "warning" run_patched

# ---- matching data (no regression) -------------------------------------------

reset_scratch
write_baseline '{"schema_version":"1.0","groups":{"fuzzy_ranking":{"1k_3char":{"median_ns":100000}}}}'
write_estimate fuzzy_ranking 1k_3char 100000
assert_exit_code "identical data → exit 0" 0 run_patched
assert_stdout_contains "identical data → PASS message" "PASS:" run_patched

# Small drift within threshold (5% under default 10%) → still passes
write_estimate fuzzy_ranking 1k_3char 105000
assert_exit_code "5% over → within default threshold, exit 0" 0 run_patched

# Drift within a custom threshold
assert_exit_code "5% over with --threshold 3 → exit 1" 1 run_patched --threshold 3

# ---- synthetic 20% regression → fail -----------------------------------------

reset_scratch
write_baseline '{"schema_version":"1.0","groups":{"fuzzy_ranking":{"1k_3char":{"median_ns":100000}}}}'
write_estimate fuzzy_ranking 1k_3char 120000
assert_exit_code "20% regression → exit 1" 1 run_patched
assert_stderr_contains "20% regression → FAIL line in stderr" "FAIL:" run_patched
assert_stderr_contains "20% regression → table shows bench name" "1k_3char" run_patched

# Same regression, higher threshold → passes
assert_exit_code "20% regression, --threshold 25 → exit 0" 0 run_patched --threshold 25

# ---- improvements (faster than baseline) don't fail --------------------------

reset_scratch
write_baseline '{"schema_version":"1.0","groups":{"fuzzy_ranking":{"1k_3char":{"median_ns":100000}}}}'
write_estimate fuzzy_ranking 1k_3char 50000
assert_exit_code "50% improvement → exit 0" 0 run_patched

# ---- multiple benches, mixed outcome → still fails on any regression --------

reset_scratch
write_baseline '{"schema_version":"1.0","groups":{"g":{"ok":{"median_ns":1000000},"bad":{"median_ns":1000000}}}}'
write_estimate g ok 1000000   # 0% change
write_estimate g bad 1300000  # 30% regression
assert_exit_code "mixed good+bad → exit 1" 1 run_patched
assert_stderr_contains "mixed run → mentions regressed bench" "bad" run_patched

# ---- baseline-only / current-only benches are notes, not failures ------------

reset_scratch
write_baseline '{"schema_version":"1.0","groups":{"g":{"matched":{"median_ns":1000000},"gone":{"median_ns":1000000}}}}'
write_estimate g matched 1000000
write_estimate g extra   1000000
assert_exit_code "missing + extra benches → exit 0" 0 run_patched
assert_stderr_contains "missing bench notes → stderr mentions 'gone'" "gone" run_patched
assert_stderr_contains "extra bench notes → stderr mentions 'extra'" "extra" run_patched

# ---- dry-run short-circuits before any I/O -----------------------------------

reset_scratch
# Baseline + criterion data with a regression — dry-run should still exit 0
write_baseline '{"schema_version":"1.0","groups":{"g":{"b":{"median_ns":1000}}}}'
write_estimate g b 5000
assert_exit_code "--dry-run over regression → exit 0" 0 run_patched --dry-run
assert_exit_code "CI_DRY_RUN=1 over regression → exit 0" 0 env CI_DRY_RUN=1 bash "$PATCHED"

# ---- done --------------------------------------------------------------------

finish
