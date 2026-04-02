# Ghost Complete -- Fish integration
# Source this in config.fish.

# Percent-encode a path for file:// URIs (RFC 8089).
function _gc_urlencode_path
    set -l path $argv[1]
    set -lx LC_ALL C  # force byte-level iteration for correct UTF-8 encoding
    set -l encoded ""
    for i in (seq (string length -- $path))
        set -l ch (string sub -s $i -l 1 -- $path)
        if string match -qr '^[a-zA-Z0-9._~:@!\$&\'()*+,;=/-]$' -- $ch
            set encoded "$encoded$ch"
        else
            set encoded "$encoded"(printf '%%%02X' "'$ch")
        end
    end
    echo -n $encoded
end

function _gc_prompt --on-event fish_prompt
    printf '\e]133;A\a'
    # OSC 7771: skip on Ghostty where OSC 133 already handles prompt detection.
    # Check GHOSTTY_RESOURCES_DIR too — TERM_PROGRAM is overwritten inside tmux.
    if test "$TERM_PROGRAM" != ghostty -a -z "$GHOSTTY_RESOURCES_DIR"
        printf '\e]7771;A\a'
    end
    # Report current working directory via OSC 7 for filesystem completions
    printf '\e]7;file://%s%s\a' "$hostname" (_gc_urlencode_path "$PWD")
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
