use crate::{
    id::{BlockId, FunctionId, GlobalId, StackSlotId, TypeId, ValueId},
    target::TargetSpec,
    types::{SourceOrigin, Symbol, TypeArena},
};

#[derive(Clone, Debug)]
pub struct HirGlobal {
    pub symbol: Symbol,
    pub typ: TypeId,
    pub mutable: bool,
    pub initializer: GlobalInitializer,
    pub linkage: Linkage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GlobalInitializer {
    Zero,
    Scalar(Constant),
}

#[derive(Clone, Debug)]
pub struct HirFunction {
    pub symbol: Symbol,
    pub signature: HirSignature,
    pub linkage: Linkage,
    pub attributes: FunctionAttributes,
    pub stack_slots: Vec<StackSlot>,
    pub values: Vec<ValueDef>,
    pub blocks: Vec<HirBlock>,
    pub entry: BlockId,
}

impl HirFunction {
    pub fn stack_slot(&self, id: StackSlotId) -> Option<&StackSlot> {
        self.stack_slots.get(id.raw().checked_sub(1)? as usize)
    }

    pub fn value(&self, id: ValueId) -> Option<&ValueDef> {
        self.values.get(id.raw().checked_sub(1)? as usize)
    }

    pub fn block(&self, id: BlockId) -> Option<&HirBlock> {
        self.blocks.get(id.raw().checked_sub(1)? as usize)
    }
}

#[derive(Clone, Debug)]
pub struct HirSignature {
    pub parameters: Vec<HirParameter>,
    pub results: Vec<TypeId>,
    pub calling_convention: CallingConvention,
}

#[derive(Clone, Debug)]
pub struct HirParameter {
    pub typ: TypeId,
    pub passing: PassingMode,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CallingConvention {
    #[default]
    Gane,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PassingMode {
    #[default]
    Direct,
    IndirectByValue,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Linkage {
    #[default]
    Internal,
    Exported,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FunctionAttributes {
    pub no_return: bool,
    pub no_unwind: bool,
    pub memory: MemoryEffect,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MemoryEffect {
    #[default]
    Unknown,
    ReadOnly,
    ReadNone,
}

#[derive(Clone, Debug)]
pub struct StackSlot {
    pub typ: TypeId,
    pub name: Option<Symbol>,
    pub origin: SourceOrigin,
}

#[derive(Clone, Debug)]
pub struct HirBlock {
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
        index: u32,
    },
    InstructionResult {
        block: BlockId,
        instruction: u32,
        index: u32,
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
    NegativeShift,
    BoundsError,
    NullDereference,
    ExplicitPanic,
}

#[derive(Clone, Debug)]
pub(crate) struct HirPackage {
    pub(crate) target: TargetSpec,
    pub(crate) types: TypeArena,
    pub(crate) globals: Vec<HirGlobal>,
    pub(crate) functions: Vec<HirFunction>,
    pub(crate) entry: FunctionId,
}

#[derive(Clone, Debug)]
pub struct UnverifiedHirPackage(HirPackage);

#[derive(Clone, Debug)]
pub struct VerifiedHirPackage(HirPackage);

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

            pub fn global(&self, id: GlobalId) -> Option<&HirGlobal> {
                self.0.globals.get(id.raw().checked_sub(1)? as usize)
            }

            pub fn function(&self, id: FunctionId) -> Option<&HirFunction> {
                self.0.functions.get(id.raw().checked_sub(1)? as usize)
            }

            pub fn globals(&self) -> impl Iterator<Item = (GlobalId, &HirGlobal)> {
                self.0
                    .globals
                    .iter()
                    .enumerate()
                    .map(|(index, global)| (GlobalId::from_raw(index as u32 + 1), global))
            }

            pub fn functions(&self) -> impl Iterator<Item = (FunctionId, &HirFunction)> {
                self.0
                    .functions
                    .iter()
                    .enumerate()
                    .map(|(index, function)| (FunctionId::from_raw(index as u32 + 1), function))
            }
        }
    };
}

package_accessors!(UnverifiedHirPackage);
package_accessors!(VerifiedHirPackage);

impl UnverifiedHirPackage {
    pub(crate) fn from_inner(package: HirPackage) -> Self {
        Self(package)
    }

    pub(crate) fn inner(&self) -> &HirPackage {
        &self.0
    }

    #[cfg(test)]
    pub(crate) fn inner_mut(&mut self) -> &mut HirPackage {
        &mut self.0
    }
}

impl VerifiedHirPackage {
    pub(crate) fn inner(&self) -> &HirPackage {
        &self.0
    }
}
