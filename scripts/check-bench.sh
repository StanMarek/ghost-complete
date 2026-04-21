#!/usr/bin/env bash
# scripts/check-bench.sh — CI gate: benchmark regression check (STUB)
#
# Compares Criterion benchmark results against a recorded baseline.
# Real implementation deferred until baseline-pre-js-port.md lands.
#
# EXIT CODES
#   0 — pass (or missing baseline/data, or dry-run)
#   1 — regression detected (gate violation) — NOT YET IMPLEMENTED
#   2 — script/usage error
#
# DRY-RUN
#   Pass --dry-run or set CI_DRY_RUN=1.  Argument validation still runs;
#   the actual regression check is skipped and the script exits 0.
#
# TODO: real implementation should:
#   - Parse target/criterion/**/estimates.json for each benchmark group
#   - Extract mean.point_estimate (nanoseconds) per benchmark
#   - Compare against the baseline table in benchmarks/baseline-pre-js-port.md
#   - Fail if any benchmark regresses by more than --threshold percent
#   - Report which benchmarks regressed and by how much

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

BASELINE_FILE="$REPO_ROOT/benchmarks/baseline-pre-js-port.md"
CRITERION_DIR="$REPO_ROOT/target/criterion"
DEFAULT_THRESHOLD=10

usage() {
    cat <<'EOF'
Usage:
  check-bench.sh [--threshold <percent>] [--dry-run] [--help]

Checks for benchmark regressions against benchmarks/baseline-pre-js-port.md.
If baseline or Criterion data are absent, exits 0 with a warning.

Options:
  --threshold <pct>   Regression threshold in percent (default: 10).
  --dry-run           Skip regression check; print what would be checked; exit 0.
  --help              Print this help and exit 0.

Environment:
  CI_DRY_RUN=1   Equivalent to --dry-run.
EOF
}

# ---- parse args ---------------------------------------------------------------

dry_run=0
threshold="$DEFAULT_THRESHOLD"
[[ "${CI_DRY_RUN:-0}" == "1" ]] && dry_run=1

while [[ $# -gt 0 ]]; do
    case "$1" in
        --help|-h)
            usage; exit 0 ;;
        --dry-run)
            dry_run=1; shift ;;
        --threshold)
            [[ -z "${2:-}" ]] && die "--threshold requires an argument"
            threshold="$2"
            # Validate it's a non-negative integer
            if ! [[ "$threshold" =~ ^[0-9]+$ ]]; then
                die "--threshold must be a non-negative integer, got: $threshold"
            fi
            shift 2 ;;
        *)
            die "unknown flag: $1 (use --help for usage)" ;;
    esac
done

# ---- dry-run early exit -------------------------------------------------------

if [[ "$dry_run" -eq 1 ]]; then
    log "[dry-run] would check benchmark regressions with --threshold ${threshold}%"
    exit 0
fi

# ---- guard: baseline file must exist ------------------------------------------

if [[ ! -f "$BASELINE_FILE" ]]; then
    printf 'warning: baseline file not found (%s) — skipping benchmark regression check\n' \
        "$BASELINE_FILE" >&2
    exit 0
fi

# ---- guard: Criterion data must exist -----------------------------------------

if [[ ! -d "$CRITERION_DIR" ]]; then
    printf 'warning: no benchmark data found (%s) — run `cargo bench` first\n' \
        "$CRITERION_DIR" >&2
    exit 0
fi

# ---- STUB body ----------------------------------------------------------------
# TODO: replace this placeholder with real regression detection.
# Real logic should:
#   1. Find all target/criterion/**/estimates.json
#   2. Extract mean.point_estimate from each
#   3. Parse the Markdown table in benchmarks/baseline-pre-js-port.md
#      (expected format: | bench_name | mean_ns | ... |)
#   4. For each bench present in both: compute pct_change = (current - base) / base * 100
#   5. If pct_change > threshold, record as regression
#   6. After all benches: if any regressions, print report and exit 1
#      Otherwise print summary and exit 0

log "benchmark regression check not yet implemented — placeholder pass"
exit 0
