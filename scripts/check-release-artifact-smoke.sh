#!/usr/bin/env bash
# Smoke a packaged macOS artifact: extract, verify executable bit + arch,
# run --version, --help, validate-specs, status --json, install --dry-run
# with an isolated HOME. Refuses to publish if any check fails.
#
# Native-arch binaries are executed end-to-end. Cross-arch binaries are
# inspected structurally (extract + file(1)) since the CI runner can't run
# them — better than nothing, and the matching-arch binary in the same
# release still gets full execution.
set -euo pipefail

usage() {
    echo "Usage: $0 <artifact.tar.{gz,xz}|artifact.zip>" >&2
    exit 64
}

[[ $# -eq 1 ]] || usage
ARTIFACT="$1"
[[ -f "${ARTIFACT}" ]] || { echo "no such file: ${ARTIFACT}" >&2; exit 1; }

WORK="$(mktemp -d)"
ISO_HOME="$(mktemp -d)"
# Capture cwd before installing the trap so an early failure between here and
# the `cd "${ISO_HOME}"` below still has a valid restore target. The trap
# leaves cwd at "/" if ORIGINAL_PWD is unset (or the directory is gone) so
# the subsequent `rm -rf` is not running from inside ISO_HOME on filesystems
# (NFS, FUSE) that return EBUSY for self-deleted cwds.
ORIGINAL_PWD="$PWD"
cleanup() {
    cd "${ORIGINAL_PWD:-/}" 2>/dev/null || true
    rm -rf "${WORK}" "${ISO_HOME}"
}
trap cleanup EXIT

case "${ARTIFACT}" in
    *.tar.gz) tar -xzf "${ARTIFACT}" -C "${WORK}" ;;
    *.tar.xz) tar -xJf "${ARTIFACT}" -C "${WORK}" ;;
    *.zip)    unzip -q "${ARTIFACT}" -d "${WORK}" ;;
    *) echo "unknown archive: ${ARTIFACT}" >&2; exit 64 ;;
esac

BIN=$(find "${WORK}" -type f -name 'ghost-complete' -perm -u+x -print -quit)
if [[ -z "${BIN}" ]]; then
    echo "FAIL: no executable ghost-complete in archive" >&2
    find "${WORK}" -type f
    exit 1
fi

ARCH=$(file "${BIN}" | awk -F': ' '{print $2}')
echo "Architecture: ${ARCH}"

# Detect runner arch; only execute matching-arch binary. The default branch
# catches future file(1) wording drift, universal Mach-O wrappers, or a host
# arch we have no pattern for — any of which would otherwise leave RUN=0 and
# silently downgrade the smoke to extraction-only without saying why.
HOST_ARCH=$(uname -m)
RUN=0
case "${HOST_ARCH}:${ARCH}" in
    arm64:*arm64*|arm64:*aarch64*) RUN=1 ;;
    x86_64:*x86_64*|x86_64:*x86-64*) RUN=1 ;;
    arm64:*|x86_64:*)
        echo "WARN: arch detection inconclusive — host ${HOST_ARCH}, file(1) returned: ${ARCH} — falling back to structural smoke (no execution test)" >&2
        ;;
    *)
        echo "WARN: unrecognized host arch ${HOST_ARCH} (file(1): ${ARCH}) — falling back to structural smoke (no execution test)" >&2
        ;;
esac

if (( RUN == 0 )); then
    echo "OK: cross-arch artifact (${HOST_ARCH} runner, ${ARCH} binary), structural smoke only"
    exit 0
fi

# Helper: print only the first N lines of a captured string. Avoids piping
# large outputs through `head`, which interacts badly with `set -o pipefail`
# when the upstream writer (or this script's own `printf`) hits SIGPIPE
# before flushing all data.
print_head() {
    local -r limit="$1"
    local -r blob="$2"
    local -i count=0
    local line
    while IFS= read -r line; do
        printf '%s\n' "${line}"
        count=$((count + 1))
        (( count >= limit )) && return 0
    done <<< "${blob}"
}

echo "--version:"
"${BIN}" --version

echo "--help excerpt:"
HELP_OUT="$("${BIN}" --help)"
print_head 5 "${HELP_OUT}"

# Run validate-specs and status from an isolated HOME with cwd inside that
# HOME. auto_detect_spec_dirs walks $XDG_CONFIG_HOME/ghost-complete/specs,
# the directory next to the binary, and ./specs in cwd; without isolation
# the workflow's repo-root cwd would let ./specs leak in and the smoke
# would validate merged (embedded + source) specs instead of the packaged
# binary's embedded-only view — precisely the regression this gate exists
# to catch. ISO_HOME contains no specs/ subdir and no config dir.
ISO_ENV=(env HOME="${ISO_HOME}" XDG_CONFIG_HOME="${ISO_HOME}/.config")
cd "${ISO_HOME}"

# Capture-and-fail pattern: `OUT=$(cmd 2>&1)` under `set -e` discards the
# captured output if cmd exits non-zero, leaving CI logs with only the exit
# code. Wrap each capture in an explicit failure printer so stderr from the
# binary surfaces in the workflow log when the smoke step fails.
echo "validate-specs (first 5 of JSONL output):"
if ! VALIDATE_OUT="$("${ISO_ENV[@]}" "${BIN}" validate-specs --json 2>&1)"; then
    echo "FAIL: validate-specs exited non-zero" >&2
    echo "Output:" >&2
    echo "${VALIDATE_OUT}" >&2
    exit 1
fi
print_head 5 "${VALIDATE_OUT}"

echo "status --json (fully_functional count):"
if ! STATUS_JSON="$("${ISO_ENV[@]}" "${BIN}" status --json 2>&1)"; then
    echo "FAIL: status --json exited non-zero" >&2
    echo "Output:" >&2
    echo "${STATUS_JSON}" >&2
    exit 1
fi
SPECS=""
while IFS= read -r line; do
    if [[ "${line}" == *'"fully_functional"'* ]]; then
        SPECS="${line}"
        break
    fi
done <<< "${STATUS_JSON}"
echo "${SPECS}"
# Match the value side of the JSON pair so we don't accidentally pass on a
# stray two-digit substring elsewhere on the line. Requires ≥10 specs.
[[ "${SPECS}" =~ \"fully_functional\":[[:space:]]*[1-9][0-9]+ ]] || {
    echo "FAIL: zero or missing fully_functional spec count" >&2
    exit 1
}

echo "install --dry-run (isolated HOME=${ISO_HOME}):"
if ! INSTALL_OUT="$("${ISO_ENV[@]}" "${BIN}" install --dry-run 2>&1)"; then
    echo "FAIL: install --dry-run exited non-zero" >&2
    echo "Output:" >&2
    echo "${INSTALL_OUT}" >&2
    exit 1
fi
print_head 20 "${INSTALL_OUT}"

# Confirm dry-run did not write anything visible under the isolated HOME.
if [[ -f "${ISO_HOME}/.zshrc" ]]; then
    echo "FAIL: install --dry-run wrote .zshrc" >&2
    exit 1
fi
if [[ -d "${ISO_HOME}/.config/ghost-complete" ]]; then
    echo "FAIL: install --dry-run wrote .config/ghost-complete" >&2
    exit 1
fi

# The EXIT trap restores cwd to ORIGINAL_PWD before `rm -rf`-ing ISO_HOME so
# the unhappy path is covered too — no trailing manual `cd` needed here.

echo "OK: release artifact smoke passed for ${ARTIFACT}"
