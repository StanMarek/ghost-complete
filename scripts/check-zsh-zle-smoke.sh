#!/usr/bin/env bash
# Standalone zsh/ZLE smoke. Runs /bin/zsh --no-rcs, sources the production
# shell integration, drives _gc_report_buffer through ZLE, asserts OSC 7772
# emission and percent-encoding for ;, BEL, ESC, %, UTF-8, and cursor
# positions. CI-required on macOS; never falls back silently.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
SHELL_INTEGRATION="${REPO_ROOT}/shell/ghost-complete.zsh"

if [[ ! -f "${SHELL_INTEGRATION}" ]]; then
    echo "FAIL: shell integration not found at ${SHELL_INTEGRATION}" >&2
    exit 1
fi

if ! command -v zsh >/dev/null 2>&1; then
    echo "FAIL: zsh not on PATH" >&2
    exit 2
fi

assert_contains() {
    local needle="$1" haystack="$2"
    if [[ "${haystack}" != *"${needle}"* ]]; then
        printf 'FAIL: expected %q in output but missing\nOutput:\n%s\n' \
            "${needle}" "${haystack}" >&2
        exit 1
    fi
}

assert_not_contains() {
    local needle="$1" haystack="$2"
    if [[ "${haystack}" == *"${needle}"* ]]; then
        printf 'FAIL: unexpected %q in output\nOutput:\n%s\n' \
            "${needle}" "${haystack}" >&2
        exit 1
    fi
}

# Test: percent-encode semicolons in buffer.
out_semi=$(GHOST_COMPLETE_ACTIVE=1 zsh --no-rcs -c "
source '${SHELL_INTEGRATION}'
BUFFER='git log; ls -la'
CURSOR=16
_gc_report_buffer
" 2>&1)
assert_contains $'\e]7772;16;' "${out_semi}"
assert_contains 'git log%3B ls -la' "${out_semi}"
assert_not_contains $';16;git log;' "${out_semi}"  # raw ; would corrupt frame

# Test: percent-encode BEL (0x07) in buffer.
out_bel=$(GHOST_COMPLETE_ACTIVE=1 zsh --no-rcs -c "
source '${SHELL_INTEGRATION}'
BUFFER=\$'\x07alert'
CURSOR=0
_gc_report_buffer
" 2>&1)
assert_contains '%07alert' "${out_bel}"

# Test: percent-encode ESC (0x1B).
out_esc=$(GHOST_COMPLETE_ACTIVE=1 zsh --no-rcs -c "
source '${SHELL_INTEGRATION}'
BUFFER=\$'\x1bX'
CURSOR=0
_gc_report_buffer
" 2>&1)
assert_contains '%1B' "${out_esc}"
assert_contains 'X' "${out_esc}"

# Test: percent-encode percent sign (literal % in buffer).
out_pct=$(GHOST_COMPLETE_ACTIVE=1 zsh --no-rcs -c "
source '${SHELL_INTEGRATION}'
BUFFER='100%'
CURSOR=4
_gc_report_buffer
" 2>&1)
assert_contains $'\e]7772;4;100%25' "${out_pct}"

# Test: UTF-8 round-trip (κόσμε).
out_utf=$(GHOST_COMPLETE_ACTIVE=1 zsh --no-rcs -c "
source '${SHELL_INTEGRATION}'
BUFFER='κόσμε'
CURSOR=5
_gc_report_buffer
" 2>&1)
assert_contains '%CE%BA%CF%8C%CF%83%CE%BC%CE%B5' "${out_utf}"

# Test: OSC 7 path encoder no longer leaks ';'.
out_osc7=$(zsh --no-rcs -c "
source '${SHELL_INTEGRATION}'
printf 'PATH=%s\n' \"\$(_gc_urlencode_path '/tmp/foo;bar/baz')\"
" 2>&1)
assert_contains 'PATH=/tmp/foo%3Bbar/baz' "${out_osc7}"

# Test: gate guard — without GHOST_COMPLETE_ACTIVE, _gc_report_buffer is a no-op.
out_gate=$(env -u GHOST_COMPLETE_ACTIVE zsh --no-rcs -c "
source '${SHELL_INTEGRATION}'
BUFFER='leaked'
CURSOR=6
_gc_report_buffer
echo 'after'
" 2>&1)
assert_not_contains $'\e]7772' "${out_gate}"
assert_contains 'after' "${out_gate}"

echo "OK: zsh/ZLE smoke passed"
