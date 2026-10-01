use crate::{
    CodegenError, LlvmBackend,
    lower::helper::{builder_error, compare_predicate, id_to_index, integer_width, trap_code},
};
use gane_ir::{
    BinaryOp, Callee, ComparePredicate, Constant, Instruction, InstructionKind, IntCastKind,
    IrFunction, IrTypeKind, Terminator, TrapReason, TypeId, UnaryOp, ValueId, VerifiedIrPackage,
};
use inkwell::{
    IntPredicate,
    basic_block::BasicBlock,
    builder::Builder,
    context::Context,
    types::{BasicTypeEnum, IntType},
    values::{
        BasicMetadataValueEnum, BasicValue, BasicValueEnum, FunctionValue, GlobalValue, PhiValue,
        PointerValue,
    },
};

pub(crate) struct FunctionLowerer<'ctx, 'pkg> {
    /// LLVM Context, usually from [`ModuleLowerer`](crate::lower::module::ModuleLowerer)
    context: &'ctx Context,
    /// Current target platform information
    backend: &'ctx LlvmBackend,
    /// The verified Ir package reference
    package: &'pkg VerifiedIrPackage,
    /// LLVM command constructor, usually from [`ModuleLowerer`](crate::lower::module::ModuleLowerer)
    builder: &'ctx Builder<'ctx>,
    /// The mapping from Gane [`TypeId`] to LLVM basic type,
    /// usually from [`ModuleLowerer`](crate::lower::module::ModuleLowerer)
    types: &'ctx [Option<BasicTypeEnum<'ctx>>],
    /// The mapping from Gane [`GlobalId`](gane_ir::id::GlobalId) to LLVM global variable handler,
    /// usually from [`ModuleLowerer`](crate::lower::module::ModuleLowerer)
    globals: &'ctx [GlobalValue<'ctx>],
    /// The mapping from Gane [`FunctionId`](gane_ir::id::FunctionId) to LLVM function handler,
    /// usually from [`ModuleLowerer`](crate::lower::module::ModuleLowerer)
    functions: &'ctx [FunctionValue<'ctx>],
    /// `__gane_trap` helper
    trap: FunctionValue<'ctx>,
    /// The LLVM function in buidling
    llvm: FunctionValue<'ctx>,
    /// The Gane Ir function for building
    function: &'pkg IrFunction,
    /// The mapping from [`ValueId`] to LLVM SSA value
    values: Vec<Option<BasicValueEnum<'ctx>>>,
    /// The mapping from [`StackSlotId`](gane_ir::id::StackSlotId) to LLVM alloca address
    slots: Vec<PointerValue<'ctx>>,
    /// The mapping from [`BlockId`](gane_ir::id::BlockId) to LLVM basic block.
    /// Normally, the 0-th block is the prologue created additionally. The real Gane block starts
    /// from the first index.
    blocks: Vec<BasicBlock<'ctx>>,
    /// The mapping from [`FunctionId`](gane_ir::id::FunctionId) to LLVM phi.
    /// Used for processing branched and loops.
    phis: Vec<Option<PhiValue<'ctx>>>,
}

impl<'ctx, 'pkg> FunctionLowerer<'ctx, 'pkg> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
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
        // Add prologue block. it is used for:
        //
        // 1. alloca memory for all the stack slots
        // 2. initialize all the stack slots with zero
        // 3. branch to the entry block of the function
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

    pub(crate) fn lower(mut self) -> Result<(), CodegenError> {
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

    /// Bind parameters to SSA value on the entry block
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
            self.values[id_to_index(*value)] = Some(parameter);
        }
    }

    /// Create all the phis of the function in advance. The incomings of the [`PhiValue`] will not be set.
    /// They will be set at when [`Terminator::Branch`] or [`Terminator::CondBranch`] is encountered.
    /// See at [`add_incoming`](crate::lower::function::FunctionLowerer::add_incoming)
    fn create_phis(&mut self) -> Result<(), CodegenError> {
        for (index, block) in self.function.blocks.iter().enumerate() {
            if index == id_to_index(self.function.entry) {
                continue;
            }
            // Skip the prologue block
            self.builder.position_at_end(self.blocks[index + 1]);
            for parameter in &block.parameters {
                let phi = self
                    .builder
                    .build_phi(self.basic_type(self.value_type(*parameter))?, "")
                    .map_err(builder_error)?;
                self.values[id_to_index(*parameter)] = Some(phi.as_basic_value());
                self.phis[id_to_index(*parameter)] = Some(phi);
            }
        }
        Ok(())
    }

    fn lower_prologue(&mut self) -> Result<(), CodegenError> {
        self.builder.position_at_end(self.blocks[0]);
        for slot in &self.function.stack_slots {
            let typ = self.basic_type(slot.typ)?;
            let pointer = self.builder.build_alloca(typ, "").map_err(builder_error)?;
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
        // Next block
        self.builder
            .build_unconditional_branch(self.blocks[id_to_index(self.function.entry) + 1])
            .map_err(builder_error)?;
        Ok(())
    }

    fn lower_instruction(&mut self, instruction: &Instruction) -> Result<(), CodegenError> {
        match &instruction.kind {
            InstructionKind::Const { value, typ } => {
                self.set_ssa(instruction, self.lower_constant_value(*typ, *value)?)
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
                            "",
                        )
                        .map_err(builder_error)?
                        .into()
                } else {
                    self.builder
                        .build_int_compare(
                            compare_predicate(*predicate),
                            self.integer(*left)?,
                            self.integer(*right)?,
                            "",
                        )
                        .map_err(builder_error)?
                        .into()
                };
                self.set_ssa(instruction, result);
            }
            InstructionKind::IntCast {
                kind,
                operand,
                target,
            } => {
                let source = self.integer(*operand)?;
                let target = self.int_type(*target)?;
                let value = match kind {
                    IntCastKind::Truncate => self.builder.build_int_truncate(source, target, ""),
                    IntCastKind::SignExtend => self.builder.build_int_s_extend(source, target, ""),
                    IntCastKind::ZeroExtend => self.builder.build_int_z_extend(source, target, ""),
                }
                .map_err(builder_error)?;
                self.set_ssa(instruction, value.into());
            }
            InstructionKind::StackAddr { slot } => {
                self.set_ssa(instruction, self.slots[id_to_index(*slot)].into())
            }
            InstructionKind::GlobalAddr { global } => self.set_ssa(
                instruction,
                self.globals[id_to_index(*global)].as_pointer_value().into(),
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
                    .build_load(typ, self.pointer(*pointer)?, "")
                    .map_err(builder_error)?;
                value
                    .as_instruction_value()
                    .expect("load is an instruction")
                    .set_alignment(self.backend.data.get_abi_alignment(&typ))
                    .map_err(|error| CodegenError::Lowering(error.to_string()))?;
                self.set_ssa(instruction, value);
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
                    .build_call(self.functions[id_to_index(*callee)], &arguments, "")
                    .map_err(builder_error)?;
                if !instruction.results.is_empty() {
                    self.set_ssa(
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
            UnaryOp::Neg => self
                .builder
                .build_int_sub(value.get_type().const_zero(), value, ""),
            UnaryOp::BitNot => self.builder.build_not(value, ""),
            UnaryOp::LogicalNot => self.builder.build_int_compare(
                IntPredicate::EQ,
                value,
                value.get_type().const_zero(),
                "",
            ),
        }
        .map_err(builder_error)?;
        self.set_ssa(instruction, result.into());
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
                        "",
                    )
                } else {
                    self.builder.build_int_unsigned_rem(
                        self.integer(left)?,
                        self.integer(right)?,
                        "",
                    )
                }
                .map_err(builder_error)?;
                self.set_ssa(instruction, result.into());
                Ok(())
            }
            BinaryOp::Shl | BinaryOp::ArithmeticShr | BinaryOp::LogicalShr => {
                self.lower_shift(instruction, op, left, right)
            }
            _ => {
                let left_value = self.integer(left)?;
                let right_value = self.integer(right)?;
                let result = match op {
                    BinaryOp::Add => self.builder.build_int_add(left_value, right_value, ""),
                    BinaryOp::Sub => self.builder.build_int_sub(left_value, right_value, ""),
                    BinaryOp::Mul => self.builder.build_int_mul(left_value, right_value, ""),
                    BinaryOp::BitAnd => self.builder.build_and(left_value, right_value, ""),
                    BinaryOp::BitOr => self.builder.build_or(left_value, right_value, ""),
                    BinaryOp::BitXor => self.builder.build_xor(left_value, right_value, ""),
                    BinaryOp::BitClear => {
                        let inverse = self
                            .builder
                            .build_not(right_value, "bitclear.not")
                            .map_err(builder_error)?;
                        self.builder.build_and(left_value, inverse, "")
                    }
                    _ => unreachable!(),
                }
                .map_err(builder_error)?;
                self.set_ssa(instruction, result.into());
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
            .build_select(special, fallback, raw, "")
            .map_err(builder_error)?;
        self.set_ssa(instruction, result);
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
            .build_select(in_range, shifted, fallback, "")
            .map_err(builder_error)?;
        self.set_ssa(instruction, result);
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
            .build_struct_gep(aggregate, self.pointer(base)?, field, "")
            .map_err(builder_error)?;
        self.set_ssa(instruction, result.into());
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
                "",
            )
        }
        .map_err(builder_error)?;
        self.set_ssa(instruction, result.into());
        Ok(())
    }

    fn lower_terminator(&mut self, terminator: &Terminator) -> Result<(), CodegenError> {
        match terminator {
            Terminator::Branch { target, arguments } => {
                self.add_incoming(*target, arguments)?;
                self.builder
                    .build_unconditional_branch(self.blocks[id_to_index(*target) + 1])
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
                        self.blocks[id_to_index(*then_target) + 1],
                        self.blocks[id_to_index(*else_target) + 1],
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
            self.phis[id_to_index(*parameter)]
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

    fn lower_constant_value(
        &self,
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

    fn basic_type(&self, id: TypeId) -> Result<BasicTypeEnum<'ctx>, CodegenError> {
        self.types
            .get(id_to_index(id))
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
        self.values[id_to_index(id)]
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

    fn set_ssa(&mut self, instruction: &Instruction, value: BasicValueEnum<'ctx>) {
        if let Some(result) = instruction.results.first() {
            self.values[id_to_index(*result)] = Some(value);
        }
    }
}
