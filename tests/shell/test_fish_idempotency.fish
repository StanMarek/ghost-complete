#!/usr/bin/env fish
# Verifies shell/ghost-complete.fish is safe to source more than once:
#   - The Ctrl-/ key binding stays a single entry, not stacked
#   - The on-event functions stay single-registered (fish itself dedups
#     these by function name, so we just confirm the count)
#
# Run: fish tests/shell/test_fish_idempotency.fish

set -l script_dir (dirname (status --current-filename))
set -l repo_root (cd $script_dir/../.. ; and pwd)
set -l integration "$repo_root/shell/ghost-complete.fish"

if not test -f "$integration"
    echo "FAIL: $integration not found" >&2
    exit 1
end

# --- Test A: Ctrl-/ binding count stays at 1 across re-sources ---
set -l count (fish -c "
    source '$integration'
    source '$integration'
    source '$integration'
    bind | string match -r '_gc_report_buffer' | count
")

if test "$count" != "1"
    echo "FAIL [bind]: _gc_report_buffer binding appears $count times, expected 1" >&2
    fish -c "source '$integration'; source '$integration'; bind" >&2
    exit 1
end

# --- Test B: event handlers are still installed after multiple sources ---
# fish identifies handlers by `function --on-event` name, so re-sourcing is
# safe by construction; this guards against future regressions where someone
# might accidentally rename or duplicate handlers.
set -l prompt_handlers (fish -c "
    source '$integration'
    source '$integration'
    functions --handlers-type event | string match -r '_gc_prompt' | count
")
if test "$prompt_handlers" != "1"
    echo "FAIL [event]: _gc_prompt event handler count = $prompt_handlers, expected 1" >&2
    exit 1
end

set -l preexec_handlers (fish -c "
    source '$integration'
    source '$integration'
    functions --handlers-type event | string match -r '_gc_preexec' | count
")
if test "$preexec_handlers" != "1"
    echo "FAIL [event]: _gc_preexec event handler count = $preexec_handlers, expected 1" >&2
    exit 1
end

echo 'PASS: fish integration is idempotent across re-sources'
