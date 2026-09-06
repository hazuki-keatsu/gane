//! The parser for Go source files.
//!
//! This module is ported from Go's standard `go/parser` package
//! (`parser.go` and `interface.go`), adapted to Rust conventions and to the
//! trimming decisions of this port:
//!
//! - ordinary comments are discarded. The parser retains only leading
//!   `//go:` and `//gane:` compiler command comments on their target AST node;
//! - Go's panic-based parse bailout is implemented with `panic!` +
//!   `std::panic::catch_unwind` at the entry points;
//! - the tracing infrastructure (`Trace` mode) is not ported;
//! - the recursion depth guard is lowered from Go's `maxNestLev` 1e5 to
//!   1e3 (Go goroutine stacks grow; Rust thread stacks do not).

mod interface;
mod parser;

pub use interface::*;
