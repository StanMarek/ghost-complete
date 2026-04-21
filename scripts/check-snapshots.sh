#!/usr/bin/env bash
# scripts/check-snapshots.sh — CI gate: spec snapshot diff check (STUB)
#
# Compares specs/*.json against specs/__snapshots__/<name>.snap.
# Real snapshot-format decision deferred to session D.
#
# EXIT CODES
#   0 — pass (or snapshots dir absent, or dry-run)
#   1 — snapshots differ (gate violation)
#   2 — script/usage error
#
# DRY-RUN
#   Pass --dry-run or set CI_DRY_RUN=1.  Argument validation still runs;
#   the actual diff is skipped and the script exits 0.
#
# TODO (session D): replace the `diff -r` placeholder below with real
# snapshot-aware comparison once the snapshot format is decided.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

SPECS_DIR="$REPO_ROOT/specs"
SNAPSHOTS_DIR="$SPECS_DIR/__snapshots__"

usage() {
    cat <<'EOF'
Usage:
  check-snapshots.sh [--dry-run] [--help]

Compares specs/*.json against corresponding specs/__snapshots__/<name>.snap.
If the snapshots directory does not exist, exits 0 with a warning.

Options:
  --dry-run   Skip diff; print what would be checked; exit 0.
  --help      Print this help and exit 0.

Environment:
  CI_DRY_RUN=1   Equivalent to --dry-run.
EOF
}

# ---- parse args ---------------------------------------------------------------

dry_run=0
[[ "${CI_DRY_RUN:-0}" == "1" ]] && dry_run=1

while [[ $# -gt 0 ]]; do
    case "$1" in
        --help|-h)  usage; exit 0 ;;
        --dry-run)  dry_run=1; shift ;;
        *) die "unknown flag: $1 (use --help for usage)" ;;
    esac
done

# ---- dry-run early exit -------------------------------------------------------

if [[ "$dry_run" -eq 1 ]]; then
    log "[dry-run] would diff specs/*.json against $SNAPSHOTS_DIR"
    exit 0
fi

# ---- guard: snapshots dir must exist ------------------------------------------

if [[ ! -d "$SNAPSHOTS_DIR" ]]; then
    printf 'warning: %s does not exist — skipping snapshot check\n' "$SNAPSHOTS_DIR" >&2
    exit 0
fi

# ---- diff specs against snapshots --------------------------------------------
# TODO (session D): replace this with format-aware snapshot comparison.
# The real implementation should:
#   1. Parse the decided snapshot format (JSON? plain-text normalised?)
#   2. For each specs/<name>.json, load the corresponding snapshot
#   3. Apply any normalisation (e.g. sort keys, strip volatile fields)
#   4. Report per-file diffs with a clear summary
#
# For now, use a recursive diff as a placeholder so the gate runs end-to-end.

fail_count=0
checked=0

for spec_file in "$SPECS_DIR"/*.json; do
    [[ -f "$spec_file" ]] || continue
    name="$(basename "$spec_file" .json)"
    snap_file="$SNAPSHOTS_DIR/${name}.snap"

    if [[ ! -f "$snap_file" ]]; then
        # Snapshot missing — treat as no-diff (snapshot hasn't been recorded yet)
        continue
    fi

    checked=$(( checked + 1 ))
    if ! diff -q "$spec_file" "$snap_file" >/dev/null 2>&1; then
        printf 'DIFF: %s differs from snapshot\n' "$name" >&2
        fail_count=$(( fail_count + 1 ))
    fi
done

if (( fail_count > 0 )); then
    printf 'FAIL: %d spec(s) differ from their snapshots (checked %d)\n' \
        "$fail_count" "$checked" >&2
    exit 1
fi

log "PASS: all $checked snapshot(s) match."
exit 0
