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

# Build a patched copy pointing at our scratch tree.
PATCHED="$SCRATCH/check-bench-patched.sh"
sed \
    -e "s|REPO_ROOT=\"\$(cd \"\$SCRIPT_DIR/..\" && pwd)\"|REPO_ROOT=\"$SCRATCH\"|" \
    "$SCRIPT" > "$PATCHED"
sed -i.bak "s|source \"\$SCRIPT_DIR/lib/common.sh\"|source \"$SCRIPT_DIR/lib/common.sh\"|" "$PATCHED"
chmod +x "$PATCHED"

run_patched() { bash "$PATCHED" "$@"; }

# ---- tests -------------------------------------------------------------------

assert_exit_code "--help exits 0"  0  bash "$SCRIPT" --help
assert_stdout_contains "--help prints usage" "Usage:" bash "$SCRIPT" --help

assert_exit_code "unknown flag → exit 2" 2  bash "$SCRIPT" --bogus

assert_exit_code "--threshold bad value → exit 2" 2  bash "$SCRIPT" --threshold abc

# Missing baseline file → exit 0 with warning
# (no benchmarks dir / file in scratch)
assert_exit_code "missing baseline → exit 0" 0 run_patched
assert_stderr_contains "missing baseline → stderr warning" "warning" run_patched

# Baseline present but no target/criterion → exit 0 with warning
mkdir -p "$SCRATCH/benchmarks"
printf '| bench | mean_ns |\n| foo | 100 |\n' > "$SCRATCH/benchmarks/baseline-pre-js-port.md"
assert_exit_code "no criterion dir → exit 0" 0 run_patched
assert_stderr_contains "no criterion dir → stderr warning" "warning" run_patched

# Both baseline and criterion present → stub exits 0
mkdir -p "$SCRATCH/target/criterion"
assert_exit_code "baseline + criterion → stub exits 0" 0 run_patched

# --dry-run → exit 0 regardless
assert_exit_code "--dry-run exits 0" 0 run_patched --dry-run
assert_exit_code "CI_DRY_RUN=1 exits 0" 0 env CI_DRY_RUN=1 bash "$PATCHED"

# --threshold override → still exits 0 (stub)
assert_exit_code "--threshold 20 exits 0" 0 run_patched --threshold 20

# ---- done --------------------------------------------------------------------

finish
