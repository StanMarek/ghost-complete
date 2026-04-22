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

# Install a fake snapshotter in the scratch tree that mirrors the real
# canonical-JSON behaviour using node's builtin JSON.  The real script
# expects tools/fig-converter/scripts/snapshot-specs.mjs under REPO_ROOT.
mkdir -p "$SCRATCH/tools/fig-converter/scripts"
cat > "$SCRATCH/tools/fig-converter/scripts/snapshot-specs.mjs" <<'FAKE_EOF'
#!/usr/bin/env node
// Minimal snapshotter stub used by the test harness.  Re-implements the
// "sorted keys, 2-space indent, trailing newline" canonical form so the
// test exercises diff logic without depending on the real repo layout.
import { readFileSync, writeFileSync, readdirSync, mkdirSync } from 'node:fs';
import { join, dirname, basename } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO = join(__dirname, '..', '..', '..');
const SPECS = join(REPO, 'specs');

function canon(v) {
    if (Array.isArray(v)) return v.map(canon);
    if (v !== null && typeof v === 'object') {
        const out = {};
        for (const k of Object.keys(v).sort()) out[k] = canon(v[k]);
        return out;
    }
    return v;
}

let out = join(SPECS, '__snapshots__');
for (let i = 2; i < process.argv.length; i++) {
    if (process.argv[i] === '--out') out = process.argv[++i];
}
mkdirSync(out, { recursive: true });
for (const f of readdirSync(SPECS).filter((n) => n.endsWith('.json'))) {
    const name = basename(f, '.json');
    const parsed = JSON.parse(readFileSync(join(SPECS, f), 'utf8'));
    writeFileSync(join(out, `${name}.snap`), JSON.stringify(canon(parsed), null, 2) + '\n');
}
FAKE_EOF

run_patched() { bash "$PATCHED" "$@"; }

# Helper: write a canonical snapshot for the given json string into
# $SCRATCH/specs/__snapshots__/<name>.snap (bypasses the snapshotter).
write_canonical_snap() {
    local name="$1" json="$2"
    node -e "
        const { writeFileSync } = require('node:fs');
        const canon = (v) => Array.isArray(v) ? v.map(canon)
            : (v !== null && typeof v === 'object')
                ? Object.keys(v).sort().reduce((o,k)=>{o[k]=canon(v[k]);return o;},{})
                : v;
        const p = JSON.parse(process.argv[1]);
        writeFileSync(process.argv[2], JSON.stringify(canon(p), null, 2) + '\n');
    " "$json" "$SCRATCH/specs/__snapshots__/${name}.snap"
}

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
write_canonical_snap "foo" '{}'
assert_exit_code "--dry-run exits 0" 0 run_patched --dry-run
assert_exit_code "CI_DRY_RUN=1 exits 0" 0 env CI_DRY_RUN=1 bash "$PATCHED"

# Matching snapshots → exit 0
# foo.json → canonical '{}' snapshot; they should match.
assert_exit_code "matching snapshots → exit 0" 0 run_patched

# Differing snapshots → exit 1
# Mutate foo.json so the fresh snapshot diverges from the recorded one.
printf '{"changed":true}' > "$SCRATCH/specs/foo.json"
assert_exit_code "differing snapshots → exit 1" 1 run_patched
assert_stderr_contains "differing snapshots → stderr names file" "DIFF: foo" run_patched

# New spec with no recorded snapshot → exit 1
printf '{}' > "$SCRATCH/specs/foo.json"  # restore foo to matching state
printf '{"x":1}' > "$SCRATCH/specs/newspec.json"
assert_exit_code "new spec without snapshot → exit 1" 1 run_patched
assert_stderr_contains "new spec → stderr names file" "DIFF: newspec" run_patched
rm "$SCRATCH/specs/newspec.json"

# Orphaned snapshot (source spec deleted) → exit 1
write_canonical_snap "gone" '{}'
assert_exit_code "orphaned snapshot → exit 1" 1 run_patched
assert_stderr_contains "orphaned snapshot → stderr names file" "DIFF: gone" run_patched
rm "$SCRATCH/specs/__snapshots__/gone.snap"

# Non-canonical source JSON still produces a matching snapshot because the
# check re-runs the canonicaliser.  Verify unsorted keys in foo.json don't
# cause a spurious diff.
printf '{"b":2,"a":1}' > "$SCRATCH/specs/foo.json"
write_canonical_snap "foo" '{"a":1,"b":2}'
assert_exit_code "unsorted-source canonical match → exit 0" 0 run_patched

# ---- done --------------------------------------------------------------------

finish
