# Repository Guidelines

## Project Structure & Module Organization

Gane is a Rust Cargo workspace. Each compiler layer has its own crate under
`crates/`: `parser` builds Go-like ASTs, `diagnostics` owns spans and rendered
diagnostics, `sema` performs package analysis, `hir` lowers AST plus semantic
facts into verified HIR, `codegen` translates verified HIR into LLVM IR, and
`driver` is the CLI. Design notes live in `docs/`. Tests sit beside source or in
`crates/*/tests/`; parser fixtures are in `crates/parser/src/parser/testdata/`.

## Build, Test, and Development Commands

- `cargo check --workspace` — fast type-check of every crate.
- `cargo test --workspace` — run unit, integration, and doctests.
- `cargo test -p gane_sema` — focus on one crate while iterating.
- `cargo run -p gane_driver -- path/to/file.go [out-dir]` — parse and semantically
  check a source file, writing AST/sema diagnostic dumps.
- `cargo fmt --check` — verify Rust formatting.

## Coding Style & Naming Conventions

Use Rust 2024 and standard `rustfmt` formatting (four-space indentation). Follow
Rust naming: `UpperCamelCase` for types and enum variants, `snake_case` for
functions, modules, variables, and test names, and `SCREAMING_SNAKE_CASE` for
constants. Keep dependencies one-way: parser → sema → HIR → codegen. HIR
lowering must not redo semantic lookup or inference. Prefer typed arena IDs and
preserve source `Span` data in diagnostics and IR-facing structures.

## Agent Workflow and Change Authorization

Before starting a task, read `PROGRESS.md` to establish the current milestone,
then read the relevant design documents in `docs/` (for example,
`docs/hir-design.md` for HIR work). Inspect the affected crate only after that.
Treat existing worktree changes as user-owned.

Write or modify source code only when the user explicitly requests implementation
or a code change. For questions, reviews, planning, and diagnosis, inspect and
report findings without editing code. Never create a commit, amend a commit, or
stage changes unless the user explicitly asks after reviewing the work.

## Testing Guidelines

Add focused regression tests named by observable behavior, e.g.
`rejects_function_declarations_without_a_body`. Include negative tests for
unsupported syntax and invalid IR. Pair new lowering work with source-to-HIR
goldens and verifier tests.

## Commit & Pull Request Guidelines

When the user asks for a commit after review, use Conventional Commit-style
subjects already used in this repository, such as
`feat(sema): reject bodyless function declarations`, `fix(parser): ...`, or
`doc: ...`. Keep commits narrow. Pull requests should state the affected layer,
behavior and diagnostics changes, linked issues, and commands run.
