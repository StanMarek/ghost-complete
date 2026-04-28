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

# Percent-encode a command-line buffer for OSC 7772 transport.
#
# STRICTER alphabet than _gc_urlencode_path: only [a-zA-Z0-9._~/-] and the
# literal space pass through. Everything else — including ';', '%', '\',
# all control bytes (0x00-0x1F, 0x7F) and high bytes (0x80-0xFF) — gets
# `%XX`-encoded. This is non-negotiable: any unencoded ';' would split
# the OSC parameter list and silently truncate the buffer at the parser;
# any unencoded BEL (0x07) or ESC+ST (0x1B 0x5C) would terminate the OSC
# envelope mid-payload; an unencoded ESC could smuggle a nested escape
# sequence into the parser's state machine. See ADR 0003.
#
# Do NOT widen the allow-list. If a future use case wants more characters
# unencoded, weigh it against the cost of re-introducing the original
# truncation/smuggling bug class and reject it anyway.
_gc_urlencode_buffer() {
    local input="$1" encoded="" i ch hex
    local LC_ALL=C  # byte-level iteration so multi-byte UTF-8 round-trips
    for (( i = 1; i <= ${#input}; i++ )); do
        ch="${input[$i]}"
        case "$ch" in
            ' '|[a-zA-Z0-9._~/-])
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

# True when the host terminal natively parses OSC 133 for its own prompt
# tracking (or emits its own proprietary markers on top, like VSCode's
# OSC 633). In those terminals our OSC 7771 fallback is redundant — the
# terminal already understands the OSC 133 we emit below, and OSC 7771
# only exists for terminals that mangle OSC 133. Currently covers
# Ghostty, Zed, and VSCode (the latter only once its shell integration
# is active, signalled by VSCODE_INJECTION being set).
_gc_native_osc133() {
    [[ "$TERM_PROGRAM" == "ghostty" || -n "$GHOSTTY_RESOURCES_DIR" ]] && return 0
    [[ -n "$ZED_TERM" ]] && return 0
    [[ -n "$VSCODE_INJECTION" ]] && return 0
    return 1
}

_gc_precmd() {
    # Mark: prompt is about to be displayed
    printf '\e]133;A\a'
    _gc_native_osc133 || printf '\e]7771;A\a'
}

_gc_preexec() {
    # Mark: command is about to execute
    printf '\e]133;C\a'
    _gc_native_osc133 || printf '\e]7771;C\a'
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

# Report current command buffer to the proxy via OSC 7772 (secure framing).
# Fires after every buffer modification (typing, deletion, cursor movement, paste).
#
# Do NOT inline `$BUFFER` raw into the OSC payload. See ADR 0003.
# Raw bytes corrupt the OSC envelope when `$BUFFER` contains `;`, `\a`,
# or `\e`: vte splits OSC params on `;`, BEL terminates the envelope, and
# embedded `\e]…\a` smuggles a nested OSC into the parser's state machine.
#
# The legacy OSC 7770 emission is gone. The parser still accepts 7770
# during the deprecation window but logs a one-shot warn! to nudge the
# user (or distro packager) towards a fresh shell integration.
_gc_report_buffer() {
    local encoded
    encoded="$(_gc_urlencode_buffer "$BUFFER")"
    # Abort if the encoder failed: emitting a partially-encoded payload
    # would defeat the framing guarantee the parser-side strict decoder
    # relies on. The parser logs and drops malformed frames anyway, but
    # don't even put one on the wire.
    [[ $? -eq 0 ]] || return 0
    printf '\e]7772;%d;%s\a' "$CURSOR" "$encoded"
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
