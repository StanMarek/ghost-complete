# Ghost Complete — Bash integration
# Source this file in your .bashrc for richer completion features.

_gc_prompt_command() {
    printf '\e]133;A\a'
}

PROMPT_COMMAND="_gc_prompt_command${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
