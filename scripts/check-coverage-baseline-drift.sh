#!/usr/bin/env bash
# scripts/check-coverage-baseline-drift.sh — CI gate (non-failing)
#
# Inspects docs/coverage-baseline.json and emits a GitHub Actions
# `::warning::` annotation if the latest release row's timestamp is more
# than DRIFT_THRESHOLD_DAYS (default: 120) in the past.
#
# Release cadence is roughly monthly. Two releases old is ~60–90 days;
# 120 is a comfortable buffer before we nag.
#
# EXIT CODES
#   0 — always. This gate NEVER fails. It only annotates.
#   (Usage errors still exit 2 so misconfigured invocations are loud.)
#
# FLAGS
#   --quiet        Suppress the "ok" line when no drift is detected.
#   --threshold N  Override DRIFT_THRESHOLD_DAYS for this invocation.
#   --baseline P   Override the baseline JSON path (default: docs/coverage-baseline.json).
#   --help         Print usage and exit 0.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

BASELINE_DEFAULT="$REPO_ROOT/docs/coverage-baseline.json"
DRIFT_THRESHOLD_DAYS="${DRIFT_THRESHOLD_DAYS:-120}"

usage() {
    cat <<'EOF'
Usage:
  check-coverage-baseline-drift.sh [--quiet] [--threshold N] [--baseline PATH] [--help]

Warns (never fails) if docs/coverage-baseline.json's latest release row is
older than the drift threshold.

Options:
  --quiet            Suppress the "ok" line when no drift is detected.
  --threshold N      Override drift threshold in days (default: 120).
  --baseline PATH    Override baseline JSON path.
  --help             Print this help.

Environment:
  DRIFT_THRESHOLD_DAYS   Same as --threshold.
EOF
}

quiet=0
baseline="$BASELINE_DEFAULT"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --help|-h)    usage; exit 0 ;;
        --quiet)      quiet=1; shift ;;
        --threshold)  DRIFT_THRESHOLD_DAYS="${2:?--threshold requires a value}"; shift 2 ;;
        --baseline)   baseline="${2:?--baseline requires a value}"; shift 2 ;;
        *) die "unknown flag: $1 (use --help for usage)" ;;
    esac
done

if [[ ! -f "$baseline" ]]; then
    # Missing baseline shouldn't fail this gate — it's non-blocking by design.
    printf '::warning::Coverage baseline not found at %s — run `ghost-complete status --json` per docs/SPECS.md\n' "$baseline"
    exit 0
fi

if ! command -v jq >/dev/null 2>&1; then
    die "jq is required but not installed"
fi

# Latest row = last entry in the releases array.
latest_ts="$(jq -r '.releases[-1].timestamp // empty' "$baseline")"
if [[ -z "$latest_ts" ]]; then
    printf '::warning::Coverage baseline at %s has no releases — run `ghost-complete status --json` per docs/SPECS.md\n' "$baseline"
    exit 0
fi

# Convert timestamp (ISO 8601) to epoch seconds. Portable across GNU + BSD date.
to_epoch() {
    local ts="$1"
    # Try GNU date first, fall back to BSD/macOS date.
    if date -u -d "$ts" +%s >/dev/null 2>&1; then
        date -u -d "$ts" +%s
    else
        # BSD date: strip trailing Z to satisfy -f
        local stripped="${ts%Z}"
        date -u -j -f "%Y-%m-%dT%H:%M:%S" "$stripped" +%s
    fi
}

latest_epoch="$(to_epoch "$latest_ts")"
now_epoch="$(date -u +%s)"

delta_seconds=$(( now_epoch - latest_epoch ))
# Integer-round delta in days. Negative deltas (future timestamps) are
# clamped to 0 — a future timestamp isn't drift, just a weird clock.
if (( delta_seconds < 0 )); then
    delta_days=0
else
    delta_days=$(( delta_seconds / 86400 ))
fi

if (( delta_days > DRIFT_THRESHOLD_DAYS )); then
    printf '::warning::Coverage baseline is %d days old — refresh via `ghost-complete status --json` per docs/SPECS.md\n' "$delta_days"
else
    if (( quiet == 0 )); then
        log "ok: coverage baseline is ${delta_days} days old (threshold ${DRIFT_THRESHOLD_DAYS})"
    fi
fi

exit 0
