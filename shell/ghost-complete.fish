# Ghost Complete -- Fish integration
# Source this in config.fish.

function _gc_prompt --on-event fish_prompt
    printf '\e]133;A\a'
    # OSC 7771: skip on Ghostty where OSC 133 already handles prompt detection.
    # Check GHOSTTY_RESOURCES_DIR too — TERM_PROGRAM is overwritten inside tmux.
    if test "$TERM_PROGRAM" != ghostty -a -z "$GHOSTTY_RESOURCES_DIR"
        printf '\e]7771;A\a'
    end
end

function _gc_preexec --on-event fish_preexec
    printf '\e]133;C\a'
    if test "$TERM_PROGRAM" != ghostty -a -z "$GHOSTTY_RESOURCES_DIR"
        printf '\e]7771;C\a'
    end
end

# Report buffer via OSC 7770
function _gc_report_buffer
    set -l buf (commandline)
    set -l cursor (commandline -C)
    printf '\e]7770;%d;%s\a' $cursor "$buf"
end

# Bind Ctrl+/ as manual trigger (0x1F)
bind \x1f '_gc_report_buffer'
