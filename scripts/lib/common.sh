#!/usr/bin/env bash
# scripts/lib/common.sh — shared helpers for CI gate scripts
# Sourced by check-*.sh; not executed directly.

# log <msg>  — print an info line to stdout
log() { printf '%s\n' "$*"; }

# die <msg>  — print error to stderr and exit 2 (usage/script error)
die() { printf 'error: %s\n' "$*" >&2; exit 2; }

# parse_size <string> → prints integer byte count, exits 2 on bad input.
# Accepted suffixes (case-insensitive):
#   (none) = bytes
#   K / KB / KiB  = 1024
#   M / MB / MiB  = 1024^2   (MB and MiB are treated identically — no SI/IEC distinction)
#   G / GB / GiB  = 1024^3
parse_size() {
    local raw="${1:?parse_size requires an argument}"
    # Extract numeric part and suffix
    local num suffix
    if [[ "$raw" =~ ^([0-9]+)([a-zA-Z]*)$ ]]; then
        num="${BASH_REMATCH[1]}"
        suffix="${BASH_REMATCH[2]}"
    else
        printf 'error: invalid size value: %s\n' "$raw" >&2
        exit 2
    fi

    # Normalise suffix to upper-case
    local upper_suffix
    upper_suffix="$(printf '%s' "$suffix" | tr '[:lower:]' '[:upper:]')"

    local multiplier
    case "$upper_suffix" in
        ""|B)          multiplier=1 ;;
        K|KB|KIB)      multiplier=1024 ;;
        M|MB|MIB)      multiplier=$((1024 * 1024)) ;;
        G|GB|GIB)      multiplier=$((1024 * 1024 * 1024)) ;;
        *)
            printf 'error: unrecognised size unit: %s (in %s)\n' "$suffix" "$raw" >&2
            exit 2
            ;;
    esac

    printf '%d' $(( num * multiplier ))
}

# bytes_to_mb <bytes> → prints human-readable "X.XX MB"
bytes_to_mb() {
    local bytes="${1:?}"
    # Use awk for floating-point division
    awk -v b="$bytes" 'BEGIN { printf "%.2f MB", b / (1024*1024) }'
}
