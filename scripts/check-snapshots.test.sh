#!/usr/bin/env bash
# scripts/check-snapshots.test.sh — tests for check-snapshots.sh
# Self-contained; no bats dependency.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$SCRIPT_DIR/check-snapshots.sh"

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

# Build a patched copy of the script pointing at our scratch tree.
# The script uses REPO_ROOT derived from SCRIPT_DIR; patch that derivation.
PATCHED="$SCRATCH/check-snapshots-patched.sh"
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

# Missing __snapshots__ dir → exit 0 with warning
mkdir -p "$SCRATCH/specs"
# (no __snapshots__ dir)
assert_exit_code "missing snapshots dir → exit 0" 0 run_patched
assert_stderr_contains "missing snapshots dir → stderr warning" "warning" run_patched

# --dry-run → exit 0 (even with specs present)
mkdir -p "$SCRATCH/specs/__snapshots__"
printf '{}' > "$SCRATCH/specs/foo.json"
printf '{}' > "$SCRATCH/specs/__snapshots__/foo.snap"
assert_exit_code "--dry-run exits 0" 0 run_patched --dry-run
assert_exit_code "CI_DRY_RUN=1 exits 0" 0 env CI_DRY_RUN=1 bash "$PATCHED"

# ---- done --------------------------------------------------------------------

finish
