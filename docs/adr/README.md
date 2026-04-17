# Architecture Decision Records

This directory contains Architecture Decision Records (ADRs) for Ghost Complete.

An ADR is a short Markdown document that captures a single architecturally
significant decision — the context that forced the call, the decision itself,
and the consequences we accept by making it. We use the "Markdown Any Decision
Records" (MADR) conventions: one file per decision, numbered sequentially, with
a short title slug.

## Format

Each ADR lives in this directory as `NNNN-kebab-title.md`, where `NNNN` is a
zero-padded four-digit sequence starting at `0001`. Sections:

- **Title** — one-line summary, prefixed with `# NNNN. Title`
- **Status** — `Proposed`, `Accepted`, `Deprecated`, or `Superseded by ADR-N`
- **Context** — what situation forced the decision; cite file/line references
  into `CLAUDE.md` or `docs/ARCHITECTURE.md` where relevant
- **Decision** — what we chose, stated as a single active-voice sentence
  followed by supporting detail
- **Consequences** — positive and negative outcomes we accept

Keep each ADR tight — roughly 50 to 150 lines. If a decision needs more than
that, it probably belongs in `docs/ARCHITECTURE.md` instead.

## Index

| ADR                                          | Title                                  | Status   |
| -------------------------------------------- | -------------------------------------- | -------- |
| [0001](0001-pty-proxy-vs-plugin.md)          | PTY proxy over shell plugin            | Accepted |
| [0002](0002-vte-vs-vt100.md)                 | Parser-only VT tracking via `vte`      | Accepted |

When you add a new ADR, append a row to this index and bump the next `NNNN`.

## When to write an ADR

Write one when a decision is:

- Hard to reverse (shapes crate boundaries, public APIs, or data formats)
- Likely to be re-litigated ("why didn't we use X?") when a new contributor
  reads the code
- Driven by constraints that are not obvious from the code itself

Routine refactors, bug fixes, and small dependency bumps do not need an ADR.
Put those in [`CHANGELOG.md`](../../CHANGELOG.md).

## Related docs

- [`docs/ARCHITECTURE.md`](../ARCHITECTURE.md) — current design, "Key Design
  Decisions" section summarises the accepted ADRs
- [`CLAUDE.md`](../../CLAUDE.md) — project overview and dependency rationale
  notes that seed many of these ADRs
