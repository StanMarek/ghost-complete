#!/usr/bin/env bash
# scripts/check-bench.sh — CI gate: benchmark regression check
#
# Compares Criterion median timings against the pre-JS-port baseline in
# benchmarks/baseline-pre-js-port.json.  Any benchmark whose current median
# exceeds `baseline_median * (1 + threshold/100)` counts as a regression
# and fails the gate.
#
# EXIT CODES
#   0 — pass (no regressions, or baseline / data missing, or dry-run)
#   1 — regression detected (gate violation)
#   2 — script/usage error
#
# DRY-RUN
#   Pass --dry-run or set CI_DRY_RUN=1.  Argument validation still runs;
#   the actual regression check is skipped and the script exits 0.
#
# DATA SOURCES
#   Baseline:        benchmarks/baseline-pre-js-port.json (machine-readable
#                    sibling of baseline-pre-js-port.md; written by T4a).
#   Current run:     target/criterion/<group>/<bench>/new/estimates.json
#                    (falling back to .../base/estimates.json when a fresh
#                    run hasn't been recorded yet).
#
# METHODOLOGY
#   For each (group, bench) that appears in BOTH baseline and current data:
#     pct_change = (current_median_ns - baseline_median_ns)
#                  / baseline_median_ns * 100
#   Regression iff pct_change > threshold (default: 10%, rounded to integer
#   percent — see `--threshold`).
#
#   Benchmarks present in only one side are reported to stderr but do not
#   fail the gate; CI treats spec additions/removals as deliberate changes
#   to be re-baselined in a separate PR.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

BASELINE_FILE="$REPO_ROOT/benchmarks/baseline-pre-js-port.json"
CRITERION_DIR="$REPO_ROOT/target/criterion"
DEFAULT_THRESHOLD=10

usage() {
    cat <<'EOF'
Usage:
  check-bench.sh [--threshold <percent>] [--dry-run] [--help]

Checks for benchmark regressions against
benchmarks/baseline-pre-js-port.json.  If the baseline file or Criterion
data directory is absent, exits 0 with a warning (local-dev friendly).

Options:
  --threshold <pct>   Regression threshold in percent (default: 10).
                      Must be a non-negative integer.
  --dry-run           Skip regression check; print what would be checked;
                      exit 0.
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
    log "[dry-run] would check benchmark regressions with --threshold ${threshold}%"
    exit 0
fi

# ---- guards: tooling + inputs -------------------------------------------------

command -v jq >/dev/null 2>&1 || die "jq is required but not on PATH"

if [[ ! -f "$BASELINE_FILE" ]]; then
    printf 'warning: baseline file not found (%s) — skipping benchmark regression check\n' \
        "$BASELINE_FILE" >&2
    exit 0
fi

if [[ ! -d "$CRITERION_DIR" ]]; then
    if [[ "${CI:-}" == "true" ]]; then
        # Hard-fail in CI: if this gate runs without criterion output, the
        # workflow forgot to run `cargo bench` (or to install a toolchain)
        # before invoking this script. Silently exiting 0 here would make the
        # gate unconditionally green.
        printf 'error: no benchmark data found (%s) and $CI=true — did the workflow run `cargo bench` before invoking check-bench.sh?\n' \
            "$CRITERION_DIR" >&2
        exit 2
    fi
    printf 'warning: no benchmark data found (%s) — run `cargo bench` first\n' \
        "$CRITERION_DIR" >&2
    exit 0
fi

# ---- scratch dir (used for safe_jq err capture + the TSV join later) ----------
# Created early so safe_jq has a stable place to drop stderr buffers.

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

# safe_jq <file> <jq-args...>
#
# Runs `jq <args> <file>` and prints stdout on success.  On jq parse/runtime
# failure, captures stderr and invokes `die` with a message that NAMES the
# offending file — exits 2 (the script's documented "script/usage error"
# code, which also covers malformed input data).  Without this wrapper, a
# corrupt JSON file would surface in CI logs as the bare `jq: parse error`
# line with exit 5 and no context about which file failed.
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

# ---- collect baseline entries -------------------------------------------------
# Emits TSV: group<TAB>bench<TAB>median_ns

baseline_tsv="$(safe_jq "$BASELINE_FILE" -r '
      .groups
      | to_entries[]
      | .key as $group
      | .value
      | to_entries[]
      | [$group, .key, (.value.median_ns|tostring)]
      | @tsv
    ')"

if [[ -z "$baseline_tsv" ]]; then
    printf 'warning: baseline file has no benchmark entries — skipping check\n' >&2
    exit 0
fi

# ---- collect current entries --------------------------------------------------
# For each group dir in target/criterion, for each bench subdir, read
# new/estimates.json (fallback: base/estimates.json) and emit TSV.

current_tsv=""
# shellcheck disable=SC2044  # group/bench names shouldn't contain whitespace
for group_dir in "$CRITERION_DIR"/*/; do
    [[ -d "$group_dir" ]] || continue
    group="$(basename "$group_dir")"
    # skip criterion's own 'report' aggregate dir
    [[ "$group" == "report" ]] && continue

    for bench_dir in "$group_dir"*/; do
        [[ -d "$bench_dir" ]] || continue
        bench="$(basename "$bench_dir")"
        # skip aggregate/report subdirs
        [[ "$bench" == "report" ]] && continue

        est="$bench_dir/new/estimates.json"
        [[ -f "$est" ]] || est="$bench_dir/base/estimates.json"
        [[ -f "$est" ]] || continue

        median_ns="$(safe_jq "$est" -r '.median.point_estimate')"
        # skip null/NaN
        [[ -z "$median_ns" || "$median_ns" == "null" ]] && continue

        current_tsv+="${group}"$'\t'"${bench}"$'\t'"${median_ns}"$'\n'
    done
done

if [[ -z "$current_tsv" ]]; then
    printf 'warning: no criterion estimates found under %s — skipping check\n' \
        "$CRITERION_DIR" >&2
    exit 0
fi

# ---- diff ---------------------------------------------------------------------
#
# No associative arrays (must work on macOS's bundled bash 3.2).
# Strategy: build temp files of "key<TAB>value" rows, then use `join` on
# sorted copies to find matched / baseline-only / current-only benches.

regressions=0
checked=0
baseline_only=0
current_only=0

baseline_keyed="$work_dir/baseline.keyed"
current_keyed="$work_dir/current.keyed"

# "<group>/<bench>\t<median_ns>"
printf '%s' "$baseline_tsv" | awk -F'\t' 'NF>=3 {printf "%s/%s\t%s\n", $1, $2, $3}' \
    | sort > "$baseline_keyed"
printf '%s' "$current_tsv"  | awk -F'\t' 'NF>=3 {printf "%s/%s\t%s\n", $1, $2, $3}' \
    | sort > "$current_keyed"

# Matched rows (inner join on the first field).
matched="$work_dir/matched"
join -t $'\t' -j 1 "$baseline_keyed" "$current_keyed" > "$matched"

# Benches present only in baseline (-v 1), only in current (-v 2).
join -t $'\t' -j 1 -v 1 "$baseline_keyed" "$current_keyed" > "$work_dir/baseline_only"
join -t $'\t' -j 1 -v 2 "$baseline_keyed" "$current_keyed" > "$work_dir/current_only"

# Report orphans on stderr (non-fatal).
while IFS=$'\t' read -r key _; do
    [[ -z "$key" ]] && continue
    printf 'note: baseline has %s but current run is missing it\n' "$key" >&2
    baseline_only=$(( baseline_only + 1 ))
done < "$work_dir/baseline_only"

while IFS=$'\t' read -r key _; do
    [[ -z "$key" ]] && continue
    printf 'note: current run has %s but baseline has no entry for it\n' "$key" >&2
    current_only=$(( current_only + 1 ))
done < "$work_dir/current_only"

# Regression rows formatted as a single column string; build up lazily.
regression_rows=""

while IFS=$'\t' read -r key baseline_ns cur_ns; do
    [[ -z "$key" ]] && continue
    checked=$(( checked + 1 ))

    # pct_change = (cur - baseline) / baseline * 100
    read -r is_regression pct <<EOF
$(awk -v cur="$cur_ns" -v base="$baseline_ns" -v thr="$threshold" '
    BEGIN {
        if (base <= 0) { print "0 0.00"; exit }
        pct = (cur - base) / base * 100.0
        reg = (pct > thr) ? 1 : 0
        printf "%d %.2f\n", reg, pct
    }
')
EOF

    if [[ "$is_regression" == "1" ]]; then
        regressions=$(( regressions + 1 ))
        g="${key%%/*}"
        b="${key#*/}"
        row="$(printf '  %-24s %-28s %12.3f ms -> %12.3f ms  (%+7s %%)' \
            "$g" "$b" \
            "$(awk -v n="$baseline_ns" 'BEGIN{printf "%.3f", n/1e6}')" \
            "$(awk -v n="$cur_ns"      'BEGIN{printf "%.3f", n/1e6}')" \
            "$pct")"
        regression_rows+="${row}"$'\n'
    fi
done < "$matched"

# ---- report -------------------------------------------------------------------

if (( regressions > 0 )); then
    printf '\nFAIL: %d benchmark(s) regressed by more than %d%%:\n' \
        "$regressions" "$threshold" >&2
    printf '  %-24s %-28s %14s    %14s       %s\n' \
        "group" "bench" "baseline" "current" "change" >&2
    printf '%s' "$regression_rows" >&2
    printf '\n(checked %d, baseline-only %d, current-only %d)\n' \
        "$checked" "$baseline_only" "$current_only" >&2
    exit 1
fi

log "PASS: checked ${checked} benchmark(s); no regressions > ${threshold}%."
if (( baseline_only > 0 || current_only > 0 )); then
    log "note: ${baseline_only} baseline-only, ${current_only} current-only (not a failure)."
fi
exit 0
