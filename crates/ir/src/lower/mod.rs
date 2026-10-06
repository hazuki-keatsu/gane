// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Hazuki Keatsu

use crate::{
    BinaryOp, BlockId, BuildError, Callee, ComparePredicate, Constant, FunctionAttributes,
    FunctionId, GlobalId, GlobalInitializer, IntCastKind, IrBuilder, IrGlobal, IrParameter,
    IrSignature, IrTypeKind, Terminator, TrapReason, TypeId, UnaryOp, UnverifiedIrPackage, ValueId,
};
use gane_parser::{
    ast,
    token::{AstNodeId, Token},
};
use gane_sema::{
    AnalysisResult, BasicType, ConstValue, ObjectId, ObjectKind, PackageInput, SelectionKind,
    Severity, TypeKind,
};
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
};

/// Lower expression into SSA value and `Place`.
/// It includes constant, binary, call, &&/||, field selection, array index and pointer dereference.
mod expressions;
/// The context for single function lowering. Saving the current function, block, local variable, loop stack and known not null pointer.
/// Used for parameter initialization, appending of instruction, setting of terminator and generation of trap guard.
mod function;
/// Used for the reading and writing of `Place` and the aggregate operation,
/// including `load`, `store`, assignment, aggregate copy/zero, snapshot, and null check.
mod memory;
/// The whole procession of lowering.
/// It will set up `global` and all the function declaration first.
/// Then, create the `FunctionLowerer` body one by one. And also, it will maintain the map from `sema` object to IR global/function.
mod package;
/// Organize the statement of source code into context free grammar.
/// It includes local var, assignment, return, if, for, break, continue, ++/--, and more.
mod statements;
/// Turn `sema` types and constants into IR types and IR constant.
/// And it will maintain type cache and pointer type cache, and process recursive type and target-relative int byte-width.
mod types;

#[derive(Debug)]
pub enum LowerError {
    /// If there is any error in `AnalysisResult.diagnostics`,
    /// `SemanticErrors` will be generated,
    /// and `node` is the first error's `AstNodeId` and `count` is the number of errors.
    SemanticErrors { node: AstNodeId, count: usize },
    /// When lowerer try to look up any semantic info in `AnalysisResult` but get none,
    /// it will be generated.
    MissingSemanticFact { node: AstNodeId, fact: &'static str },
    /// A guard for unsupported syntax.
    Unsupported {
        node: AstNodeId,
        construct: &'static str,
    },
    /// When `sema` give out the constant but it cannot be porting to the target ir type,
    /// it will be generated.
    ///
    /// For example, integer constant exceeds the range of i32/i64,
    /// byte constant is not in the range of `0..=255`,
    /// the length of array is not positive integer,
    /// in the 32-bit target, the length of array exceeds the max value of u32,
    /// constant type does not match the target type,
    /// there is any constant type not being supported by m0,
    /// and so on.
    InvalidConstant { node: AstNodeId },
    /// When lowerer try to build an IR by calling `IrBuilder` but builder rejects the request,
    /// it will be generated.
    Build { node: AstNodeId, source: BuildError },
}

impl LowerError {
    pub fn node(&self) -> AstNodeId {
        match *self {
            Self::SemanticErrors { node, .. }
            | Self::MissingSemanticFact { node, .. }
            | Self::Unsupported { node, .. }
            | Self::InvalidConstant { node }
            | Self::Build { node, .. } => node,
        }
    }
}

impl fmt::Display for LowerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SemanticErrors { count, .. } => {
                write!(formatter, "semantic analysis contains {count} error(s)")
            }
            Self::MissingSemanticFact { fact, .. } => {
                write!(formatter, "missing semantic fact: {fact}")
            }
            Self::Unsupported { construct, .. } => {
                write!(formatter, "unsupported lowering construct: {construct}")
            }
            Self::InvalidConstant { .. } => {
                formatter.write_str("constant cannot be represented by its IR type")
            }
            Self::Build { source, .. } => write!(formatter, "IR builder failed: {source}"),
        }
    }
}

impl Error for LowerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Build { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub fn lower_package(
    input: &PackageInput<'_>,
    analysis: &AnalysisResult,
    target: crate::TargetSpec,
) -> Result<UnverifiedIrPackage, LowerError> {
    let errors = analysis
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        let node = errors[0]
            .primary
            .ast_node()
            .or_else(|| input.files.first().map(|file| file.ast.node_id()))
            .unwrap_or(AstNodeId::INVALID);
        return Err(LowerError::SemanticErrors {
            node,
            count: errors.len(),
        });
    }

    PackageLowerer::new(analysis, target).lower(input)
}

/// Place - A pointer for reading and writing value
///
/// More common name for this type may be left value
#[derive(Clone, Copy)]
struct Place {
    pointer: ValueId,
    typ: TypeId,
}

#[derive(Clone, Copy)]
enum Rvalue {
    Scalar(ValueId),
    AggregateCopy(Place),
    AggregateZero(TypeId),
}

#[derive(Clone)]
struct LoweredFunction {
    id: FunctionId,
    signature: IrSignature,
}

/// Current jumpable recursive context
///
/// When lowering, the header will be the target for `continue` and the exit will be the target for `break`.
#[derive(Clone, Copy)]
struct Loop {
    header: BlockId,
    exit: Option<BlockId>,
}

struct PackageLowerer<'a> {
    analysis: &'a AnalysisResult,
    builder: IrBuilder,
    target_width: u8,
    /// The mapping from `sema` global object to IR global variable Id.
    /// It will be filled when lowering global declaration.
    /// It will be used when processing the global reference.
    globals: HashMap<ObjectId, GlobalId>,
    /// The mapping from `sema` function object to IR lowered function.
    /// It will be used when processing function calling and function lowering.
    functions: HashMap<ObjectId, LoweredFunction>,
    /// The cache from IR pointee type to the corresponding Ptr<T> type,
    /// avoiding creating the same pointer type repeatedly.
    pointer_types: HashMap<TypeId, TypeId>,
    /// The mapping from `sema` type id to IR type id.
    /// It will guarantee the same semantic type will matching the same IR type stably.
    /// And it supports the reservation and setting of the recursive struct / array type.
    type_map: HashMap<gane_sema::TypeId, TypeId>,
}

struct FunctionLowerer<'package, 'analysis> {
    package: &'package mut PackageLowerer<'analysis>,
    /// The `LoweredFunction`'s Id in working.
    /// The target for adding instruction, creating block and setting terminator.
    function: FunctionId,
    /// The position for the insert of instruction.
    /// When encounter `if`, loop, short-circuit expression or null check, it will switch to another block.
    block: BlockId,
    /// The return type of current function.
    results: Vec<TypeId>,
    /// The mapping from `sema` local variable object to IR `Place`.
    /// `Place` normally contains a pointer to stack slot and variable type.
    locals: HashMap<ObjectId, Place>,
    /// The set of the known non-null pointers.
    /// When take an operation on these pointer such as load, store, field access and so on,
    /// the null check will be ignored.
    known_non_null: HashSet<ValueId>,
    /// The current nested-loop stack.
    /// Each `Loop` stores the loop header and exit block, which are used to implement continue and break.
    loops: Vec<Loop>,
}

impl<'a> PackageLowerer<'a> {
    fn new(analysis: &'a AnalysisResult, target: crate::TargetSpec) -> Self {
        let target_width = target.pointer_width();
        Self {
            analysis,
            builder: IrBuilder::new(target),
            target_width,
            globals: HashMap::new(),
            functions: HashMap::new(),
            pointer_types: HashMap::new(),
            type_map: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests;
