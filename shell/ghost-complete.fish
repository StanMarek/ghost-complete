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

# True when the host terminal already injects native OSC 133, making our
# redundant OSC 7771 unnecessary. Currently covers Ghostty (native) and
# Zed (native). VSCode arrives in a later commit.
function _gc_native_osc133
    if test "$TERM_PROGRAM" = ghostty -o -n "$GHOSTTY_RESOURCES_DIR"
        return 0
    end
    if test -n "$ZED_TERM"
        return 0
    end
    return 1
end

function _gc_prompt --on-event fish_prompt
    printf '\e]133;A\a'
    if not _gc_native_osc133
        printf '\e]7771;A\a'
    end
    # Report current working directory via OSC 7 for filesystem completions
    printf '\e]7;file://%s%s\a' "$hostname" (_gc_urlencode_path "$PWD")
end

function _gc_preexec --on-event fish_preexec
    printf '\e]133;C\a'
    if not _gc_native_osc133
        printf '\e]7771;C\a'
    end
end

# Report buffer via OSC 7770
function _gc_report_buffer
    set -l buf (commandline)
    set -l cursor (commandline -C)
    printf '\e]7770;%d;%s\a' $cursor "$buf"
end

# Bind Ctrl+/ as manual trigger (0x1F).
# Guard with a sentinel so re-sourcing the script (e.g. on config reload)
# doesn't stack duplicate bindings — fish's `bind` happily appends the same
# binding multiple times.
if not set -q __gc_bindings_installed
    set -g __gc_bindings_installed 1
    bind \x1f '_gc_report_buffer'
end
