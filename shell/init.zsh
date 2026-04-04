# Ghost Complete — terminal init (sourced near the top of .zshrc)
# Detects the terminal emulator and exec's ghost-complete as a PTY proxy.
__ghost_complete_init() {
  if [[ -n "$TMUX" ]]; then
    # Inside tmux: launch per-pane. Skip GHOST_COMPLETE_ACTIVE (leaks from
    # outer shell and would block the per-pane proxy we actually want).
    # PPID check prevents recursion: ghost-complete spawns the inner shell,
    # so the inner shell's PPID is ghost-complete.
    [[ "$(ps -o comm= -p $PPID 2>/dev/null)" == "ghost-complete" ]] && return
    if [[ -n "$GHOSTTY_RESOURCES_DIR" ]] || \
       [[ -n "$KITTY_WINDOW_ID" ]] || \
       [[ -n "$WEZTERM_UNIX_SOCKET" ]] || \
       [[ -n "$ALACRITTY_SOCKET" ]] || \
       [[ -n "$ITERM_SESSION_ID" ]] || \
       [[ "$TERM_PROGRAM" == "rio" ]]; then
      if command -v ghost-complete >/dev/null 2>&1; then
        export GHOST_COMPLETE_ACTIVE=1
        exec ghost-complete
      fi
    fi
  else
    # Outside tmux: env var guard is reliable (no multiplexer to strip it)
    [[ -n "$GHOST_COMPLETE_ACTIVE" ]] && return
    local supported=0
    if [[ -n "$KITTY_WINDOW_ID" ]] || [[ -n "$ALACRITTY_SOCKET" ]]; then
      supported=1
    else
      case "$TERM_PROGRAM" in
        ghostty|WezTerm|rio|iTerm.app|Apple_Terminal) supported=1 ;;
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
