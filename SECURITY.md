# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.10.x  | Yes       |

## Reporting a Vulnerability

If you discover a security vulnerability in Ghost Complete, please report it responsibly.

**Do not open a public issue.**

Instead, email the maintainer directly or use [GitHub's private vulnerability reporting](https://github.com/StanMarek/ghost-complete/security/advisories/new).

## What Constitutes a Security Issue

Ghost Complete is a PTY proxy that sits between your terminal and shell. Security-relevant issues include:

- **Arbitrary code execution** via crafted terminal escape sequences
- **Information disclosure** (e.g., leaking environment variables, command history to unintended destinations)
- **PTY escape** allowing a child process to break out of the proxy
- **Completion spec injection** where a malicious spec file could execute code

General bugs (crashes, rendering glitches, incorrect completions) should be reported as regular issues.

## Response

We will acknowledge receipt within 48 hours and aim to provide a fix or mitigation within 7 days for critical issues.
