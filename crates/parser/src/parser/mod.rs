//! The parser for Go source files.
//!
//! This module is ported from Go's standard `go/parser` package
//! (`parser.go` and `interface.go`), adapted to Rust conventions and to the
//! trimming decisions of this port:
//!
//! - comments are discarded: the scanner is run without `SCAN_COMMENTS`, and
//!   parser.go's comment collection (`ParseComments`, comment groups, `Doc`
//!   and `Comment` fields, `expectSemi`'s line comment) plus the `//go:build`
//!   minimum-version sniffing (`File.GoVersion`) are not ported;
//! - deprecated identifier resolution (`go/parser/resolver.go`) is ported
//!   and runs by default at the end of [`parser::parse_file`], exactly like
//!   Go; the `SKIP_OBJECT_RESOLUTION` mode bit turns it off;
//! - Go's panic-based parse bailout is implemented with `panic!` +
//!   `std::panic::catch_unwind` at the entry points;
//! - the tracing infrastructure (`Trace` mode) is not ported;
//! - the recursion depth guard is lowered from Go's `maxNestLev` 1e5 to
//!   1e3 (Go goroutine stacks grow; Rust thread stacks do not).

mod interface;
mod parser;
mod resolver;

pub use interface::*;
