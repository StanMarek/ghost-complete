#!/usr/bin/env bash
# scripts/check-bench.sh — CI gate: benchmark regression check (base↔HEAD)
#
# Reads Criterion's per-bench change/estimates.json files, which are
# produced when `cargo bench -- --baseline <name>` is run against a
# previously saved baseline on the same machine. Fails if any bench's
# median change exceeds `threshold` percent.
#
# This design deliberately avoids a stored JSON baseline compared
# across separate CI runs: shared hosted runners vary materially
# between invocations (measured inter-run variance on macos-latest is
# routinely ±15-20% on single-threaded latency benches), so a
# stored-baseline gate is noise-dominated and cannot reliably
# distinguish real regressions from runner variance.
#
# Instead, the CI job benches the base tree AND the HEAD tree on the
# SAME runner back-to-back: `cargo bench -- --save-baseline base`
# against the base tree, then `cargo bench -- --baseline base` against
# HEAD. Criterion writes the comparison deltas to
# `target/criterion/<group>/<bench>/change/estimates.json`, and this
# script reads that field directly.
#
# EXIT CODES
#   0 — pass (no regressions, or data missing, or dry-run)
#   1 — regression detected (gate violation)
#   2 — script/usage error
#
# DRY-RUN
#   Pass --dry-run or set CI_DRY_RUN=1. Argument validation still
#   runs; the actual regression check is skipped and the script exits 0.
#
# DATA SOURCE
#   target/criterion/<group>/<bench>/change/estimates.json
#   Specifically `.median.point_estimate`, which Criterion emits as a
#   fraction (e.g. 0.13 = +13%).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

CRITERION_DIR="$REPO_ROOT/target/criterion"
DEFAULT_THRESHOLD=10

usage() {
    cat <<'EOF'
Usage:
  check-bench.sh [--threshold <percent>] [--dry-run] [--help]

Checks Criterion change/estimates.json files for regressions >threshold%
relative to the last `--save-baseline`-captured run on the same machine.
If the criterion data directory or change records are absent, exits 0
with a warning (local-dev friendly; hard-fails in CI).

Options:
  --threshold <pct>   Regression threshold in percent (default: 10).
                      Must be a non-negative integer.
  --dry-run           Skip regression check; print what would be
                      checked; exit 0.
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
    log "[dry-run] would check benchmark change records with --threshold ${threshold}%"
    exit 0
fi

# ---- guards -------------------------------------------------------------------

command -v jq >/dev/null 2>&1 || die "jq is required but not on PATH"

if [[ ! -d "$CRITERION_DIR" ]]; then
    if [[ "${CI:-}" == "true" ]]; then
        printf 'error: no benchmark data found (%s) and $CI=true — did the workflow run `cargo bench --save-baseline base` and `cargo bench --baseline base`?\n' \
            "$CRITERION_DIR" >&2
        exit 2
    fi
    printf 'warning: no benchmark data found (%s) — run `cargo bench` first\n' \
        "$CRITERION_DIR" >&2
    exit 0
fi

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

safe_jq() {
    local file="$1"; shift
    local err_file="$work_dir/jq-err.$$"
    local out
    if ! out="$(jq "$@" "$file" 2>"$err_file")"; then
        local err; err="$(cat "$err_file")"
        rm -f "$err_file"
        die "failed to parse $file: $err"
    fi
    rm -f "$err_file"
    printf '%s' "$out"
}

# ---- collect change records ---------------------------------------------------
# For each bench dir, read change/estimates.json if present. Emit TSV:
# group<TAB>bench<TAB>median_pct<TAB>lower_pct<TAB>upper_pct

change_tsv=""
for group_dir in "$CRITERION_DIR"/*/; do
    [[ -d "$group_dir" ]] || continue
    group="$(basename "$group_dir")"
    [[ "$group" == "report" ]] && continue

    for bench_dir in "$group_dir"*/; do
        [[ -d "$bench_dir" ]] || continue
        bench="$(basename "$bench_dir")"
        [[ "$bench" == "report" ]] && continue

        est="$bench_dir/change/estimates.json"
        [[ -f "$est" ]] || continue

        # .median.point_estimate is a fraction; convert to percent.
        row="$(safe_jq "$est" -r '
            [
              (.median.point_estimate * 100),
              (.median.confidence_interval.lower_bound * 100),
              (.median.confidence_interval.upper_bound * 100)
            ]
            | @tsv
        ')"
        [[ -z "$row" ]] && continue
        change_tsv+="${group}"$'\t'"${bench}"$'\t'"${row}"$'\n'
    done
done

if [[ -z "$change_tsv" ]]; then
    if [[ "${CI:-}" == "true" ]]; then
        printf 'error: no change/estimates.json files under %s and $CI=true — did the bench step pass both --save-baseline on the base tree AND --baseline on the HEAD tree?\n' \
            "$CRITERION_DIR" >&2
        exit 2
    fi
    printf 'warning: no change/estimates.json files under %s — skipping check (did you run `cargo bench -- --baseline <name>` on the current tree after saving a baseline on the base tree?)\n' \
        "$CRITERION_DIR" >&2
    exit 0
fi

# ---- diff ---------------------------------------------------------------------

regressions=0
checked=0
regression_rows=""

while IFS=$'\t' read -r group bench median_pct lower_pct upper_pct; do
    [[ -z "$group" ]] && continue
    checked=$(( checked + 1 ))

    is_regression="$(awk -v pct="$median_pct" -v thr="$threshold" \
        'BEGIN { print (pct > thr) ? 1 : 0 }')"

    if [[ "$is_regression" == "1" ]]; then
        regressions=$(( regressions + 1 ))
        row="$(printf '  %-24s %-28s  %+7.2f %%  (95%% CI: %+6.2f %% .. %+6.2f %%)' \
            "$group" "$bench" "$median_pct" "$lower_pct" "$upper_pct")"
        regression_rows+="${row}"$'\n'
    fi
done <<< "$change_tsv"

# ---- report -------------------------------------------------------------------

if (( regressions > 0 )); then
    printf '\nFAIL: %d benchmark(s) regressed by more than %d%% (median of change vs base tree):\n' \
        "$regressions" "$threshold" >&2
    printf '  %-24s %-28s  %9s  %s\n' \
        "group" "bench" "change" "confidence" >&2
    printf '%s' "$regression_rows" >&2
    printf '\n(checked %d benchmark(s))\n' "$checked" >&2
    exit 1
fi

log "PASS: checked ${checked} benchmark(s); no median regressions > ${threshold}%."
exit 0
