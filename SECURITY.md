# Security Policy

## Supported Versions

We provide security fixes for the **latest released minor** and any **in-flight release candidate** for the next minor. Older minors are end-of-life; please upgrade.

The current supported version is whatever `git describe --tags --abbrev=0` reports on `master`. Patch releases (0.x.y → 0.x.(y+1)) are issued as needed for security and critical-correctness fixes.

## Reporting a Vulnerability

Report security findings privately via [GitHub's private vulnerability reporting](https://github.com/StanMarek/ghost-complete/security/advisories/new). Areas of particular interest:

- terminal escape handling (any input that escapes the proxy boundary)
- local config/spec parsing (file-read paths trusted by the proxy)
- JS runtime sandbox escapes (see [`docs/JS_RUNTIME.md`](docs/JS_RUNTIME.md))
- shell integration touching `.zshrc` outside managed blocks

Please do **not** open public GitHub issues for security findings.

## What Constitutes a Security Issue

Ghost Complete is a PTY proxy that sits between your terminal and shell. Security-relevant issues include:

- **Arbitrary code execution** via crafted terminal escape sequences
- **Information disclosure** (e.g., leaking environment variables, command history to unintended destinations)
- **PTY escape** allowing a child process to break out of the proxy
- **Completion spec injection** where a malicious spec file could execute code
- **JS runtime sandbox escape** allowing a `requires_js` spec to escape `gc-jsrt`'s resource caps or restricted host API

General bugs (crashes, rendering glitches, incorrect completions) should be reported as regular issues.

## Response

We will acknowledge receipt within 48 hours and aim to provide a fix or mitigation within 7 days for critical issues.
