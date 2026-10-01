use std::{error::Error, fmt};

use gane_ir::{TargetSpec, VerifiedIrPackage};
use inkwell::targets::{TargetData, TargetMachine};

pub struct LlvmBackend {
    pub(crate) _machine: TargetMachine,
    pub(crate) data: TargetData,
    pub(crate) target: TargetSpec,
}

impl LlvmBackend {
    pub fn for_host() -> Result<Self, CodegenError> {
        crate::target::for_host()
    }

    pub fn target_spec(&self) -> &TargetSpec {
        &self.target
    }

    pub fn emit_llvm_ir(&self, package: &VerifiedIrPackage) -> Result<String, CodegenError> {
        if package.target() != &self.target {
            return Err(CodegenError::TargetMismatch {
                package: Box::new(package.target().clone()),
                backend: Box::new(self.target.clone()),
            });
        }
        crate::lower::emit(self, package)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodegenError {
    TargetInitialization(String),
    TargetMismatch {
        package: Box<TargetSpec>,
        backend: Box<TargetSpec>,
    },
    Lowering(String),
    InvalidModule(String),
}

impl fmt::Display for CodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TargetInitialization(message) => {
                write!(f, "LLVM target initialization failed: {message}")
            }
            Self::TargetMismatch { package, backend } => write!(
                f,
                "IR target does not match LLVM backend (package: {}, backend: {})",
                package.triple(),
                backend.triple()
            ),
            Self::Lowering(message) => write!(f, "LLVM lowering failed: {message}"),
            Self::InvalidModule(message) => write!(f, "invalid LLVM module: {message}"),
        }
    }
}

impl Error for CodegenError {}
