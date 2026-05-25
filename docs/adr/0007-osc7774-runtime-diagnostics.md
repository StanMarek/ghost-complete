# 0007. OSC 7774 runtime diagnostic frame

- **Status:** Accepted
- **Date:** 2026-05-21
- **Supersedes:** —
- **Superseded by:** —

## Context

Two shell-side failure modes degraded silently before this ADR:

1. **`_gc_report_env` budget exhaustion.** `_gc_report_env` builds an
   OSC 7773 env snapshot payload under a shell-side byte budget (512 KiB
   total, 16 KiB per encoded `KEY=VALUE` entry). When a variable exceeds
   the per-value cap, or the running total would exceed the total budget,
   the variable is silently dropped and a `truncated` flag is set. Prior
   to this ADR the proxy had no way to distinguish "env snapshot is
   complete" from "env snapshot was truncated mid-way" — both produced
   identical OSC 7773 frames.

2. **`_gc_install_zle_hook` non-user widget.** The ZLE hook installer
   registers `zle-line-pre-redraw` so the proxy receives buffer updates
   on every keystroke. When that widget slot is already occupied by a
   non-user widget (`completion:*`, `builtin:*`, etc.), chaining is
   unsafe and the installer intentionally no-ops. Prior to this ADR the
   hook was silently absent: buffer reports never arrived and the proxy
   had no visibility into why.

Both failure modes called for a structured, machine-readable channel that
carries shell-side warnings to the proxy without producing any visible
artifacts in the terminal emulator. OSC 7773 (env snapshot) and OSC 7772
(buffer report) are already stripped from the terminal-bound byte stream
by `PrivateOscFilter` in `crates/gc-pty/src/proxy.rs`. Extending the
same stripping to a new code is trivial and free of terminal-side risk.

## Decision

Reserve OSC code `7774` for Ghost-Complete-private runtime diagnostics:

```
\e]7774;<reason_code>;<percent_encoded_detail>\a
```

BEL-terminated, consistent with the other GC-private 777x frames. The
code joins `PrivateOscFilter`'s `PRIVATE_CODES` set alongside 7770–7773
and is stripped before any bytes reach the terminal emulator.

### Wire format

- **`<reason_code>`** — plain ASCII token identifying the failure class.
  Initial set: `env_truncated`, `zle_hook_disabled`.
- **`<percent_encoded_detail>`** — an additional payload, percent-encoded
  using `_gc_urlencode_buffer`'s allow-list (`[A-Za-z0-9._~/-]` plus the
  literal space; all other bytes as `%XX` uppercase hex). A literal `%`
  byte is itself encoded as `%25`, which keeps the encoder round-trip-safe
  over already-encoded payloads. The encoding rules are identical to
  those specified for OSC 7772 in ADR 0003.

### `env_truncated` emission

Emitted by `_gc_report_env` in `shell/ghost-complete.zsh` when at least
one variable was dropped due to the per-value cap or the total-budget
ceiling. The detail field is a **decimal byte count equal to the number
of bytes successfully emitted** into the OSC 7773 payload — it is the
final value of the shell's `$used` accumulator (the total size of the
payload actually emitted), not the number of bytes dropped. A `$used`
value much smaller than `_GC_ENV_TOTAL_BUDGET` suggests a per-value-cap
rejection cut the sweep short; a value close to the budget indicates
total-budget exhaustion. The split is not exact — `essentials` are
emitted first, so even a small `$used` includes those bytes.

The frame is guarded by a one-shot latch (`_GC_ENV_TRUNCATED_REPORTED`)
so at most one `env_truncated` diagnostic is emitted per shell session,
regardless of how many subsequent `_gc_report_env` calls truncate.

Example wire frame: `\e]7774;env_truncated;65536\a` — 65536 bytes of
env payload were successfully included; further entries were dropped.

### `zle_hook_disabled` emission

Emitted by `_gc_install_zle_hook` in `shell/ghost-complete.zsh` when the
`zle-line-pre-redraw` widget slot is occupied by a non-user widget and
cannot be safely chained. The detail field is the full widget descriptor
(e.g. `completion:...`), percent-encoded with `_gc_urlencode_buffer`.

Example wire frame: `\e]7774;zle_hook_disabled;completion%3Afoo\a`.

### Emission gating

Both emission sites check `[[ -n "$GHOST_COMPLETE_ACTIVE" ]]` before
printing, consistent with every other GC-private 777x frame. The
diagnostic is therefore only ever emitted when the proxy is the process
receiving the output.

### Parser-side consumption

`crates/gc-parser/src/performer.rs::osc_dispatch` matches `b"7774"`,
parses `params[1]` (reason code) and `params[2]` (detail) into a
structured `Diagnostic` enum variant (`EnvTruncated`, `ZleHookDisabled`,
or `Unknown` for codes a stale parser does not recognise), stores it on
the private `TerminalState::last_diagnostic` field, and emits a
`tracing::warn!` with the full message. The field is overwritten on
each received diagnostic; callers drain it via the public
`TerminalState::take_diagnostic()` accessor and should do so promptly.

### Filter-side consumption

`PrivateOscFilter` in `crates/gc-pty/src/proxy.rs` previously used a
static prefix matcher keyed on a single hard-coded code (`OscPrefix {
matched: usize }` against `CODE = b"7773"`). As part of this ADR's
footprint, that matcher was replaced with a digit-accumulating state
machine (`CodeAcc { acc: Vec<u8> }`) that buffers the numeric OSC code
bytes and matches the accumulator against `PRIVATE_CODES` on the OSC
terminator byte. `PRIVATE_CODES` is now the full set `[b"7770",
b"7771", b"7772", b"7773", b"7774"]`. Once the digit-accumulating
machine is in place, supporting OSC 7774 is just an entry in the slice
— but the refactor itself is new in this PR, not pre-existing.

## Consequences

### Positive

- **Silent failures become observable.** Both failure modes now produce a
  structured `tracing::warn!` in the proxy log. An operator watching
  the proxy's trace output sees
  `shell-side runtime diagnostic: env_truncated:65536` or
  `shell-side runtime diagnostic: zle_hook_disabled:completion%3A...`
  instead of guessing why env completions are incomplete or why buffer
  reports are absent.
- **No terminal-visible artifacts.** `PrivateOscFilter` strips 7774
  frames from the terminal-bound byte stream by the same mechanism as
  7770–7773. Terminals that don't understand GC-private codes (every
  terminal) never see them.
- **Wire-safe encoding.** The `_gc_urlencode_buffer` alphabet contains no
  OSC delimiters (`\a`, `\x1b`, `;`), so the frame is safe against the
  same injection classes documented in ADR 0003.

### Negative

- **One-shot latch for `env_truncated`.** The `_GC_ENV_TRUNCATED_REPORTED`
  latch suppresses subsequent diagnostics in the same shell session. If
  the user sources an update to `ghost-complete.zsh` mid-session, the
  latch will be stale. This is acceptable for the diagnostic use case —
  the proxy only needs to know once that the snapshot is incomplete.
- **`last_diagnostic` is last-write-wins.** If two diagnostics arrive in
  quick succession the first is overwritten. In practice `env_truncated`
  is one-shot and `zle_hook_disabled` fires once at ZLE init time, so
  concurrent delivery is not expected.

### Neutral

- The parser captures the most recent OSC 7774 frame as an
  `Option<Diagnostic>` and exposes it via `TerminalState::take_diagnostic()`
  (the underlying field is private). The accessor is currently
  observation-only — the parser's own tests are the sole in-tree
  consumer; it is reserved for future proxy-side use.

## Alternatives considered

- **Append a suffix to the OSC 7773 frame.** Rejected. OSC 7773 is
  purpose-built for the env snapshot payload; mixing diagnostic metadata
  into the payload would require the Rust parser to differentiate "is this
  a partial snapshot or a complete one with a trailing diagnostic token",
  adding ambiguity with no benefit over a dedicated code.
- **Write a plain log line from the shell to a temp file.** Rejected. The
  proxy process and the shell are connected only via the PTY pair; a
  shared temp file introduces a race, a cleanup concern, and a potential
  TOCTOU. OSC frames are already the established in-band channel.
- **Reuse OSC 7771 with an extended payload.** Rejected. OSC 7771 is the
  prompt-boundary fallback marker (analogous to OSC 133 for terminals that
  mangle it). Multiplexing a diagnostic channel onto a timing-sensitive
  prompt-boundary code would conflate two unrelated semantics and
  complicate the parser dispatch arm.

## References

- `crates/gc-parser/src/performer.rs` — OSC 7774 dispatch arm
- `crates/gc-parser/src/state.rs` — `TerminalState.last_diagnostic` field
- `shell/ghost-complete.zsh` — `_gc_report_env` (`env_truncated` emission
  and `_GC_ENV_TRUNCATED_REPORTED` latch), `_gc_install_zle_hook`
  (`zle_hook_disabled` emission)
- `crates/gc-pty/src/proxy.rs` — `PrivateOscFilter::PRIVATE_CODES`
- [ADR-0003](0003-osc7772-buffer-framing.md) — establishes the 777x
  GC-private OSC namespace, percent-encoding alphabet, and
  `PrivateOscFilter` stripping mechanism that OSC 7774 extends
