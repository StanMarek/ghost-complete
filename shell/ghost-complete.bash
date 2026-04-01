# Ghost Complete -- Bash integration
# Source this in .bashrc. Requires Bash 4.4+ for bind -x.

_gc_prompt_command() {
    printf '\e]133;A\a'
    # Check GHOSTTY_RESOURCES_DIR too — TERM_PROGRAM is overwritten inside tmux
    [[ "$TERM_PROGRAM" != "ghostty" && -z "$GHOSTTY_RESOURCES_DIR" ]] && printf '\e]7771;A\a'
    # Report current working directory via OSC 7 for filesystem completions
    printf '\e]7;file://%s%s\a' "$HOSTNAME" "$PWD"
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
