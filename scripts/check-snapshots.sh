#!/usr/bin/env bash
# scripts/check-snapshots.sh — CI gate: spec snapshot diff check
#
# Re-runs the snapshotter (tools/fig-converter/scripts/snapshot-specs.mjs)
# into a temp directory and diffs the result against
# specs/__snapshots__/.  This catches any spec drift that hasn't been
# re-baselined.
#
# EXIT CODES
#   0 — pass (or snapshots dir absent, or dry-run)
#   1 — snapshots differ (gate violation)
#   2 — script/usage error
#
# DRY-RUN
#   Pass --dry-run or set CI_DRY_RUN=1.  Argument validation still runs;
#   the actual diff is skipped and the script exits 0.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

SPECS_DIR="$REPO_ROOT/specs"
SNAPSHOTS_DIR="$SPECS_DIR/__snapshots__"
SNAPSHOTTER="$REPO_ROOT/tools/fig-converter/scripts/snapshot-specs.mjs"

usage() {
    cat <<'EOF'
Usage:
  check-snapshots.sh [--dry-run] [--help]

Re-runs the spec snapshotter into a temp directory and diffs the output
against specs/__snapshots__/.  Reports any spec that has drifted from its
recorded snapshot.  If the snapshots directory does not exist, exits 0
with a warning.

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
    log "[dry-run] would re-snapshot specs/ and diff against $SNAPSHOTS_DIR"
    exit 0
fi

# ---- guard: snapshots dir must exist ------------------------------------------

if [[ ! -d "$SNAPSHOTS_DIR" ]]; then
    if [[ "${CI:-}" == "true" ]]; then
        # Hard-fail in CI: ~700 .snap files are committed and load-bearing.
        # A missing snapshots dir under $CI=true indicates a checkout failure;
        # silently exiting 0 would make the gate unconditionally green.
        printf 'error: %s does not exist and $CI=true — snapshots are committed, this indicates a checkout failure\n' \
            "$SNAPSHOTS_DIR" >&2
        exit 2
    fi
    printf 'warning: %s does not exist — skipping snapshot check\n' "$SNAPSHOTS_DIR" >&2
    exit 0
fi

# ---- guard: snapshotter script must exist -------------------------------------

if [[ ! -f "$SNAPSHOTTER" ]]; then
    die "snapshotter not found: $SNAPSHOTTER"
fi

# ---- re-snapshot into temp dir ------------------------------------------------

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

if ! node "$SNAPSHOTTER" --out "$tmp_dir" >/dev/null; then
    die "snapshotter failed"
fi

# ---- diff ---------------------------------------------------------------------

fail_count=0
checked=0

# Compare every recorded snapshot against its freshly-generated counterpart.
for snap_file in "$SNAPSHOTS_DIR"/*.snap; do
    [[ -f "$snap_file" ]] || continue
    name="$(basename "$snap_file" .snap)"
    fresh="$tmp_dir/${name}.snap"

    checked=$(( checked + 1 ))

    if [[ ! -f "$fresh" ]]; then
        printf 'DIFF: %s — snapshot exists but source spec is gone\n' "$name" >&2
        fail_count=$(( fail_count + 1 ))
        continue
    fi

    if ! diff -q "$snap_file" "$fresh" >/dev/null 2>&1; then
        printf 'DIFF: %s differs from snapshot\n' "$name" >&2
        fail_count=$(( fail_count + 1 ))
    fi
done

# Also report specs that have no recorded snapshot yet.
for fresh in "$tmp_dir"/*.snap; do
    [[ -f "$fresh" ]] || continue
    name="$(basename "$fresh" .snap)"
    if [[ ! -f "$SNAPSHOTS_DIR/${name}.snap" ]]; then
        printf 'DIFF: %s — new spec has no recorded snapshot\n' "$name" >&2
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
