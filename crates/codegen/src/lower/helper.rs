use gane_ir::{
    BlockId, ComparePredicate, FunctionId, GlobalId, StackSlotId, TrapReason, TypeId, ValueId,
};
use inkwell::IntPredicate;

use crate::CodegenError;

pub(crate) trait RawId {
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

pub(crate) fn id_to_index(id: impl RawId) -> usize {
    id.raw_id() as usize - 1
}

pub(crate) fn integer_width(typ: TypeId) -> u32 {
    match typ.raw() {
        2 => 1,
        3 => 8,
        4 => 16,
        5 => 32,
        6 => 64,
        _ => unreachable!("verified shift operand is integer"),
    }
}

pub(crate) fn compare_predicate(predicate: ComparePredicate) -> IntPredicate {
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

pub(crate) fn trap_code(reason: TrapReason) -> u32 {
    match reason {
        TrapReason::DivisionByZero => 1,
        TrapReason::NegativeShift => 2,
        TrapReason::BoundsError => 3,
        TrapReason::NullDereference => 4,
        TrapReason::ExplicitPanic => 5,
    }
}

pub(crate) fn builder_error(error: inkwell::builder::BuilderError) -> CodegenError {
    CodegenError::Lowering(error.to_string())
}
