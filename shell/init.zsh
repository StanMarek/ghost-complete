# Ghost Complete — terminal init (sourced near the top of .zshrc)
# Detects the terminal emulator and exec's ghost-complete as a PTY proxy.
__ghost_complete_init() {
  if [[ -n "$TMUX" ]]; then
    # Inside tmux: two guards prevent stacking proxies.
    #
    # 1) PPID check — catches the direct child shell. Works because
    #    `exec ghost-complete` replaces the shell process, so the spawned
    #    inner shell's PPID is the ghost-complete binary itself.
    # 2) GHOST_COMPLETE_PANE — catches subshells (zsh/bash typed at the
    #    prompt). spawn.rs sets GHOST_COMPLETE_PANE=$TMUX_PANE in the child
    #    env; subshells inherit it. A new tmux pane gets a fresh env without
    #    this variable, so it correctly launches a new proxy.
    #
    # We cannot use GHOST_COMPLETE_ACTIVE here because it is always present
    # in tmux — set by proxy.rs (tmux setenv) for future-pane propagation,
    # and inherited from the outer terminal shell that launched tmux.
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
    # because no multiplexer re-injects it into unrelated shell sessions.
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
        ghostty|WezTerm|rio|iTerm.app|Apple_Terminal|gnome-terminal|tilix|xterm|konsole|foot|terminator) supported=1 ;;
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
