# Contributing to Gane

Thank you for your interest in Gane. 

This project is still in its early stages, so before you plan to make any kind of contribution, please express your ideas in the issue first. This will reduce the risk of being rejected.

This is an experimental Go-like compiler, not a complete Go implementation. Before proposing language support, read the [README](../README.md) and the [M0 language support document](../docs/v0/language-support.md): syntax accepted by the parser may still be rejected by semantic analysis or IR lowering.

## Before you start

- For bugs, open an issue with a minimal `.go` reproducer, the command you ran, expected and actual behavior, diagnostics, and your Rust/LLVM versions and operating system when relevant. Say whether the failure occurs in parsing, semantic analysis, IR lowering/verification, interpretation, or code generation.
- For larger changes or new language features, open an issue first to discuss scope and design.
- Consult the relevant design notes in [`docs/`](../docs/) before changing a compiler layer. Keep proposals within the current subset unless the scope has been agreed on.

## Set up and test

You need a Rust toolchain and LLVM **22.1.x**, including `bin/llvm-config` and LLVM development libraries. Set `LLVM_SYS_221_PREFIX` to the LLVM installation prefix before building. For example, on macOS:

```sh
brew install llvm@22
export LLVM_SYS_221_PREFIX="$(brew --prefix llvm@22)"
```

See the [README quick start](../README.md#quick-start) for a sample input and driver usage. To run the pipeline on your own single-file program:

```sh
cargo run -p gane_driver -- single path/to/file.go --out-dir out
```

The driver writes AST, sema, Gane IR, and LLVM IR dumps and runs the IR interpreter. It does not link an executable. From the repository root, run the same checks as CI:

```sh
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

During development, use focused tests such as `cargo test -p gane_sema`; run the full checks before requesting review, or explain which checks you could not run.

## Make a focused change

- Keep changes within the appropriate layer: `parser` builds ASTs, `sema` records semantic facts, `ir` lowers and verifies them, `interpreter` executes verified IR, `codegen` produces LLVM IR, and `driver` exposes the CLI. IR lowering should use semantic facts rather than repeating name lookup or type inference.
- Follow Rust 2024 conventions and `rustfmt`. Preserve source spans in diagnostics and IR-facing structures. Where applicable, expose crate APIs through `src/interface.rs`; keep `lib.rs` for module declarations and re-exports.
- Add a regression test named for the observable behavior. Include rejection tests for unsupported or invalid inputs. For new lowering, include source-to-IR coverage and verifier tests; check both interpreter and LLVM paths where the change affects them.
- Update the language support document or relevant design notes when behavior or milestone status changes. Do not claim end-to-end support based on parsing alone.

## Licensing and third-party code

Gane's original code is licensed under [Apache-2.0](../LICENSE). The parser crate contains adaptations of Go standard library code under Go's BSD-style license; see the [NOTICE](../NOTICE) for attribution and the full license text. Only submit material you have the right to contribute under the applicable license. Preserve existing copyright and license headers, and identify any new third-party source or test fixtures in your pull request so their attribution can be reviewed. Do not relabel Go-derived code as Apache-only.

## Pull requests

Keep pull requests narrow and explain the affected compiler layer, user-visible behavior and diagnostic changes, and tests run. Link relevant issues and include a minimal before/after example when useful. If you cannot run a platform or LLVM-dependent check, mention that explicitly. Use descriptive commit subjects; the repository uses Conventional Commit-style subjects such as `fix(parser): ...` and `feat(sema): ...`.
