# Ghost Complete — Fish integration
# Source this file in your config.fish for richer completion features.

function _gc_prompt --on-event fish_prompt
    printf '\e]133;A\a'
end

function _gc_preexec --on-event fish_preexec
    printf '\e]133;C\a'
end
