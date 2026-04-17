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
    local LC_ALL=C  # force byte-level iteration for correct UTF-8 encoding
    for (( i = 1; i <= ${#input}; i++ )); do
        ch="${input[$i]}"
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

# Use add-zsh-hook (which dedups) rather than `precmd_functions+=(…)` /
# `chpwd_functions+=(…)` which append unconditionally and grow the hook
# list every time the file is re-sourced.
autoload -Uz add-zsh-hook
add-zsh-hook precmd _gc_precmd
add-zsh-hook preexec _gc_preexec

# Report current working directory via OSC 7 on directory change.
# This enables the proxy to track CWD and provide correct filesystem completions.
_gc_chpwd() {
    printf '\e]7;file://%s%s\a' "$HOST" "$(_gc_urlencode_path "$PWD")"
}

add-zsh-hook chpwd _gc_chpwd
# Also emit on first prompt in case the shell started in a non-default directory
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

# Chain into the existing zle-line-pre-redraw widget without using
# add-zle-hook-widget: its dispatcher renames $WIDGET to
# `azhw:zle-line-pre-redraw`, breaking frameworks (z4h, p10k) that key
# behavior off $WIDGET inside the hook (e.g., z4h's _zsh_highlight()
# guard for the post-Enter prompt render).
_gc_install_zle_hook() {
    # Idempotent: skip if our wrapper or the direct-install fallback is
    # already in place. The two exact strings correspond to the two
    # install branches below — anything else falls through to re-evaluate.
    local current="${widgets[zle-line-pre-redraw]:-}"
    if [[ "$current" == "user:_gc_zle_line_pre_redraw" || "$current" == "user:_gc_report_buffer" ]]; then
        return
    fi
    # Three-way dispatch:
    #   1. No widget registered         → direct-install _gc_report_buffer.
    #   2. Existing plain user widget   → chain over it so $WIDGET is preserved.
    #   3. Existing non-user widget
    #      (completion:/builtin:/…)     → leave it alone. We can't chain a
    #      non-user widget safely, and silently clobbering someone else's
    #      registration is worse than losing our buffer hook.
    if (( ! ${+widgets[zle-line-pre-redraw]} )); then
        zle -N zle-line-pre-redraw _gc_report_buffer
    elif [[ "${widgets[zle-line-pre-redraw]}" == user:* ]]; then
        local existing="${widgets[zle-line-pre-redraw]}"
        zle -N _gc_orig_zle_line_pre_redraw "${existing#user:}"
        _gc_zle_line_pre_redraw() {
            _gc_report_buffer
            zle _gc_orig_zle_line_pre_redraw -- "$@"
        }
        zle -N zle-line-pre-redraw _gc_zle_line_pre_redraw
    fi
    # Non-user widget: intentionally no-op. Ghost Complete loses its buffer
    # hook in this case; that's acceptable degradation.
}

_gc_install_zle_hook
unset -f _gc_install_zle_hook
