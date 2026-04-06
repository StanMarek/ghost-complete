# Ghost Complete — terminal init for Bash
# Source this near the top of .bashrc to enable Ghost Complete PTY proxy.
# Detects the terminal emulator and exec's ghost-complete as a PTY proxy.

__ghost_complete_init() {
    # Only run in interactive mode
    [[ $- != *i* ]] && return

    if [[ -n "$TMUX" ]]; then
        # Inside tmux: guards prevent stacking proxies.
        # PPID check catches direct child shell.
        # GHOST_COMPLETE_PANE catches subshells.
        [[ "$(ps -o comm= -p "$PPID" 2>/dev/null)" == "ghost-complete" ]] && return
        [[ -n "$GHOST_COMPLETE_PANE" && "$GHOST_COMPLETE_PANE" == "$TMUX_PANE" ]] && return
        
        if [[ -n "$GHOSTTY_RESOURCES_DIR" ]] || \
           [[ -n "$KITTY_WINDOW_ID" ]] || \
           [[ -n "$WEZTERM_UNIX_SOCKET" ]] || \
           [[ -n "$ALACRITTY_SOCKET" ]] || \
           [[ -n "$ITERM_SESSION_ID" ]] || \
           [[ "$TERM_PROGRAM" == "rio" ]] || \
           [[ -n "$KONSOLE_VERSION" ]] || \
           [[ -n "$GNOME_TERMINAL_SCREEN" ]] || \
           [[ -n "$VTE_VERSION" ]] || \
           [[ -n "$TILIX_ID" ]] || \
           [[ -n "$FOOT_SERVER_SOCKET" ]] || \
           [[ -n "$TERMINATOR_UUID" ]]; then
            if command -v ghost-complete >/dev/null 2>&1; then
                export GHOST_COMPLETE_ACTIVE=1
                exec ghost-complete
            fi
        fi
    else
        # Outside tmux: GHOST_COMPLETE_ACTIVE is a reliable recursion guard
        [[ -n "$GHOST_COMPLETE_ACTIVE" ]] && return
        
        local supported=0
        if [[ -n "$KITTY_WINDOW_ID" ]] || \
           [[ -n "$ALACRITTY_SOCKET" ]] || \
           [[ -n "$GHOSTTY_RESOURCES_DIR" ]] || \
           [[ -n "$KONSOLE_VERSION" ]] || \
           [[ -n "$GNOME_TERMINAL_SCREEN" ]] || \
           [[ -n "$VTE_VERSION" ]] || \
           [[ -n "$TILIX_ID" ]] || \
           [[ -n "$FOOT_SERVER_SOCKET" ]] || \
           [[ -n "$TERMINATOR_UUID" ]]; then
            supported=1
        else
            case "$TERM_PROGRAM" in
                ghostty|WezTerm|rio|iTerm.app|Apple_Terminal|gnome-terminal|tilix|xterm|konsole|foot|terminator)
                    supported=1
                    ;;
            esac
        fi
        
        if [[ $supported -eq 1 ]] && command -v ghost-complete >/dev/null 2>&1; then
            export GHOST_COMPLETE_ACTIVE=1
            exec ghost-complete
        fi
    fi
}

__ghost_complete_init
unset -f __ghost_complete_init
