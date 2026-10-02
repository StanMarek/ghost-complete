#!/usr/bin/env bash
# scripts/check-new-advisories.sh — CI gate for pull requests
#
# Compares two `cargo audit --json` reports, one for the PR's base lockfile
# and one for its head lockfile, and fails only on RustSec advisories the
# head introduces.
#
# The advisory DB changes independently of the code. A plain `cargo audit`
# on a PR therefore fails for any advisory filed against a crate already on
# master, whether or not the PR touches it. Those stay visible as warnings;
# master itself is audited on push and on a schedule.
#
# An advisory counts as present when any version of the crate it names is
# affected, so bumping a vulnerable crate to a version that is still
# vulnerable is not reported as new. Informational advisories (unmaintained,
# unsound) count; yanked crates carry no advisory and are ignored.
#
# EXIT CODES
#   0 — the head lockfile introduces no advisory
#   1 — the head lockfile introduces at least one advisory
#   2 — usage error, or a report that is not cargo-audit JSON (for example,
#       the advisory DB could not be fetched)
#
# FLAGS
#   --base PATH   cargo-audit JSON report for the base lockfile
#   --head PATH   cargo-audit JSON report for the head lockfile
#   --help        Print usage and exit 0.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

usage() {
    cat <<'EOF'
Usage:
  check-new-advisories.sh --base BASE.json --head HEAD.json [--help]

Fails only on RustSec advisories that the head lockfile introduces relative
to the base lockfile. Advisories already on the base are reported as
warnings, and advisories the head fixes as notices.

Produce the reports with:
  cargo audit --json > HEAD.json
  cargo audit --json --no-fetch --file BASE.lock > BASE.json

Options:
  --base PATH   cargo-audit JSON report for the base lockfile.
  --head PATH   cargo-audit JSON report for the head lockfile.
  --help        Print this help.
EOF
}

base=""
head=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --help|-h) usage; exit 0 ;;
        --base)    base="${2:?--base requires a value}"; shift 2 ;;
        --head)    head="${2:?--head requires a value}"; shift 2 ;;
        *) die "unknown flag: $1 (use --help for usage)" ;;
    esac
done

[[ -n "$base" ]] || die "--base is required (use --help for usage)"
[[ -n "$head" ]] || die "--head is required (use --help for usage)"

for report in "$base" "$head"; do
    [[ -f "$report" ]] || die "report not found: $report"
    jq -e '.vulnerabilities.list | type == "array"' "$report" >/dev/null 2>&1 \
        || die "not a cargo-audit JSON report: $report"
done

# One TSV row per finding: status, advisory id, crate, version, title.
findings="$(jq -r -n --slurpfile base "$base" --slurpfile head "$head" '
    def advisories:
        [.vulnerabilities.list[], ((.warnings // {})[][])]
        | map(select(.advisory != null)
              | {id: .advisory.id, crate: .package.name,
                 version: .package.version, title: .advisory.title});
    def key: .id + " " + .crate;
    ($base[0] | advisories) as $b
    | ($head[0] | advisories) as $h
    | ($b | map(key)) as $base_keys
    | ($h | map(key)) as $head_keys
    | ($h[] | key as $k
            | [if any($base_keys[]; . == $k) then "existing" else "new" end,
               .id, .crate, .version, .title]),
      ($b[] | key as $k | select(any($head_keys[]; . == $k) | not)
            | ["fixed", .id, .crate, .version, .title])
    | @tsv')"

# Workflow-command message escaping: %, CR and LF.
escape() {
    local s="${1//%/%25}"
    s="${s//$'\r'/%0D}"
    printf '%s' "${s//$'\n'/%0A}"
}

introduced=0
existing=0
fixed=0

while IFS=$'\t' read -r status id crate version title; do
    [[ -n "$status" ]] || continue
    message="$(escape "$crate $version: $title")"
    case "$status" in
        new)
            introduced=$(( introduced + 1 ))
            printf '::error title=%s introduced by this PR::%s\n' "$id" "$message"
            ;;
        existing)
            existing=$(( existing + 1 ))
            printf '::warning title=%s already on the base branch::%s\n' "$id" "$message"
            ;;
        fixed)
            fixed=$(( fixed + 1 ))
            printf '::notice title=%s fixed by this PR::%s\n' "$id" "$message"
            ;;
    esac
done <<< "$findings"

log "advisories: $introduced introduced, $existing already on the base branch, $fixed fixed"
[[ "$introduced" -eq 0 ]]
