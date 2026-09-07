use std::{fmt, vec};

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
        Error {
            pos,
            msg: msg.into(),
        }
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

/// ErrorList is a list of errors.
///
/// Usage:
/// ```rust
/// list.iter() // to get immutable iterator
/// list.iter_mut() // to get mutable iterator
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ErrorList(Vec<Error>);

impl IntoIterator for ErrorList {
    type Item = Error;
    type IntoIter = vec::IntoIter<Error>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a ErrorList {
    type Item = &'a Error;
    type IntoIter = std::slice::Iter<'a, Error>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a> IntoIterator for &'a mut ErrorList {
    type Item = &'a mut Error;
    type IntoIter = std::slice::IterMut<'a, Error>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}

impl FromIterator<Error> for ErrorList {
    fn from_iter<T: IntoIterator<Item = Error>>(iter: T) -> Self {
        ErrorList(iter.into_iter().collect())
    }
}

// iter() and iter_mut()
impl ErrorList {
    pub fn iter(&self) -> std::slice::Iter<'_, Error> {
        self.0.iter()
    }

    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, Error> {
        self.0.iter_mut()
    }
}

impl ErrorList {
    /// Add adds an error with given position and error message to an ErrorList.
    pub fn add(&mut self, pos: Position, msg: impl Into<String>) {
        self.0.push(Error::new(pos, msg));
    }

    /// Reset resets an ErrorList to no errors.
    pub fn reset(&mut self) {
        self.0.clear();
    }

    /// Returns the number of errors in the list.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns true if the list contains no errors.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Sort sorts an ErrorList. Error entries are sorted by position
    /// (filename, line, column), and then by error message.
    pub fn sort(&mut self) {
        self.0.sort_by(|a, b| less(&a.pos, &a.msg, &b.pos, &b.msg));
    }

    /// RemoveMultiples sorts an ErrorList and removes all but the first
    /// error per line.
    pub fn remove_multiples(&mut self) {
        self.sort();
        let mut last_file = String::new();
        let mut last_line = 0;
        self.0.retain(|e| {
            let keep = e.pos.file_name != last_file || e.pos.line != last_line;
            if keep {
                last_file = e.pos.file_name.clone();
                last_line = e.pos.line;
            }
            keep
        });
    }

    /// Err returns an error equivalent to this error list.
    /// If the list is empty, Err returns None.
    pub fn err(&self) -> Option<&Self> {
        (!self.is_empty()).then_some(self)
    }
}

/// Less compares two errors by position, then by message.
///
/// Note that it is not sufficient to simply compare file offsets because
/// the offsets do not reflect modified line information (through //line
/// comments).
fn less(a_pos: &Position, a_msg: &str, b_pos: &Position, b_msg: &str) -> std::cmp::Ordering {
    a_pos
        .file_name
        .cmp(&b_pos.file_name)
        .then(a_pos.line.cmp(&b_pos.line))
        .then(a_pos.column.cmp(&b_pos.column))
        .then(a_msg.cmp(b_msg))
}

impl fmt::Display for ErrorList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.len() {
            0 => f.write_str("no errors"),
            1 => write!(f, "{}", self.0[0]),
            n => write!(f, "{} (and {} more errors)", self.0[0], n - 1),
        }
    }
}

impl std::error::Error for ErrorList {}

// /// PrintError is a utility function that prints a list of errors to `w`,
// /// one error per line, if the `err` parameter is an [ErrorList]. Otherwise
// /// it prints the err string.
// pub fn print_error<W: fmt::Write>(w: &mut W, err: &(dyn std::error::Error + 'static)) {
//     if let Some(list) = err.downcast_ref::<ErrorList>() {
//         for e in &list.0 {
//             let _ = writeln!(w, "{}", e);
//         }
//     } else {
//         let _ = writeln!(w, "{}", err);
//     }
// }
