use gane_ir::{
    BinaryOp, BlockId, Callee, ComparePredicate, Constant, FunctionId, GlobalId, GlobalInitializer,
    Instruction, InstructionKind, IntCastKind, IrFunction, IrTypeKind, StackSlotId, Terminator,
    TrapReason, TypeId, UnaryOp, ValueId, VerifiedIrPackage,
};
use inkwell::{
    AddressSpace, IntPredicate,
    attributes::{Attribute, AttributeLoc},
    basic_block::BasicBlock,
    builder::Builder,
    context::Context,
    intrinsics::Intrinsic,
    module::{Linkage, Module},
    targets::TargetTriple,
    types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType, IntType},
    values::{
        BasicMetadataValueEnum, BasicValue, BasicValueEnum, FunctionValue, GlobalValue, PhiValue,
        PointerValue,
    },
};

use crate::{CodegenError, LlvmBackend};

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

struct ModuleLowerer<'ctx, 'pkg> {
    context: &'ctx Context,
    backend: &'ctx LlvmBackend,
    package: &'pkg VerifiedIrPackage,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    types: Vec<Option<BasicTypeEnum<'ctx>>>,
    globals: Vec<GlobalValue<'ctx>>,
    functions: Vec<FunctionValue<'ctx>>,
    trap: Option<FunctionValue<'ctx>>,
    llvm_trap: Option<FunctionValue<'ctx>>,
}

impl<'ctx, 'pkg> ModuleLowerer<'ctx, 'pkg> {
    fn new(
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

    fn lower(&mut self) -> Result<(), CodegenError> {
        self.check_symbols()?;
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
                self.types[type_index(id)] = Some(
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
                self.types[type_index(id)]
                    .expect("all structs were predeclared")
                    .into_struct_type()
                    .set_body(&fields, false);
            }
        }
        Ok(())
    }

    fn basic_type(&mut self, id: TypeId) -> Result<BasicTypeEnum<'ctx>, CodegenError> {
        if let Some(typ) = self.types.get(type_index(id)).and_then(|typ| *typ) {
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
        self.types[type_index(id)] = Some(typ);
        Ok(typ)
    }

    fn declare_globals(&mut self) -> Result<(), CodegenError> {
        for (_, global) in self.package.globals() {
            let typ = self.basic_type(global.typ)?;
            let value = self.module.add_global(typ, None, &global.symbol);
            value.set_linkage(Linkage::Internal);
            value.set_initializer(&self.global_initializer(global.typ, &global.initializer)?);
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
        let entry = self.functions[type_index(self.package.entry())];
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

struct FunctionLowerer<'ctx, 'pkg> {
    context: &'ctx Context,
    backend: &'ctx LlvmBackend,
    package: &'pkg VerifiedIrPackage,
    builder: &'ctx Builder<'ctx>,
    types: &'ctx [Option<BasicTypeEnum<'ctx>>],
    globals: &'ctx [GlobalValue<'ctx>],
    functions: &'ctx [FunctionValue<'ctx>],
    trap: FunctionValue<'ctx>,
    llvm: FunctionValue<'ctx>,
    function: &'pkg IrFunction,
    values: Vec<Option<BasicValueEnum<'ctx>>>,
    slots: Vec<PointerValue<'ctx>>,
    blocks: Vec<BasicBlock<'ctx>>,
    phis: Vec<Option<PhiValue<'ctx>>>,
}

impl<'ctx, 'pkg> FunctionLowerer<'ctx, 'pkg> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        context: &'ctx Context,
        backend: &'ctx LlvmBackend,
        package: &'pkg VerifiedIrPackage,
        builder: &'ctx Builder<'ctx>,
        types: &'ctx [Option<BasicTypeEnum<'ctx>>],
        globals: &'ctx [GlobalValue<'ctx>],
        functions: &'ctx [FunctionValue<'ctx>],
        trap: FunctionValue<'ctx>,
        llvm: FunctionValue<'ctx>,
        function: &'pkg IrFunction,
    ) -> Self {
        let prologue = context.append_basic_block(llvm, "prologue");
        let mut blocks = vec![prologue];
        for index in 0..function.blocks.len() {
            blocks.push(context.append_basic_block(llvm, &format!("b{}", index + 1)));
        }
        Self {
            context,
            backend,
            package,
            builder,
            types,
            globals,
            functions,
            trap,
            llvm,
            function,
            values: vec![None; function.values.len()],
            slots: Vec::new(),
            blocks,
            phis: vec![None; function.values.len()],
        }
    }

    fn lower(mut self) -> Result<(), CodegenError> {
        self.bind_entry_parameters();
        self.create_phis()?;
        self.lower_prologue()?;
        for index in 0..self.function.blocks.len() {
            self.builder.position_at_end(self.blocks[index + 1]);
            let block = &self.function.blocks[index];
            for instruction in &block.instructions {
                self.lower_instruction(instruction)?;
            }
            self.lower_terminator(&block.terminator)?;
        }
        Ok(())
    }

    fn bind_entry_parameters(&mut self) {
        let entry = self
            .function
            .block(self.function.entry)
            .expect("verified entry block");
        for (index, value) in entry.parameters.iter().enumerate() {
            let parameter = self
                .llvm
                .get_nth_param(index as u32)
                .expect("verified parameter count");
            parameter.set_name(&value_name(*value));
            self.values[type_index(*value)] = Some(parameter);
        }
    }

    fn create_phis(&mut self) -> Result<(), CodegenError> {
        for (index, block) in self.function.blocks.iter().enumerate() {
            if index == type_index(self.function.entry) {
                continue;
            }
            self.builder.position_at_end(self.blocks[index + 1]);
            for parameter in &block.parameters {
                let phi = self
                    .builder
                    .build_phi(
                        self.basic_type(self.value_type(*parameter))?,
                        &value_name(*parameter),
                    )
                    .map_err(builder_error)?;
                self.values[type_index(*parameter)] = Some(phi.as_basic_value());
                self.phis[type_index(*parameter)] = Some(phi);
            }
        }
        Ok(())
    }

    fn lower_prologue(&mut self) -> Result<(), CodegenError> {
        self.builder.position_at_end(self.blocks[0]);
        for slot in &self.function.stack_slots {
            let typ = self.basic_type(slot.typ)?;
            let pointer = self
                .builder
                .build_alloca(typ, slot.name.as_deref().unwrap_or("slot"))
                .map_err(builder_error)?;
            pointer
                .as_instruction()
                .expect("alloca is an instruction")
                .set_alignment(self.backend.data.get_abi_alignment(&typ))
                .map_err(|error| CodegenError::Lowering(error.to_string()))?;
            let store = self
                .builder
                .build_store(pointer, typ.const_zero())
                .map_err(builder_error)?;
            store
                .set_alignment(self.backend.data.get_abi_alignment(&typ))
                .map_err(|error| CodegenError::Lowering(error.to_string()))?;
            self.slots.push(pointer);
        }
        self.builder
            .build_unconditional_branch(self.blocks[type_index(self.function.entry) + 1])
            .map_err(builder_error)?;
        Ok(())
    }

    fn lower_instruction(&mut self, instruction: &Instruction) -> Result<(), CodegenError> {
        match &instruction.kind {
            InstructionKind::Const { value, typ } => {
                self.set_result(instruction, self.constant(*typ, *value)?)
            }
            InstructionKind::Unary { op, operand } => {
                self.lower_unary(instruction, *op, *operand)?
            }
            InstructionKind::Binary { op, left, right } => {
                self.lower_binary(instruction, *op, *left, *right)?
            }
            InstructionKind::Compare {
                predicate,
                left,
                right,
            } => {
                let left_type = self.value_type(*left);
                let result = if self.is_pointer(left_type) {
                    let left = self
                        .builder
                        .build_ptr_to_int(self.pointer(*left)?, self.iptr_type(), "ptr.bits")
                        .map_err(builder_error)?;
                    let right = self
                        .builder
                        .build_ptr_to_int(self.pointer(*right)?, self.iptr_type(), "ptr.bits")
                        .map_err(builder_error)?;
                    self.builder
                        .build_int_compare(
                            if *predicate == ComparePredicate::Equal {
                                IntPredicate::EQ
                            } else {
                                IntPredicate::NE
                            },
                            left,
                            right,
                            &value_name(self.result_id(instruction)?),
                        )
                        .map_err(builder_error)?
                        .into()
                } else {
                    self.builder
                        .build_int_compare(
                            compare_predicate(*predicate),
                            self.integer(*left)?,
                            self.integer(*right)?,
                            &value_name(self.result_id(instruction)?),
                        )
                        .map_err(builder_error)?
                        .into()
                };
                self.set_result(instruction, result);
            }
            InstructionKind::IntCast {
                kind,
                operand,
                target,
            } => {
                let source = self.integer(*operand)?;
                let target = self.int_type(*target)?;
                let value = match kind {
                    IntCastKind::Truncate => self.builder.build_int_truncate(
                        source,
                        target,
                        &value_name(self.result_id(instruction)?),
                    ),
                    IntCastKind::SignExtend => self.builder.build_int_s_extend(
                        source,
                        target,
                        &value_name(self.result_id(instruction)?),
                    ),
                    IntCastKind::ZeroExtend => self.builder.build_int_z_extend(
                        source,
                        target,
                        &value_name(self.result_id(instruction)?),
                    ),
                }
                .map_err(builder_error)?;
                self.set_result(instruction, value.into());
            }
            InstructionKind::StackAddr { slot } => {
                self.set_result(instruction, self.slots[type_index(*slot)].into())
            }
            InstructionKind::GlobalAddr { global } => self.set_result(
                instruction,
                self.globals[type_index(*global)].as_pointer_value().into(),
            ),
            InstructionKind::GepField { base, field } => {
                self.lower_gep_field(instruction, *base, *field)?
            }
            InstructionKind::GepIndex { base, index } => {
                self.lower_gep_index(instruction, *base, *index)?
            }
            InstructionKind::Load { pointer } => {
                self.null_guard(*pointer)?;
                let typ = self.basic_type(self.pointee_id(*pointer)?)?;
                let value = self
                    .builder
                    .build_load(
                        typ,
                        self.pointer(*pointer)?,
                        &value_name(self.result_id(instruction)?),
                    )
                    .map_err(builder_error)?;
                value
                    .as_instruction_value()
                    .expect("load is an instruction")
                    .set_alignment(self.backend.data.get_abi_alignment(&typ))
                    .map_err(|error| CodegenError::Lowering(error.to_string()))?;
                self.set_result(instruction, value);
            }
            InstructionKind::Store { pointer, value } => {
                self.null_guard(*pointer)?;
                let typ = self.basic_type(self.value_type(*value))?;
                let store = self
                    .builder
                    .build_store(self.pointer(*pointer)?, self.value(*value)?)
                    .map_err(builder_error)?;
                store
                    .set_alignment(self.backend.data.get_abi_alignment(&typ))
                    .map_err(|error| CodegenError::Lowering(error.to_string()))?;
            }
            InstructionKind::AggregateZero { destination, typ } => {
                self.null_guard(*destination)?;
                let typ = self.basic_type(*typ)?;
                let store = self
                    .builder
                    .build_store(self.pointer(*destination)?, typ.const_zero())
                    .map_err(builder_error)?;
                store
                    .set_alignment(self.backend.data.get_abi_alignment(&typ))
                    .map_err(|error| CodegenError::Lowering(error.to_string()))?;
            }
            InstructionKind::AggregateCopy {
                destination,
                source,
                typ,
            } => {
                self.null_guard(*destination)?;
                self.null_guard(*source)?;
                let typ = self.basic_type(*typ)?;
                self.builder
                    .build_memmove(
                        self.pointer(*destination)?,
                        self.backend.data.get_abi_alignment(&typ),
                        self.pointer(*source)?,
                        self.backend.data.get_abi_alignment(&typ),
                        self.iptr_type()
                            .const_int(self.backend.data.get_abi_size(&typ), false),
                    )
                    .map_err(builder_error)?;
            }
            InstructionKind::Call {
                callee: Callee::Function(callee),
                arguments,
            } => {
                let arguments = arguments
                    .iter()
                    .map(|argument| self.value(*argument).map(Into::into))
                    .collect::<Result<Vec<BasicMetadataValueEnum<'ctx>>, _>>()?;
                let call = self
                    .builder
                    .build_call(
                        self.functions[type_index(*callee)],
                        &arguments,
                        &value_name_or_empty(instruction),
                    )
                    .map_err(builder_error)?;
                if !instruction.results.is_empty() {
                    self.set_result(
                        instruction,
                        call.try_as_basic_value()
                            .basic()
                            .expect("verified non-void call"),
                    );
                }
            }
        }
        Ok(())
    }

    fn lower_unary(
        &mut self,
        instruction: &Instruction,
        op: UnaryOp,
        operand: ValueId,
    ) -> Result<(), CodegenError> {
        let value = self.integer(operand)?;
        let result = match op {
            UnaryOp::Neg => self.builder.build_int_sub(
                value.get_type().const_zero(),
                value,
                &value_name(self.result_id(instruction)?),
            ),
            UnaryOp::BitNot => self
                .builder
                .build_not(value, &value_name(self.result_id(instruction)?)),
            UnaryOp::LogicalNot => self.builder.build_int_compare(
                IntPredicate::EQ,
                value,
                value.get_type().const_zero(),
                &value_name(self.result_id(instruction)?),
            ),
        }
        .map_err(builder_error)?;
        self.set_result(instruction, result.into());
        Ok(())
    }

    fn lower_binary(
        &mut self,
        instruction: &Instruction,
        op: BinaryOp,
        left: ValueId,
        right: ValueId,
    ) -> Result<(), CodegenError> {
        match op {
            BinaryOp::SignedDiv | BinaryOp::SignedRem => {
                self.lower_signed_divrem(instruction, op, left, right)
            }
            BinaryOp::UnsignedDiv | BinaryOp::UnsignedRem => {
                self.nonzero_guard(right)?;
                let result = if op == BinaryOp::UnsignedDiv {
                    self.builder.build_int_unsigned_div(
                        self.integer(left)?,
                        self.integer(right)?,
                        &value_name(self.result_id(instruction)?),
                    )
                } else {
                    self.builder.build_int_unsigned_rem(
                        self.integer(left)?,
                        self.integer(right)?,
                        &value_name(self.result_id(instruction)?),
                    )
                }
                .map_err(builder_error)?;
                self.set_result(instruction, result.into());
                Ok(())
            }
            BinaryOp::Shl | BinaryOp::ArithmeticShr | BinaryOp::LogicalShr => {
                self.lower_shift(instruction, op, left, right)
            }
            _ => {
                let left_value = self.integer(left)?;
                let right_value = self.integer(right)?;
                let result = match op {
                    BinaryOp::Add => self.builder.build_int_add(
                        left_value,
                        right_value,
                        &value_name(self.result_id(instruction)?),
                    ),
                    BinaryOp::Sub => self.builder.build_int_sub(
                        left_value,
                        right_value,
                        &value_name(self.result_id(instruction)?),
                    ),
                    BinaryOp::Mul => self.builder.build_int_mul(
                        left_value,
                        right_value,
                        &value_name(self.result_id(instruction)?),
                    ),
                    BinaryOp::BitAnd => self.builder.build_and(
                        left_value,
                        right_value,
                        &value_name(self.result_id(instruction)?),
                    ),
                    BinaryOp::BitOr => self.builder.build_or(
                        left_value,
                        right_value,
                        &value_name(self.result_id(instruction)?),
                    ),
                    BinaryOp::BitXor => self.builder.build_xor(
                        left_value,
                        right_value,
                        &value_name(self.result_id(instruction)?),
                    ),
                    BinaryOp::BitClear => {
                        let inverse = self
                            .builder
                            .build_not(right_value, "bitclear.not")
                            .map_err(builder_error)?;
                        self.builder.build_and(
                            left_value,
                            inverse,
                            &value_name(self.result_id(instruction)?),
                        )
                    }
                    _ => unreachable!(),
                }
                .map_err(builder_error)?;
                self.set_result(instruction, result.into());
                Ok(())
            }
        }
    }

    fn lower_signed_divrem(
        &mut self,
        instruction: &Instruction,
        op: BinaryOp,
        left: ValueId,
        right: ValueId,
    ) -> Result<(), CodegenError> {
        self.nonzero_guard(right)?;
        let left_value = self.integer(left)?;
        let right_value = self.integer(right)?;
        let typ = left_value.get_type();
        let min = typ.const_int(1_u64 << (integer_width(self.value_type(left)) - 1), false);
        let is_min = self
            .builder
            .build_int_compare(IntPredicate::EQ, left_value, min, "div.min")
            .map_err(builder_error)?;
        let is_minus_one = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                right_value,
                typ.const_all_ones(),
                "div.minus_one",
            )
            .map_err(builder_error)?;
        let special = self
            .builder
            .build_and(is_min, is_minus_one, "div.special")
            .map_err(builder_error)?;
        let safe_right = self
            .builder
            .build_select(special, typ.const_int(1, false), right_value, "div.safe")
            .map_err(builder_error)?
            .into_int_value();
        let raw = if op == BinaryOp::SignedDiv {
            self.builder
                .build_int_signed_div(left_value, safe_right, "div.raw")
        } else {
            self.builder
                .build_int_signed_rem(left_value, safe_right, "rem.raw")
        }
        .map_err(builder_error)?;
        let fallback = if op == BinaryOp::SignedDiv {
            min
        } else {
            typ.const_zero()
        };
        let result = self
            .builder
            .build_select(
                special,
                fallback,
                raw,
                &value_name(self.result_id(instruction)?),
            )
            .map_err(builder_error)?;
        self.set_result(instruction, result);
        Ok(())
    }

    fn lower_shift(
        &mut self,
        instruction: &Instruction,
        op: BinaryOp,
        left: ValueId,
        right: ValueId,
    ) -> Result<(), CodegenError> {
        let count = self.integer(right)?;
        let negative = self
            .builder
            .build_int_compare(
                IntPredicate::SLT,
                count,
                self.context.i64_type().const_zero(),
                "shift.negative",
            )
            .map_err(builder_error)?;
        self.guard(negative, false, TrapReason::NegativeShift)?;
        let left_value = self.integer(left)?;
        let typ = left_value.get_type();
        let width = integer_width(self.value_type(left));
        let in_range = self
            .builder
            .build_int_compare(
                IntPredicate::ULT,
                count,
                self.context.i64_type().const_int(width as u64, false),
                "shift.in_range",
            )
            .map_err(builder_error)?;
        let safe_count = self
            .builder
            .build_select(
                in_range,
                count,
                self.context.i64_type().const_zero(),
                "shift.safe64",
            )
            .map_err(builder_error)?
            .into_int_value();
        let count = if width == 64 {
            safe_count
        } else {
            self.builder
                .build_int_truncate(safe_count, typ, "shift.count")
                .map_err(builder_error)?
        };
        let shifted = match op {
            BinaryOp::Shl => self
                .builder
                .build_left_shift(left_value, count, "shift.raw"),
            BinaryOp::ArithmeticShr => {
                self.builder
                    .build_right_shift(left_value, count, true, "shift.raw")
            }
            BinaryOp::LogicalShr => {
                self.builder
                    .build_right_shift(left_value, count, false, "shift.raw")
            }
            _ => unreachable!(),
        }
        .map_err(builder_error)?;
        let fallback = if op == BinaryOp::ArithmeticShr {
            self.builder
                .build_right_shift(
                    left_value,
                    typ.const_int((width - 1) as u64, false),
                    true,
                    "shift.sign",
                )
                .map_err(builder_error)?
        } else {
            typ.const_zero()
        };
        let result = self
            .builder
            .build_select(
                in_range,
                shifted,
                fallback,
                &value_name(self.result_id(instruction)?),
            )
            .map_err(builder_error)?;
        self.set_result(instruction, result);
        Ok(())
    }

    fn lower_gep_field(
        &mut self,
        instruction: &Instruction,
        base: ValueId,
        field: u32,
    ) -> Result<(), CodegenError> {
        self.null_guard(base)?;
        let aggregate = self.basic_type(self.pointee_id(base)?)?.into_struct_type();
        let result = self
            .builder
            .build_struct_gep(
                aggregate,
                self.pointer(base)?,
                field,
                &value_name(self.result_id(instruction)?),
            )
            .map_err(builder_error)?;
        self.set_result(instruction, result.into());
        Ok(())
    }

    fn lower_gep_index(
        &mut self,
        instruction: &Instruction,
        base: ValueId,
        index: ValueId,
    ) -> Result<(), CodegenError> {
        self.null_guard(base)?;
        let aggregate_id = self.pointee_id(base)?;
        let length = match &self
            .package
            .types()
            .get(aggregate_id)
            .expect("verified array")
            .kind
        {
            IrTypeKind::Array { length, .. } => *length,
            _ => unreachable!("verified gep index points to array"),
        };
        let index_value = self.integer(index)?;
        let in_range = self
            .builder
            .build_int_compare(
                IntPredicate::ULT,
                index_value,
                self.iptr_type().const_int(length, false),
                "index.in_range",
            )
            .map_err(builder_error)?;
        self.guard(in_range, true, TrapReason::BoundsError)?;
        // SAFETY: the verifier establishes the complete array pointee and integer index types.
        let result = unsafe {
            self.builder.build_gep(
                self.basic_type(aggregate_id)?,
                self.pointer(base)?,
                &[self.iptr_type().const_zero(), index_value],
                &value_name(self.result_id(instruction)?),
            )
        }
        .map_err(builder_error)?;
        self.set_result(instruction, result.into());
        Ok(())
    }

    fn lower_terminator(&mut self, terminator: &Terminator) -> Result<(), CodegenError> {
        match terminator {
            Terminator::Branch { target, arguments } => {
                self.add_incoming(*target, arguments)?;
                self.builder
                    .build_unconditional_branch(self.blocks[type_index(*target) + 1])
                    .map_err(builder_error)?;
            }
            Terminator::CondBranch {
                condition,
                then_target,
                then_arguments,
                else_target,
                else_arguments,
            } => {
                self.add_incoming(*then_target, then_arguments)?;
                self.add_incoming(*else_target, else_arguments)?;
                self.builder
                    .build_conditional_branch(
                        self.integer(*condition)?,
                        self.blocks[type_index(*then_target) + 1],
                        self.blocks[type_index(*else_target) + 1],
                    )
                    .map_err(builder_error)?;
            }
            Terminator::Return { values } => {
                if let Some(value) = values.first() {
                    self.builder
                        .build_return(Some(&self.value(*value)?))
                        .map_err(builder_error)?;
                } else {
                    self.builder.build_return(None).map_err(builder_error)?;
                }
            }
            Terminator::Trap { reason } => {
                self.emit_trap(*reason)?;
                self.builder.build_unreachable().map_err(builder_error)?;
            }
            Terminator::Unreachable => {
                self.builder.build_unreachable().map_err(builder_error)?;
            }
        }
        Ok(())
    }

    fn add_incoming(
        &self,
        target: gane_ir::BlockId,
        arguments: &[ValueId],
    ) -> Result<(), CodegenError> {
        let target_block = self.function.block(target).expect("verified target block");
        let tail = self
            .builder
            .get_insert_block()
            .expect("builder is positioned");
        for (parameter, argument) in target_block.parameters.iter().zip(arguments) {
            self.phis[type_index(*parameter)]
                .expect("entry cannot be a branch target")
                .add_incoming(&[(&self.value(*argument)?, tail)]);
        }
        Ok(())
    }

    fn null_guard(&mut self, pointer: ValueId) -> Result<(), CodegenError> {
        let condition = self
            .builder
            .build_is_not_null(self.pointer(pointer)?, "not_null")
            .map_err(builder_error)?;
        self.guard(condition, true, TrapReason::NullDereference)
    }

    fn nonzero_guard(&mut self, value: ValueId) -> Result<(), CodegenError> {
        let value = self.integer(value)?;
        let condition = self
            .builder
            .build_int_compare(
                IntPredicate::NE,
                value,
                value.get_type().const_zero(),
                "nonzero",
            )
            .map_err(builder_error)?;
        self.guard(condition, true, TrapReason::DivisionByZero)
    }

    fn guard(
        &mut self,
        condition: inkwell::values::IntValue<'ctx>,
        continue_when_true: bool,
        reason: TrapReason,
    ) -> Result<(), CodegenError> {
        let continuation = self.context.append_basic_block(self.llvm, "guard.cont");
        let trap = self.context.append_basic_block(self.llvm, "guard.trap");
        let (then_block, else_block) = if continue_when_true {
            (continuation, trap)
        } else {
            (trap, continuation)
        };
        self.builder
            .build_conditional_branch(condition, then_block, else_block)
            .map_err(builder_error)?;
        self.builder.position_at_end(trap);
        self.emit_trap(reason)?;
        self.builder.build_unreachable().map_err(builder_error)?;
        self.builder.position_at_end(continuation);
        Ok(())
    }

    fn emit_trap(&self, reason: TrapReason) -> Result<(), CodegenError> {
        self.builder
            .build_call(
                self.trap,
                &[self
                    .context
                    .i32_type()
                    .const_int(trap_code(reason) as u64, false)
                    .into()],
                "",
            )
            .map_err(builder_error)?;
        Ok(())
    }

    fn constant(&self, typ: TypeId, value: Constant) -> Result<BasicValueEnum<'ctx>, CodegenError> {
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

    fn basic_type(&self, id: TypeId) -> Result<BasicTypeEnum<'ctx>, CodegenError> {
        self.types
            .get(type_index(id))
            .and_then(|typ| *typ)
            .ok_or_else(|| CodegenError::Lowering(format!("invalid basic type {}", id.raw())))
    }

    fn int_type(&self, id: TypeId) -> Result<IntType<'ctx>, CodegenError> {
        match self.basic_type(id)? {
            BasicTypeEnum::IntType(typ) => Ok(typ),
            _ => Err(CodegenError::Lowering("expected an integer type".into())),
        }
    }

    fn iptr_type(&self) -> IntType<'ctx> {
        self.context.ptr_sized_int_type(&self.backend.data, None)
    }

    fn value(&self, id: ValueId) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        self.values[type_index(id)]
            .ok_or_else(|| CodegenError::Lowering(format!("value {} was not lowered", id.raw())))
    }

    fn integer(&self, id: ValueId) -> Result<inkwell::values::IntValue<'ctx>, CodegenError> {
        Ok(self.value(id)?.into_int_value())
    }

    fn pointer(&self, id: ValueId) -> Result<PointerValue<'ctx>, CodegenError> {
        Ok(self.value(id)?.into_pointer_value())
    }

    fn value_type(&self, id: ValueId) -> TypeId {
        self.function.value(id).expect("verified value").typ
    }

    fn pointee_id(&self, id: ValueId) -> Result<TypeId, CodegenError> {
        match &self
            .package
            .types()
            .get(self.value_type(id))
            .expect("verified type")
            .kind
        {
            IrTypeKind::Ptr { pointee, .. } => Ok(*pointee),
            _ => Err(CodegenError::Lowering(
                "pointer operation on non-pointer".into(),
            )),
        }
    }

    fn is_pointer(&self, id: TypeId) -> bool {
        matches!(
            self.package.types().get(id).expect("verified type").kind,
            IrTypeKind::Ptr { .. }
        )
    }

    fn result_id(&self, instruction: &Instruction) -> Result<ValueId, CodegenError> {
        instruction
            .results
            .first()
            .copied()
            .ok_or_else(|| CodegenError::Lowering("instruction has no result".into()))
    }

    fn set_result(&mut self, instruction: &Instruction, value: BasicValueEnum<'ctx>) {
        if let Some(result) = instruction.results.first() {
            self.values[type_index(*result)] = Some(value);
        }
    }
}

trait RawId {
    fn raw_id(self) -> u32;
}

macro_rules! raw_id {
    ($($id:ty),* $(,)?) => {
        $(impl RawId for $id {
            fn raw_id(self) -> u32 { self.raw() }
        })*
    };
}

raw_id!(TypeId, ValueId, BlockId, StackSlotId, GlobalId, FunctionId);

fn type_index(id: impl RawId) -> usize {
    id.raw_id() as usize - 1
}

fn value_name(value: ValueId) -> String {
    format!("v{}", value.raw())
}

fn value_name_or_empty(instruction: &Instruction) -> String {
    instruction
        .results
        .first()
        .map(|value| value_name(*value))
        .unwrap_or_default()
}

fn integer_width(typ: TypeId) -> u32 {
    match typ.raw() {
        2 => 1,
        3 => 8,
        4 => 16,
        5 => 32,
        6 => 64,
        _ => unreachable!("verified shift operand is integer"),
    }
}

fn compare_predicate(predicate: ComparePredicate) -> IntPredicate {
    match predicate {
        ComparePredicate::Equal => IntPredicate::EQ,
        ComparePredicate::NotEqual => IntPredicate::NE,
        ComparePredicate::SignedLess => IntPredicate::SLT,
        ComparePredicate::SignedLessEqual => IntPredicate::SLE,
        ComparePredicate::SignedGreater => IntPredicate::SGT,
        ComparePredicate::SignedGreaterEqual => IntPredicate::SGE,
        ComparePredicate::UnsignedLess => IntPredicate::ULT,
        ComparePredicate::UnsignedLessEqual => IntPredicate::ULE,
        ComparePredicate::UnsignedGreater => IntPredicate::UGT,
        ComparePredicate::UnsignedGreaterEqual => IntPredicate::UGE,
    }
}

fn trap_code(reason: TrapReason) -> u32 {
    match reason {
        TrapReason::DivisionByZero => 1,
        TrapReason::NegativeShift => 2,
        TrapReason::BoundsError => 3,
        TrapReason::NullDereference => 4,
        TrapReason::ExplicitPanic => 5,
    }
}

fn builder_error(error: inkwell::builder::BuilderError) -> CodegenError {
    CodegenError::Lowering(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use gane_ir::{
        Constant, FunctionAttributes, IrBuilder, IrSignature, TargetSpec, Terminator,
        verify_and_check_escape,
    };

    use super::*;

    fn verified_program(backend: &LlvmBackend) -> VerifiedIrPackage {
        let mut builder = IrBuilder::new(backend.target_spec().clone());
        let i64 = builder.types().i64();
        let pointer = builder.add_type(IrTypeKind::Ptr {
            pointee: i64,
            address_space: 0,
        });
        let function = builder.declare_function(
            "gane.main".into(),
            IrSignature {
                parameters: Vec::new(),
                results: Vec::new(),
            },
            FunctionAttributes::default(),
        );
        builder.set_entry(function).unwrap();
        let entry = builder.entry_block(function).unwrap();
        let join = builder.create_block(function).unwrap();
        let parameter = builder
            .append_block_parameter(function, join, i64, None)
            .unwrap();
        let slot = builder.add_stack_slot(function, i64, None, None).unwrap();
        let value = builder
            .append_instruction(
                function,
                entry,
                InstructionKind::Const {
                    value: Constant::Integer(41),
                    typ: i64,
                },
                [i64],
                None,
            )
            .unwrap()[0];
        let min = builder
            .append_instruction(
                function,
                entry,
                InstructionKind::Const {
                    value: Constant::Integer(1_u64 << 63),
                    typ: i64,
                },
                [i64],
                None,
            )
            .unwrap()[0];
        let minus_one = builder
            .append_instruction(
                function,
                entry,
                InstructionKind::Const {
                    value: Constant::Integer(u64::MAX),
                    typ: i64,
                },
                [i64],
                None,
            )
            .unwrap()[0];
        builder
            .append_instruction(
                function,
                entry,
                InstructionKind::Binary {
                    op: BinaryOp::SignedDiv,
                    left: min,
                    right: minus_one,
                },
                [i64],
                None,
            )
            .unwrap();
        builder
            .set_terminator(
                function,
                entry,
                Terminator::Branch {
                    target: join,
                    arguments: vec![value],
                },
            )
            .unwrap();
        let address = builder
            .append_instruction(
                function,
                join,
                InstructionKind::StackAddr { slot },
                [pointer],
                None,
            )
            .unwrap()[0];
        builder
            .append_instruction(
                function,
                join,
                InstructionKind::Store {
                    pointer: address,
                    value: parameter,
                },
                [],
                None,
            )
            .unwrap();
        builder
            .set_terminator(function, join, Terminator::Return { values: Vec::new() })
            .unwrap();
        verify_and_check_escape(builder.finish().unwrap()).unwrap()
    }

    fn aggregate_program(backend: &LlvmBackend) -> VerifiedIrPackage {
        let mut builder = IrBuilder::new(backend.target_spec().clone());
        let i64 = builder.types().i64();
        let array = builder.add_type(IrTypeKind::Array {
            length: 2,
            element: i64,
        });
        let pointer = builder.add_type(IrTypeKind::Ptr {
            pointee: array,
            address_space: 0,
        });
        let function = builder.declare_function(
            "gane.main".into(),
            IrSignature {
                parameters: Vec::new(),
                results: Vec::new(),
            },
            FunctionAttributes::default(),
        );
        builder.set_entry(function).unwrap();
        let entry = builder.entry_block(function).unwrap();
        let source_slot = builder.add_stack_slot(function, array, None, None).unwrap();
        let destination_slot = builder.add_stack_slot(function, array, None, None).unwrap();
        let source = builder
            .append_instruction(
                function,
                entry,
                InstructionKind::StackAddr { slot: source_slot },
                [pointer],
                None,
            )
            .unwrap()[0];
        let destination = builder
            .append_instruction(
                function,
                entry,
                InstructionKind::StackAddr {
                    slot: destination_slot,
                },
                [pointer],
                None,
            )
            .unwrap()[0];
        builder
            .append_instruction(
                function,
                entry,
                InstructionKind::AggregateCopy {
                    destination,
                    source,
                    typ: array,
                },
                [],
                None,
            )
            .unwrap();
        builder
            .set_terminator(function, entry, Terminator::Return { values: Vec::new() })
            .unwrap();
        verify_and_check_escape(builder.finish().unwrap()).unwrap()
    }

    fn trap_program(backend: &LlvmBackend) -> VerifiedIrPackage {
        let mut builder = IrBuilder::new(backend.target_spec().clone());
        let function = builder.declare_function(
            "gane.main".into(),
            IrSignature {
                parameters: Vec::new(),
                results: Vec::new(),
            },
            FunctionAttributes::default(),
        );
        builder.set_entry(function).unwrap();
        let entry = builder.entry_block(function).unwrap();
        builder
            .set_terminator(
                function,
                entry,
                Terminator::Trap {
                    reason: TrapReason::ExplicitPanic,
                },
            )
            .unwrap();
        verify_and_check_escape(builder.finish().unwrap()).unwrap()
    }

    fn run_lli(llvm_ir: &str) -> Option<std::process::ExitStatus> {
        let lli = std::env::var_os("LLVM_SYS_221_PREFIX")
            .map(|prefix| std::path::PathBuf::from(prefix).join("bin/lli"))
            .or_else(|| command_in_path("lli"))?;
        let path = std::env::temp_dir().join(format!(
            "gane-codegen-{}-{:?}.ll",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::write(&path, llvm_ir).unwrap();
        let status = Command::new(lli).arg(&path).output().unwrap().status;
        fs::remove_file(path).unwrap();
        Some(status)
    }

    fn command_in_path(name: &str) -> Option<std::path::PathBuf> {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    }

    #[test]
    fn builds_phi_and_stack_slots_with_inkwell() {
        let backend = LlvmBackend::for_host().unwrap();
        let package = verified_program(&backend);
        assert_eq!(gane_interpreter::interpret(&package), Ok(()));
        let llvm_ir = backend.emit_llvm_ir(&package).unwrap();
        assert!(llvm_ir.contains("phi i64"));
        assert!(!llvm_ir.contains("nsw"));
        assert!(run_lli(&llvm_ir).is_none_or(|status| status.success()));
    }

    #[test]
    fn builds_aggregate_copy_with_target_data_memmove() {
        let backend = LlvmBackend::for_host().unwrap();
        let package = aggregate_program(&backend);
        assert_eq!(gane_interpreter::interpret(&package), Ok(()));
        let llvm_ir = backend.emit_llvm_ir(&package).unwrap();
        assert!(llvm_ir.contains("llvm.memmove.p0.p0"));
        assert!(run_lli(&llvm_ir).is_none_or(|status| status.success()));
    }

    #[test]
    fn emits_explicit_traps() {
        let backend = LlvmBackend::for_host().unwrap();
        let package = trap_program(&backend);
        assert!(matches!(
            gane_interpreter::interpret(&package),
            Err(gane_interpreter::InterpreterError::Trap {
                reason: TrapReason::ExplicitPanic,
                ..
            })
        ));
        let llvm_ir = backend.emit_llvm_ir(&package).unwrap();
        assert!(llvm_ir.contains("@__gane_trap"));
        assert!(run_lli(&llvm_ir).is_none_or(|status| !status.success()));
    }

    #[test]
    fn rejects_a_non_host_target() {
        let backend = LlvmBackend::for_host().unwrap();
        let mut builder = IrBuilder::new(TargetSpec::for_test_64());
        let function = builder.declare_function(
            "gane.main".into(),
            IrSignature {
                parameters: Vec::new(),
                results: Vec::new(),
            },
            FunctionAttributes::default(),
        );
        builder.set_entry(function).unwrap();
        let entry = builder.entry_block(function).unwrap();
        builder
            .set_terminator(function, entry, Terminator::Return { values: Vec::new() })
            .unwrap();
        let package = verify_and_check_escape(builder.finish().unwrap()).unwrap();
        assert!(matches!(
            backend.emit_llvm_ir(&package),
            Err(CodegenError::TargetMismatch { .. })
        ));
    }
}
