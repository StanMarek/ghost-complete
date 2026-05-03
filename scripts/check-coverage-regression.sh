#!/usr/bin/env bash
# scripts/check-coverage-regression.sh — CI gate (introduced in UX-9 Phase 7)
#
# Reads the latest release row from docs/coverage-baseline.json and the live
# corpus counters from `ghost-complete status --json`, then fails when the
# unsupported requires_js generator count exceeds the baseline by more than
# the configured tolerance, or when any command becomes nonfunctional.
#
# The intent is to surface PRs that silently regress JS coverage (e.g., a
# converter change that drops js_runtime metadata, or a spec edit that
# moves a previously-supported generator into the unsupported bucket).
#
# EXIT CODES
#   0 — pass (or "no baseline yet" with a warning).
#   1 — regression detected (gate violation).
#   2 — usage / script error.
#
# FLAGS
#   --baseline PATH        Override docs/coverage-baseline.json path.
#   --tolerance N          Allow up to N additional unsupported generators
#                          before failing (default: 0).
#   --binary PATH          Path to a prebuilt ghost-complete binary
#                          (default: target/release/ghost-complete).
#   --status-json PATH     Skip the binary invocation and read a pre-captured
#                          status --json file directly.
#                          Overrides --binary.
#   --quiet                Suppress the "ok" line on success.
#   --help                 Print usage and exit 0.
#
# ENVIRONMENT
#   COVERAGE_REGRESSION_TOLERANCE   Same as --tolerance.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

BASELINE_DEFAULT="$REPO_ROOT/docs/coverage-baseline.json"
BIN_DEFAULT="$REPO_ROOT/target/release/ghost-complete"
TOLERANCE="${COVERAGE_REGRESSION_TOLERANCE:-0}"

usage() {
    cat <<'EOF'
Usage:
  check-coverage-regression.sh [--baseline PATH] [--tolerance N]
                               [--binary PATH | --status-json PATH]
                               [--quiet] [--help]

Fails when the unsupported requires_js generator count rises above the
baseline by more than the configured tolerance, or when any command
becomes nonfunctional.

Options:
  --baseline PATH       Override baseline JSON path.
  --tolerance N         Allow up to N additional unsupported generators
                        (default: 0).
  --binary PATH         Path to ghost-complete binary
                        (default: target/release/ghost-complete).
  --status-json PATH    Skip binary invocation; read a pre-captured
                        status --json file. Overrides --binary.
  --quiet               Suppress "ok:" line on success.
  --help                Print this help.

Environment:
  COVERAGE_REGRESSION_TOLERANCE   Same as --tolerance.
EOF
}

baseline="$BASELINE_DEFAULT"
binary="$BIN_DEFAULT"
status_json=""
quiet=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --help|-h)        usage; exit 0 ;;
        --baseline)       baseline="${2:?--baseline requires a value}"; shift 2 ;;
        --tolerance)      TOLERANCE="${2:?--tolerance requires a value}"; shift 2 ;;
        --binary)         binary="${2:?--binary requires a value}"; shift 2 ;;
        --status-json)    status_json="${2:?--status-json requires a value}"; shift 2 ;;
        --quiet)          quiet=1; shift ;;
        *) die "unknown flag: $1 (use --help for usage)" ;;
    esac
done

if ! command -v jq >/dev/null 2>&1; then
    die "jq is required but not installed"
fi

# Validate tolerance is a non-negative integer.
if ! [[ "$TOLERANCE" =~ ^[0-9]+$ ]]; then
    die "tolerance must be a non-negative integer (got: $TOLERANCE)"
fi

# ---- read baseline -----------------------------------------------------------

if [[ ! -f "$baseline" ]]; then
    # Missing baseline: treat as a soft signal; the drift gate handles
    # the "you forgot to refresh" case. Don't fail the regression gate
    # when the file is gone — it can't tell us anything.
    printf '::warning::Coverage baseline not found at %s — skipping regression check\n' "$baseline" >&2
    exit 0
fi

# Latest row = last entry in the releases array. Read both the legacy
# fields and the UX-9 fields. The UX-9 fields may be absent on older
# baseline rows — `// 0` fills them in so the comparison still works.
baseline_unsupported="$(jq -r '.releases[-1].requires_js_generators_unsupported // 0' "$baseline")"
baseline_supported="$(jq -r '.releases[-1].requires_js_generators_supported // 0' "$baseline")"
baseline_total="$(jq -r '.releases[-1].requires_js_generators_total // .releases[-1].requires_js_generators // 0' "$baseline")"
baseline_version="$(jq -r '.releases[-1].version // "unknown"' "$baseline")"

# ---- collect current status numbers ------------------------------------------

if [[ -n "$status_json" ]]; then
    # Allow `-` for stdin and process substitution paths under /dev/fd or
    # /proc/self/fd so the script composes nicely in pipelines without
    # round-tripping through a temp file.
    if [[ "$status_json" == "-" ]]; then
        status_payload="$(cat)"
    elif [[ -r "$status_json" ]]; then
        status_payload="$(cat "$status_json")"
    elif [[ ! -e "$status_json" ]]; then
        die "status-json file does not exist: $status_json"
    else
        die "status-json file is not readable: $status_json"
    fi
else
    if [[ ! -x "$binary" ]]; then
        die "ghost-complete binary not found or not executable: $binary (build with cargo build --release, or pass --status-json)"
    fi
    # Run with --log-level error to avoid noisy tracing output on stderr;
    # capture stdout only.
    status_payload="$("$binary" --log-level error status --json)"
fi

current_unsupported="$(printf '%s' "$status_payload" | jq -r '.spec_counts.requires_js_generators_unsupported // 0')"
current_supported="$(printf '%s' "$status_payload" | jq -r '.spec_counts.requires_js_generators_supported // 0')"
current_total="$(printf '%s' "$status_payload" | jq -r '.spec_counts.requires_js_generators_total // 0')"
current_nonfunctional="$(printf '%s' "$status_payload" | jq -r '.spec_counts.commands_nonfunctional // 0')"

# ---- gate logic --------------------------------------------------------------

fail=0

# Any nonfunctional command is always a defect — independent of baseline.
if (( current_nonfunctional > 0 )); then
    printf 'FAIL: %d command(s) are nonfunctional. Any nonfunctional command is a regression — investigate before merging.\n' \
        "$current_nonfunctional" >&2
    fail=1
fi

# Unsupported delta: fail when the new count exceeds baseline + tolerance.
unsupported_max=$(( baseline_unsupported + TOLERANCE ))
if (( current_unsupported > unsupported_max )); then
    delta=$(( current_unsupported - baseline_unsupported ))
    printf 'FAIL: requires_js_generators_unsupported regressed by %d (baseline=%d, current=%d, tolerance=%d).\n' \
        "$delta" "$baseline_unsupported" "$current_unsupported" "$TOLERANCE" >&2
    printf '      To accept the new floor, update docs/coverage-baseline.json with a fresh release row.\n' >&2
    fail=1
fi

# Soft warning: an INCREASE that is still within tolerance — annotate so
# maintainers see it in the PR checks panel without blocking the merge.
if (( current_unsupported > baseline_unsupported && current_unsupported <= unsupported_max )); then
    printf '::warning::Unsupported requires_js generators rose %d within tolerance (baseline=%d, current=%d, tolerance=%d). Consider refreshing docs/coverage-baseline.json to record the new floor.\n' \
        "$(( current_unsupported - baseline_unsupported ))" \
        "$baseline_unsupported" "$current_unsupported" "$TOLERANCE"
fi

if (( fail != 0 )); then
    exit 1
fi

if (( quiet == 0 )); then
    log "ok: requires_js coverage holds (unsupported: ${current_unsupported}/${current_total}, supported: ${current_supported}; baseline ${baseline_version} unsupported=${baseline_unsupported}, tolerance=${TOLERANCE}, nonfunctional=${current_nonfunctional})"
fi

exit 0
