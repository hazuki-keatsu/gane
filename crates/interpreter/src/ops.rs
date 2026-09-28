use gane_ir::{
    BinaryOp, Constant, IrFunction, IrTypeKind, StackSlotId, TrapReason, TypeArena, TypeId, ValueId,
};

use crate::{
    errors::InterpreterError,
    runtime::{Pointer, PointerRoot, Projection, RuntimeValue},
};

pub(crate) fn binary(
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

pub(crate) fn mask(width: u32) -> u64 {
    if width == 64 {
        u64::MAX
    } else {
        (1_u64 << width) - 1
    }
}

pub(crate) fn signed(bits: u64, width: u32) -> i64 {
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

pub(crate) fn constant(value: Constant) -> RuntimeValue {
    match value {
        Constant::Bool(value) => RuntimeValue::Bits(u64::from(value)),
        Constant::Integer(value) => RuntimeValue::Bits(value),
        Constant::Null => RuntimeValue::Pointer(Pointer::Null),
    }
}

pub(crate) fn pointer_parts(
    pointer: &Pointer,
) -> Result<(PointerRoot, &[Projection]), InterpreterError> {
    match pointer {
        Pointer::Null => Err(InterpreterError::Trap(TrapReason::NullDereference)),
        Pointer::Address { root, projections } => Ok((*root, projections)),
    }
}

pub(crate) fn index(id: impl IntoIndex) -> usize {
    id.into_index()
}

pub(crate) trait IntoIndex {
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

impl IntoIndex for gane_ir::GlobalId {
    fn into_index(self) -> usize {
        self.raw() as usize - 1
    }
}

pub(crate) fn value_type(function: &IrFunction, value: ValueId) -> TypeId {
    function
        .value(value)
        .expect("verified IR has valid value IDs")
        .typ
}

pub(crate) fn integer_width(types: &TypeArena, typ: TypeId) -> u32 {
    match types.get(typ).expect("verified IR has valid types").kind {
        IrTypeKind::I8 => 8,
        IrTypeKind::I16 => 16,
        IrTypeKind::I32 => 32,
        IrTypeKind::I64 => 64,
        _ => unreachable!("verified IR supplied a non-integer operation"),
    }
}

pub(crate) fn pointee(types: &TypeArena, typ: TypeId) -> TypeId {
    match types.get(typ).expect("verified IR has valid types").kind {
        IrTypeKind::Ptr { pointee, .. } => pointee,
        _ => unreachable!("verified IR supplied a non-pointer operation"),
    }
}
