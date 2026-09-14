//! The public seam of semantic analysis.
//!
//! An analysis is created with [`analyze_package`] and then queried through
//! [`AnalysisResult`]. Arena storage, name interning, and lexical lookup stay
//! behind this module so callers depend on semantic identities rather than the
//! checker's implementation layout.

pub use crate::{
    checker::analyze_package,
    types::{
        BasicType, ChanDirection, ConstValue, FileId, IntegerValue, NameId, NodeId, Object,
        ObjectId, ObjectKind, Package, PackageId, PackagePath, ScopeId, Selection, SelectionKind,
        SemanticInfo, Tuple, TupleId, Type, TypeAndValue, TypeId, TypeKind, UnderlyingState,
        ValueMode,
    },
};
pub use gane_diagnostics::{Diagnostic, DiagnosticCode, Severity, Span};

pub use crate::checker::{AnalysisResult, PackageFile, PackageInput, PredeclaredTypes};

impl AnalysisResult {
    /// Returns the object for `id`, or the canonical invalid object for an
    /// unknown ID. Invalid IDs are intentionally safe during error recovery.
    pub fn object(&self, id: ObjectId) -> &Object {
        self.symbols.object(id)
    }

    /// Resolves an interned identifier back to its source spelling.
    pub fn name(&self, id: NameId) -> Option<&str> {
        self.symbols.name(id)
    }

    /// Returns the type for `id`, or the canonical invalid type for an unknown
    /// ID. Invalid IDs are intentionally safe during error recovery.
    pub fn type_of(&self, id: TypeId) -> &Type {
        self.types.get(id)
    }

    /// Returns a function's parameter or result tuple when `id` is valid.
    pub fn tuple(&self, id: TupleId) -> Option<&Tuple> {
        self.types.tuple(id)
    }

    /// Finds a package-level declaration by source spelling.
    pub fn package_member(&self, spelling: &str) -> Option<ObjectId> {
        self.scopes
            .scope(self.package.scope)?
            .names
            .iter()
            .find_map(|(&name, &object)| (self.name(name) == Some(spelling)).then_some(object))
    }

    /// Returns the lexical scope associated with an input source file.
    pub fn file_scope(&self, file: FileId) -> Option<ScopeId> {
        self.file_scopes.get(&file).copied()
    }

    /// Returns the checker-local identity assigned to a source span.
    pub fn node_at(&self, span: Span) -> Option<NodeId> {
        self.nodes.get(span)
    }

    /// Returns the final underlying type of a resolved named type.
    pub fn underlying_type(&self, typ: TypeId) -> TypeId {
        self.types.underlying(typ)
    }

    /// Tests semantic type identity using the same rules used by checking.
    pub fn identical_types(&self, left: TypeId, right: TypeId) -> bool {
        self.types.identical(left, right)
    }

    /// Tests whether an assignment is accepted by the current language subset.
    pub fn is_assignable(&self, source: TypeId, target: TypeId) -> bool {
        self.types.assignable_to(source, target)
    }

    /// Tests whether `typ` is a basic type after named-type unwrapping.
    pub fn is_basic_type(&self, typ: TypeId, basic: BasicType) -> bool {
        self.types.is_basic(typ, basic)
    }

    /// Returns the pointee type when `typ` is a pointer after unwrapping.
    pub fn deref_type(&self, typ: TypeId) -> Option<TypeId> {
        self.types.deref(typ)
    }

    /// Returns the element type when `typ` is an array after unwrapping.
    pub fn array_element_type(&self, typ: TypeId) -> Option<TypeId> {
        self.types.array_element(typ)
    }

    /// Reports whether `typ` is comparable in the current language subset.
    pub fn is_comparable_type(&self, typ: TypeId) -> bool {
        self.types.comparable(typ)
    }

    /// Reports whether at least one error diagnostic was emitted.
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
    }
}
