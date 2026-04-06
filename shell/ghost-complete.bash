# Ghost Complete -- Bash integration
# Source this in .bashrc. Requires Bash 4.4+ for bind -x.

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

_gc_prompt_command() {
    printf '\e]133;A\a'
    # Check GHOSTTY_RESOURCES_DIR too — TERM_PROGRAM is overwritten inside tmux
    [[ "$TERM_PROGRAM" != "ghostty" && -z "$GHOSTTY_RESOURCES_DIR" ]] && printf '\e]7771;A\a'
    # Report current working directory via OSC 7 for filesystem completions
    printf '\e]7;file://%s%s\a' "${HOSTNAME:-}" "$(_gc_urlencode_path "$PWD")"
}
PROMPT_COMMAND="_gc_prompt_command${PROMPT_COMMAND:+;$PROMPT_COMMAND}"

# Mark command execution
_gc_preexec_enabled=true
_gc_debug_trap() {
    if [[ "$_gc_preexec_enabled" == true ]]; then
        _gc_preexec_enabled=false
        printf '\e]133;C\a'
        [[ "$TERM_PROGRAM" != "ghostty" && -z "$GHOSTTY_RESOURCES_DIR" ]] && printf '\e]7771;C\a'
    fi
}
trap '_gc_debug_trap' DEBUG

# Re-enable preexec detection after each prompt
_gc_reset_preexec() {
    _gc_preexec_enabled=true
}
PROMPT_COMMAND="_gc_reset_preexec;${PROMPT_COMMAND}"

# Report buffer to proxy via OSC 7770
_gc_report_buffer() {
    printf '\e]7770;%d;%s\a' "$READLINE_POINT" "$READLINE_LINE"
}

# Bind Ctrl+/ as manual trigger (0x1F)
bind -x '"\C-_": _gc_report_buffer'

# ============================================================================
# AUTO-TRIGGER SUPPORT
# ============================================================================
# Auto-trigger completions on certain characters (space, /, -, .)
# This works by binding keys to self-insert + report buffer.
# ============================================================================

# Check if auto-trigger is enabled (can be disabled by setting GC_NO_AUTO_TRIGGER=1)
if [[ -z "$GC_NO_AUTO_TRIGGER" ]]; then
    
    # Helper: insert character and report buffer
    _gc_self_insert_and_report() {
        local char="$1"
        # Insert the character at cursor position
        if [[ $READLINE_POINT -eq ${#READLINE_LINE} ]]; then
            READLINE_LINE="${READLINE_LINE}${char}"
        else
            READLINE_LINE="${READLINE_LINE:0:$READLINE_POINT}${char}${READLINE_LINE:$READLINE_POINT}"
        fi
        ((READLINE_POINT++))
        # Report buffer to proxy
        printf '\e]7770;%d;%s\a' "$READLINE_POINT" "$READLINE_LINE"
    }

    # Auto-trigger on space
    _gc_space() { _gc_self_insert_and_report ' '; }
    bind -x '" ": _gc_space'

    # Auto-trigger on forward slash (path completion)
    _gc_slash() { _gc_self_insert_and_report '/'; }
    bind -x '"/": _gc_slash'

    # Auto-trigger on dash (option completion)
    _gc_dash() { _gc_self_insert_and_report '-'; }
    bind -x '"-": _gc_dash'

    # Auto-trigger on dot (file extension, hidden files)
    _gc_dot() { _gc_self_insert_and_report '.'; }
    bind -x '".": _gc_dot'

    # Auto-trigger on equals (--option=value completion)
    _gc_equals() { _gc_self_insert_and_report '='; }
    bind -x '"=": _gc_equals'

    # Auto-trigger on colon (for paths like user@host:path)
    _gc_colon() { _gc_self_insert_and_report ':'; }
    bind -x '":": _gc_colon'

    # Also report buffer on backspace/delete for live updates
    _gc_backward_delete() {
        if [[ $READLINE_POINT -gt 0 ]]; then
            READLINE_LINE="${READLINE_LINE:0:$((READLINE_POINT-1))}${READLINE_LINE:$READLINE_POINT}"
            ((READLINE_POINT--))
        fi
        printf '\e]7770;%d;%s\a' "$READLINE_POINT" "$READLINE_LINE"
    }
    bind -x '"\C-h": _gc_backward_delete'
    bind -x '"\C-?": _gc_backward_delete'

    # Report on cursor movement for context updates
    _gc_forward_char() {
        if [[ $READLINE_POINT -lt ${#READLINE_LINE} ]]; then
            ((READLINE_POINT++))
        fi
        printf '\e]7770;%d;%s\a' "$READLINE_POINT" "$READLINE_LINE"
    }
    bind -x '"\C-f": _gc_forward_char'
    bind -x '"\e[C": _gc_forward_char'

    _gc_backward_char() {
        if [[ $READLINE_POINT -gt 0 ]]; then
            ((READLINE_POINT--))
        fi
        printf '\e]7770;%d;%s\a' "$READLINE_POINT" "$READLINE_LINE"
    }
    bind -x '"\C-b": _gc_backward_char'
    bind -x '"\e[D": _gc_backward_char'

fi
