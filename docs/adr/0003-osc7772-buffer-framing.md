# 0003. OSC 7772 percent-encoded buffer framing

- **Status:** Accepted
- **Date:** 2026-04-28
- **Supersedes:** —
- **Superseded by:** —

## Context

`shell/ghost-complete.zsh` historically reported the live `$BUFFER` to the
proxy via:

```zsh
printf '\e]7770;%d;%s\a' "$CURSOR" "$BUFFER"
```

interpolating `$BUFFER` raw into the OSC payload. `vte = "0.15"` splits OSC
parameters on `;` (byte `0x3B`), so a buffer like `if true; then` produces a
4-param OSC: `params[2] = "if true"`, `params[3] = " then"`. The dispatch
arm in `crates/gc-parser/src/performer.rs` only reads `params[2]`, so the
buffer is silently truncated at the first semicolon. Every keystroke
through `; & | && || ;;` was wrong.

Two more failure modes compound the problem:

- **`\a` (BEL, `0x07`) inside `$BUFFER`** terminates the OSC mid-payload.
  The parser sees a short, "valid" OSC for everything before the BEL.
- **Embedded `\x1b]…\a` or `\x1b\\` (ST)** does not just terminate the
  outer envelope; it **smuggles a nested escape sequence into the parser's
  state machine**. The OSC 7770 path is the only place the proxy ingests
  fully attacker-controlled bytes (user typing or pasting). A user who
  pasted text crafted to look like an OSC 7 (CWD update) would see the
  proxy's view of the working directory shift mid-line — silently.

The threat model is byte-level corruption / smuggling, not eavesdropping;
this is a local-process side channel between the user's shell and our
PTY proxy. See the ADR's [`Threat Model`](#threat-model) section below.

## Decision

We percent-encode `$BUFFER` in the zsh emitter and bump the OSC number to
`7772` so the parser dispatch is atomic at `match params[0]`:

```zsh
\e]7772;<cursor>;<percent_encoded_buffer>\a
```

- **Encoded alphabet.** Bytes in `[A-Za-z0-9._~/-]` and the literal space
  pass through. Everything else — including `;`, `\a`, `\x1b`, `%`, `\\`,
  all `< 0x20` controls, `0x7F`, and `0x80`–`0xFF` — is encoded as `%XX`
  (uppercase hex). UTF-8 multibyte sequences are encoded byte-by-byte and
  reassembled on decode.
- **Decoder behaviour.** `crates/gc-parser/src/performer.rs::percent_decode_buffer`
  rejects (returns `None`) on any invalid hex digit or truncated `%`; the
  whole frame is dropped. Decoded payload is then `String::from_utf8`-validated.
  Any failure path drops the frame (logs a `tracing::warn!`; prior buffer
  state untouched).
- **Migration.** OSC 7770 stays in the parser as a deprecated, read-only
  legacy path for one minor release. First hit per process logs a one-shot
  `tracing::warn!`; subsequent hits drop to `trace!`. The 7770 dispatch is
  scheduled for `#[ignore]` in v0.11.0 and deletion in v0.12.0. The zsh
  emitter only writes 7772.

## Consequences

### Positive

- **Correctness.** `if true; then echo a; fi` and every other shell idiom
  containing `;`, `&`, `|`, `*`, `(`, `)`, `=`, `\`, control bytes, or
  multi-byte UTF-8 round-trips byte-for-byte. The canonical bug witness is
  pinned in `osc7772_regression_pin_canonical_bug_witness` and the proptest
  fuzzer covers arbitrary 4 KiB UTF-8 strings (`crates/gc-parser/tests/osc7772_proptest.rs`).
- **Security.** A buffer crafted to look like `\e]7;file:///etc/passwd\a`
  encodes to `%1B%5D7%3Bfile%3A///etc/passwd%07`. vte sees a single,
  opaque OSC 7772 parameter. The bytes only become `\e]7;…\a` *after*
  percent-decode inside `osc_dispatch`, where they go straight to
  `set_command_buffer` — they never re-enter the parser. The integration
  test `osc7772_real_zsh_roundtrip` includes this fixture and asserts
  `state.cwd()` does not change.
- **Wire safety.** The encoded alphabet is `[A-Za-z0-9._~/-% ]`. None of
  those bytes terminate an OSC, nest an escape, or get mangled by tmux's
  passthrough mode.
- **Performance.** Encoder is one zsh string-concatenation loop using the
  same idiom as `_gc_urlencode_path`; decoder is ~25 lines of pure Rust.
  Both run well under the per-keystroke budget. The decoder bench lives in
  `crates/gc-parser/benches/parser_bench.rs` (`osc7772_decode` group) for
  reproducibility.

### Negative

- **Wire size.** Worst case (every byte encoded) is 3× expansion. Typical
  buffers (mostly letters, digits, spaces, paths) hit <2×. vte's
  `MAX_OSC_PARAMS = 16` is irrelevant because the new framing uses a
  single payload parameter, and the `std`-feature OSC raw buffer grows
  unbounded.
- **One shell-side encode loop.** Every keystroke pays a per-byte loop in
  zsh. Encoder cost is well under the per-keystroke budget for typical
  interactive `$BUFFER` sizes — invisible next to the existing
  keystroke→suggestion budget.
- **A deprecation cycle.** The parser keeps two OSC arms (`7770` legacy +
  `7772`) for one release. Net code grew ~50 lines until v0.12.0 deletes
  the legacy arm.

### Neutral

- bash/fish integrations still emit raw OSC 7770
  (`shell/ghost-complete.bash` `_gc_report_buffer` interpolates
  `$READLINE_LINE`; `shell/ghost-complete.fish` `_gc_report_buffer`
  interpolates `commandline`) and inherit the same `;` / `\a` / `\e`
  truncation and smuggling bug class. They are explicitly out of scope
  for this PR — per `CLAUDE.md` these scripts are not actively tested —
  but they MUST be migrated to OSC 7772 before the v0.12.0 deletion of
  the legacy 7770 dispatch arm. Otherwise their buffer reporting breaks
  entirely the moment the legacy parser path goes away.

## Threat Model

| Source | Trust | Surface |
|---|---|---|
| User typing at prompt | Untrusted in payload bytes; trusted in intent | `$BUFFER` may contain any byte zsh permits in a line |
| Pasted text from clipboard | Untrusted | Same surface; can include `\x1b…\a` crafted to escape framing |
| Hostile shell snippet (`.zshrc` injection) | Out of scope | A user with hostile `.zshrc` already owns the proxy |
| Network / file content displayed by shell | Out of scope | Shell output flows through `gc-parser` separately and is already sanitised |

Concrete attacker capability denied by this ADR: a user types
`\e]7;file:///etc\a` at the prompt, expecting the proxy to update its CWD.
Today (post-fix) the bytes encode to a single opaque payload; the parser's
state machine never sees the inner escape.

## Alternatives considered

- **Length-prefix `;<len>;<bytes>`.** Rejected. vte 0.15 has no raw-byte
  OSC hook — `osc_dispatch` is called with parameters already pre-split on
  `;`, and bytes `0x07` / `0x1B\\` in the payload still terminate the OSC
  inside vte before our handler runs. The framing layer is owned by vte;
  we cannot bypass its delimiter rules.
- **Base64.** Rejected on cost. zsh has no builtin base64; spawning
  `base64(1)` per keystroke adds a fork-exec on every redraw, dwarfing
  the in-process zsh encoding loop and visible against the
  keystroke→suggestion budget. Also fragile on minimal images (alpine,
  busybox) where `base64(1)` is not in the default toolchain.
- **DCS (Device Control String) sideband.** Rejected. vte's DCS hook
  surface is narrower than OSC, multiplexers (tmux, screen) handle DCS
  passthrough less consistently, and the framing problem is identical
  (DCS terminates on `\x1b\\` exactly like OSC). Higher integration risk
  for no security or performance win.
- **Dual-accept on 7770; sniff first byte for `%`.** Rejected. Raw
  buffers legitimately contain `%` (e.g. `printf '%s'`); no reliable
  single-OSC sniff exists. Bumping the OSC number makes the protocol
  switch atomic at the dispatch level and detectable in regression tests
  (no caller of 7770 should exist after the deprecation window).

## References

- `crates/gc-parser/src/performer.rs` — OSC 7772 dispatch + `percent_decode_buffer`
- `shell/ghost-complete.zsh` — `_gc_urlencode_buffer`, `_gc_report_buffer`
- `crates/gc-parser/tests/osc7772_proptest.rs` — proptest round-trip
- `crates/gc-pty/tests/osc7772_zsh_roundtrip.rs` — real zsh integration test
- [ADR-0002](0002-vte-vs-vt100.md) — why we run on `vte` (and therefore
  inherit its OSC delimiter rules)
