use crate::machine::Interpreter;
use gane_ir::VerifiedIrPackage;

pub use crate::errors::InterpreterError;

/// Executes a verified V0 IR package from `gane.main`.
pub fn interpret(package: &VerifiedIrPackage) -> Result<(), InterpreterError> {
    Interpreter::new(package).run()
}
