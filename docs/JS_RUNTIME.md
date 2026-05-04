# JS Runtime (`gc-jsrt`)

Reference for the bounded QuickJS evaluator that backs `requires_js`
spec generators.

The crate lives at [`crates/gc-jsrt/`](../crates/gc-jsrt/) and is a
self-contained dependency target — `gc-suggest` calls into it via the
`JsWorker` handle and never sees the `rquickjs` types directly.

## Status

The runtime foundation, bounded sandbox, output normalization, engine
dispatch, cache partitioning, and provider kill switch are wired into
the suggestion path. `ghost-complete doctor` and `status --json`
surface per-runtime diagnostics, and a coverage regression gate guards
against silent drops in `requires_js_generators_supported`.

`gc-suggest` dispatches all populated `js_runtime.kind` variants:
`post_process`, `script_function`, and `custom`. The dispatch wiring
is gated on `[suggest.providers] js_runtime` in `config.toml`.
Default is `true`. Setting it to `false` skips JS-backed generators
without disabling static spec data such as subcommands, options, and
argument hints.

## Class A / B / C distinctions

Every `requires_js` spec splits into one of three runtime classes,
mirrored by [`JsRuntimeKind`](../crates/gc-suggest/src/specs.rs).
The live count is reported by `ghost-complete status --json`:

| Class | Kind             | Pattern                                            | Status |
| ----- | ---------------- | -------------------------------------------------- | ------ |
| A     | `PostProcess`    | `script: [...]` + `postProcess: out => [...]`      | Active |
| B     | `ScriptFunction` | `script: (tokens) => [...args]`                    | Active |
| C     | `Custom`         | `custom: async (tokens) => [{name, description?}]` | Active |

All three reduce to the same `JsWorker.evaluate(program, input,
deadline)` primitive — only the input shape and call-site differ.

## Sandbox model

The sandbox is built up in three layers:

1. **Constructor choice.** `Context::full(&runtime)` enables only
   ECMA-262 intrinsics (`JSON`, `Math`, `Date`, regex, `Promise`,
   `Array`, `Object`, `String`, `Number`, …). No module loader, no
   `require`, no `import`.
2. **Global stripping.** [`crates/gc-jsrt/src/sandbox.rs`](../crates/gc-jsrt/src/sandbox.rs)
   removes any Node/Deno/Bun extension that could plausibly arrive
   from a future feature flag or accidental linkage:
   `require`, `module`, `exports`, `process`, `Deno`, `Bun`,
   `setTimeout`/`setInterval`/`setImmediate`,
   `clearTimeout`/`clearInterval`/`clearImmediate`, `queueMicrotask`,
   `fetch`, `XMLHttpRequest`, `WebSocket`, `EventSource`, `Request`,
   `Response`, `Headers`, `FormData`, `Worker`, `SharedWorker`,
   `MessageChannel`, `MessagePort`, `BroadcastChannel`,
   `localStorage`, `sessionStorage`, `indexedDB`, `Buffer`,
   `ReadableStream`, `WritableStream`, `TransformStream`,
   `alert`, `confirm`, `prompt`, `navigator`, `document`, `window`.
3. **Defense-in-depth shadowing.** `eval` and `Function` are
   replaced with closures that throw `disabled in gc-jsrt`. The
   QuickJS intrinsics list does include them by default — we
   override the names so a corpus author cannot accidentally (or
   maliciously) reach them.

Each evaluation opens a **fresh `Context`** before running the
program, so two specs can never observe each other's globals. The
underlying `Runtime` is reused for warm GC.

## Wall-clock timeout caveats

The runtime installs an interrupt handler via
[`Runtime::set_interrupt_handler`](https://docs.rs/rquickjs/0.10.0/rquickjs/struct.Runtime.html#method.set_interrupt_handler).
It is called periodically by the QuickJS bytecode dispatch loop and
returns `true` once the wall-clock deadline has passed.

**This is bounded but not hard real-time.** A few patterns can
overshoot the deadline:

- Pathological regex backtracking inside a single `String.match`
  call. QuickJS does not interrupt mid-regex.
- Very large `JSON.parse` / `JSON.stringify` calls. The bridge
  function is native and not interruptible.
- Tight loops over typed arrays, where the per-instruction
  interrupt check skips array bounds.

The mitigations are layered:

- **Output cap** (256 KiB JSON, 1024 suggestions, 256-byte names) —
  prevents a runaway loop that builds a giant array from filling
  the heap.
- **Memory limit** (8 MiB) — QuickJS hard-aborts with `Allocation`
  if total resident memory crosses the limit.
- **Stack limit** (512 KiB) — prevents a `function f() { f() }`
  bomb from blowing the host stack.
- **GC threshold** (2 MiB) — runs a sweep often enough to keep
  cyclic garbage from masking real growth against the memory cap.

## Output normalization

[`crates/gc-jsrt/src/normalize.rs`](../crates/gc-jsrt/src/normalize.rs)
serialises the JS return value through `JSON.stringify` and parses
the resulting bytes back as `serde_json::Value`. This sidesteps
several pitfalls in one move:

- Cyclic objects throw on `JSON.stringify` (mapped to
  `JsDiagnosticCode::InvalidShape`).
- Functions, symbols, and host objects either omit themselves or
  render as `null`, which the normalizer rejects.
- The UTF-16/UTF-8 boundary is crossed exactly once, by the JSON
  serializer.

Accepted return shapes:

| JS value                                  | Result                                       |
| ----------------------------------------- | -------------------------------------------- |
| `'foo'`                                   | `[JsSuggestion { name: 'foo' }]`             |
| `['a', 'b']`                              | two `JsSuggestion`s                          |
| `{ name: 'x', description: 'y' }`         | one `JsSuggestion` with description          |
| `[{name, displayName, text}]`             | object array; first key wins (in that order) |
| `Promise<any of the above>`               | resolved synchronously, then normalized      |
| `(async () => ...)()`                     | same — Promises are unwrapped                |

Rejected with diagnostics:

- `null` / `undefined` → `EmptyOutput`
- `[]` → `EmptyOutput`
- numbers / booleans at the root → `InvalidShape`
- objects without a `name` / `displayName` / `text` → `InvalidShape`
- functions or symbols → `InvalidShape`
- cyclic objects → `InvalidShape`
- arrays > 1024 elements → truncated + `OversizedOutput`
- strings > 256 bytes (name) or 1024 bytes (description) → `OversizedOutput`
- total JSON > 256 KiB → `OversizedOutput`

A diagnostic on a successful `JsRuntimeOutput` is **non-fatal**: the
suggestion engine surfaces an empty (or partial) result without
aborting completion. `JsRuntimeError` is reserved for infrastructure
failures the engine cannot recover from (`WorkerDead`, `Internal`).

## Cache key composition

JS-backed generator results are cached with runtime-specific
partitions so two generators that share an argv but use different JS
sources cannot share post-processed suggestions. `post_process` and
`script_function` suggestion caches are keyed by the command, resolved
argv, optional cache directory, and a hash of the JS source namespaced
by runtime kind. Their raw stdout cache remains keyed by the resolved
argv. `custom` generators have no argv; their suggestion cache keys
the command, optional cache directory, JS source, and token
fingerprint.

`cache_by_directory` in the spec's `cache` block continues to apply
unchanged.

## The kill switch

```toml
[suggest.providers]
js_runtime = false   # disable the JS evaluator entirely
```

When `false`, `requires_js` generators with a populated `js_runtime`
shape (`post_process`, `script_function`, or `custom`) short-circuit
to the skipped path. The engine does not spawn the backing script for
JS-backed post-process generators and does not evaluate QuickJS for
any JS-backed generator. Static spec data (subcommands, options,
argument hints) continues to work.

The flag is read at engine builder time (same convention as
`max_results` / `generator_timeout_ms`), so changes require a proxy
restart.

## Concurrency model

```
                tokio task A ──┐
                tokio task B ──┼─► mpsc channel ─► gc-jsrt-worker thread
                tokio task N ──┘                   (one rquickjs::Runtime,
                                                   fresh Context per job)
                       ▲                            │
                       │                            │
                       └────────── oneshot ◄────────┘
```

- One `JsWorker` owns one OS thread and one runtime.
- Multiple Tokio tasks may call `JsWorker::evaluate` concurrently;
  the channel serialises them onto the worker.
- The runtime is reused across jobs (warm GC, no allocator churn).
- The `Context` is fresh per job for global isolation.

`JsWorker` is `Clone`. The internal `WorkerHandle` is reference-counted;
the worker thread shuts down once the last clone is dropped.

## See also

- ADR [`0006`](adr/0006-quickjs-runtime-foundation.md) — the
  decision record for this design.
- [`docs/COMPLETION_SPEC.md`](COMPLETION_SPEC.md) — `js_runtime`
  schema in the spec format.
- [`crates/gc-jsrt/`](../crates/gc-jsrt/) — implementation and tests.
