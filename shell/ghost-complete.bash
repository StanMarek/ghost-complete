# Ghost Complete -- Bash integration
# Source this in .bashrc. Requires Bash 4.4+ for bind -x.

# No-op cleanly when the ghost-complete binary isn't installed. This keeps the
# script safe to source unconditionally from .bashrc on machines where the
# binary hasn't been built/installed yet.
if ! command -v ghost-complete >/dev/null 2>&1; then
    return 0 2>/dev/null || exit 0
fi

# Percent-encode a path for use in file:// URIs (RFC 8089).
_gc_urlencode_path() {
    local input="$1" encoded="" i ch hex
    local LC_ALL=C  # force byte-level iteration for correct UTF-8 encoding
    for (( i = 0; i < ${#input}; i++ )); do
        ch="${input:$i:1}"
        case "$ch" in
            [a-zA-Z0-9._~:@!\$\&\'\(\)\*+,\;=/-])
                encoded+="$ch"
                ;;
            *)
                printf -v hex '%%%02X' "'$ch"
                encoded+="$hex"
                ;;
        esac
    done
    printf '%s' "$encoded"
}

# True when the host terminal already injects native OSC 133 (and any
# proprietary markers like VSCode's OSC 633), making our redundant
# OSC 7771 unnecessary. Currently covers Ghostty, Zed, and VSCode
# (when its shell integration is active via VSCODE_INJECTION=1).
_gc_native_osc133() {
    [[ "$TERM_PROGRAM" == "ghostty" || -n "$GHOSTTY_RESOURCES_DIR" ]] && return 0
    [[ -n "$ZED_TERM" ]] && return 0
    [[ -n "$VSCODE_INJECTION" ]] && return 0
    return 1
}

_gc_prompt_command() {
    printf '\e]133;A\a'
    _gc_native_osc133 || printf '\e]7771;A\a'
    # Report current working directory via OSC 7 for filesystem completions
    printf '\e]7;file://%s%s\a' "${HOSTNAME:-}" "$(_gc_urlencode_path "$PWD")"
}
# Idempotent prepend: avoid chaining `_gc_prompt_command;_gc_prompt_command;…`
# when the script is sourced more than once (e.g. from a re-sourced .bashrc).
[[ "$PROMPT_COMMAND" == *_gc_prompt_command* ]] || \
    PROMPT_COMMAND="_gc_prompt_command${PROMPT_COMMAND:+;$PROMPT_COMMAND}"

# Mark command execution
_gc_preexec_enabled=true
_gc_debug_trap() {
    if [[ "$_gc_preexec_enabled" == true ]]; then
        _gc_preexec_enabled=false
        printf '\e]133;C\a'
        _gc_native_osc133 || printf '\e]7771;C\a'
    fi
    # Re-invoke any pre-existing DEBUG trap captured at install time so we
    # don't silently clobber the user's own (or another tool's) trap.
    # Default to ':' so the chain stays a no-op when there was no prior trap.
    eval "${_gc_existing_debug_trap_cmd:-:}"
}
# Capture the user's existing DEBUG trap (if any) once and chain it into our
# handler instead of clobbering it. Caveat: bash does NOT propagate the
# DEBUG trap into command substitutions or sourced files unless the caller
# has `set -T` (functrace) enabled in the outer shell. When -T is off (the
# common case), `trap -p DEBUG` here returns empty even if the user had a
# trap installed — we can't see it, so chaining is impossible. In that case
# our own `trap '_gc_debug_trap' DEBUG` below replaces the user's trap, but
# this matches what the bash spec says happens to DEBUG traps inside
# sourced files anyway. Users who care about chaining should `set -T` in
# their .bashrc before sourcing this file.
if [[ -z "${_gc_existing_debug_trap_captured:-}" ]]; then
    _gc_existing_debug_trap_captured=1
    # `trap -p DEBUG` prints `trap -- '<cmd>' DEBUG` (or empty when unset
    # / not visible). Strip the fixed prefix/suffix to recover the raw cmd.
    # bash's `trap -p` quotes embedded single quotes as '\'' so the
    # recovered cmd is still safe to `eval`.
    _gc_existing_debug_trap_raw="$(trap -p DEBUG)"
    _gc_existing_debug_trap_raw="${_gc_existing_debug_trap_raw%$'\n'}"
    if [[ "$_gc_existing_debug_trap_raw" == "trap -- '"*"' DEBUG" ]]; then
        _gc_existing_debug_trap_cmd="${_gc_existing_debug_trap_raw#trap -- \'}"
        _gc_existing_debug_trap_cmd="${_gc_existing_debug_trap_cmd%\' DEBUG}"
    else
        _gc_existing_debug_trap_cmd=""
    fi
    unset _gc_existing_debug_trap_raw
fi
trap '_gc_debug_trap' DEBUG

# Re-enable preexec detection after each prompt
_gc_reset_preexec() {
    _gc_preexec_enabled=true
}
# Same idempotency guard as above so the reset hook is only chained once.
[[ "$PROMPT_COMMAND" == *_gc_reset_preexec* ]] || \
    PROMPT_COMMAND="_gc_reset_preexec;${PROMPT_COMMAND}"

# Report buffer to proxy via OSC 7770
_gc_report_buffer() {
    printf '\e]7770;%d;%s\a' "$READLINE_POINT" "$READLINE_LINE"
}

# Bind Ctrl+/ as manual trigger (0x1F)
bind -x '"\C-_": _gc_report_buffer'
