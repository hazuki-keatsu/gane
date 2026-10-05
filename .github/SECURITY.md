# Security Policy

Gane is an experimental Go-like compiler under active development. It is not a production-hardened compiler or a replacement for the Go toolchain. Treat source files, generated IR, and resulting programs as potentially untrusted; do not rely on Gane as a security boundary.

## Supported versions

There is no stable release or formal security maintenance schedule. Security fixes are considered for the current development branch (`main`). Older revisions and unreleased snapshots are not maintained separately. This policy does not promise a response or fix within a particular timeframe.

## Report a vulnerability

For most bugs and suspected vulnerabilities, open a [public issue](https://github.com/hazuki-keatsu/gane/issues). Include a reproducer if it is safe to share publicly.

If **you believe the vulnerability is especially dangerous** and publishing its details could put others at risk, use [GitHub private vulnerability reporting](https://github.com/hazuki-keatsu/gane/security/advisories/new) instead, if available. If that option is unavailable, open a public issue asking the maintainers to arrange a private reporting channel, but **do not include exploit details or a sensitive proof of concept** in that issue.

A useful report includes:

- A concise description of the impact and the trust boundary involved.
- The affected commit or revision, operating system, Rust version, and LLVM version, if relevant.
- Minimal reproduction steps and a small input file or IR example (share privately if sensitive).
- Expected and observed behavior, including any crash or unexpected code execution.
- Whether you believe the issue is already public.

For private reports, please allow maintainers time to investigate and coordinate disclosure before publishing details. We will review reports on a best-effort basis; acknowledgement, remediation, and disclosure dates cannot be guaranteed. When appropriate, we may coordinate a fix and a GitHub security advisory with the reporter.

## Scope

Examples of potentially security-relevant issues include memory unsafety in the compiler or interpreter, malformed input causing unintended code execution during compilation, and verifier or code-generation bugs that turn verified IR into unintended undefined behavior or bypass intended runtime checks. An unsupported Go construct, an ordinary diagnostic mismatch, or a program intentionally compiled and run by the user is not automatically a vulnerability; report ordinary bugs through [issues](https://github.com/hazuki-keatsu/gane/issues) instead. When in doubt about whether sharing details would be dangerous, use your judgment and omit sensitive details from a public report.
