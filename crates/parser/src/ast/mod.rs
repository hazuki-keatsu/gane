// SPDX-License-Identifier: BSD-3-Clause
// SPDX-FileCopyrightText: 2009 The Go Authors.
// SPDX-FileCopyrightText: 2026 Hazuki Keatsu
//
// Adapted from the Go standard library for Gane.

pub mod directive;
pub mod types;
pub mod walk;

mod strconv;

pub use directive::*;
pub use types::*;
pub use walk::*;
