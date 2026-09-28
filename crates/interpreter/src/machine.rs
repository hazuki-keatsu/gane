use core::fmt;

use crate::{
    errors::InterpreterError,
    memory::{Object, project_object, project_object_mut, zero_object},
    ops::{
        binary, constant, index, integer_width, mask, pointee, pointer_parts, signed, value_type,
    },
    runtime::{Pointer, PointerRoot, Projection, RuntimeValue},
};

use gane_ir::{
    BlockId, Callee, ComparePredicate, FunctionId, GlobalInitializer, Instruction, InstructionKind,
    IntCastKind, IrFunction, IrTypeKind, Terminator, TrapReason, TypeId, UnaryOp, ValueId,
    VerifiedIrPackage,
};

struct Frame {
    function: FunctionId,
    block: BlockId,
    /// `instruction == instructions.len()` means terminator is under execution.
    instruction: usize,
    values: Vec<Option<RuntimeValue>>,
    slots: Vec<Object>,
}

struct StackDisplay<'a> {
    package: &'a VerifiedIrPackage,
    frames: &'a [Frame],
}

pub(crate) struct Interpreter<'a> {
    package: &'a VerifiedIrPackage,
    display_stack_details: bool,
    /// Global variable object
    globals: Vec<Object>,
    /// Calling stack frames
    frames: Vec<Frame>,
}

impl fmt::Display for StackDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (trace_index, frame) in self.frames.iter().rev().enumerate() {
            let function = self
                .package
                .function(frame.function)
                .expect("active frame has a valid function");
            writeln!(
                f,
                "#{} {} bb{}:{}",
                trace_index,
                function.symbol,
                frame.block.raw(),
                frame.instruction
            )?;

            writeln!(f, "  values:")?;
            for (value_index, value) in frame.values.iter().enumerate() {
                match value {
                    Some(value) => {
                        writeln!(f, "     %{} = {:?}", value_index + 1, value)?;
                    }
                    None => {
                        writeln!(f, "     %{} = <unavailable>", value_index + 1)?;
                    }
                }
            }

            writeln!(f, "  slots:")?;
            for (slot_index, object) in frame.slots.iter().enumerate() {
                writeln!(f, "    ${} = {:?}", slot_index + 1, object)?;
            }
        }
        Ok(())
    }
}

impl<'a> Interpreter<'a> {
    pub(crate) fn new(package: &'a VerifiedIrPackage, display_stack_details: bool) -> Self {
        let globals = package
            .globals()
            .map(|(_, global)| match global.initializer {
                GlobalInitializer::Zero => zero_object(package.types(), global.typ),
                GlobalInitializer::Scalar(value) => Object::Scalar(constant(value)),
            })
            .collect();
        Self {
            package,
            display_stack_details,
            globals,
            frames: Vec::new(),
        }
    }

    pub(crate) fn run(mut self) -> Result<(), InterpreterError> {
        let result = self
            .execute_function(self.package.entry(), Vec::new())
            .map(|_| ());
        debug_assert!(self.frames.is_empty());
        result
    }

    fn execute_function(
        &mut self,
        function_id: FunctionId,
        arguments: Vec<RuntimeValue>,
    ) -> Result<Option<RuntimeValue>, InterpreterError> {
        let package = self.package;
        let function = package
            .function(function_id)
            .expect("verified IR has valid function IDs");
        let frame_id = self.frames.len();
        self.frames.push(Frame {
            function: function_id,
            block: function.entry,
            instruction: 0,
            values: vec![None; function.values.len()],
            slots: function
                .stack_slots
                .iter()
                .map(|slot| zero_object(package.types(), slot.typ))
                .collect(),
        });

        let mut result = self.execute_frame(frame_id, function, arguments);
        if self.display_stack_details {
            if let Err(error) = &mut result {
                error.attach_trace_with(|| self.stack_frame_to_string());
            }
        }

        debug_assert_eq!(self.frames.len(), frame_id + 1);
        self.frames
            .pop()
            .expect("executing function has an active stack frame");
        debug_assert_eq!(self.frames.len(), frame_id);
        result
    }

    fn execute_frame(
        &mut self,
        frame_id: usize,
        function: &IrFunction,
        arguments: Vec<RuntimeValue>,
    ) -> Result<Option<RuntimeValue>, InterpreterError> {
        let mut block_id = function.entry;
        let mut block_arguments = arguments;
        loop {
            self.frames[frame_id].block = block_id;
            let block = function
                .block(block_id)
                .expect("verified IR has valid block IDs");
            let parameters = block.parameters.clone();
            let instructions = block.instructions.clone();
            let terminator = block.terminator.clone();
            self.bind_block_parameters(frame_id, &parameters, block_arguments);

            for (instruction_index, instruction) in instructions.iter().enumerate() {
                self.frames[frame_id].instruction = instruction_index;

                let results = self.execute_instruction(frame_id, function, instruction)?;
                self.bind_instruction_results(frame_id, &instruction.results, results);
            }

            self.frames[frame_id].instruction = instructions.len();
            match terminator {
                Terminator::Branch { target, arguments } => {
                    block_id = target;
                    block_arguments = self.values(frame_id, &arguments);
                }
                Terminator::CondBranch {
                    condition,
                    then_target,
                    then_arguments,
                    else_target,
                    else_arguments,
                } => {
                    let (target, arguments) = if self.value(frame_id, condition).bits() != 0 {
                        (then_target, then_arguments)
                    } else {
                        (else_target, else_arguments)
                    };
                    block_id = target;
                    block_arguments = self.values(frame_id, &arguments);
                }
                Terminator::Return { values } => {
                    return Ok(values.first().map(|value| self.value(frame_id, *value)));
                }
                Terminator::Trap { reason } => {
                    return Err(InterpreterError::trap(reason));
                }
                Terminator::Unreachable => {
                    return Err(InterpreterError::ReachedUnreachable { trace: None });
                }
            }
        }
    }

    fn execute_instruction(
        &mut self,
        frame: usize,
        function: &IrFunction,
        instruction: &Instruction,
    ) -> Result<Vec<RuntimeValue>, InterpreterError> {
        let one = |value| Ok(vec![value]);
        match &instruction.kind {
            InstructionKind::Const { value, .. } => one(constant(*value)),
            InstructionKind::Unary { op, operand } => {
                let typ = value_type(function, *operand);
                let bits = self.value(frame, *operand).bits();
                one(RuntimeValue::Bits(match op {
                    UnaryOp::Neg => {
                        0_u64.wrapping_sub(bits) & mask(integer_width(self.package.types(), typ))
                    }
                    UnaryOp::BitNot => !bits & mask(integer_width(self.package.types(), typ)),
                    UnaryOp::LogicalNot => u64::from(bits == 0),
                }))
            }
            InstructionKind::Binary { op, left, right } => {
                let typ = value_type(function, *left);
                let width = integer_width(self.package.types(), typ);
                one(RuntimeValue::Bits(binary(
                    *op,
                    self.value(frame, *left).bits(),
                    self.value(frame, *right).bits(),
                    width,
                )?))
            }
            InstructionKind::Compare {
                predicate,
                left,
                right,
            } => {
                let typ = value_type(function, *left);
                let left = self.value(frame, *left);
                let right = self.value(frame, *right);
                one(RuntimeValue::Bits(u64::from(
                    self.compare(*predicate, typ, &left, &right),
                )))
            }
            InstructionKind::IntCast {
                kind,
                operand,
                target,
            } => {
                let source = integer_width(self.package.types(), value_type(function, *operand));
                let target_width = integer_width(self.package.types(), *target);
                let bits = self.value(frame, *operand).bits();
                let value = match kind {
                    IntCastKind::Truncate | IntCastKind::ZeroExtend => bits,
                    IntCastKind::SignExtend => signed(bits, source) as u64,
                } & mask(target_width);
                one(RuntimeValue::Bits(value))
            }
            InstructionKind::StackAddr { slot } => one(RuntimeValue::Pointer(Pointer::Address {
                root: PointerRoot::Stack {
                    frame,
                    slot: index(*slot),
                },
                projections: Vec::new(),
            })),
            InstructionKind::GlobalAddr { global } => {
                one(RuntimeValue::Pointer(Pointer::Address {
                    root: PointerRoot::Global(index(*global)),
                    projections: Vec::new(),
                }))
            }
            InstructionKind::GepField { base, field } => one(RuntimeValue::Pointer(self.project(
                self.value(frame, *base).pointer(),
                Projection::Field(*field as usize),
            )?)),
            InstructionKind::GepIndex { base, index: value } => {
                let base_value = self.value(frame, *base);
                if base_value.pointer() == &Pointer::Null {
                    return Err(InterpreterError::trap(TrapReason::NullDereference));
                }
                let base_type = value_type(function, *base);
                let array = pointee(self.package.types(), base_type);
                let length = match &self
                    .package
                    .types()
                    .get(array)
                    .expect("verified IR has valid types")
                    .kind
                {
                    IrTypeKind::Array { length, .. } => *length,
                    _ => unreachable!("verified IR gep_index points to an array"),
                };
                let index_value = self.value(frame, *value).bits();
                if index_value >= length {
                    return Err(InterpreterError::trap(TrapReason::BoundsError));
                }
                one(RuntimeValue::Pointer(self.project(
                    base_value.pointer(),
                    Projection::Index(
                        usize::try_from(index_value).expect("allocated IR array index fits usize"),
                    ),
                )?))
            }
            InstructionKind::Load { pointer } => {
                one(match self.object(self.value(frame, *pointer).pointer())? {
                    Object::Scalar(value) => value.clone(),
                    _ => unreachable!("verified IR only loads scalar objects"),
                })
            }
            InstructionKind::Store { pointer, value } => {
                let value = self.value(frame, *value);
                match self.object_mut(self.value(frame, *pointer).pointer())? {
                    Object::Scalar(destination) => *destination = value,
                    _ => unreachable!("verified IR only stores scalar objects"),
                }
                Ok(Vec::new())
            }
            InstructionKind::AggregateZero { destination, typ } => {
                let zero = zero_object(self.package.types(), *typ);
                *self.object_mut(self.value(frame, *destination).pointer())? = zero;
                Ok(Vec::new())
            }
            InstructionKind::AggregateCopy {
                destination,
                source,
                ..
            } => {
                let source = self.object(self.value(frame, *source).pointer())?.clone();
                *self.object_mut(self.value(frame, *destination).pointer())? = source;
                Ok(Vec::new())
            }
            InstructionKind::Call {
                callee: Callee::Function(callee),
                arguments,
            } => Ok(self
                .execute_function(*callee, self.values(frame, arguments))?
                .into_iter()
                .collect()),
        }
    }

    fn compare(
        &self,
        predicate: ComparePredicate,
        typ: TypeId,
        left: &RuntimeValue,
        right: &RuntimeValue,
    ) -> bool {
        match predicate {
            ComparePredicate::Equal => left == right,
            ComparePredicate::NotEqual => left != right,
            ComparePredicate::SignedLess
            | ComparePredicate::SignedLessEqual
            | ComparePredicate::SignedGreater
            | ComparePredicate::SignedGreaterEqual => {
                let (left, right) = (
                    signed(left.bits(), integer_width(self.package.types(), typ)),
                    signed(right.bits(), integer_width(self.package.types(), typ)),
                );
                match predicate {
                    ComparePredicate::SignedLess => left < right,
                    ComparePredicate::SignedLessEqual => left <= right,
                    ComparePredicate::SignedGreater => left > right,
                    ComparePredicate::SignedGreaterEqual => left >= right,
                    _ => unreachable!(),
                }
            }
            ComparePredicate::UnsignedLess
            | ComparePredicate::UnsignedLessEqual
            | ComparePredicate::UnsignedGreater
            | ComparePredicate::UnsignedGreaterEqual => match predicate {
                ComparePredicate::UnsignedLess => left.bits() < right.bits(),
                ComparePredicate::UnsignedLessEqual => left.bits() <= right.bits(),
                ComparePredicate::UnsignedGreater => left.bits() > right.bits(),
                ComparePredicate::UnsignedGreaterEqual => left.bits() >= right.bits(),
                _ => unreachable!(),
            },
        }
    }

    fn project(
        &self,
        pointer: &Pointer,
        projection: Projection,
    ) -> Result<Pointer, InterpreterError> {
        match pointer {
            Pointer::Null => Err(InterpreterError::trap(TrapReason::NullDereference)),
            Pointer::Address { root, projections } => {
                let mut projections = projections.clone();
                projections.push(projection);
                Ok(Pointer::Address {
                    root: *root,
                    projections,
                })
            }
        }
    }

    fn object(&self, pointer: &Pointer) -> Result<&Object, InterpreterError> {
        let (root, projections) = pointer_parts(pointer)?;
        let object = match root {
            PointerRoot::Global(global) => &self.globals[global],
            PointerRoot::Stack { frame, slot } => &self.frames[frame].slots[slot],
        };
        Ok(project_object(object, projections))
    }

    fn object_mut(&mut self, pointer: &Pointer) -> Result<&mut Object, InterpreterError> {
        let (root, projections) = pointer_parts(pointer)?;
        let object = match root {
            PointerRoot::Global(global) => &mut self.globals[global],
            PointerRoot::Stack { frame, slot } => &mut self.frames[frame].slots[slot],
        };
        Ok(project_object_mut(object, projections))
    }

    /// Fill the block parameters by SSA value of the stack frame
    fn bind_block_parameters(
        &mut self,
        frame: usize,
        parameters: &[ValueId],
        arguments: Vec<RuntimeValue>,
    ) {
        assert_eq!(
            parameters.len(),
            arguments.len(),
            "verified IR block arguments match"
        );
        for (parameter, argument) in parameters.iter().zip(arguments) {
            self.frames[frame].values[index(*parameter)] = Some(argument);
        }
    }

    fn bind_instruction_results(
        &mut self,
        frame: usize,
        results: &[ValueId],
        values: Vec<RuntimeValue>,
    ) {
        assert_eq!(
            results.len(),
            values.len(),
            "verified IR instruction result count matches"
        );
        for (result, value) in results.iter().zip(values) {
            self.frames[frame].values[index(*result)] = Some(value);
        }
    }

    fn value(&self, frame: usize, value: ValueId) -> RuntimeValue {
        self.frames[frame].values[index(value)]
            .clone()
            .expect("verified IR only reads defined values")
    }

    fn values(&self, frame: usize, values: &[ValueId]) -> Vec<RuntimeValue> {
        values
            .iter()
            .map(|value| self.value(frame, *value))
            .collect()
    }

    fn stack_frame_to_string(&self) -> String {
        StackDisplay {
            package: self.package,
            frames: &self.frames,
        }
        .to_string()
    }
}
