# Ghost Complete — terminal init (sourced near the top of .zshrc)
# Detects the terminal emulator and exec's ghost-complete as a PTY proxy.

# True if a `ps -o comm` value, which on macOS is argv0, names the proxy: by
# name or by path, with or without the leading dash of a login-style launch
# (a terminal that starts its command the way login(1) starts a shell).
_gc_is_proxy_comm() {
  local name=${1##*/}
  [[ "${name#-}" == "ghost-complete" ]]
}

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
    _gc_is_proxy_comm "$comm" && return 0
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

# Returns 0 if PID $1 is this shell or one of its ancestors, 1 if it is
# neither, 2 if the walk could not complete. Callers should treat 2 like 0.
_gc_is_self_or_ancestor() {
  local target=$1 pid=$PPID
  local -i depth=0
  [[ "$target" == "$$" ]] && return 0
  while [[ "$pid" != "1" && "$pid" != "0" && -n "$pid" ]]; do
    [[ "$pid" == "$target" ]] && return 0
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

# A terminal that injects its zsh integration through ZDOTDIR (Ghostty,
# kitty) loses it in the shell behind the proxy: its .zshenv already ran here
# and put the user's ZDOTDIR back, and the hook it left for the first prompt
# never runs, because we exec before that prompt. Point ZDOTDIR at the
# integration again so the shell the proxy starts loads it too. A no-op in
# shells the terminal didn't inject, such as tmux panes.
_gc_rearm_terminal_integration() {
  emulate -L zsh
  local hook var dir
  # Each terminal's first-prompt hook, and the variable its .zshenv restores
  # the user's ZDOTDIR from.
  for hook var in \
      _ghostty_deferred_init GHOSTTY_ZSH_ZDOTDIR \
      _ksi_deferred_init KITTY_ORIG_ZDOTDIR; do
    # Still pending: the terminal injected this shell, no prompt has run yet.
    (( ${precmd_functions[(Ie)$hook]:-0} )) || continue
    dir=${functions_source[$hook]:A:h}
    # That .zshenv is what puts the user's ZDOTDIR back. Without it zsh would
    # look for the user's startup files in the integration directory.
    [[ -n $dir && -r $dir/.zshenv ]] || return
    # Hand over only the ZDOTDIR the new shell would inherit anyway. One set
    # without export (typically by ~/.zshenv) is not inherited: the new shell
    # must find ~/.zshenv again to set it.
    if [[ ${parameters[ZDOTDIR]-} == *export* ]]; then
      export $var="$ZDOTDIR"
    else
      unset $var
    fi
    export ZDOTDIR=$dir
    return
  done
}

# Replace this shell with the proxy.
_gc_exec_proxy() {
  export GHOST_COMPLETE_ACTIVE=1
  _gc_rearm_terminal_integration
  exec ghost-complete
}

# This file's directory, as .zshrc names it: install puts the hooks script
# next to it. Not resolved through symlinks, which may point elsewhere.
typeset -g _GC_SHELL_DIR=${${(%):-%x}:a:h}

# Load the hooks that report prompts, the working directory and the command
# line to the proxy. Runs as a precmd hook, so the hooks load at the first
# prompt, after the rest of .zshrc, where a second .zshrc block used to
# load them.
_gc_load_hooks() {
  emulate -L zsh
  add-zsh-hook -d precmd _gc_load_hooks
  # Already loaded, by a .zshrc that still sources ghost-complete.zsh or
  # when .zshrc is sourced again. Loading them again would wrap whatever
  # wrapped our zle widget since.
  (( ${+functions[_gc_precmd]} )) && return
  local script=$_GC_SHELL_DIR/ghost-complete.zsh
  if [[ ! -r $script ]]; then
    print -ru2 -- "ghost-complete: hooks script missing: $script"
    print -ru2 -- "ghost-complete: run 'ghost-complete install' to restore it"
    return
  fi
  local -a before=($precmd_functions)
  builtin source $script
  # zsh runs a prompt's precmd hooks from the list as it was before the
  # first one ran, so the ones just added would first run at the next
  # prompt. Run them for this one.
  local hook
  for hook in ${precmd_functions:|before}; do
    $hook
  done
}

# This shell runs behind the proxy: load the hooks at its first prompt.
_gc_load_hooks_at_first_prompt() {
  autoload -Uz add-zsh-hook
  add-zsh-hook precmd _gc_load_hooks
}

__ghost_complete_init() {
  # A proxy that fails to start execs the shell in its place, marked with
  # its pid. That shell (exec keeps the pid), and any zsh it starts on the
  # way to .zshrc (a $SHELL wrapper script, a .bashrc that runs zsh), must
  # not start the proxy again: it would fail the same way and fall back
  # again, forever. A marker from outside our ancestry was inherited from
  # elsewhere (a tmux server, an editor) and means nothing here.
  if [[ -n "$GHOST_COMPLETE_FALLBACK_PID" ]]; then
    local marker=$GHOST_COMPLETE_FALLBACK_PID
    unset GHOST_COMPLETE_FALLBACK_PID
    _gc_is_self_or_ancestor "$marker"
    case $? in
      1) ;;
      *) return ;;
    esac
  fi
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
    if _gc_is_proxy_comm "$(ps -o comm= -p "$PPID" 2>/dev/null)" || \
       [[ -n "$GHOST_COMPLETE_PANE" && "$GHOST_COMPLETE_PANE" == "$TMUX_PANE" ]]; then
      _gc_load_hooks_at_first_prompt
      return
    fi
    if [[ -n "$GHOSTTY_RESOURCES_DIR" ]] || \
       [[ -n "$KITTY_WINDOW_ID" ]] || \
       [[ -n "$WEZTERM_UNIX_SOCKET" ]] || \
       [[ -n "$ALACRITTY_SOCKET" ]] || \
       [[ -n "$ZED_TERM" ]] || \
       [[ -n "$VSCODE_IPC_HOOK_CLI" ]] || \
       [[ -n "$ITERM_SESSION_ID" ]] || \
       [[ "$TERM_PROGRAM" == "rio" ]] || \
       [[ "$TERM_PROGRAM" == "otty" ]]; then
      if command -v ghost-complete >/dev/null 2>&1; then
        _gc_exec_proxy
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
        0) _gc_load_hooks_at_first_prompt; return ;;
        1) unset GHOST_COMPLETE_ACTIVE ;;
        *) _gc_load_hooks_at_first_prompt; return ;;
      esac
    fi
    local supported=0
    if [[ -n "$KITTY_WINDOW_ID" ]] \
      || [[ -n "$WEZTERM_UNIX_SOCKET" ]] \
      || [[ -n "$ALACRITTY_SOCKET" ]] \
      || [[ -n "$ZED_TERM" ]] \
      || [[ -n "$VSCODE_IPC_HOOK_CLI" ]]; then
      supported=1
    else
      case "$TERM_PROGRAM" in
        ghostty|otty|WezTerm|rio|iTerm.app|Apple_Terminal|zed|vscode) supported=1 ;;
      esac
    fi
    if [[ $supported -eq 1 ]] && command -v ghost-complete >/dev/null 2>&1; then
      _gc_exec_proxy
    fi
  fi
}
__ghost_complete_init
unset -f __ghost_complete_init
