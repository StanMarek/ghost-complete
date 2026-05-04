#!/usr/bin/env bash
# scripts/count-spec-coverage.sh — pre-loader spec corpus counter
#
# Reports the corpus-level metrics that `docs/coverage-baseline.json` rows
# track. Reads from `specs/*.json` (the embedded spec source) by default.
#
# OUTPUTS
#   Plain text by default. `--json` emits a single JSON object.
#
# METRICS
#   spec_files_total                    — *.json files in spec dir.
#   commands_addressable                — unique top-level `name` values.
#   file_scan_fully_functional          — files with zero requires_js generators.
#   file_scan_partially_functional      — files with ≥1 requires_js generator.
#   commands_nonfunctional              — always 0 in this counter; load-failure
#                                          classification is owned by the runtime.
#                                          Use `ghost-complete status --json`
#                                          for load-failure detection.
#   requires_js_generators_total        — every {requires_js: true} object,
#                                          counted via `[..|objects|select(.requires_js==true)]`.
#   requires_js_generators_supported    — always 0 in this counter; runtime-supported
#                                          coverage comes from
#                                          `ghost-complete status --json`.
#   requires_js_generators_unsupported  — always == total in this counter
#                                          (no js_runtime inspection here).
#   command_alias_conflicts             — files where the JSON `name` differs
#                                          from the file stem.
#
# NOTE: the fully/partially counts here are FILE-level and use the
# `file_scan_*` prefix to disambiguate from the runtime-loader-level
# counts (`ghost-complete status --json`'s `commands_fully_functional` /
# `commands_partially_functional` fields). `status` keys SpecStore on
# JSON `name` and applies first-match-wins dedup, so a duplicate-name
# pair where one variant is partial and the other is fully-functional
# may be classified differently depending on load order. The runtime is
# the source of truth for what users see at completion time; this
# script reports the corpus-wide truth, before any loader-side
# filtering. Keys that align across the two scans (e.g. `commands_addressable`,
# `requires_js_generators_total`) keep the `commands_*` prefix.
#
# Cross-checks against `ghost-complete status --json` are documented in
# docs/SPECS.md.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

SPECS_DIR="$REPO_ROOT/specs"
emit_json=0

usage() {
    cat <<'EOF'
Usage:
  count-spec-coverage.sh [--specs PATH] [--json] [--help]

Counts the pre-loader baseline metrics across the spec corpus.
It does not compute runtime-supported js_runtime coverage; use
`ghost-complete status --json` for supported/unsupported runtime counts.

Options:
  --specs PATH   Override the spec directory (default: $REPO_ROOT/specs).
  --json         Emit a single JSON object instead of human-readable text.
  --help         Print this help.

Requires:
  jq, find
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        -h|--help)  usage; exit 0 ;;
        --specs)    SPECS_DIR="${2:?--specs requires a value}"; shift 2 ;;
        --json)     emit_json=1; shift ;;
        *)          echo "unknown flag: $1 (use --help)" >&2; exit 2 ;;
    esac
done

if [[ ! -d "$SPECS_DIR" ]]; then
    echo "spec dir does not exist: $SPECS_DIR" >&2
    exit 2
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "jq is required but not installed" >&2
    exit 2
fi

# Use a lightweight jq query repeated across files. Per-file overhead is
# small compared to the total count walks. (A merge-then-walk approach
# would be faster but the corpus is ~50MB and jq's slurp-then-walk hits
# memory on 53MB AWS spec.)

count_spec_files=0
count_requires_js=0
count_partial=0
count_full=0
count_alias=0
names_tmp="$(mktemp)"
trap 'rm -f "$names_tmp"' EXIT

while IFS= read -r f; do
    count_spec_files=$((count_spec_files + 1))
    base="$(basename "$f" .json)"
    name="$(jq -r '.name // ""' "$f" 2>/dev/null || echo '')"
    js_per_file="$(jq -r '[.. | objects | select(has("requires_js") and .requires_js == true)] | length' "$f" 2>/dev/null || echo 0)"
    count_requires_js=$((count_requires_js + js_per_file))
    if (( js_per_file > 0 )); then
        count_partial=$((count_partial + 1))
    else
        count_full=$((count_full + 1))
    fi
    if [[ "$name" != "$base" ]]; then
        count_alias=$((count_alias + 1))
    fi
    printf '%s\n' "$name" >> "$names_tmp"
done < <(find "$SPECS_DIR" -maxdepth 1 -name '*.json' -type f | sort)

count_addressable="$(sort -u "$names_tmp" | wc -l | tr -d ' ')"
# Pre-loader counter: load-failure classification is owned by the runtime.
count_nonfunctional=0
# Pre-loader counter: js_runtime metadata is not inspected here. The runtime
# coverage split comes from `ghost-complete status --json`.
count_supported=0
count_unsupported=$count_requires_js

if (( emit_json )); then
    cat <<EOF
{
  "spec_files_total": $count_spec_files,
  "commands_addressable": $count_addressable,
  "file_scan_fully_functional": $count_full,
  "file_scan_partially_functional": $count_partial,
  "commands_nonfunctional": $count_nonfunctional,
  "requires_js_generators_total": $count_requires_js,
  "requires_js_generators_supported": $count_supported,
  "requires_js_generators_unsupported": $count_unsupported,
  "command_alias_conflicts": $count_alias
}
EOF
else
    cat <<EOF
spec_files_total                    $count_spec_files
commands_addressable                $count_addressable
file_scan_fully_functional          $count_full
file_scan_partially_functional      $count_partial
commands_nonfunctional              $count_nonfunctional
requires_js_generators_total        $count_requires_js
requires_js_generators_supported    $count_supported
requires_js_generators_unsupported  $count_unsupported
command_alias_conflicts             $count_alias
EOF
fi
