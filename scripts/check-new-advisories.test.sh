#!/usr/bin/env bash
# scripts/check-new-advisories.test.sh — tests for check-new-advisories.sh.
# Self-contained; no bats dependency. Fixtures mimic `cargo audit --json`.
#
# Covers:
#   - flag parsing (--help, unknown flag, missing --base/--head)
#   - a report that is not cargo-audit JSON (e.g. advisory DB fetch failed)
#     -> exit 2, never a silent pass
#   - advisory already on the base branch -> warning, exit 0
#   - advisory introduced by the head lockfile -> error, exit 1
#   - vulnerable crate bumped to a still-vulnerable version -> not "new"
#   - advisory fixed by the head lockfile -> notice, exit 0
#   - informational advisories (unmaintained/unsound) count; yanked does not
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$SCRIPT_DIR/check-new-advisories.sh"

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

finish() {
    printf '\n%d passed, %d failed\n' "$_pass" "$_fail"
    [[ "$_fail" -eq 0 ]]
}

# ---- scratch setup -----------------------------------------------------------

SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT

# Write a `cargo audit --json` report.
#   $1 path, then one entry per finding as <kind>:<advisory-id>:<crate>:<version>
#   kind = vuln | unmaintained | unsound | yanked (yanked has no advisory)
write_report() {
    local path="$1"; shift
    printf '%s\n' "$@" | jq -R -s '
        split("\n")
        | map(select(length > 0) | split(":")
              | {kind: .[0], id: .[1], name: .[2], version: .[3]})
        | def finding: {
              kind: .kind,
              advisory: (if .kind == "yanked" then null
                         else {id: .id, title: ("title of " + .id)} end),
              package: {name: .name, version: .version}
          };
          {
              vulnerabilities: {
                  found: any(.[]; .kind == "vuln"),
                  list: [.[] | select(.kind == "vuln") | finding]
              },
              warnings: (map(select(.kind != "vuln")) | group_by(.kind)
                         | map({key: .[0].kind, value: map(finding)})
                         | from_entries)
          }' > "$path"
}

clean="$SCRATCH/clean.json"
write_report "$clean"

h2_old="$SCRATCH/h2-old.json"
write_report "$h2_old" "vuln:RUSTSEC-2026-0258:h2:0.4.14"

h2_still_vulnerable="$SCRATCH/h2-still-vulnerable.json"
write_report "$h2_still_vulnerable" "vuln:RUSTSEC-2026-0258:h2:0.4.15"

h2_and_rustls="$SCRATCH/h2-and-rustls.json"
write_report "$h2_and_rustls" \
    "vuln:RUSTSEC-2026-0258:h2:0.4.14" \
    "vuln:RUSTSEC-2026-0285:rustls:0.23.40"

unsound="$SCRATCH/unsound.json"
write_report "$unsound" "unsound:RUSTSEC-2026-0253:lru:0.16.3"

yanked="$SCRATCH/yanked.json"
write_report "$yanked" "yanked::some-crate:1.0.0"

not_a_report="$SCRATCH/not-a-report.json"
printf '' > "$not_a_report"

# ---- flag / usage smoke ------------------------------------------------------

assert_exit_code "--help exits 0" 0 bash "$SCRIPT" --help
assert_stdout_contains "--help prints usage" "Usage:" bash "$SCRIPT" --help
assert_exit_code "-h exits 0" 0 bash "$SCRIPT" -h

assert_exit_code "unknown flag → exit 2" 2 bash "$SCRIPT" --bogus
assert_stderr_contains "unknown flag → stderr names offending flag" \
    "--bogus" bash "$SCRIPT" --bogus

assert_exit_code "missing --head → exit 2" 2 bash "$SCRIPT" --base "$clean"
assert_exit_code "missing --base → exit 2" 2 bash "$SCRIPT" --head "$clean"

# ---- unusable reports fail closed --------------------------------------------

# `cargo audit` prints nothing on stdout when it cannot load the advisory DB.
# That must fail the gate, not read as "no advisories".
assert_exit_code "empty head report → exit 2" 2 \
    bash "$SCRIPT" --base "$clean" --head "$not_a_report"
assert_exit_code "empty base report → exit 2" 2 \
    bash "$SCRIPT" --base "$not_a_report" --head "$clean"
assert_stderr_contains "empty report → stderr names the file" \
    "$not_a_report" bash "$SCRIPT" --base "$clean" --head "$not_a_report"
assert_exit_code "missing report file → exit 2" 2 \
    bash "$SCRIPT" --base "$clean" --head "$SCRATCH/does-not-exist.json"

# ---- clean ---------------------------------------------------------------------

assert_exit_code "clean base and head → exit 0" 0 \
    bash "$SCRIPT" --base "$clean" --head "$clean"
assert_stdout_not_contains "clean → no annotations" \
    "::" bash "$SCRIPT" --base "$clean" --head "$clean"

# ---- advisories already on the base branch -----------------------------------

assert_exit_code "advisory already on base → exit 0" 0 \
    bash "$SCRIPT" --base "$h2_old" --head "$h2_old"
assert_stdout_contains "advisory already on base → warning" \
    "::warning title=RUSTSEC-2026-0258 already on the base branch::h2 0.4.14" \
    bash "$SCRIPT" --base "$h2_old" --head "$h2_old"
assert_stdout_not_contains "advisory already on base → no error" \
    "::error" bash "$SCRIPT" --base "$h2_old" --head "$h2_old"

# A bump that is still inside the vulnerable range is no worse than base.
assert_exit_code "still-vulnerable bump → exit 0" 0 \
    bash "$SCRIPT" --base "$h2_old" --head "$h2_still_vulnerable"
assert_stdout_contains "still-vulnerable bump → warning names new version" \
    "::warning title=RUSTSEC-2026-0258 already on the base branch::h2 0.4.15" \
    bash "$SCRIPT" --base "$h2_old" --head "$h2_still_vulnerable"

# ---- advisories introduced by the head lockfile -------------------------------

assert_exit_code "new advisory → exit 1" 1 \
    bash "$SCRIPT" --base "$h2_old" --head "$h2_and_rustls"
assert_stdout_contains "new advisory → error annotation" \
    "::error title=RUSTSEC-2026-0285 introduced by this PR::rustls 0.23.40: title of RUSTSEC-2026-0285" \
    bash "$SCRIPT" --base "$h2_old" --head "$h2_and_rustls"
assert_stdout_contains "new advisory → pre-existing one stays a warning" \
    "::warning title=RUSTSEC-2026-0258 already on the base branch::h2 0.4.14" \
    bash "$SCRIPT" --base "$h2_old" --head "$h2_and_rustls"

assert_exit_code "new unsound advisory → exit 1" 1 \
    bash "$SCRIPT" --base "$clean" --head "$unsound"
assert_exit_code "yanked crate (no advisory) → exit 0" 0 \
    bash "$SCRIPT" --base "$clean" --head "$yanked"

# ---- advisories fixed by the head lockfile ------------------------------------

assert_exit_code "fixed advisory → exit 0" 0 \
    bash "$SCRIPT" --base "$h2_and_rustls" --head "$h2_old"
assert_stdout_contains "fixed advisory → notice" \
    "::notice title=RUSTSEC-2026-0285 fixed by this PR::rustls 0.23.40" \
    bash "$SCRIPT" --base "$h2_and_rustls" --head "$h2_old"

# ---- done --------------------------------------------------------------------

finish
