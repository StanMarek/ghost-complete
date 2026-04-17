#!/usr/bin/env bash
# Verifies shell/ghost-complete.bash is safe to source more than once:
#   - PROMPT_COMMAND keeps exactly one copy of each Ghost Complete hook
#   - DEBUG trap stays a single layer deep and chains a pre-existing trap
#   - The script no-ops cleanly when the `ghost-complete` binary isn't on PATH
#
# Run: bash tests/shell/test_bash_idempotency.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
INTEGRATION="$REPO_ROOT/shell/ghost-complete.bash"

if [[ ! -f "$INTEGRATION" ]]; then
    echo "FAIL: $INTEGRATION not found" >&2
    exit 1
fi

# Make sure the binary-existence guard sees something. We don't actually need
# to run the binary; a stub on PATH is enough for the rest of the script to
# install its hooks.
TMPDIR_FAKE="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_FAKE"' EXIT
cat >"$TMPDIR_FAKE/ghost-complete" <<'STUB'
#!/usr/bin/env bash
exit 0
STUB
chmod +x "$TMPDIR_FAKE/ghost-complete"

# --- Test A: PROMPT_COMMAND prepend is idempotent across re-sources ---
PATH_FOR_BASH="$TMPDIR_FAKE:$PATH"
out="$(PATH="$PATH_FOR_BASH" bash --noprofile --norc -c "
    source '$INTEGRATION'
    source '$INTEGRATION'
    source '$INTEGRATION'
    printf '%s' \"\$PROMPT_COMMAND\"
")"

# `_gc_prompt_command` and `_gc_reset_preexec` should each appear exactly once.
count_prompt=$(grep -o '_gc_prompt_command' <<<"$out" | wc -l | tr -d ' ')
count_reset=$(grep -o '_gc_reset_preexec' <<<"$out" | wc -l | tr -d ' ')
if [[ "$count_prompt" != "1" ]]; then
    echo "FAIL [prompt-cmd]: _gc_prompt_command appears $count_prompt times in PROMPT_COMMAND" >&2
    echo "  PROMPT_COMMAND=$out" >&2
    exit 1
fi
if [[ "$count_reset" != "1" ]]; then
    echo "FAIL [prompt-cmd]: _gc_reset_preexec appears $count_reset times in PROMPT_COMMAND" >&2
    echo "  PROMPT_COMMAND=$out" >&2
    exit 1
fi

# --- Test B: DEBUG trap stays a single layer deep across re-sources ---
trap_out="$(PATH="$PATH_FOR_BASH" bash --noprofile --norc -c "
    source '$INTEGRATION'
    source '$INTEGRATION'
    source '$INTEGRATION'
    trap -p DEBUG
")"
count_dbg=$(grep -o '_gc_debug_trap' <<<"$trap_out" | wc -l | tr -d ' ')
if [[ "$count_dbg" != "1" ]]; then
    echo "FAIL [debug-trap]: _gc_debug_trap appears $count_dbg times in DEBUG trap" >&2
    echo "  trap -p DEBUG=$trap_out" >&2
    exit 1
fi

# --- Test C: pre-existing DEBUG trap is preserved when -T is set ---
# bash only exposes the DEBUG trap to sourced files / command substitutions
# when functrace (`set -T`) is enabled in the outer shell. When -T is on we
# must capture and chain the user's trap; when -T is off we can't see it at
# all (a fundamental bash limitation, documented in the script).
chain_out="$(PATH="$PATH_FOR_BASH" bash --noprofile --norc -c "
    set -T
    _user_debug() { :; }
    trap '_user_debug' DEBUG
    source '$INTEGRATION'
    source '$INTEGRATION'
    trap -p DEBUG
")"
if ! grep -q '_gc_debug_trap' <<<"$chain_out"; then
    echo 'FAIL [debug-chain]: Ghost Complete DEBUG trap not installed' >&2
    echo "  trap -p DEBUG=$chain_out" >&2
    exit 1
fi
# The user's prior trap body must still be reachable via the captured cmd
# variable, so re-sourcing must not overwrite the captured value. We
# explicitly clear our own DEBUG trap before reading the value, because
# Ghost Complete's debug trap emits OSC 133;C bytes that would otherwise
# pollute stdout-based capture.
raw_out="$(PATH="$PATH_FOR_BASH" bash --noprofile --norc -c "
    set -T
    _user_debug() { :; }
    trap '_user_debug' DEBUG
    source '$INTEGRATION'
    source '$INTEGRATION'
    # Wrap the captured value in unambiguous markers so we can extract it
    # even though Ghost Complete's DEBUG trap leaks OSC 133;C bytes into
    # stdout on the way to our printf.
    printf '<<<%s>>>' \"\$_gc_existing_debug_trap_cmd\"
")"
# Extract content between <<< and >>> markers.
captured_out="${raw_out#*<<<}"
captured_out="${captured_out%>>>*}"
if [[ "$captured_out" != "_user_debug" ]]; then
    echo "FAIL [debug-chain]: captured user trap body was '$captured_out', expected '_user_debug'" >&2
    echo "  raw=$raw_out" >&2
    exit 1
fi

# --- Test D: missing binary makes the script a clean no-op ---
# Drop the stub from PATH and confirm the script returns without setting
# PROMPT_COMMAND or installing a DEBUG trap.
noop_out="$(PATH="/usr/bin:/bin" bash --noprofile --norc -c "
    PROMPT_COMMAND='_existing_user_cmd'
    source '$INTEGRATION'
    printf 'pc=%s|debug=%s' \"\$PROMPT_COMMAND\" \"\$(trap -p DEBUG)\"
")"
if [[ "$noop_out" != "pc=_existing_user_cmd|debug=" ]]; then
    echo "FAIL [no-binary]: script touched env when binary missing: $noop_out" >&2
    exit 1
fi

echo 'PASS: bash integration is idempotent and chains pre-existing DEBUG trap'
