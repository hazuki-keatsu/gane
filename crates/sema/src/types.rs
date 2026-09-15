//! Core semantic data structures.
//!
//! This module owns identity and type representation for semantic analysis.
//! Objects, scopes, packages, and checker results use the same stable arena IDs
//! so they can refer to recursive declarations without Rust reference cycles.

use gane_parser::token::AstNodeId;
use std::collections::{BTreeMap, HashMap};

macro_rules! arena_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
        pub struct $name(u32);

        impl $name {
            pub const fn from_raw(raw: u32) -> Self {
                Self(raw)
            }
            pub const fn raw(self) -> u32 {
                self.0
            }
        }
    };
}

arena_id!(TypeId);
arena_id!(ObjectId);
arena_id!(ScopeId);
arena_id!(NameId);
arena_id!(PackageId);
arena_id!(FileId);
arena_id!(TupleId);

impl TypeId {
    /// The canonical poison type. TypeArena always stores it at index zero.
    pub const INVALID: Self = Self(0);
}

impl ObjectId {
    /// The canonical invalid object. ObjectArena always stores it at index zero.
    pub const INVALID: Self = Self(0);
}

impl ScopeId {
    /// The root lexical scope containing predeclared names.
    pub const UNIVERSE: Self = Self(0);
}

/// A declared or predeclared entity that can be found by name lookup.
#[derive(Clone, Debug)]
pub struct Object {
    pub kind: ObjectKind,
    pub name: NameId,
    pub package: Option<PackageId>,
    pub parent: ScopeId,
    /// Source declaration identity, if this object originates in source.
    pub declaration: Option<AstNodeId>,
    /// Always valid; semantic errors use [`TypeId::INVALID`].
    pub typ: TypeId,
}

#[derive(Clone, Debug)]
pub enum ObjectKind {
    Invalid,
    Const { value: ConstValue },
    Var { embedded: bool },
    Func { signature: TypeId },
    TypeName { named: TypeId, is_alias: bool },
    Field { index: u32, embedded: bool },
    Param { index: u32 },

    // Reserved for Go features outside the MVP.
    Builtin,
    Nil,
    PkgName { package: PackageId },
    Label,
    TypeParam { index: u32 },
}

/// A compile-time constant. `IntegerValue` is arbitrary precision without
/// imposing a big-number dependency on the rest of sema.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConstValue {
    Unknown,
    Bool(bool),
    Int(IntegerValue),
    // Future: Rune, Rational, Complex, String.
}

/// An arbitrary-precision signed integer in little-endian base 2^32 limbs.
/// Zero is always represented with `negative == false` and no limbs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntegerValue {
    pub negative: bool,
    pub limbs: Vec<u32>,
}

impl IntegerValue {
    pub const ZERO: Self = Self {
        negative: false,
        limbs: Vec::new(),
    };

    pub fn from_u64(value: u64) -> Self {
        if value == 0 {
            return Self::ZERO;
        }
        let low = value as u32;
        let high = (value >> 32) as u32;
        let limbs = if high == 0 {
            vec![low]
        } else {
            vec![low, high]
        };
        Self {
            negative: false,
            limbs,
        }
    }

    pub fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    pub fn from_i128(value: i128) -> Self {
        let negative = value.is_negative();
        let mut magnitude = value.unsigned_abs();
        let mut limbs = Vec::new();
        while magnitude != 0 {
            limbs.push(magnitude as u32);
            magnitude >>= 32;
        }
        Self { negative, limbs }
    }

    /// Returns None only when the arbitrary-precision value exceeds i128.
    pub fn to_i128(&self) -> Option<i128> {
        if self.limbs.len() > 4 {
            return None;
        }
        let magnitude = self
            .limbs
            .iter()
            .enumerate()
            .fold(0_u128, |value, (index, limb)| {
                value | ((*limb as u128) << (index * 32))
            });
        if self.negative {
            if magnitude == i128::MAX as u128 + 1 {
                Some(i128::MIN)
            } else {
                (magnitude <= i128::MAX as u128).then(|| -(magnitude as i128))
            }
        } else {
            (magnitude <= i128::MAX as u128).then(|| magnitude as i128)
        }
    }
}

/// A lexical scope. It owns only name bindings; type and control-flow rules
/// belong to their respective checker modules.
#[derive(Clone, Debug)]
pub struct Scope {
    pub parent: Option<ScopeId>,
    #[allow(dead_code)]
    pub kind: ScopeKind,
    #[allow(dead_code)]
    pub anchor: Option<AstNodeId>,
    /// BTreeMap gives deterministic diagnostics and test output.
    pub names: BTreeMap<NameId, ObjectId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScopeKind {
    Universe,
    Package,
    File,
    Function,
    Block,
    // Future: If, For, Switch, TypeParams, Labels.
}

/// Package identity and its top-level scope. Loading source files and import
/// resolution deliberately remain outside this data structure.
#[derive(Clone, Debug)]
pub struct Package {
    pub id: PackageId,
    pub name: NameId,
    pub path: PackagePath,
    pub scope: ScopeId,
    pub files: Vec<FileId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct PackagePath(pub String);

/// A single entry in the type arena.
#[derive(Clone, Debug)]
pub struct Type {
    pub kind: TypeKind,
}

/// The structural form of a type. Identity for named types is their TypeId,
/// not the identity of their underlying type.
#[derive(Clone, Debug)]
pub enum TypeKind {
    Invalid,
    Basic(BasicType),
    Named {
        object: ObjectId,
        underlying: UnderlyingState,
        methods: Vec<ObjectId>,
    },
    Pointer {
        base: TypeId,
    },
    Array {
        len: ConstValue,
        elem: TypeId,
    },
    Struct {
        fields: Vec<ObjectId>,
    },
    Signature {
        receiver: Option<ObjectId>,
        params: TupleId,
        results: TupleId,
        variadic: bool,
    },

    // Future Go forms. The MVP checker must reject source that creates them.
    Slice {
        elem: TypeId,
    },
    Map {
        key: TypeId,
        value: TypeId,
    },
    Interface {
        methods: Vec<ObjectId>,
        complete: bool,
    },
    Chan {
        direction: ChanDirection,
        elem: TypeId,
    },
    TypeParam {
        object: ObjectId,
        constraint: TypeId,
    },
    Instance {
        origin: TypeId,
        args: Vec<TypeId>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BasicType {
    Void,
    Bool,
    Int,
    Byte,
    // Future: String, signed/unsigned widths, Float*, Complex*, UnsafePointer.
}

/// The resolution state prevents recursive named-type declarations from being
/// mistaken for completed types.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnderlyingState {
    Unresolved,
    Resolving,
    Resolved(TypeId),
    Invalid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChanDirection {
    Send,
    Receive,
    Both,
}

/// Function parameter or result variables. A tuple is deliberately not a
/// TypeKind: like go/types.Tuple, it is a signature component rather than a
/// first-class Go type.
#[derive(Clone, Debug, Default)]
pub struct Tuple {
    pub vars: Vec<ObjectId>,
}

/// Owns all semantic types and signature tuples. Index zero is permanently
/// reserved for `Invalid`, making error recovery cheap and uniform.
#[derive(Clone, Debug)]
pub struct TypeArena {
    types: Vec<Type>,
    tuples: Vec<Tuple>,
}

impl Default for TypeArena {
    fn default() -> Self {
        Self {
            types: vec![Type {
                kind: TypeKind::Invalid,
            }],
            tuples: Vec::new(),
        }
    }
}

impl TypeArena {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn alloc(&mut self, kind: TypeKind) -> TypeId {
        let id = TypeId::from_raw(self.types.len() as u32);
        self.types.push(Type { kind });
        id
    }

    pub fn get(&self, id: TypeId) -> &Type {
        self.types.get(id.raw() as usize).unwrap_or(&self.types[0])
    }

    pub fn get_mut(&mut self, id: TypeId) -> Option<&mut Type> {
        self.types.get_mut(id.raw() as usize)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.types.len()
    }

    pub fn alloc_tuple(&mut self, tuple: Tuple) -> TupleId {
        let id = TupleId::from_raw(self.tuples.len() as u32);
        self.tuples.push(tuple);
        id
    }

    pub fn tuple(&self, id: TupleId) -> Option<&Tuple> {
        self.tuples.get(id.raw() as usize)
    }
    /// Returns the final non-named type when a named type has been resolved.
    /// Invalid or unresolved named types remain unchanged for error recovery.
    pub fn underlying(&self, mut ty: TypeId) -> TypeId {
        let mut remaining = self.types.len();
        while remaining > 0 {
            remaining -= 1;
            match self.get(ty).kind {
                TypeKind::Named {
                    underlying: UnderlyingState::Resolved(next),
                    ..
                } => ty = next,
                _ => break,
            }
        }
        ty
    }

    pub fn identical(&self, left: TypeId, right: TypeId) -> bool {
        if left == right {
            return true;
        }
        match (&self.get(left).kind, &self.get(right).kind) {
            (TypeKind::Basic(left), TypeKind::Basic(right)) => left == right,
            (TypeKind::Pointer { base: left }, TypeKind::Pointer { base: right }) => {
                self.identical(*left, *right)
            }
            (
                TypeKind::Array {
                    len: left_len,
                    elem: left_elem,
                },
                TypeKind::Array {
                    len: right_len,
                    elem: right_elem,
                },
            ) => left_len == right_len && self.identical(*left_elem, *right_elem),
            _ => false,
        }
    }

    /// MVP assignment is exact type identity, except that Invalid suppresses
    /// follow-up errors. Future Go assignment rules extend this one query.
    pub fn assignable_to(&self, source: TypeId, target: TypeId) -> bool {
        source == TypeId::INVALID || target == TypeId::INVALID || self.identical(source, target)
    }

    pub fn is_basic(&self, ty: TypeId, basic: BasicType) -> bool {
        matches!(self.get(self.underlying(ty)).kind, TypeKind::Basic(found) if found == basic)
    }

    pub fn deref(&self, ty: TypeId) -> Option<TypeId> {
        match self.get(self.underlying(ty)).kind {
            TypeKind::Pointer { base } => Some(base),
            _ => None,
        }
    }

    pub fn array_element(&self, ty: TypeId) -> Option<TypeId> {
        match self.get(self.underlying(ty)).kind {
            TypeKind::Array { elem, .. } => Some(elem),
            _ => None,
        }
    }

    pub fn comparable(&self, ty: TypeId) -> bool {
        matches!(
            self.get(self.underlying(ty)).kind,
            TypeKind::Basic(BasicType::Bool | BasicType::Int | BasicType::Byte)
                | TypeKind::Pointer { .. }
        )
    }
}

/// Immutable semantic facts made available after checking a package.
#[derive(Clone, Debug, Default)]
pub struct SemanticInfo {
    pub(crate) defs: HashMap<AstNodeId, ObjectId>,
    pub(crate) uses: HashMap<AstNodeId, ObjectId>,
    pub(crate) types: HashMap<AstNodeId, TypeAndValue>,
    pub(crate) scopes: HashMap<AstNodeId, ScopeId>,
    pub(crate) selections: HashMap<AstNodeId, Selection>,
}

#[derive(Clone, Debug)]
pub struct TypeAndValue {
    pub typ: TypeId,
    pub mode: ValueMode,
    pub constant: Option<ConstValue>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueMode {
    Invalid,
    NoValue,
    Value,
    Variable,
    TypeExpr,
    Builtin,
    Nil,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Selection {
    pub object: ObjectId,
    pub kind: SelectionKind,
    /// Field embedding path; MVP selections contain exactly one field index.
    pub index: Vec<u32>,
    pub indirect: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectionKind {
    Field,
    MethodValue,
    MethodExpr,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_type_is_reserved_at_zero() {
        let arena = TypeArena::new();
        assert!(matches!(arena.get(TypeId::INVALID).kind, TypeKind::Invalid));
        assert_eq!(arena.len(), 1);
    }

    #[test]
    fn allocated_types_have_stable_distinct_ids() {
        let mut arena = TypeArena::new();
        let boolean = arena.alloc(TypeKind::Basic(BasicType::Bool));
        let integer = arena.alloc(TypeKind::Basic(BasicType::Int));
        assert_ne!(boolean, integer);
        assert_eq!(boolean.raw(), 1);
        assert!(matches!(
            arena.get(integer).kind,
            TypeKind::Basic(BasicType::Int)
        ));
    }

    #[test]
    fn integer_zero_has_a_canonical_representation() {
        assert_eq!(IntegerValue::from_u64(0), IntegerValue::ZERO);
        assert!(IntegerValue::ZERO.is_zero());
        assert!(!IntegerValue::from_u64(1).is_zero());
    }
}
