// SPDX-License-Identifier: BSD-3-Clause
// SPDX-FileCopyrightText: 2009 The Go Authors.
// SPDX-FileCopyrightText: 2026 Hazuki Keatsu
//
// Adapted from the Go standard library for Gane.

pub mod ast;
pub mod parser;
mod scanner;
pub mod token;

pub use scanner::{Error, ErrorList};
