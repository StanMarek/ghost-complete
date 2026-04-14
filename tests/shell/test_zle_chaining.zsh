#!/usr/bin/env zsh
# Verifies the manual zle-line-pre-redraw chaining in shell/ghost-complete.zsh
# preserves $WIDGET when the wrapper executes — the property z4h relies on
# in its _zsh_highlight() guard — and that the install function is
# idempotent across re-sources.
#
# Run: zsh tests/shell/test_zle_chaining.zsh
#
# Uses `zsh --no-rcs` for the assertion subshell to isolate the test from
# any user rc files that may already register widgets via
# add-zle-hook-widget (which would mask the no-prior-widget branch).

set -euo pipefail

SCRIPT_DIR="${0:A:h}"
REPO_ROOT="${SCRIPT_DIR:h:h}"
INTEGRATION="$REPO_ROOT/shell/ghost-complete.zsh"

if [[ ! -f "$INTEGRATION" ]]; then
    print -u2 "FAIL: $INTEGRATION not found"
    exit 1
fi

# --- Test A: chaining branch (a prior widget exists) ---
zsh --no-rcs -c "
    set -e
    zmodload zsh/zle
    _baseline_pre_redraw() { :; }
    zle -N zle-line-pre-redraw _baseline_pre_redraw

    source '$INTEGRATION'

    # Wrapper is registered under the canonical name.
    if [[ \"\${widgets[zle-line-pre-redraw]}\" != *_gc_zle_line_pre_redraw* ]]; then
        print -u2 'FAIL [chain]: zle-line-pre-redraw not bound to _gc_zle_line_pre_redraw'
        print -u2 \"  actual: \${widgets[zle-line-pre-redraw]}\"
        exit 1
    fi

    # Original baseline widget is preserved as _gc_orig.
    if [[ \"\${widgets[_gc_orig_zle_line_pre_redraw]}\" != *_baseline_pre_redraw* ]]; then
        print -u2 'FAIL [chain]: original widget not preserved as _gc_orig_zle_line_pre_redraw'
        print -u2 \"  actual: \${widgets[_gc_orig_zle_line_pre_redraw]:-<unset>}\"
        exit 1
    fi

    # Re-sourcing is idempotent: chain stays a single layer deep.
    source '$INTEGRATION'
    if [[ \"\${widgets[zle-line-pre-redraw]}\" != *_gc_zle_line_pre_redraw* ]]; then
        print -u2 'FAIL [chain]: re-source mutated the registration'
        print -u2 \"  actual: \${widgets[zle-line-pre-redraw]}\"
        exit 1
    fi
"

# --- Test B: direct-install branch (no prior widget) ---
zsh --no-rcs -c "
    set -e
    zmodload zsh/zle

    source '$INTEGRATION'

    # No prior widget existed → _gc_report_buffer registered directly.
    if [[ \"\${widgets[zle-line-pre-redraw]}\" != *_gc_report_buffer* ]]; then
        print -u2 'FAIL [direct]: zle-line-pre-redraw not bound to _gc_report_buffer'
        print -u2 \"  actual: \${widgets[zle-line-pre-redraw]}\"
        exit 1
    fi

    # _gc_orig should NOT be created when there was no prior widget.
    if (( \${+widgets[_gc_orig_zle_line_pre_redraw]} )); then
        print -u2 'FAIL [direct]: _gc_orig_zle_line_pre_redraw was created unnecessarily'
        print -u2 \"  actual: \${widgets[_gc_orig_zle_line_pre_redraw]}\"
        exit 1
    fi

    # Re-sourcing is idempotent for the direct-install branch too.
    source '$INTEGRATION'
    if [[ \"\${widgets[zle-line-pre-redraw]}\" != *_gc_report_buffer* ]]; then
        print -u2 'FAIL [direct]: re-source mutated the registration'
        print -u2 \"  actual: \${widgets[zle-line-pre-redraw]}\"
        exit 1
    fi
    if (( \${+widgets[_gc_orig_zle_line_pre_redraw]} )); then
        print -u2 'FAIL [direct]: re-source created _gc_orig spuriously'
        exit 1
    fi
"

# --- Test C: functional $WIDGET preservation (regression guard for core fix) ---
# The whole point of manual chaining (vs add-zle-hook-widget) is that when
# the line editor fires zle-line-pre-redraw, `$WIDGET` inside the chained
# baseline widget must still read `zle-line-pre-redraw` — not
# `azhw:zle-line-pre-redraw` and not `_gc_orig_zle_line_pre_redraw`. Drive
# a real zle session via expect so the line editor itself fires the hook.
if ! (( ${+commands[expect]} )); then
    print -u2 'SKIP [functional]: expect(1) not installed; cannot drive zle'
else
    DRIVER="$(mktemp -t gc_zle_driver.XXXXXX)"
    DRIVER_EXP="$(mktemp -t gc_zle_driver_exp.XXXXXX)"
    WIDGET_OUT="$(mktemp -t gc_zle_widget.XXXXXX)"
    trap "rm -f '$DRIVER' '$DRIVER_EXP' '$WIDGET_OUT'" EXIT

    cat >"$DRIVER" <<ZEOF
#!/usr/bin/env zsh
set -e
zmodload zsh/zle
typeset -g CAPTURED_WIDGET="<unset>"
_baseline_pre_redraw() { CAPTURED_WIDGET="\$WIDGET" }
zle -N zle-line-pre-redraw _baseline_pre_redraw
source '$INTEGRATION'
_exit_line() { zle .accept-line }
zle -N _exit_line
bindkey ' ' _exit_line
typeset -g FOO=""
vared -p "" FOO
print -- "\$CAPTURED_WIDGET" > '$WIDGET_OUT'
ZEOF

    cat >"$DRIVER_EXP" <<'EEOF'
#!/usr/bin/expect -f
log_user 0
set timeout 5
spawn zsh --no-rcs [lindex $argv 0]
sleep 0.3
send " "
expect eof
EEOF
    chmod +x "$DRIVER_EXP"

    if ! "$DRIVER_EXP" "$DRIVER" >/dev/null 2>&1; then
        print -u2 'FAIL [functional]: expect driver exited non-zero'
        exit 1
    fi

    captured="$(<"$WIDGET_OUT")"
    if [[ "$captured" != "zle-line-pre-redraw" ]]; then
        print -u2 "FAIL [functional]: \$WIDGET inside baseline was '$captured', expected 'zle-line-pre-redraw'"
        exit 1
    fi
fi

# --- Test D: non-user widget is preserved (not silently clobbered) ---
# When something has already registered a non-user widget at
# zle-line-pre-redraw (completion:/builtin:/etc.), Ghost Complete must not
# overwrite that registration. It's safer to lose our buffer hook than to
# clobber a hook the user or their framework deliberately installed.
zsh --no-rcs -c "
    set -e
    zmodload zsh/zle
    # zle -C produces a 'completion:<builtin>:<fn>' registration.
    _baseline_complete() { :; }
    zle -C zle-line-pre-redraw list-choices _baseline_complete

    orig=\"\${widgets[zle-line-pre-redraw]}\"
    if [[ \"\$orig\" != completion:* ]]; then
        print -u2 \"FAIL [preserve]: baseline non-user widget missing; got '\$orig'\"
        exit 1
    fi

    source '$INTEGRATION'

    # The original non-user registration must be intact, byte-for-byte.
    if [[ \"\${widgets[zle-line-pre-redraw]}\" != \"\$orig\" ]]; then
        print -u2 'FAIL [preserve]: non-user widget was clobbered'
        print -u2 \"  expected: \$orig\"
        print -u2 \"  actual: \${widgets[zle-line-pre-redraw]}\"
        exit 1
    fi

    # Neither the wrapper nor the direct-install fallback may be bound at
    # the canonical name — that would mean we clobbered the baseline.
    if [[ \"\${widgets[zle-line-pre-redraw]}\" == *_gc_zle_line_pre_redraw* ]]; then
        print -u2 'FAIL [preserve]: _gc_zle_line_pre_redraw was bound at zle-line-pre-redraw'
        exit 1
    fi
    if [[ \"\${widgets[zle-line-pre-redraw]}\" == *_gc_report_buffer* ]]; then
        print -u2 'FAIL [preserve]: _gc_report_buffer was bound at zle-line-pre-redraw'
        exit 1
    fi

    # Re-sourcing must remain a no-op for the non-user case.
    source '$INTEGRATION'
    if [[ \"\${widgets[zle-line-pre-redraw]}\" != \"\$orig\" ]]; then
        print -u2 'FAIL [preserve]: re-source mutated the non-user registration'
        print -u2 \"  actual: \${widgets[zle-line-pre-redraw]}\"
        exit 1
    fi
"

print 'PASS: zle chaining preserves widget identity, idempotent, and non-user widgets survive'
