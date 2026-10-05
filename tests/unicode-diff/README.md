# Go/Gane Unicode classification diff

This helper compares the Unicode predicates used by Gane's scanner with the Go standard library reference predicates:

- Go: `unicode.IsLetter`, `unicode.IsDigit`, and `unicode.IsPrint`.
- Gane: the same category rules implemented with `unicode-general-category`, as in `crates/parser/src/scanner/scanner_impl.rs`.

It checks every Unicode scalar value, prints compressed code-point ranges for differences, and probes the public Gane parser with representative mismatching identifiers. Ordinary output reports differences but exits successfully; `--check-equal` makes any difference a failing compatibility check.

## Run

```sh
./tests/unicode-diff/compare.sh
```

Requirements: Go 1.27.x, Rust/Cargo, and network access on the first Cargo run if the pinned crate is not cached. The script also checks that the Go Unicode tables are 17.0.0. Set `GO=/path/to/go1.27.0` to select a particular Go executable; the installed Go patch version is printed in the report.

The Rust helper pins `unicode-general-category` to 1.1.0, matching the version currently used by the workspace. Update that pin if the workspace dependency changes.

To use the comparison as a strict compatibility assertion:

```sh
./tests/unicode-diff/compare.sh --check-equal
```

That command currently exits nonzero because Go 1.27.x uses Unicode 17.0.0 and the pinned Rust crate uses Unicode 16.0.0; the reported differences are expected until the data tables are aligned.
