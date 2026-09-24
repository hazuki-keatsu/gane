use crate::{
    BinaryOp, Callee, ComparePredicate, Constant, FunctionId, GlobalInitializer, Instruction,
    InstructionKind, IntCastKind, IrFunction, IrTypeKind, StackSlotId, Terminator, TrapReason,
    TypeArena, TypeId, UnaryOp, ValueId, VerifiedIrPackage,
};
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

/// Executes a verified V0 IR package from `gane.main`.
pub fn interpret(package: &VerifiedIrPackage) -> Result<(), InterpreterError> {
    Interpreter::new(package).run()
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RuntimeValue {
    Bits(u64),
    Pointer(Pointer),
}

impl RuntimeValue {
    fn bits(&self) -> u64 {
        match self {
            Self::Bits(bits) => *bits,
            Self::Pointer(_) => unreachable!("verified IR supplied a pointer as an integer"),
        }
    }

    fn pointer(&self) -> &Pointer {
        match self {
            Self::Pointer(pointer) => pointer,
            Self::Bits(_) => unreachable!("verified IR supplied an integer as a pointer"),
        }
    }
}

#[derive(Clone, Debug)]
enum Object {
    Scalar(RuntimeValue),
    Array(Vec<Object>),
    Struct(Vec<Object>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Pointer {
    Null,
    Address {
        root: PointerRoot,
        projections: Vec<Projection>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PointerRoot {
    Global(usize),
    Stack { frame: usize, slot: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Projection {
    Field(usize),
    Index(usize),
}

struct Frame {
    values: Vec<Option<RuntimeValue>>,
    slots: Vec<Object>,
}

struct Interpreter<'a> {
    package: &'a VerifiedIrPackage,
    globals: Vec<Object>,
    // ponytail: retain frames for the whole run; reclaim proven-dead frames if recursion becomes
    // a measured memory problem.
    frames: Vec<Frame>,
}

impl<'a> Interpreter<'a> {
    fn new(package: &'a VerifiedIrPackage) -> Self {
        let globals = package
            .globals()
            .map(|(_, global)| match global.initializer {
                GlobalInitializer::Zero => zero_object(package.types(), global.typ),
                GlobalInitializer::Scalar(value) => Object::Scalar(constant(value)),
            })
            .collect();
        Self {
            package,
            globals,
            frames: Vec::new(),
        }
    }

    fn run(mut self) -> Result<(), InterpreterError> {
        self.execute_function(self.package.entry(), Vec::new())?;
        Ok(())
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
            values: vec![None; function.values.len()],
            slots: function
                .stack_slots
                .iter()
                .map(|slot| zero_object(package.types(), slot.typ))
                .collect(),
        });

        let mut block_id = function.entry;
        let mut block_arguments = arguments;
        loop {
            let block = function
                .block(block_id)
                .expect("verified IR has valid block IDs");
            let parameters = block.parameters.clone();
            let instructions = block.instructions.clone();
            let terminator = block.terminator.clone();
            self.bind_block_parameters(frame_id, &parameters, block_arguments);

            for instruction in &instructions {
                let results = self.execute_instruction(frame_id, function, instruction)?;
                self.bind_instruction_results(frame_id, &instruction.results, results);
            }

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
                Terminator::Trap { reason } => return Err(InterpreterError::Trap(reason)),
                Terminator::Unreachable => return Err(InterpreterError::ReachedUnreachable),
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
                one(RuntimeValue::Bits(self.binary(
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
                    return Err(InterpreterError::Trap(TrapReason::NullDereference));
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
                    return Err(InterpreterError::Trap(TrapReason::BoundsError));
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

    fn binary(
        &self,
        op: BinaryOp,
        left: u64,
        right: u64,
        width: u32,
    ) -> Result<u64, InterpreterError> {
        let mask = mask(width);
        let value = match op {
            BinaryOp::Add => left.wrapping_add(right),
            BinaryOp::Sub => left.wrapping_sub(right),
            BinaryOp::Mul => left.wrapping_mul(right),
            BinaryOp::SignedDiv | BinaryOp::SignedRem => {
                let left = signed(left, width);
                let right = signed(right, width);
                if right == 0 {
                    return Err(InterpreterError::Trap(TrapReason::DivisionByZero));
                }
                if left == min_signed(width) && right == -1 {
                    if op == BinaryOp::SignedDiv {
                        left as u64
                    } else {
                        0
                    }
                } else if op == BinaryOp::SignedDiv {
                    (left / right) as u64
                } else {
                    (left % right) as u64
                }
            }
            BinaryOp::UnsignedDiv | BinaryOp::UnsignedRem => {
                if right == 0 {
                    return Err(InterpreterError::Trap(TrapReason::DivisionByZero));
                }
                if op == BinaryOp::UnsignedDiv {
                    left / right
                } else {
                    left % right
                }
            }
            BinaryOp::Shl | BinaryOp::ArithmeticShr | BinaryOp::LogicalShr => {
                if (right as i64) < 0 {
                    return Err(InterpreterError::Trap(TrapReason::NegativeShift));
                }
                if right >= u64::from(width) {
                    match op {
                        BinaryOp::ArithmeticShr if signed(left, width) < 0 => mask,
                        _ => 0,
                    }
                } else {
                    let count = right as u32;
                    match op {
                        BinaryOp::Shl => left << count,
                        BinaryOp::ArithmeticShr => (signed(left, width) >> count) as u64,
                        BinaryOp::LogicalShr => left >> count,
                        _ => unreachable!(),
                    }
                }
            }
            BinaryOp::BitAnd => left & right,
            BinaryOp::BitOr => left | right,
            BinaryOp::BitXor => left ^ right,
            BinaryOp::BitClear => left & !right,
        };
        Ok(value & mask)
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
            Pointer::Null => Err(InterpreterError::Trap(TrapReason::NullDereference)),
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
}

fn zero_object(types: &TypeArena, typ: TypeId) -> Object {
    match types
        .get(typ)
        .expect("verified IR has valid types")
        .kind
        .clone()
    {
        IrTypeKind::I1 | IrTypeKind::I8 | IrTypeKind::I16 | IrTypeKind::I32 | IrTypeKind::I64 => {
            Object::Scalar(RuntimeValue::Bits(0))
        }
        IrTypeKind::Ptr { .. } => Object::Scalar(RuntimeValue::Pointer(Pointer::Null)),
        IrTypeKind::Array { length, element } => {
            Object::Array((0..length).map(|_| zero_object(types, element)).collect())
        }
        IrTypeKind::Struct { fields } => Object::Struct(
            fields
                .into_iter()
                .map(|field| zero_object(types, field))
                .collect(),
        ),
        IrTypeKind::Void => unreachable!("verified IR has no void objects"),
    }
}

fn constant(value: Constant) -> RuntimeValue {
    match value {
        Constant::Bool(value) => RuntimeValue::Bits(u64::from(value)),
        Constant::Integer(value) => RuntimeValue::Bits(value),
        Constant::Null => RuntimeValue::Pointer(Pointer::Null),
    }
}

fn project_object<'a>(mut object: &'a Object, projections: &[Projection]) -> &'a Object {
    for projection in projections {
        object = match (object, projection) {
            (Object::Struct(fields), Projection::Field(field)) => &fields[*field],
            (Object::Array(elements), Projection::Index(index)) => &elements[*index],
            _ => unreachable!("verified IR pointer projection matches its object"),
        };
    }
    object
}

fn project_object_mut<'a>(
    mut object: &'a mut Object,
    projections: &[Projection],
) -> &'a mut Object {
    for projection in projections {
        object = match (object, projection) {
            (Object::Struct(fields), Projection::Field(field)) => &mut fields[*field],
            (Object::Array(elements), Projection::Index(index)) => &mut elements[*index],
            _ => unreachable!("verified IR pointer projection matches its object"),
        };
    }
    object
}

fn pointer_parts(pointer: &Pointer) -> Result<(PointerRoot, &[Projection]), InterpreterError> {
    match pointer {
        Pointer::Null => Err(InterpreterError::Trap(TrapReason::NullDereference)),
        Pointer::Address { root, projections } => Ok((*root, projections)),
    }
}

fn index(id: impl IntoIndex) -> usize {
    id.into_index()
}

trait IntoIndex {
    fn into_index(self) -> usize;
}

impl IntoIndex for ValueId {
    fn into_index(self) -> usize {
        self.raw() as usize - 1
    }
}

impl IntoIndex for StackSlotId {
    fn into_index(self) -> usize {
        self.raw() as usize - 1
    }
}

impl IntoIndex for crate::GlobalId {
    fn into_index(self) -> usize {
        self.raw() as usize - 1
    }
}

fn value_type(function: &IrFunction, value: ValueId) -> TypeId {
    function
        .value(value)
        .expect("verified IR has valid value IDs")
        .typ
}

fn integer_width(types: &TypeArena, typ: TypeId) -> u32 {
    match types.get(typ).expect("verified IR has valid types").kind {
        IrTypeKind::I8 => 8,
        IrTypeKind::I16 => 16,
        IrTypeKind::I32 => 32,
        IrTypeKind::I64 => 64,
        _ => unreachable!("verified IR supplied a non-integer operation"),
    }
}

fn pointee(types: &TypeArena, typ: TypeId) -> TypeId {
    match types.get(typ).expect("verified IR has valid types").kind {
        IrTypeKind::Ptr { pointee, .. } => pointee,
        _ => unreachable!("verified IR supplied a non-pointer operation"),
    }
}

fn mask(width: u32) -> u64 {
    if width == 64 {
        u64::MAX
    } else {
        (1_u64 << width) - 1
    }
}

fn signed(bits: u64, width: u32) -> i64 {
    if width == 64 {
        bits as i64
    } else {
        ((bits << (64 - width)) as i64) >> (64 - width)
    }
}

fn min_signed(width: u32) -> i64 {
    if width == 64 {
        i64::MIN
    } else {
        -(1_i64 << (width - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        FunctionAttributes, IrBuilder, IrGlobal, IrParameter, IrSignature, TargetSpec,
        verify_and_check_escape,
    };

    fn signature(parameters: Vec<IrParameter>, results: Vec<TypeId>) -> IrSignature {
        IrSignature {
            parameters,
            results,
        }
    }

    fn main_function(builder: &mut IrBuilder) -> (FunctionId, crate::BlockId) {
        let main = builder.declare_function(
            "gane.main".into(),
            signature(Vec::new(), Vec::new()),
            FunctionAttributes::default(),
        );
        builder.set_entry(main).unwrap();
        (main, builder.entry_block(main).unwrap())
    }

    fn verified(builder: IrBuilder) -> VerifiedIrPackage {
        verify_and_check_escape(builder.finish().unwrap()).unwrap()
    }

    fn constant(
        builder: &mut IrBuilder,
        function: FunctionId,
        block: crate::BlockId,
        typ: TypeId,
        value: Constant,
    ) -> ValueId {
        builder
            .append_instruction(
                function,
                block,
                InstructionKind::Const { value, typ },
                [typ],
                None,
            )
            .unwrap()[0]
    }

    fn compare(
        builder: &mut IrBuilder,
        function: FunctionId,
        block: crate::BlockId,
        predicate: ComparePredicate,
        left: ValueId,
        right: ValueId,
    ) -> ValueId {
        let i1 = builder.types().i1();
        builder
            .append_instruction(
                function,
                block,
                InstructionKind::Compare {
                    predicate,
                    left,
                    right,
                },
                [i1],
                None,
            )
            .unwrap()[0]
    }

    fn require(
        builder: &mut IrBuilder,
        function: FunctionId,
        block: crate::BlockId,
        condition: ValueId,
    ) -> crate::BlockId {
        let success = builder.create_block(function).unwrap();
        let failure = builder.create_block(function).unwrap();
        builder
            .set_terminator(
                function,
                block,
                Terminator::CondBranch {
                    condition,
                    then_target: success,
                    then_arguments: Vec::new(),
                    else_target: failure,
                    else_arguments: Vec::new(),
                },
            )
            .unwrap();
        builder
            .set_terminator(
                function,
                failure,
                Terminator::Trap {
                    reason: TrapReason::ExplicitPanic,
                },
            )
            .unwrap();
        success
    }

    fn require_compare(
        builder: &mut IrBuilder,
        function: FunctionId,
        block: crate::BlockId,
        predicate: ComparePredicate,
        left: ValueId,
        right: ValueId,
    ) -> crate::BlockId {
        let condition = compare(builder, function, block, predicate, left, right);
        require(builder, function, block, condition)
    }

    #[test]
    fn interprets_wrapping_integer_operations_casts_and_shifts() {
        let mut builder = IrBuilder::new(TargetSpec::for_test_64());
        let i8 = builder.types().i8();
        let i64 = builder.types().i64();
        let (main, mut block) = main_function(&mut builder);

        let max = constant(&mut builder, main, block, i8, Constant::Integer(255));
        let one = constant(&mut builder, main, block, i8, Constant::Integer(1));
        let wrapped = builder
            .append_instruction(
                main,
                block,
                InstructionKind::Binary {
                    op: BinaryOp::Add,
                    left: max,
                    right: one,
                },
                [i8],
                None,
            )
            .unwrap()[0];
        let zero = constant(&mut builder, main, block, i8, Constant::Integer(0));
        block = require_compare(
            &mut builder,
            main,
            block,
            ComparePredicate::Equal,
            wrapped,
            zero,
        );

        let negative_one = constant(&mut builder, main, block, i8, Constant::Integer(255));
        let zero = constant(&mut builder, main, block, i8, Constant::Integer(0));
        let signed_less = compare(
            &mut builder,
            main,
            block,
            ComparePredicate::SignedLess,
            negative_one,
            zero,
        );
        block = require(&mut builder, main, block, signed_less);
        let unsigned_greater = compare(
            &mut builder,
            main,
            block,
            ComparePredicate::UnsignedGreater,
            negative_one,
            zero,
        );
        block = require(&mut builder, main, block, unsigned_greater);

        let sign_extended = builder
            .append_instruction(
                main,
                block,
                InstructionKind::IntCast {
                    kind: IntCastKind::SignExtend,
                    operand: negative_one,
                    target: i64,
                },
                [i64],
                None,
            )
            .unwrap()[0];
        let zero64 = constant(&mut builder, main, block, i64, Constant::Integer(0));
        let is_negative = compare(
            &mut builder,
            main,
            block,
            ComparePredicate::SignedLess,
            sign_extended,
            zero64,
        );
        block = require(&mut builder, main, block, is_negative);
        let zero_extended = builder
            .append_instruction(
                main,
                block,
                InstructionKind::IntCast {
                    kind: IntCastKind::ZeroExtend,
                    operand: negative_one,
                    target: i64,
                },
                [i64],
                None,
            )
            .unwrap()[0];
        let two_fifty_five = constant(&mut builder, main, block, i64, Constant::Integer(255));
        block = require_compare(
            &mut builder,
            main,
            block,
            ComparePredicate::Equal,
            zero_extended,
            two_fifty_five,
        );

        let minimum = constant(
            &mut builder,
            main,
            block,
            i64,
            Constant::Integer(i64::MIN as u64),
        );
        let negative_one = constant(&mut builder, main, block, i64, Constant::Integer(u64::MAX));
        let division = builder
            .append_instruction(
                main,
                block,
                InstructionKind::Binary {
                    op: BinaryOp::SignedDiv,
                    left: minimum,
                    right: negative_one,
                },
                [i64],
                None,
            )
            .unwrap()[0];
        block = require_compare(
            &mut builder,
            main,
            block,
            ComparePredicate::Equal,
            division,
            minimum,
        );
        let remainder = builder
            .append_instruction(
                main,
                block,
                InstructionKind::Binary {
                    op: BinaryOp::SignedRem,
                    left: minimum,
                    right: negative_one,
                },
                [i64],
                None,
            )
            .unwrap()[0];
        let zero64 = constant(&mut builder, main, block, i64, Constant::Integer(0));
        block = require_compare(
            &mut builder,
            main,
            block,
            ComparePredicate::Equal,
            remainder,
            zero64,
        );

        let one8 = constant(&mut builder, main, block, i8, Constant::Integer(1));
        let negative8 = constant(&mut builder, main, block, i8, Constant::Integer(128));
        let wide = constant(&mut builder, main, block, i64, Constant::Integer(8));
        let shifted_left = builder
            .append_instruction(
                main,
                block,
                InstructionKind::Binary {
                    op: BinaryOp::Shl,
                    left: one8,
                    right: wide,
                },
                [i8],
                None,
            )
            .unwrap()[0];
        let logical_right = builder
            .append_instruction(
                main,
                block,
                InstructionKind::Binary {
                    op: BinaryOp::LogicalShr,
                    left: one8,
                    right: wide,
                },
                [i8],
                None,
            )
            .unwrap()[0];
        let arithmetic_right = builder
            .append_instruction(
                main,
                block,
                InstructionKind::Binary {
                    op: BinaryOp::ArithmeticShr,
                    left: negative8,
                    right: wide,
                },
                [i8],
                None,
            )
            .unwrap()[0];
        let zero = constant(&mut builder, main, block, i8, Constant::Integer(0));
        block = require_compare(
            &mut builder,
            main,
            block,
            ComparePredicate::Equal,
            shifted_left,
            zero,
        );
        block = require_compare(
            &mut builder,
            main,
            block,
            ComparePredicate::Equal,
            logical_right,
            zero,
        );
        let all_ones = constant(&mut builder, main, block, i8, Constant::Integer(255));
        block = require_compare(
            &mut builder,
            main,
            block,
            ComparePredicate::Equal,
            arithmetic_right,
            all_ones,
        );
        builder
            .set_terminator(main, block, Terminator::Return { values: Vec::new() })
            .unwrap();

        assert_eq!(interpret(&verified(builder)), Ok(()));
    }

    #[test]
    fn interprets_calls_loops_block_parameters_and_zeroed_slots() {
        let mut builder = IrBuilder::new(TargetSpec::for_test_64());
        let i64 = builder.types().i64();
        let pointer = builder.add_type(IrTypeKind::Ptr {
            pointee: i64,
            address_space: 0,
        });
        let increment = builder.declare_function(
            "gane.increment".into(),
            signature(vec![IrParameter { typ: i64 }], vec![i64]),
            FunctionAttributes::default(),
        );
        let increment_entry = builder.entry_block(increment).unwrap();
        let parameter = builder.entry_parameters(increment).unwrap()[0];
        let one = constant(
            &mut builder,
            increment,
            increment_entry,
            i64,
            Constant::Integer(1),
        );
        let result = builder
            .append_instruction(
                increment,
                increment_entry,
                InstructionKind::Binary {
                    op: BinaryOp::Add,
                    left: parameter,
                    right: one,
                },
                [i64],
                None,
            )
            .unwrap()[0];
        builder
            .set_terminator(
                increment,
                increment_entry,
                Terminator::Return {
                    values: vec![result],
                },
            )
            .unwrap();

        let (main, entry) = main_function(&mut builder);
        let header = builder.create_block(main).unwrap();
        let counter = builder
            .append_block_parameter(main, header, i64, None)
            .unwrap();
        let body = builder.create_block(main).unwrap();
        let exit = builder.create_block(main).unwrap();
        let slot = builder.add_stack_slot(main, i64, None, None).unwrap();
        let address = builder
            .append_instruction(
                main,
                entry,
                InstructionKind::StackAddr { slot },
                [pointer],
                None,
            )
            .unwrap()[0];
        let initial = builder
            .append_instruction(
                main,
                entry,
                InstructionKind::Load { pointer: address },
                [i64],
                None,
            )
            .unwrap()[0];
        let zero = constant(&mut builder, main, entry, i64, Constant::Integer(0));
        let mut block = require_compare(
            &mut builder,
            main,
            entry,
            ComparePredicate::Equal,
            initial,
            zero,
        );
        builder
            .set_terminator(
                main,
                block,
                Terminator::Branch {
                    target: header,
                    arguments: vec![zero],
                },
            )
            .unwrap();

        let limit = constant(&mut builder, main, header, i64, Constant::Integer(3));
        let before_limit = compare(
            &mut builder,
            main,
            header,
            ComparePredicate::SignedLess,
            counter,
            limit,
        );
        builder
            .set_terminator(
                main,
                header,
                Terminator::CondBranch {
                    condition: before_limit,
                    then_target: body,
                    then_arguments: Vec::new(),
                    else_target: exit,
                    else_arguments: Vec::new(),
                },
            )
            .unwrap();
        let next = builder
            .append_instruction(
                main,
                body,
                InstructionKind::Call {
                    callee: Callee::Function(increment),
                    arguments: vec![counter],
                },
                [i64],
                None,
            )
            .unwrap()[0];
        builder
            .set_terminator(
                main,
                body,
                Terminator::Branch {
                    target: header,
                    arguments: vec![next],
                },
            )
            .unwrap();
        let expected = constant(&mut builder, main, exit, i64, Constant::Integer(3));
        let reached_limit = compare(
            &mut builder,
            main,
            exit,
            ComparePredicate::Equal,
            counter,
            expected,
        );
        block = require(&mut builder, main, exit, reached_limit);
        builder
            .set_terminator(main, block, Terminator::Return { values: Vec::new() })
            .unwrap();

        assert_eq!(interpret(&verified(builder)), Ok(()));
    }

    #[test]
    fn interprets_globals_structured_memory_zero_and_copy() {
        let mut builder = IrBuilder::new(TargetSpec::for_test_64());
        let i64 = builder.types().i64();
        let array = builder.add_type(IrTypeKind::Array {
            length: 2,
            element: i64,
        });
        let record = builder.add_type(IrTypeKind::Struct {
            fields: vec![i64, array],
        });
        let integer_pointer = builder.add_type(IrTypeKind::Ptr {
            pointee: i64,
            address_space: 0,
        });
        let array_pointer = builder.add_type(IrTypeKind::Ptr {
            pointee: array,
            address_space: 0,
        });
        let record_pointer = builder.add_type(IrTypeKind::Ptr {
            pointee: record,
            address_space: 0,
        });
        let global = builder.add_global(IrGlobal {
            symbol: "gane.counter".into(),
            typ: i64,
            initializer: GlobalInitializer::Scalar(Constant::Integer(4)),
        });
        let (main, mut block) = main_function(&mut builder);
        let first_slot = builder.add_stack_slot(main, record, None, None).unwrap();
        let second_slot = builder.add_stack_slot(main, record, None, None).unwrap();
        let first = builder
            .append_instruction(
                main,
                block,
                InstructionKind::StackAddr { slot: first_slot },
                [record_pointer],
                None,
            )
            .unwrap()[0];
        let second = builder
            .append_instruction(
                main,
                block,
                InstructionKind::StackAddr { slot: second_slot },
                [record_pointer],
                None,
            )
            .unwrap()[0];
        let distinct = compare(
            &mut builder,
            main,
            block,
            ComparePredicate::NotEqual,
            first,
            second,
        );
        block = require(&mut builder, main, block, distinct);
        let address = builder
            .append_instruction(
                main,
                block,
                InstructionKind::GlobalAddr { global },
                [integer_pointer],
                None,
            )
            .unwrap()[0];
        let global_value = builder
            .append_instruction(
                main,
                block,
                InstructionKind::Load { pointer: address },
                [i64],
                None,
            )
            .unwrap()[0];
        let four = constant(&mut builder, main, block, i64, Constant::Integer(4));
        block = require_compare(
            &mut builder,
            main,
            block,
            ComparePredicate::Equal,
            global_value,
            four,
        );
        let seven = constant(&mut builder, main, block, i64, Constant::Integer(7));
        builder
            .append_instruction(
                main,
                block,
                InstructionKind::Store {
                    pointer: address,
                    value: seven,
                },
                [],
                None,
            )
            .unwrap();
        let global_value = builder
            .append_instruction(
                main,
                block,
                InstructionKind::Load { pointer: address },
                [i64],
                None,
            )
            .unwrap()[0];
        block = require_compare(
            &mut builder,
            main,
            block,
            ComparePredicate::Equal,
            global_value,
            seven,
        );

        builder
            .append_instruction(
                main,
                block,
                InstructionKind::AggregateZero {
                    destination: first,
                    typ: record,
                },
                [],
                None,
            )
            .unwrap();
        let first_field = builder
            .append_instruction(
                main,
                block,
                InstructionKind::GepField {
                    base: first,
                    field: 0,
                },
                [integer_pointer],
                None,
            )
            .unwrap()[0];
        let initial = builder
            .append_instruction(
                main,
                block,
                InstructionKind::Load {
                    pointer: first_field,
                },
                [i64],
                None,
            )
            .unwrap()[0];
        let zero = constant(&mut builder, main, block, i64, Constant::Integer(0));
        block = require_compare(
            &mut builder,
            main,
            block,
            ComparePredicate::Equal,
            initial,
            zero,
        );
        let first_array = builder
            .append_instruction(
                main,
                block,
                InstructionKind::GepField {
                    base: first,
                    field: 1,
                },
                [array_pointer],
                None,
            )
            .unwrap()[0];
        let index_zero = constant(&mut builder, main, block, i64, Constant::Integer(0));
        let index_one = constant(&mut builder, main, block, i64, Constant::Integer(1));
        let first_element = builder
            .append_instruction(
                main,
                block,
                InstructionKind::GepIndex {
                    base: first_array,
                    index: index_zero,
                },
                [integer_pointer],
                None,
            )
            .unwrap()[0];
        let second_element = builder
            .append_instruction(
                main,
                block,
                InstructionKind::GepIndex {
                    base: first_array,
                    index: index_one,
                },
                [integer_pointer],
                None,
            )
            .unwrap()[0];
        let one = constant(&mut builder, main, block, i64, Constant::Integer(1));
        let two = constant(&mut builder, main, block, i64, Constant::Integer(2));
        let three = constant(&mut builder, main, block, i64, Constant::Integer(3));
        for (pointer, value) in [
            (first_field, one),
            (first_element, two),
            (second_element, three),
        ] {
            builder
                .append_instruction(
                    main,
                    block,
                    InstructionKind::Store { pointer, value },
                    [],
                    None,
                )
                .unwrap();
        }
        builder
            .append_instruction(
                main,
                block,
                InstructionKind::AggregateCopy {
                    destination: second,
                    source: first,
                    typ: record,
                },
                [],
                None,
            )
            .unwrap();
        builder
            .append_instruction(
                main,
                block,
                InstructionKind::AggregateCopy {
                    destination: second,
                    source: second,
                    typ: record,
                },
                [],
                None,
            )
            .unwrap();
        builder
            .append_instruction(
                main,
                block,
                InstructionKind::AggregateZero {
                    destination: first,
                    typ: record,
                },
                [],
                None,
            )
            .unwrap();
        let reset = builder
            .append_instruction(
                main,
                block,
                InstructionKind::Load {
                    pointer: first_field,
                },
                [i64],
                None,
            )
            .unwrap()[0];
        block = require_compare(
            &mut builder,
            main,
            block,
            ComparePredicate::Equal,
            reset,
            zero,
        );
        let second_field = builder
            .append_instruction(
                main,
                block,
                InstructionKind::GepField {
                    base: second,
                    field: 0,
                },
                [integer_pointer],
                None,
            )
            .unwrap()[0];
        let second_array = builder
            .append_instruction(
                main,
                block,
                InstructionKind::GepField {
                    base: second,
                    field: 1,
                },
                [array_pointer],
                None,
            )
            .unwrap()[0];
        let copied_first = builder
            .append_instruction(
                main,
                block,
                InstructionKind::GepIndex {
                    base: second_array,
                    index: index_zero,
                },
                [integer_pointer],
                None,
            )
            .unwrap()[0];
        let copied_second = builder
            .append_instruction(
                main,
                block,
                InstructionKind::GepIndex {
                    base: second_array,
                    index: index_one,
                },
                [integer_pointer],
                None,
            )
            .unwrap()[0];
        for (pointer, value) in [
            (second_field, one),
            (copied_first, two),
            (copied_second, three),
        ] {
            let loaded = builder
                .append_instruction(main, block, InstructionKind::Load { pointer }, [i64], None)
                .unwrap()[0];
            block = require_compare(
                &mut builder,
                main,
                block,
                ComparePredicate::Equal,
                loaded,
                value,
            );
        }
        builder
            .set_terminator(main, block, Terminator::Return { values: Vec::new() })
            .unwrap();

        assert_eq!(interpret(&verified(builder)), Ok(()));
    }

    fn binary_error(op: BinaryOp, left: u64, right: u64) -> InterpreterError {
        let mut builder = IrBuilder::new(TargetSpec::for_test_64());
        let i64 = builder.types().i64();
        let (main, block) = main_function(&mut builder);
        let left = constant(&mut builder, main, block, i64, Constant::Integer(left));
        let right = constant(&mut builder, main, block, i64, Constant::Integer(right));
        builder
            .append_instruction(
                main,
                block,
                InstructionKind::Binary { op, left, right },
                [i64],
                None,
            )
            .unwrap();
        builder
            .set_terminator(main, block, Terminator::Return { values: Vec::new() })
            .unwrap();
        interpret(&verified(builder)).unwrap_err()
    }

    #[test]
    fn reports_total_operation_and_terminator_traps() {
        assert_eq!(
            binary_error(BinaryOp::SignedDiv, 1, 0),
            InterpreterError::Trap(TrapReason::DivisionByZero)
        );
        assert_eq!(
            binary_error(BinaryOp::Shl, 1, u64::MAX),
            InterpreterError::Trap(TrapReason::NegativeShift)
        );

        let mut builder = IrBuilder::new(TargetSpec::for_test_64());
        let i64 = builder.types().i64();
        let array = builder.add_type(IrTypeKind::Array {
            length: 1,
            element: i64,
        });
        let array_pointer = builder.add_type(IrTypeKind::Ptr {
            pointee: array,
            address_space: 0,
        });
        let integer_pointer = builder.add_type(IrTypeKind::Ptr {
            pointee: i64,
            address_space: 0,
        });
        let (main, block) = main_function(&mut builder);
        let slot = builder.add_stack_slot(main, array, None, None).unwrap();
        let array = builder
            .append_instruction(
                main,
                block,
                InstructionKind::StackAddr { slot },
                [array_pointer],
                None,
            )
            .unwrap()[0];
        let out_of_bounds = constant(&mut builder, main, block, i64, Constant::Integer(1));
        builder
            .append_instruction(
                main,
                block,
                InstructionKind::GepIndex {
                    base: array,
                    index: out_of_bounds,
                },
                [integer_pointer],
                None,
            )
            .unwrap();
        builder
            .set_terminator(main, block, Terminator::Return { values: Vec::new() })
            .unwrap();
        assert_eq!(
            interpret(&verified(builder)),
            Err(InterpreterError::Trap(TrapReason::BoundsError))
        );

        let mut builder = IrBuilder::new(TargetSpec::for_test_64());
        let i64 = builder.types().i64();
        let pointer = builder.add_type(IrTypeKind::Ptr {
            pointee: i64,
            address_space: 0,
        });
        let (main, block) = main_function(&mut builder);
        let null = constant(&mut builder, main, block, pointer, Constant::Null);
        builder
            .append_instruction(
                main,
                block,
                InstructionKind::Load { pointer: null },
                [i64],
                None,
            )
            .unwrap();
        builder
            .set_terminator(main, block, Terminator::Return { values: Vec::new() })
            .unwrap();
        assert_eq!(
            interpret(&verified(builder)),
            Err(InterpreterError::Trap(TrapReason::NullDereference))
        );

        let mut builder = IrBuilder::new(TargetSpec::for_test_64());
        let (main, block) = main_function(&mut builder);
        builder
            .set_terminator(
                main,
                block,
                Terminator::Trap {
                    reason: TrapReason::ExplicitPanic,
                },
            )
            .unwrap();
        assert_eq!(
            interpret(&verified(builder)),
            Err(InterpreterError::Trap(TrapReason::ExplicitPanic))
        );

        let mut builder = IrBuilder::new(TargetSpec::for_test_64());
        let (main, block) = main_function(&mut builder);
        builder
            .set_terminator(main, block, Terminator::Unreachable)
            .unwrap();
        assert_eq!(
            interpret(&verified(builder)),
            Err(InterpreterError::ReachedUnreachable)
        );
    }
}
