#!/usr/bin/env bash
# scripts/check-binary-size.sh — CI gate: ghost-complete binary size check
#
# USAGE
#   check-binary-size.sh --absolute-max <size>   fail if binary > size
#   check-binary-size.sh --delta-max <size>       fail if (binary - baseline) > size
#
# EXIT CODES
#   0 — gate passed (or dry-run)
#   1 — gate violated (binary too large / delta too large)
#   2 — script/usage error (bad flags, missing dependency, build failure)
#
# DRY-RUN
#   Pass --dry-run or set CI_DRY_RUN=1.  Argument validation still runs; the
#   actual size check is skipped and the script exits 0.  This lets cargo-test
#   / npm-test call-sites invoke the script without triggering a real failure.
#
# SIZE UNITS (case-insensitive)
#   Bare integer = bytes.  K/KB/KiB = 1024.  M/MB/MiB = 1024^2.
#   G/GB/GiB = 1024^3.  MB and MiB are treated identically (no SI/IEC split).
#
# BASELINE FILE
#   benchmarks/binary-size-baseline.txt — one of:
#     49283072                              (bare integer, bytes)
#     49283072\ttarget/release/ghost-complete   (du -b output)
#   Leading whitespace is ignored.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

BINARY_PATH="$REPO_ROOT/target/release/ghost-complete"
BASELINE_FILE="$REPO_ROOT/benchmarks/binary-size-baseline.txt"

usage() {
    cat <<'EOF'
Usage:
  check-binary-size.sh --absolute-max <size>
  check-binary-size.sh --delta-max <size>
  check-binary-size.sh --help

Options:
  --absolute-max <size>   Fail if binary size exceeds <size>.
  --delta-max <size>      Fail if binary size minus recorded baseline exceeds <size>.
                          Requires benchmarks/binary-size-baseline.txt.
  --dry-run               Skip size check; print what would be checked; exit 0.
  --help                  Print this help and exit 0.

Size units (case-insensitive): bare integer (bytes), K/KB/KiB, M/MB/MiB, G/GB/GiB.
MB and MiB are treated identically as 1024*1024 bytes.

Environment:
  CI_DRY_RUN=1   Equivalent to --dry-run.
EOF
}

# ---- parse args ---------------------------------------------------------------

mode=""          # "absolute" | "delta"
limit_raw=""     # raw size string from CLI
dry_run=0

[[ "${CI_DRY_RUN:-0}" == "1" ]] && dry_run=1

while [[ $# -gt 0 ]]; do
    case "$1" in
        --help|-h)
            usage; exit 0 ;;
        --dry-run)
            dry_run=1; shift ;;
        --absolute-max)
            [[ -z "${2:-}" ]] && die "--absolute-max requires an argument"
            mode="absolute"
            limit_raw="$2"
            shift 2 ;;
        --delta-max)
            [[ -z "${2:-}" ]] && die "--delta-max requires an argument"
            mode="delta"
            limit_raw="$2"
            shift 2 ;;
        *)
            die "unknown flag: $1 (use --help for usage)" ;;
    esac
done

[[ -z "$mode" ]] && die "one of --absolute-max or --delta-max is required (use --help for usage)"

# Validate the size argument early (even in dry-run, so bad flags surface).
limit_bytes="$(parse_size "$limit_raw")"

# ---- dry-run early exit -------------------------------------------------------

if [[ "$dry_run" -eq 1 ]]; then
    limit_mb="$(bytes_to_mb "$limit_bytes")"
    log "[dry-run] would check binary size with --${mode}-max ${limit_raw} (${limit_mb})"
    exit 0
fi

# ---- build binary if missing --------------------------------------------------

if [[ ! -f "$BINARY_PATH" ]]; then
    log "Binary not found at $BINARY_PATH — building with cargo build --release"
    if ! command -v cargo &>/dev/null; then
        die "cargo not found; cannot build binary (install Rust: https://rustup.rs)"
    fi
    if ! (cd "$REPO_ROOT" && cargo build --release); then
        die "cargo build --release failed"
    fi
fi

# ---- measure actual binary size -----------------------------------------------

if [[ ! -f "$BINARY_PATH" ]]; then
    die "binary still missing after build: $BINARY_PATH"
fi

actual_bytes="$(wc -c < "$BINARY_PATH")"
# wc -c can leave leading whitespace on some platforms; strip it
actual_bytes="${actual_bytes// /}"
actual_mb="$(bytes_to_mb "$actual_bytes")"

# ---- run the gate -------------------------------------------------------------

case "$mode" in
    absolute)
        limit_mb="$(bytes_to_mb "$limit_bytes")"
        log "Binary size: ${actual_mb} (limit: ${limit_mb})"
        if (( actual_bytes > limit_bytes )); then
            printf 'FAIL: binary size %s exceeds absolute limit %s\n' "$actual_mb" "$limit_mb" >&2
            exit 1
        fi
        log "PASS: binary size within limit."
        ;;

    delta)
        # Read baseline
        if [[ ! -f "$BASELINE_FILE" ]]; then
            die "baseline file not found: $BASELINE_FILE (required for --delta-max)"
        fi
        # Support both "49283072" and "49283072\t<path>" (du -b output).
        # Read the first non-empty line, strip leading whitespace, take first field.
        baseline_bytes=""
        while IFS= read -r line || [[ -n "$line" ]]; do
            # Strip leading whitespace
            line="${line#"${line%%[! ]*}"}"
            [[ -z "$line" ]] && continue
            # Take first whitespace-delimited token (handles tab-separated du -b format)
            baseline_bytes="${line%%[$'\t' ]*}"
            break
        done < "$BASELINE_FILE"

        if [[ -z "$baseline_bytes" ]] || ! [[ "$baseline_bytes" =~ ^[0-9]+$ ]]; then
            die "could not parse baseline bytes from $BASELINE_FILE (got: '$baseline_bytes')"
        fi

        delta=$(( actual_bytes - baseline_bytes ))
        limit_mb="$(bytes_to_mb "$limit_bytes")"
        baseline_mb="$(bytes_to_mb "$baseline_bytes")"
        delta_mb="$(bytes_to_mb "$(( delta < 0 ? 0 : delta ))")"

        log "Binary size: ${actual_mb}  Baseline: ${baseline_mb}  Delta: +${delta_mb}  Limit: ${limit_mb}"

        if (( delta > limit_bytes )); then
            printf 'FAIL: binary growth %s exceeds delta limit %s (baseline: %s, current: %s)\n' \
                "$delta_mb" "$limit_mb" "$baseline_mb" "$actual_mb" >&2
            exit 1
        fi
        log "PASS: binary delta within limit."
        ;;
esac

exit 0
