# Ghost Complete — terminal init (sourced near the top of .zshrc)
# Detects the terminal emulator and exec's ghost-complete as a PTY proxy.

# Walk PPID ancestry looking for the ghost-complete binary. Returns 0 if
# found, 1 if confirmed absent (walk reached init/root), 2 if the walk could
# not complete (ps failure, disappeared PID, pathological depth). Callers
# should treat 2 as "uncertain" and take the safe path (honor the guard).
_gc_ancestor_is_proxy() {
  local pid=$PPID comm
  local -i depth=0
  while [[ "$pid" != "1" && "$pid" != "0" && -n "$pid" ]]; do
    if ! comm=$(ps -o comm= -p "$pid" 2>/dev/null); then
      return 2
    fi
    [[ -z "$comm" ]] && return 2
    [[ "${comm##*/}" == "ghost-complete" ]] && return 0
    if ! pid=$(ps -o ppid= -p "$pid" 2>/dev/null); then
      return 2
    fi
    pid="${pid// /}"
    [[ -z "$pid" ]] && return 2
    (( depth++ ))
    (( depth > 32 )) && return 2
  done
  return 1
}

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
       [[ -n "$ZED_TERM" ]] || \
       [[ -n "$VSCODE_IPC_HOOK_CLI" ]] || \
       [[ -n "$ITERM_SESSION_ID" ]] || \
       [[ "$TERM_PROGRAM" == "rio" ]]; then
      if command -v ghost-complete >/dev/null 2>&1; then
        export GHOST_COMPLETE_ACTIVE=1
        exec ghost-complete
      fi
    fi
  else
    # Outside tmux: GHOST_COMPLETE_ACTIVE is normally a reliable recursion
    # guard, BUT editors like VSCode/Zed propagate env vars from a launching
    # shell into their integrated terminal. If a user runs `code .` from a
    # ghost-complete-managed shell, GHOST_COMPLETE_ACTIVE=1 leaks into
    # VSCode's integrated zsh and would incorrectly disable the proxy there.
    # Fix: if our parent-process ancestry does not include a ghost-complete
    # process, the variable is a leak from a sibling terminal — drop it. We walk the
    # full PPID ancestry (not just $PPID) so subshells like `zsh`/`bash`
    # typed at the prompt still hit the guard via their grandparent
    # ghost-complete process. If the walk is inconclusive (ps failure),
    # default to honoring the guard — preventing recursive proxy stacking
    # is more important than recovering from a leaked env var.
    if [[ -n "$GHOST_COMPLETE_ACTIVE" ]]; then
      _gc_ancestor_is_proxy
      case $? in
        0) return ;;
        1) unset GHOST_COMPLETE_ACTIVE ;;
        *) return ;;
      esac
    fi
    local supported=0
    if [[ -n "$KITTY_WINDOW_ID" ]] \
      || [[ -n "$ALACRITTY_SOCKET" ]] \
      || [[ -n "$ZED_TERM" ]] \
      || [[ -n "$VSCODE_IPC_HOOK_CLI" ]]; then
      supported=1
    else
      case "$TERM_PROGRAM" in
        ghostty|WezTerm|rio|iTerm.app|Apple_Terminal|zed|vscode) supported=1 ;;
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
