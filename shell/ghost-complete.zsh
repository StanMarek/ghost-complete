# Ghost Complete — Zsh integration
# Source this file in your .zshrc for richer completion features.
#
# Provides OSC 133 semantic prompt markers so the proxy can detect
# prompt boundaries and track the current command buffer.

_gc_precmd() {
    # Mark: prompt is about to be displayed
    printf '\e]133;A\a'
}

_gc_preexec() {
    # Mark: command is about to execute
    printf '\e]133;C\a'
}

precmd_functions+=(_gc_precmd)
preexec_functions+=(_gc_preexec)
