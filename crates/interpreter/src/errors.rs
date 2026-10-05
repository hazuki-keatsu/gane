// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Hazuki Keatsu

use gane_ir::TrapReason;
use std::{error::Error, fmt};

/// An observable result of executing a verified IR package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InterpreterError {
    Trap {
        reason: TrapReason,
        trace: Option<String>,
    },
    ReachedUnreachable {
        trace: Option<String>,
    },
    DanglingStackPointer {
        trace: Option<String>,
    },
}

impl InterpreterError {
    pub(crate) fn trap(reason: TrapReason) -> Self {
        Self::Trap {
            reason,
            trace: None,
        }
    }

    pub(crate) fn dangling_stack_pointer() -> Self {
        Self::DanglingStackPointer { trace: None }
    }

    pub(crate) fn attach_trace_with(&mut self, make_trace: impl FnOnce() -> String) {
        let trace = match self {
            Self::Trap { trace, .. }
            | Self::ReachedUnreachable { trace }
            | Self::DanglingStackPointer { trace } => trace,
        };
        trace.get_or_insert_with(make_trace);
    }
}

impl fmt::Display for InterpreterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let trace = match self {
            Self::Trap { reason, trace } => {
                write!(formatter, "IR trapped: {reason:?}")?;
                trace
            }
            Self::ReachedUnreachable { trace } => {
                formatter.write_str("reached IR unreachable")?;
                trace
            }
            Self::DanglingStackPointer { trace } => {
                formatter.write_str("dereferenced dangling stack pointer")?;
                trace
            }
        };
        if let Some(trace) = trace {
            write!(formatter, "\n{}", trace.trim_end())?;
        }
        Ok(())
    }
}

impl Error for InterpreterError {}
