// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Hazuki Keatsu

use gane_ir::VerifiedIrPackage;
use inkwell::context::Context;

use crate::{CodegenError, LlvmBackend, lower::module::ModuleLowerer};

mod function;
mod helper;
mod module;

#[cfg(test)]
mod tests;

pub(crate) fn emit(
    backend: &LlvmBackend,
    package: &VerifiedIrPackage,
) -> Result<String, CodegenError> {
    let context = Context::create();
    let mut lowerer = ModuleLowerer::new(&context, backend, package);
    lowerer.lower()?;
    lowerer
        .module
        .verify()
        .map_err(|error| CodegenError::InvalidModule(error.to_string()))?;
    Ok(lowerer.module.print_to_string().to_string())
}
