use crate::machine::Interpreter;
use gane_ir::VerifiedIrPackage;

pub use crate::errors::InterpreterError;

/// Executes a verified V0 IR package from `gane.main`.
pub fn interpret(package: &VerifiedIrPackage) -> Result<(), InterpreterError> {
    Interpreter::new(package, false).run()
}

/// Executes a verified V0 IR package and includes the active stack in trap errors.
pub fn interpret_with_stack_details(package: &VerifiedIrPackage) -> Result<(), InterpreterError> {
    Interpreter::new(package, true).run()
}
