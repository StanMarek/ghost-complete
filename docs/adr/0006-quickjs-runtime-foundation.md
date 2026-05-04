# 0006. QuickJS runtime foundation for `requires_js` specs

- **Status:** Accepted; extended in the same release (see Status note)
- **Date:** 2026-05-03
- **Supersedes:** —
- **Superseded by:** —

## Status note (added post-merge)

This ADR was drafted assuming Phase 3 (the `gc-jsrt` foundation) would
ship ahead of Phases 4–8. The release that landed this ADR also landed
those follow-on phases: `gc-suggest` dispatches all three
`js_runtime.kind` variants (`post_process`, `script_function`,
`custom`), the kill switch defaults to on, `ghost-complete doctor` and
`status --json` surface per-runtime diagnostics, and a coverage
regression gate is wired in CI. The Decision and Consequences sections
below describe the snapshot the ADR was written against; treat the
"Phase 4 will…" / "Phase 4 follow-up" wording as historical scoping,
not as a description of what the merged tree does. See
[`docs/JS_RUNTIME.md`](../JS_RUNTIME.md) for the current behaviour.

## Context

180 of the 709 embedded specs carry `requires_js: true` generators;
`aws` alone ships ~1843 of them. ADR 0005 closed the demo gap with
native Rust providers for `make`, `npm run`, and `cargo run -p`, but
the rest of the corpus remains stuck behind the JS gate.

Phase 2 (UX-9, commit `36b83ca`) preserved the structured
`js_runtime` metadata at spec-load time without yet executing it.
Phase 3 — this ADR — lands a bounded JavaScript evaluator that
later phases will dispatch to.

The constraints are:

1. **Sandboxing.** Spec JS runs unprivileged. No filesystem, no
   network, no spawning, no module loader, no Node-style globals.
2. **Wall-clock bounded.** A runaway `while(true)` must not freeze
   the proxy.
3. **Memory bounded.** A `let a=[]; while(true) a.push(...)` must
   not bring the host down.
4. **Binary size sensitive.** The corpus already pushed the binary
   to 103 MB; we want headroom under the 110 MB ceiling.
5. **Latency sensitive.** Suggestion ranking budgets sit around
   50 ms end-to-end; we cannot afford a per-keystroke runtime spin-up.

## Decision

Add a new workspace crate `gc-jsrt` that owns the JavaScript
evaluation surface. Other crates depend on it as a dispatch target;
the rquickjs dependency does not leak past this boundary.

Architecture:

- **rquickjs 0.10** with `default-features = false` and only the
  `rust-alloc` feature enabled. No `loader`, `dyn-load`, `futures`,
  `parallel`, `macro`, or `phf`. The runtime exposes only standard
  ECMA-262 intrinsics (`JSON`, `Math`, `Date`, regex, …) plus the
  thrower stubs we install for `eval`/`Function`.
- **One sync `rquickjs::Runtime` per worker, lifetime-pinned to a
  dedicated OS thread.** The Tokio side talks to the worker through
  `std::sync::mpsc` (cheap, blocking on the worker side) plus
  per-job `tokio::sync::oneshot` reply channels. `AsyncRuntime` is
  not used — our JS is short, synchronous post-processing logic; a
  sync runtime is the smaller primitive.
- **Fresh `Context` per job.** The runtime is reused across jobs to
  keep the GC warm and avoid allocator churn, but each job opens a
  brand-new context so two unrelated specs cannot pollute each
  other's globals.
- **Sandbox configuration** strips `require`, `process`, `Deno`,
  `Bun`, `setTimeout`/`setInterval`/`setImmediate`,
  `fetch`/`XMLHttpRequest`/`WebSocket`, `Buffer`, `Worker`, etc.,
  then shadows `eval` and `Function` with closures that throw
  `disabled in gc-jsrt`. Defense in depth — none of these are
  reachable from the QuickJS intrinsics we enable, but listing them
  explicitly makes the contract auditable.
- **Wall-clock interrupt** via `Runtime::set_interrupt_handler`. A
  shared `AtomicI64` carries a deadline relative to the worker's
  process-start `Instant`; the handler returns `true` once `now()`
  passes it. The handler is cleared back to `i64::MAX` after each
  job through an RAII guard.
- **Hard caps.** Memory limit 8 MiB, max stack 512 KiB, GC threshold
  2 MiB. Output is normalized through `JSON.stringify` (which throws
  cleanly on cycles) with a 256 KiB total-bytes cap and a per-suggestion
  256-byte name / 1024-byte description cap, plus a 1024-suggestion
  array cap with truncation.

Phase 3 ships the foundation only — the `gc-suggest` engine does
**not** yet call into `gc-jsrt`. The `[suggest.providers] js_runtime`
config field is wired into the schema (default `true`); Phase 4 begins
honouring it for `post_process` generators.

## Consequences

### Positive

- The corpus' three JS shapes (`post_process`, `script_function`,
  `custom`) all reduce to the same `evaluate(program, input,
  deadline)` primitive. Phase 4 and Phase 5 build dispatch on top
  of one stable surface.
- The dependency boundary keeps rquickjs off `gc-suggest`'s
  compile graph in tests and benchmarks that don't need it.
- `Runtime::set_interrupt_handler` plus the per-job context reset
  gives a much stronger isolation guarantee than embedding JS in
  the same address space without bounds.
- The sync runtime is ~2x faster to spawn than `AsyncRuntime` and
  has a smaller dependency footprint.

### Negative

- **Bounded but not hard real-time.** The interrupt handler fires
  on QuickJS' periodic checks; a long native operation (pathological
  regex, JSON parsing of huge strings) may overshoot the deadline.
  Memory and output caps are the second line of defence.
- **Binary size.** Phase 3 alone added no measurable size to the
  binary because nothing links `gc-jsrt` yet. Phase 4 will pull the
  dependency in; budget is tracked in
  `benchmarks/binary-size-baseline.txt` against the 110 MB ceiling.
- **Per-job context allocation** is a fixed cost. Microbenchmarks
  pending — Phase 4 will report numbers when the engine path is
  wired.

### Alternatives considered

1. **Boa engine.** Rust-native, no FFI, simpler build, but ~3x
   slower on script-heavy benchmarks and the API surface lacks the
   per-context isolation primitives we need.
2. **deno_core.** Heavier dependency tree (V8), much larger binary
   impact, and brings Node-style APIs we'd then need to strip.
3. **Subprocess to a real Node runtime.** Defeats the latency
   budget — process spawn alone exceeds 50 ms on macOS.
4. **rquickjs `AsyncRuntime`.** Useful when JS calls back into Rust
   futures; we don't, so the extra complexity (futures lock, async
   mutex) is unjustified.

## References

- Phase 2 commit: `36b83ca` — `js_runtime` schema + converter.
- Phase 3 implementation: `crates/gc-jsrt/`.
- Phase 4 follow-up: `gc-suggest` dispatch for `post_process` kind.
- rquickjs upstream: <https://github.com/delskayn/rquickjs>.
