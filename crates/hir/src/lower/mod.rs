use crate::{
    BinaryOp, BlockId, BuildError, Callee, ComparePredicate, Constant, FunctionAttributes,
    FunctionId, GlobalId, GlobalInitializer, HirBuilder, HirGlobal, HirParameter, HirSignature,
    HirTypeKind, IntCastKind, Terminator, TrapReason, TypeId, UnaryOp, UnverifiedHirPackage,
    ValueId,
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

mod declarations;
mod expressions;
mod memory;
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
                formatter.write_str("constant cannot be represented by its HIR type")
            }
            Self::Build { source, .. } => write!(formatter, "HIR builder failed: {source}"),
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
) -> Result<UnverifiedHirPackage, LowerError> {
    let errors = analysis
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        let node = errors[0]
            .primary
            .node
            .or_else(|| input.files.first().map(|file| file.ast.node_id()))
            .unwrap_or(AstNodeId::INVALID);
        return Err(LowerError::SemanticErrors {
            node,
            count: errors.len(),
        });
    }

    Lowerer::new(analysis, target).lower(input)
}

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
    signature: HirSignature,
}

#[derive(Clone, Copy)]
struct Loop {
    header: BlockId,
    exit: Option<BlockId>,
}

struct Lowerer<'a> {
    analysis: &'a AnalysisResult,
    builder: HirBuilder,
    function: FunctionId,
    block: BlockId,
    target_width: u8,
    locals: HashMap<ObjectId, Place>,
    globals: HashMap<ObjectId, GlobalId>,
    functions: HashMap<ObjectId, LoweredFunction>,
    results: Vec<TypeId>,
    pointer_types: HashMap<TypeId, TypeId>,
    type_map: HashMap<gane_sema::TypeId, TypeId>,
    known_non_null: HashSet<ValueId>,
    loops: Vec<Loop>,
}

impl<'a> Lowerer<'a> {
    fn new(analysis: &'a AnalysisResult, target: crate::TargetSpec) -> Self {
        let target_width = target.pointer_width();
        Self {
            analysis,
            builder: HirBuilder::new(target),
            function: FunctionId::INVALID,
            block: BlockId::INVALID,
            target_width,
            locals: HashMap::new(),
            globals: HashMap::new(),
            functions: HashMap::new(),
            results: Vec::new(),
            pointer_types: HashMap::new(),
            type_map: HashMap::new(),
            known_non_null: HashSet::new(),
            loops: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests;
