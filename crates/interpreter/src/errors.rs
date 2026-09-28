use gane_ir::TrapReason;
use std::{error::Error, fmt};

/// An observable result of executing a verified IR package.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterpreterError {
    Trap(TrapReason),
    ReachedUnreachable,
}

impl fmt::Display for InterpreterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Trap(reason) => write!(formatter, "IR trapped: {reason:?}"),
            Self::ReachedUnreachable => formatter.write_str("reached IR unreachable"),
        }
    }
}

impl Error for InterpreterError {}
