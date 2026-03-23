# Ghost Complete — Zsh integration
# Source this file in your .zshrc for richer completion features.
#
# Provides prompt boundary markers so the proxy can detect prompt
# boundaries and track the current command buffer.
# OSC 133: native semantic prompts (Ghostty, iTerm2 partial)
# OSC 7771: terminal-agnostic prompt boundary for Ghost Complete

_gc_precmd() {
    # Mark: prompt is about to be displayed
    printf '\e]133;A\a'
    printf '\e]7771;A\a'
}

_gc_preexec() {
    # Mark: command is about to execute
    printf '\e]133;C\a'
    printf '\e]7771;C\a'
}

precmd_functions+=(_gc_precmd)
preexec_functions+=(_gc_preexec)

# Report current command buffer to the proxy via custom OSC 7770.
# Fires after every buffer modification (typing, deletion, cursor movement, paste).
_gc_report_buffer() {
    printf '\e]7770;%d;%s\a' "$CURSOR" "$BUFFER"
}

autoload -Uz add-zle-hook-widget
add-zle-hook-widget line-pre-redraw _gc_report_buffer
