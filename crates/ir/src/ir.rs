// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Hazuki Keatsu

use crate::{
    id::{BlockId, FunctionId, GlobalId, StackSlotId, TypeId, ValueId},
    target::TargetSpec,
    types::{SourceOrigin, Symbol, TypeArena},
};

#[derive(Clone, Debug)]
pub struct IrGlobal {
    pub symbol: Symbol,
    pub typ: TypeId,
    pub initializer: GlobalInitializer,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GlobalInitializer {
    Zero,
    Scalar(Constant),
}

#[derive(Clone, Debug)]
pub struct IrFunction {
    /// The name of the function
    pub symbol: Symbol,
    /// The parameters and returns of the function
    pub signature: IrSignature,
    /// The additional attributes for the function
    ///
    /// v0: no_return is false forever
    pub attributes: FunctionAttributes,
    /// The local memory object for the function
    pub stack_slots: Vec<StackSlot>,
    /// The SSA values in the function
    pub values: Vec<ValueDef>,
    /// The instruction blocks in the function
    pub blocks: Vec<IrBlock>,
    /// The entry point of the function
    ///
    /// v0: the entry will be ^1 forever
    pub entry: BlockId,
}

impl IrFunction {
    pub fn stack_slot(&self, id: StackSlotId) -> Option<&StackSlot> {
        self.stack_slots.get(id.raw().checked_sub(1)? as usize)
    }

    pub fn value(&self, id: ValueId) -> Option<&ValueDef> {
        self.values.get(id.raw().checked_sub(1)? as usize)
    }

    pub fn block(&self, id: BlockId) -> Option<&IrBlock> {
        self.blocks.get(id.raw().checked_sub(1)? as usize)
    }
}

#[derive(Clone, Debug)]
pub struct IrSignature {
    pub parameters: Vec<IrParameter>,
    pub results: Vec<TypeId>,
}

#[derive(Clone, Debug)]
pub struct IrParameter {
    pub typ: TypeId,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FunctionAttributes {
    pub no_return: bool,
}

#[derive(Clone, Debug)]
pub struct StackSlot {
    pub typ: TypeId,
    pub name: Option<Symbol>,
    pub origin: SourceOrigin,
}

#[derive(Clone, Debug)]
pub struct IrBlock {
    pub parameters: Vec<ValueId>,
    pub instructions: Vec<Instruction>,
    pub terminator: Terminator,
}

#[derive(Clone, Debug)]
pub struct ValueDef {
    pub typ: TypeId,
    pub origin: ValueOrigin,
    pub source: SourceOrigin,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueOrigin {
    BlockParameter {
        block: BlockId,
        parameter_index: u32,
    },
    InstructionResult {
        block: BlockId,
        instruction_index: u32,
        result_index: u32,
    },
}

#[derive(Clone, Debug)]
pub struct Instruction {
    pub results: Vec<ValueId>,
    pub kind: InstructionKind,
    pub source: SourceOrigin,
}

#[derive(Clone, Debug)]
pub enum InstructionKind {
    Const {
        value: Constant,
        typ: TypeId,
    },
    Unary {
        op: UnaryOp,
        operand: ValueId,
    },
    Binary {
        op: BinaryOp,
        left: ValueId,
        right: ValueId,
    },
    Compare {
        predicate: ComparePredicate,
        left: ValueId,
        right: ValueId,
    },
    IntCast {
        kind: IntCastKind,
        operand: ValueId,
        target: TypeId,
    },
    StackAddr {
        slot: StackSlotId,
    },
    GlobalAddr {
        global: GlobalId,
    },
    GepField {
        base: ValueId,
        field: u32,
    },
    GepIndex {
        base: ValueId,
        index: ValueId,
    },
    Load {
        pointer: ValueId,
    },
    Store {
        pointer: ValueId,
        value: ValueId,
    },
    AggregateZero {
        destination: ValueId,
        typ: TypeId,
    },
    AggregateCopy {
        destination: ValueId,
        source: ValueId,
        typ: TypeId,
    },
    Call {
        callee: Callee,
        arguments: Vec<ValueId>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Constant {
    Bool(bool),
    Integer(u64),
    Null,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnaryOp {
    Neg,
    BitNot,
    LogicalNot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    SignedDiv,
    UnsignedDiv,
    SignedRem,
    UnsignedRem,
    Shl,
    ArithmeticShr,
    LogicalShr,
    BitAnd,
    BitOr,
    BitXor,
    BitClear,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComparePredicate {
    Equal,
    NotEqual,
    SignedLess,
    SignedLessEqual,
    SignedGreater,
    SignedGreaterEqual,
    UnsignedLess,
    UnsignedLessEqual,
    UnsignedGreater,
    UnsignedGreaterEqual,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntCastKind {
    Truncate,
    SignExtend,
    ZeroExtend,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Callee {
    Function(FunctionId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Terminator {
    Branch {
        target: BlockId,
        arguments: Vec<ValueId>,
    },
    CondBranch {
        condition: ValueId,
        then_target: BlockId,
        then_arguments: Vec<ValueId>,
        else_target: BlockId,
        else_arguments: Vec<ValueId>,
    },
    Return {
        values: Vec<ValueId>,
    },
    Trap {
        reason: TrapReason,
    },
    Unreachable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrapReason {
    DivisionByZero,
    /// Use negative number as the value of shift times
    NegativeShift,
    /// Access arrays out of the bounds
    BoundsError,
    /// Dereference the null pointer
    NullDereference,
    ExplicitPanic,
}

#[derive(Clone, Debug)]
pub(crate) struct IrPackage {
    pub(crate) target: TargetSpec,
    pub(crate) types: TypeArena,
    pub(crate) globals: Vec<IrGlobal>,
    pub(crate) functions: Vec<IrFunction>,
    pub(crate) entry: FunctionId,
}

#[derive(Clone, Debug)]
pub struct UnverifiedIrPackage(IrPackage);

#[derive(Clone, Debug)]
pub struct VerifiedIrPackage(IrPackage);

macro_rules! package_accessors {
    ($wrapper:ident) => {
        impl $wrapper {
            pub fn target(&self) -> &TargetSpec {
                &self.0.target
            }

            pub fn types(&self) -> &TypeArena {
                &self.0.types
            }

            pub fn entry(&self) -> FunctionId {
                self.0.entry
            }

            pub fn global(&self, id: GlobalId) -> Option<&IrGlobal> {
                self.0.globals.get(id.raw().checked_sub(1)? as usize)
            }

            pub fn function(&self, id: FunctionId) -> Option<&IrFunction> {
                self.0.functions.get(id.raw().checked_sub(1)? as usize)
            }

            pub fn globals(&self) -> impl Iterator<Item = (GlobalId, &IrGlobal)> {
                self.0
                    .globals
                    .iter()
                    .enumerate()
                    .map(|(index, global)| (GlobalId::from_raw(index as u32 + 1), global))
            }

            pub fn functions(&self) -> impl Iterator<Item = (FunctionId, &IrFunction)> {
                self.0
                    .functions
                    .iter()
                    .enumerate()
                    .map(|(index, function)| (FunctionId::from_raw(index as u32 + 1), function))
            }
        }
    };
}

package_accessors!(UnverifiedIrPackage);
package_accessors!(VerifiedIrPackage);

impl UnverifiedIrPackage {
    pub(crate) fn from_inner(package: IrPackage) -> Self {
        Self(package)
    }

    pub(crate) fn inner(&self) -> &IrPackage {
        &self.0
    }

    pub(crate) fn into_inner(self) -> IrPackage {
        self.0
    }

    #[cfg(test)]
    pub(crate) fn inner_mut(&mut self) -> &mut IrPackage {
        &mut self.0
    }
}

impl VerifiedIrPackage {
    pub(crate) fn from_inner(package: IrPackage) -> Self {
        Self(package)
    }

    pub(crate) fn inner(&self) -> &IrPackage {
        &self.0
    }
}
