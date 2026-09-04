use std::fmt;

use crate::token::Position;

/// A scanner error, together with the position at which it occurred.
///
/// This module is ported from Go's standard `go/scanner` package
/// ([errors.go](https://cs.opensource.google/go/go/+/master:src/go/scanner/errors.go)).
#[derive(Clone, Debug, PartialEq)]
pub struct Error {
    pub pos: Position,
    pub msg: String,
}

impl Error {
    /// Creates a new scanner error at the given position with the given message.
    pub fn new(pos: Position, msg: impl Into<String>) -> Error {
        Error { pos, msg: msg.into() }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.pos.is_valid() || !self.pos.file_name.is_empty() {
            write!(f, "{}: {}", self.pos, self.msg)
        } else {
            write!(f, "{}", self.msg)
        }
    }
}

impl std::error::Error for Error {}
