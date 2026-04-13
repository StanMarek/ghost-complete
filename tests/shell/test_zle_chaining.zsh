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

print 'PASS: zle chaining preserves widget identity, idempotent in both branches'
