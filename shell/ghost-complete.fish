# Ghost Complete -- Fish integration
# Source this in config.fish.

function _gc_prompt --on-event fish_prompt
    printf '\e]133;A\a'
    printf '\e]7771;A\a'
end

function _gc_preexec --on-event fish_preexec
    printf '\e]133;C\a'
    printf '\e]7771;C\a'
end

# Report buffer via OSC 7770
function _gc_report_buffer
    set -l buf (commandline)
    set -l cursor (commandline -C)
    printf '\e]7770;%d;%s\a' $cursor "$buf"
end

# Bind Ctrl+/ as manual trigger (0x1F)
bind \x1f '_gc_report_buffer'
