// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Hazuki Keatsu

use crate::{
    CodegenError, LlvmBackend,
    lower::{
        function::FunctionLowerer,
        helper::{builder_error, id_to_index},
    },
};
use gane_ir::{Constant, GlobalInitializer, IrFunction, IrTypeKind, TypeId, VerifiedIrPackage};
use inkwell::{
    AddressSpace,
    attributes::{Attribute, AttributeLoc},
    builder::Builder,
    context::Context,
    intrinsics::Intrinsic,
    module::{Linkage, Module},
    targets::TargetTriple,
    types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType, IntType},
    values::{BasicValueEnum, FunctionValue, GlobalValue},
};

pub(crate) struct ModuleLowerer<'ctx, 'pkg> {
    /// The object factory and life circle root of LLVM
    context: &'ctx Context,
    /// The target for building
    backend: &'ctx LlvmBackend,
    /// The gane ir inputted
    package: &'pkg VerifiedIrPackage,
    /// The LLVM Module in building
    pub(crate) module: Module<'ctx>,
    /// LLVM command builder
    builder: Builder<'ctx>,
    /// The cache for recording the mapping from [`TypeId`](gane_ir::TypeId) to LLVM [`BasicType`].
    /// The reason of using [`Option`] is that [`Void`](gane_ir::IrTypeKind::Void) is not the basic type in
    /// LLVM, and it must be creating opaque struct first and filling the content nextly when creating recursive struct.
    types: Vec<Option<BasicTypeEnum<'ctx>>>,
    /// The mapping from [`GlobalId`](gane_ir::GlobalId) to LLVM [`GlobalValue`].
    /// When processing `GlobalAddr { global }`, the result can be get immediately.
    globals: Vec<GlobalValue<'ctx>>,
    /// The mapping from [`FunctionId`](gane_ir::FunctionId) to LLVM [`FunctionValue`].
    functions: Vec<FunctionValue<'ctx>>,
    /// The helper function in Gane `__gane_trap`.
    ///
    /// When the guard failure like null, bound crossing, being divided by zero and so on occurs, it will be called.
    /// The reason why it is set to `Option` is when [`ModuleLowerer::new()`] was called, it had been declared,
    /// and after [`ModuleLowerer::declare_helpers()`], it will be set to `Some(..)`.
    trap: Option<FunctionValue<'ctx>>,
    /// The built-in llvm.trap intrinsic. `__gane_trap` will call it, then unreachable.
    llvm_trap: Option<FunctionValue<'ctx>>,
}

impl<'ctx, 'pkg> ModuleLowerer<'ctx, 'pkg> {
    pub(crate) fn new(
        context: &'ctx Context,
        backend: &'ctx LlvmBackend,
        package: &'pkg VerifiedIrPackage,
    ) -> Self {
        let module = context.create_module("gane");
        module.set_triple(&TargetTriple::create(backend.target.triple()));
        module.set_data_layout(&backend.data.get_data_layout());
        Self {
            context,
            backend,
            package,
            module,
            builder: context.create_builder(),
            types: Vec::new(),
            globals: Vec::new(),
            functions: Vec::new(),
            trap: None,
            llvm_trap: None,
        }
    }

    pub(crate) fn lower(&mut self) -> Result<(), CodegenError> {
        // Check if or not there is reserved symbol name in the global
        self.check_symbols()?;
        // Lower the type in Gane Ir to the llvm basic type
        self.lower_types()?;
        self.declare_globals()?;
        self.declare_functions()?;
        self.declare_helpers()?;
        self.define_trap_helper()?;
        for index in 0..self.functions.len() {
            self.lower_function(index)?;
        }
        self.define_host_main()?;
        Ok(())
    }

    fn check_symbols(&self) -> Result<(), CodegenError> {
        for (_, global) in self.package.globals() {
            self.check_symbol(&global.symbol)?;
        }
        for (_, function) in self.package.functions() {
            self.check_symbol(&function.symbol)?;
        }
        Ok(())
    }

    fn check_symbol(&self, symbol: &str) -> Result<(), CodegenError> {
        if matches!(symbol, "main" | "__gane_trap") || symbol.starts_with("llvm.") {
            return Err(CodegenError::Lowering(format!(
                "package symbol `{symbol}` is reserved by codegen"
            )));
        }
        Ok(())
    }

    fn lower_types(&mut self) -> Result<(), CodegenError> {
        self.types = vec![None; self.package.types().iter().count()];
        for (id, typ) in self.package.types().iter() {
            if matches!(typ.kind, IrTypeKind::Struct { .. }) {
                self.types[id_to_index(id)] = Some(
                    self.context
                        .opaque_struct_type(&format!("gane.type.{}", id.raw()))
                        .into(),
                );
            }
        }
        let ids = self
            .package
            .types()
            .iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        for id in ids {
            if id != self.package.types().void() {
                self.basic_type(id)?;
            }
        }
        for (id, typ) in self.package.types().iter() {
            if let IrTypeKind::Struct { fields } = &typ.kind {
                let fields = fields
                    .iter()
                    .map(|field| self.basic_type(*field))
                    .collect::<Result<Vec<_>, _>>()?;
                self.types[id_to_index(id)]
                    .expect("all structs were predeclared")
                    .into_struct_type()
                    .set_body(&fields, false);
            }
        }
        Ok(())
    }

    /// Convert the Gane Ir type to LLVM basic type by [`TypeId`].
    /// Side-effect is [`ModuleLowerer::types`] will be filled.
    fn basic_type(&mut self, id: TypeId) -> Result<BasicTypeEnum<'ctx>, CodegenError> {
        if let Some(typ) = self.types.get(id_to_index(id)).and_then(|typ| *typ) {
            return Ok(typ);
        }
        let kind = self
            .package
            .types()
            .get(id)
            .ok_or_else(|| CodegenError::Lowering(format!("invalid type {}", id.raw())))?
            .kind
            .clone();
        let typ = match kind {
            IrTypeKind::Void => {
                return Err(CodegenError::Lowering(
                    "void is not a basic LLVM type".into(),
                ));
            }
            IrTypeKind::I1 => self.context.bool_type().into(),
            IrTypeKind::I8 => self.context.i8_type().into(),
            IrTypeKind::I16 => self.context.i16_type().into(),
            IrTypeKind::I32 => self.context.i32_type().into(),
            IrTypeKind::I64 => self.context.i64_type().into(),
            IrTypeKind::Ptr { address_space, .. } => self
                .context
                .ptr_type(AddressSpace::from(u16::try_from(address_space).map_err(
                    |_| CodegenError::Lowering(format!("invalid address space {address_space}")),
                )?))
                .into(),
            IrTypeKind::Array { length, element } => self
                .basic_type(element)?
                .array_type(u32::try_from(length).map_err(|_| {
                    CodegenError::Lowering(format!(
                        "array length {length} exceeds Inkwell's LLVM array API"
                    ))
                })?)
                .into(),
            IrTypeKind::Struct { .. } => unreachable!("structs are predeclared"),
        };
        self.types[id_to_index(id)] = Some(typ);
        Ok(typ)
    }

    /// Convert the global declaration in Gane Ir to the LLVM global declaration.
    /// Side-effect is [ModuleLowerer::globals] will be modified.
    fn declare_globals(&mut self) -> Result<(), CodegenError> {
        for (_, global) in self.package.globals() {
            let typ = self.basic_type(global.typ)?;
            let value = self.module.add_global(typ, None, &global.symbol);
            // Make the global variable only can be seen in the module rather than the go's package.
            value.set_linkage(Linkage::Internal);
            // Initialize the global variable
            value.set_initializer(&self.global_initializer(global.typ, &global.initializer)?);
            // Push `GlobalValue` into the mapping from `GlobalId` to `GlobalValue`.
            self.globals.push(value);
        }
        Ok(())
    }

    fn global_initializer(
        &mut self,
        typ: TypeId,
        initializer: &GlobalInitializer,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        match initializer {
            GlobalInitializer::Zero => Ok(self.basic_type(typ)?.const_zero()),
            GlobalInitializer::Scalar(value) => self.constant(typ, *value),
        }
    }

    fn constant(
        &mut self,
        typ: TypeId,
        value: Constant,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        match value {
            Constant::Bool(value) => Ok(self
                .context
                .bool_type()
                .const_int(value as u64, false)
                .into()),
            Constant::Integer(value) => Ok(self.int_type(typ)?.const_int(value, false).into()),
            Constant::Null => Ok(self
                .basic_type(typ)?
                .into_pointer_type()
                .const_null()
                .into()),
        }
    }

    fn declare_functions(&mut self) -> Result<(), CodegenError> {
        for (_, function) in self.package.functions() {
            let typ = self.function_type(function)?;
            let value = self
                .module
                .add_function(&function.symbol, typ, Some(Linkage::Internal));
            self.add_target_attributes(value);
            if function.attributes.no_return {
                self.add_enum_attribute(value, "noreturn");
            }
            self.functions.push(value);
        }
        Ok(())
    }

    fn function_type(&mut self, function: &IrFunction) -> Result<FunctionType<'ctx>, CodegenError> {
        let parameters = function
            .signature
            .parameters
            .iter()
            .map(|parameter| self.basic_type(parameter.typ).map(Into::into))
            .collect::<Result<Vec<BasicMetadataTypeEnum<'ctx>>, _>>()?;
        let result = function
            .signature
            .results
            .first()
            .copied()
            .unwrap_or(self.package.types().void());
        if result == self.package.types().void() {
            return Ok(self.context.void_type().fn_type(&parameters, false));
        }
        match self.basic_type(result)? {
            BasicTypeEnum::IntType(typ) => Ok(typ.fn_type(&parameters, false)),
            BasicTypeEnum::PointerType(typ) => Ok(typ.fn_type(&parameters, false)),
            _ => Err(CodegenError::Lowering(
                "function result is not scalar".into(),
            )),
        }
    }

    fn declare_helpers(&mut self) -> Result<(), CodegenError> {
        let trap = self.module.add_function(
            "__gane_trap",
            self.context
                .void_type()
                .fn_type(&[self.context.i32_type().into()], false),
            Some(Linkage::Internal),
        );
        self.add_enum_attribute(trap, "cold");
        self.add_enum_attribute(trap, "noreturn");
        self.trap = Some(trap);
        self.llvm_trap = Intrinsic::find("llvm.trap")
            .and_then(|intrinsic| intrinsic.get_declaration(&self.module, &[]));
        if self.llvm_trap.is_none() {
            return Err(CodegenError::Lowering(
                "LLVM does not provide llvm.trap".into(),
            ));
        }
        let pointer = self.context.ptr_type(AddressSpace::default()).into();
        let length = self
            .context
            .ptr_sized_int_type(&self.backend.data, None)
            .into();
        if Intrinsic::find("llvm.memmove")
            .and_then(|intrinsic| {
                intrinsic.get_declaration(&self.module, &[pointer, pointer, length])
            })
            .is_none()
        {
            return Err(CodegenError::Lowering(
                "LLVM does not provide llvm.memmove".into(),
            ));
        }
        Ok(())
    }

    fn define_trap_helper(&self) -> Result<(), CodegenError> {
        let trap = self.trap.expect("trap helper is declared");
        let block = self.context.append_basic_block(trap, "entry");
        self.builder.position_at_end(block);
        self.builder
            .build_call(self.llvm_trap.expect("llvm.trap is declared"), &[], "")
            .map_err(builder_error)?;
        self.builder.build_unreachable().map_err(builder_error)?;
        Ok(())
    }

    fn lower_function(&self, index: usize) -> Result<(), CodegenError> {
        let (_, function) = self
            .package
            .functions()
            .nth(index)
            .expect("function index is valid");
        FunctionLowerer::new(
            self.context,
            self.backend,
            self.package,
            &self.builder,
            &self.types,
            &self.globals,
            &self.functions,
            self.trap.expect("trap helper is declared"),
            self.functions[index],
            function,
        )
        .lower()
    }

    fn define_host_main(&self) -> Result<(), CodegenError> {
        // Find out the entry function of the module
        let entry = self.functions[id_to_index(self.package.entry())];
        // All the functions in Gane Ir are started with "gane.", so naming the entry as "main" is OK.
        // That `linkage` was set to `None` means using default linkage -- normally it's external -- which
        // can let operating system or c runtime find it out.
        let main =
            self.module
                .add_function("main", self.context.i32_type().fn_type(&[], false), None);
        self.add_target_attributes(main);
        let block = self.context.append_basic_block(main, "entry");
        self.builder.position_at_end(block);
        self.builder
            .build_call(entry, &[], "")
            .map_err(builder_error)?;
        let attributes = self
            .package
            .function(self.package.entry())
            .expect("verified entry function")
            .attributes;
        if attributes.no_return {
            self.builder.build_unreachable().map_err(builder_error)?;
        } else {
            self.builder
                .build_return(Some(&self.context.i32_type().const_zero()))
                .map_err(builder_error)?;
        }
        Ok(())
    }

    fn add_target_attributes(&self, function: FunctionValue<'ctx>) {
        function.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("target-cpu", self.backend.target.cpu()),
        );
        function.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("target-features", self.backend.target.features()),
        );
    }

    fn add_enum_attribute(&self, function: FunctionValue<'ctx>, name: &str) {
        function.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_enum_attribute(Attribute::get_named_enum_kind_id(name), 0),
        );
    }

    fn int_type(&mut self, typ: TypeId) -> Result<IntType<'ctx>, CodegenError> {
        match self.basic_type(typ)? {
            BasicTypeEnum::IntType(typ) => Ok(typ),
            _ => Err(CodegenError::Lowering("expected an integer type".into())),
        }
    }
}
