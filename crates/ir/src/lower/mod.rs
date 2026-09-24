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

mod expressions;
mod function;
mod memory;
mod package;
mod statements;
mod types;

#[derive(Debug)]
pub enum LowerError {
    SemanticErrors {
        node: AstNodeId,
        count: usize,
    },
    MissingSemanticFact {
        node: AstNodeId,
        fact: &'static str,
    },
    Unsupported {
        node: AstNodeId,
        construct: &'static str,
    },
    InvalidConstant {
        node: AstNodeId,
    },
    Build {
        node: AstNodeId,
        source: BuildError,
    },
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
    globals: HashMap<ObjectId, GlobalId>,
    functions: HashMap<ObjectId, LoweredFunction>,
    pointer_types: HashMap<TypeId, TypeId>,
    type_map: HashMap<gane_sema::TypeId, TypeId>,
}

struct FunctionLowerer<'package, 'analysis> {
    package: &'package mut PackageLowerer<'analysis>,
    function: FunctionId,
    block: BlockId,
    results: Vec<TypeId>,
    locals: HashMap<ObjectId, Place>,
    known_non_null: HashSet<ValueId>,
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
