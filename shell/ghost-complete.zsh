# Ghost Complete — Zsh integration
# Source this file in your .zshrc for richer completion features.
#
# Provides prompt boundary markers so the proxy can detect prompt
# boundaries and track the current command buffer.
# OSC 133: native semantic prompts (Ghostty, iTerm2 partial)
# OSC 7771: terminal-agnostic prompt boundary for Ghost Complete
# OSC 7: current working directory reporting

# Percent-encode a path for use in file:// URIs (RFC 8089).
# Encodes everything except unreserved chars and '/'.
_gc_urlencode_path() {
    local input="$1" encoded="" i ch hex
    for (( i = 1; i <= ${#input}; i++ )); do
        ch="${input[i]}"
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

_gc_precmd() {
    # Mark: prompt is about to be displayed
    printf '\e]133;A\a'
    # OSC 7771: redundant on Ghostty (OSC 133 already handled), needed elsewhere.
    # Check GHOSTTY_RESOURCES_DIR too — TERM_PROGRAM is overwritten inside tmux.
    [[ "$TERM_PROGRAM" != "ghostty" && -z "$GHOSTTY_RESOURCES_DIR" ]] && printf '\e]7771;A\a'
}

_gc_preexec() {
    # Mark: command is about to execute
    printf '\e]133;C\a'
    [[ "$TERM_PROGRAM" != "ghostty" && -z "$GHOSTTY_RESOURCES_DIR" ]] && printf '\e]7771;C\a'
}

precmd_functions+=(_gc_precmd)
preexec_functions+=(_gc_preexec)

# Report current working directory via OSC 7 on directory change.
# This enables the proxy to track CWD and provide correct filesystem completions.
_gc_chpwd() {
    printf '\e]7;file://%s%s\a' "$HOST" "$(_gc_urlencode_path "$PWD")"
}

chpwd_functions+=(_gc_chpwd)
# Also emit on first prompt in case the shell started in a non-default directory
autoload -Uz add-zsh-hook
add-zsh-hook precmd _gc_osc7_precmd
_gc_osc7_precmd() {
    printf '\e]7;file://%s%s\a' "$HOST" "$(_gc_urlencode_path "$PWD")"
    # Remove self after first run — chpwd hook handles subsequent changes
    add-zsh-hook -d precmd _gc_osc7_precmd
}

# Report current command buffer to the proxy via custom OSC 7770.
# Fires after every buffer modification (typing, deletion, cursor movement, paste).
_gc_report_buffer() {
    printf '\e]7770;%d;%s\a' "$CURSOR" "$BUFFER"
}

autoload -Uz add-zle-hook-widget
add-zle-hook-widget line-pre-redraw _gc_report_buffer
