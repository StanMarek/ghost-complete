#!/usr/bin/env bash
# scripts/check-binary-size.test.sh — tests for check-binary-size.sh
# Self-contained; no bats dependency.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$SCRIPT_DIR/check-binary-size.sh"

# ---- tiny test harness -------------------------------------------------------

_pass=0
_fail=0

_check() {
    local desc="$1"; shift
    if "$@" 2>/dev/null; then
        _pass=$(( _pass + 1 ))
        printf 'PASS: %s\n' "$desc"
    else
        _fail=$(( _fail + 1 ))
        printf 'FAIL: %s\n' "$desc"
    fi
}

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

# Capture stdout; assert it contains a substring
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

# Capture stderr; assert it contains a substring
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

# ---- helpers -----------------------------------------------------------------

# Create a scratch dir; register cleanup
SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT

# Stub cargo so the script never actually builds anything.
# We create a fake cargo in $SCRATCH/bin that exits 0.
mkdir -p "$SCRATCH/bin"
cat >"$SCRATCH/bin/cargo" <<'SH'
#!/usr/bin/env bash
# Stub: pretend build succeeded (binary must already exist in the test)
exit 0
SH
chmod +x "$SCRATCH/bin/cargo"

# Helper: run script with our stub cargo on PATH
run_script() {
    PATH="$SCRATCH/bin:$PATH" bash "$SCRIPT" "$@"
}

# ---- basic flag tests --------------------------------------------------------

assert_exit_code "--help exits 0"          0  bash "$SCRIPT" --help
assert_stdout_contains "--help prints usage" "Usage:" bash "$SCRIPT" --help

assert_exit_code "no flags → exit 2"       2  run_script
assert_stderr_contains "no flags → usage error" "--absolute-max" run_script

assert_exit_code "unknown flag → exit 2"   2  run_script --bogus-flag
assert_stderr_contains "unknown flag → stderr"  "unknown flag"  run_script --bogus-flag

# ---- dry-run -----------------------------------------------------------------

assert_exit_code "--dry-run --absolute-max 30MB → exit 0" 0 \
    run_script --dry-run --absolute-max 30MB

assert_stdout_contains "--dry-run prints [dry-run]" "[dry-run]" \
    run_script --dry-run --absolute-max 30MB

assert_exit_code "CI_DRY_RUN=1 → exit 0" 0 \
    env CI_DRY_RUN=1 PATH="$SCRATCH/bin:$PATH" bash "$SCRIPT" --absolute-max 30MB

# ---- size parser (tested via --dry-run so no filesystem needed) --------------

parse_ok() {
    local desc="$1" raw="$2"
    assert_exit_code "parse_ok: $desc" 0 run_script --dry-run --absolute-max "$raw"
}
parse_fail() {
    local desc="$1" raw="$2"
    assert_exit_code "parse_fail: $desc → exit 2" 2 run_script --dry-run --absolute-max "$raw"
}

parse_ok  "30MB"   30MB
parse_ok  "30mb"   30mb
parse_ok  "30M"    30M
parse_ok  "30MiB"  30MiB
parse_ok  "1024 bytes" 1024
parse_ok  "1K"     1K
parse_ok  "1G"     1G
parse_ok  "1GB"    1GB
parse_ok  "1GiB"   1GiB

parse_fail "invalid unit 30XB" 30XB

# ---- integration: absolute-max -----------------------------------------------

# Create a synthetic binary (exactly 5 MB)
FAKE_BINARY_DIR="$SCRATCH/target/release"
mkdir -p "$FAKE_BINARY_DIR"
dd if=/dev/zero of="$FAKE_BINARY_DIR/ghost-complete" bs=1024 count=5120 2>/dev/null

# Override REPO_ROOT by creating a wrapper that points at our scratch tree.
# We achieve this by sourcing the library with an overridden path, but the
# simplest approach is to build a temporary copy of the script with patched paths.
PATCHED="$SCRATCH/check-binary-size-patched.sh"
sed \
    -e "s|REPO_ROOT=\"\$(cd \"\$SCRIPT_DIR/..\" && pwd)\"|REPO_ROOT=\"$SCRATCH\"|" \
    "$SCRIPT" > "$PATCHED"
# Fix the source path for common.sh in the patched copy
sed -i.bak "s|source \"\$SCRIPT_DIR/lib/common.sh\"|source \"$SCRIPT_DIR/lib/common.sh\"|" "$PATCHED"
chmod +x "$PATCHED"

run_patched() {
    PATH="$SCRATCH/bin:$PATH" bash "$PATCHED" "$@"
}

# 5 MB binary vs 10 MB limit → PASS
assert_exit_code "absolute: 5MB < 10MB limit → pass" 0 \
    run_patched --absolute-max 10MB

# 5 MB binary vs 4 MB limit → FAIL (exit 1)
assert_exit_code "absolute: 5MB > 4MB limit → fail" 1 \
    run_patched --absolute-max 4MB

# ---- integration: delta-max --------------------------------------------------

# Baseline: 4 MB
BASELINE_DIR="$SCRATCH/benchmarks"
mkdir -p "$BASELINE_DIR"
printf '%d\n' $(( 4 * 1024 * 1024 )) > "$BASELINE_DIR/binary-size-baseline.txt"

# Binary is 5 MB → delta = 1 MB
# delta-max 2MB → PASS
assert_exit_code "delta: 1MB delta < 2MB limit → pass" 0 \
    run_patched --delta-max 2MB

# delta-max 512K → FAIL (1 MB > 512 KB)
assert_exit_code "delta: 1MB delta > 512K limit → fail" 1 \
    run_patched --delta-max 512K

# Baseline in du -b format: "<bytes>\t<path>"
printf '%d\ttarget/release/ghost-complete\n' $(( 4 * 1024 * 1024 )) \
    > "$BASELINE_DIR/binary-size-baseline.txt"

assert_exit_code "delta: du-b format baseline → pass" 0 \
    run_patched --delta-max 2MB

# Baseline with leading whitespace
printf '  %d\n' $(( 4 * 1024 * 1024 )) \
    > "$BASELINE_DIR/binary-size-baseline.txt"

assert_exit_code "delta: leading-whitespace baseline → pass" 0 \
    run_patched --delta-max 2MB

# Missing baseline file for --delta-max → exit 2
rm "$BASELINE_DIR/binary-size-baseline.txt"
assert_exit_code "delta: missing baseline → exit 2" 2 \
    run_patched --delta-max 2MB

# ---- done --------------------------------------------------------------------

finish
