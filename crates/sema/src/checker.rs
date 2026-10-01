//! Package-level declaration collection.
//!
//! This is the first semantic checker slice: it creates the universe and
//! package scopes, records top-level objects, and fills definition facts. Type
//! headers, values, and function bodies deliberately remain later phases.

use std::collections::{HashMap, HashSet};

use gane_diagnostics::{Diagnostic, DiagnosticCode, Diagnostics, Label};
use gane_parser::{
    ast::{self, Decl, Spec},
    token::{AstNodeId, Token},
};

use crate::{
    scope::{DeclareError, DuplicateDeclaration, ScopeArena},
    symbol_table::{SymbolTable, declared_object},
    types::{
        BasicType, ConstValue, FileId, IntegerValue, ObjectId, ObjectKind, Package, PackageId,
        PackagePath, ScopeId, ScopeKind, SemanticInfo, TypeAndValue, TypeArena, TypeId, TypeKind,
        UnderlyingState, ValueMode,
    },
};

const DUPLICATE_DECLARATION: DiagnosticCode = DiagnosticCode("E2002");
const INVALID_PACKAGE: DiagnosticCode = DiagnosticCode("E2003");
const MIXED_PACKAGE: DiagnosticCode = DiagnosticCode("E2004");
const EMPTY_PACKAGE: DiagnosticCode = DiagnosticCode("E2005");
const DUPLICATE_FILE: DiagnosticCode = DiagnosticCode("E2006");
const MIXED_FILESET: DiagnosticCode = DiagnosticCode("E2007");
const UNKNOWN_TYPE: DiagnosticCode = DiagnosticCode("E2101");
const UNSUPPORTED_TYPE: DiagnosticCode = DiagnosticCode("E2102");
const INVALID_RECURSIVE_TYPE: DiagnosticCode = DiagnosticCode("E2103");
const DUPLICATE_FIELD: DiagnosticCode = DiagnosticCode("E2104");
const UNDEFINED_NAME: DiagnosticCode = DiagnosticCode("E2201");
const TYPE_USED_AS_VALUE: DiagnosticCode = DiagnosticCode("E2202");
const TYPE_MISMATCH: DiagnosticCode = DiagnosticCode("E2301");
const EXPECTED_VARIABLE: DiagnosticCode = DiagnosticCode("E2302");
const EXPECTED_BOOLEAN: DiagnosticCode = DiagnosticCode("E2303");
const INVALID_OPERATION: DiagnosticCode = DiagnosticCode("E2304");
const MISSING_VARIABLE_TYPE: DiagnosticCode = DiagnosticCode("E2305");
const INVALID_RETURN: DiagnosticCode = DiagnosticCode("E2401");
const MISSING_RETURN: DiagnosticCode = DiagnosticCode("E2402");
const INVALID_BRANCH: DiagnosticCode = DiagnosticCode("E2403");
const INVALID_ENTRY_POINT: DiagnosticCode = DiagnosticCode("E2404");
const UNSUPPORTED_FEATURE: DiagnosticCode = DiagnosticCode("E2405");

/// An AST file selected by the package loader. Loading, build constraints, and
/// import resolution stay outside sema.
#[derive(Clone, Copy, Debug)]
pub struct PackageFile<'ast> {
    pub id: FileId,
    pub ast: &'ast ast::File,
}

/// Input for one semantic package check.
#[derive(Clone, Debug)]
pub struct PackageInput<'ast> {
    pub path: PackagePath,
    pub files: Vec<PackageFile<'ast>>,
}

impl<'ast> PackageInput<'ast> {
    pub fn single(path: impl Into<String>, id: FileId, ast: &'ast ast::File) -> Self {
        Self {
            path: PackagePath(path.into()),
            files: vec![PackageFile { id, ast }],
        }
    }
}

/// Built-in type identities shared by all later checking phases.
#[derive(Clone, Copy, Debug)]
pub struct PredeclaredTypes {
    pub void: TypeId,
    pub bool_: TypeId,
    pub int: TypeId,
    pub byte: TypeId,
}

/// The IR-relevant, compile-time result of a package variable initializer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GlobalInitializer {
    /// The source declaration has Gane's ordinary zero initialization.
    Zero,
    /// A scalar bool/int/byte constant initializer.
    Scalar(ConstValue),
}

/// Frozen output of package declaration collection and type-header resolution.
#[derive(Clone, Debug)]
pub struct AnalysisResult {
    pub package: Package,
    pub predeclared: PredeclaredTypes,
    pub(crate) types: TypeArena,
    pub(crate) symbols: SymbolTable,
    pub(crate) scopes: ScopeArena,
    pub(crate) file_scopes: HashMap<FileId, ScopeId>,
    pub(crate) info: SemanticInfo,
    pub(crate) global_initializers: HashMap<ObjectId, GlobalInitializer>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Analyzes one package. Function bodies and declaration value expressions are
/// intentionally deferred, but type headers and function signatures are complete.
pub fn analyze_package(input: PackageInput<'_>) -> AnalysisResult {
    let mut checker = Checker::new(input);
    checker.check_package_clause();
    checker.create_universe();
    checker.create_package_scope();
    checker.create_file_scopes();
    checker.collect_top_level();
    checker.resolve_type_headers();
    checker.check_global_values();
    checker.check_function_bodies();
    checker.validate_entry_point();
    checker.finish()
}

struct Checker<'ast> {
    input: PackageInput<'ast>,
    types: TypeArena,
    symbols: SymbolTable,
    scopes: ScopeArena,
    info: SemanticInfo,
    diagnostics: Diagnostics,
    package_name: Option<crate::types::NameId>,
    package_scope: Option<ScopeId>,
    file_scopes: HashMap<FileId, ScopeId>,
    predeclared: Option<PredeclaredTypes>,
    type_specs: HashMap<TypeId, ScopedTypeSpec>,
    func_decls: HashMap<TypeId, ScopedFuncDecl>,
    function_scopes: HashMap<TypeId, ScopeId>,
    pending_global_initializers: HashMap<ObjectId, PendingGlobalInitializer>,
    resolved_global_initializers: HashMap<ObjectId, GlobalInitializer>,
    global_states: HashMap<ObjectId, InitState>,
}

struct ControlContext {
    results: Vec<TypeId>,
    loop_breaks: Vec<bool>,
}

#[derive(Clone)]
struct PendingGlobalInitializer {
    object: ObjectId,
    is_const: bool,
    typ: Option<ast::Expr>,
    value: Option<ast::Expr>,
    anchor: Option<AstNodeId>,
    file: FileId,
}

#[derive(Clone)]
struct ScopedTypeSpec {
    spec: ast::TypeSpec,
    file: FileId,
}

#[derive(Clone)]
struct ScopedFuncDecl {
    decl: ast::FuncDecl,
    file: FileId,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum InitState {
    Resolving,
    Done,
}

impl<'ast> Checker<'ast> {
    fn new(input: PackageInput<'ast>) -> Self {
        Self {
            input,
            types: TypeArena::new(),
            symbols: SymbolTable::new(),
            scopes: ScopeArena::new(),
            info: SemanticInfo::default(),
            diagnostics: Diagnostics::default(),
            package_name: None,
            package_scope: None,
            file_scopes: HashMap::new(),
            predeclared: None,
            type_specs: HashMap::new(),
            func_decls: HashMap::new(),
            function_scopes: HashMap::new(),
            pending_global_initializers: HashMap::new(),
            resolved_global_initializers: HashMap::new(),
            global_states: HashMap::new(),
        }
    }

    fn check_package_clause(&mut self) {
        let Some(first) = self.input.files.first() else {
            self.diagnostics
                .error(EMPTY_PACKAGE, None, "package contains no source files");
            self.package_name = Some(self.symbols.intern(""));
            return;
        };

        let expected = first.ast.name.name.as_str();
        let session = first.ast.node_id().parse_session();
        self.package_name = Some(self.symbols.intern(expected));
        if expected != "main" {
            self.diagnostics.error(
                INVALID_PACKAGE,
                Some(first.ast.name.node_id()),
                "MVP only supports `package main`",
            );
        }

        for file in self.input.files.iter().skip(1) {
            if file.ast.node_id().parse_session() != session {
                self.diagnostics.error(
                    MIXED_FILESET,
                    Some(file.ast.node_id()),
                    "all package files must come from the same parser FileSet",
                );
            }
            if file.ast.name.name != expected {
                self.diagnostics.error(
                    MIXED_PACKAGE,
                    Some(file.ast.name.node_id()),
                    format!(
                        "file belongs to package `{}`, expected `{expected}`",
                        file.ast.name.name
                    ),
                );
            }
        }
    }

    fn create_universe(&mut self) {
        let void = self.types.alloc(TypeKind::Basic(BasicType::Void));
        let bool_ = self.types.alloc(TypeKind::Basic(BasicType::Bool));
        let int = self.types.alloc(TypeKind::Basic(BasicType::Int));
        let byte = self.types.alloc(TypeKind::Basic(BasicType::Byte));
        let predeclared = PredeclaredTypes {
            void,
            bool_,
            int,
            byte,
        };
        self.predeclared = Some(predeclared);

        self.declare_predeclared_type("bool", bool_);
        self.declare_predeclared_type("int", int);
        self.declare_predeclared_type("byte", byte);
        self.declare_predeclared_const("true", bool_, ConstValue::Bool(true));
        self.declare_predeclared_const("false", bool_, ConstValue::Bool(false));
        self.declare_predeclared_nil();
    }

    fn create_package_scope(&mut self) {
        let anchor = self
            .input
            .files
            .first()
            .map(|file| Some(file.ast.node_id()))
            .unwrap_or_default();
        self.package_scope = Some(self.scopes.child(
            self.scopes.universe(),
            ScopeKind::Package,
            anchor,
        ));
    }

    fn create_file_scopes(&mut self) {
        let package_scope = self.package_scope.expect("package scope must exist");
        let files = self
            .input
            .files
            .iter()
            .map(|file| (file.id, Some(file.ast.node_id())))
            .collect::<Vec<_>>();

        for (file, anchor) in files {
            if self.file_scopes.contains_key(&file) {
                self.diagnostics.error(
                    DUPLICATE_FILE,
                    anchor,
                    format!("duplicate input file id {}", file.raw()),
                );
                continue;
            }
            let scope = self.scopes.child(package_scope, ScopeKind::File, anchor);
            self.file_scopes.insert(file, scope);
        }
    }

    fn collect_top_level(&mut self) {
        // Clone per-file declaration lists so collection can mutably update
        // checker arenas without holding an immutable borrow of self.input.
        let files: Vec<(FileId, Vec<Decl>)> = self
            .input
            .files
            .iter()
            .map(|file| (file.id, file.ast.decls.clone()))
            .collect();

        for (file, declarations) in files {
            for declaration in &declarations {
                match declaration {
                    Decl::GenDecl(decl) => self.collect_gen_decl(decl, file),
                    Decl::FuncDecl(decl) => self.collect_func_decl(decl, file),
                    Decl::BadDecl(_) => {}
                }
            }
        }
    }

    fn collect_gen_decl(&mut self, decl: &ast::GenDecl, file: FileId) {
        match decl.tok {
            Token::Type => {
                for spec in &decl.specs {
                    if let Spec::TypeSpec(spec) = spec {
                        self.collect_type_spec(spec, file);
                    }
                }
            }
            Token::Const => {
                for spec in &decl.specs {
                    if let Spec::ValueSpec(spec) = spec {
                        self.collect_value_spec(spec, true, file);
                    }
                }
            }
            Token::Var => {
                for spec in &decl.specs {
                    if let Spec::ValueSpec(spec) = spec {
                        self.collect_value_spec(spec, false, file);
                    }
                }
            }
            Token::Import => self.unsupported(Some(decl.node_id()), "import"),
            _ => {}
        }
    }

    fn collect_type_spec(&mut self, spec: &ast::TypeSpec, file: FileId) {
        if spec.assign.is_valid() {
            self.unsupported(Some(spec.node_id()), "type alias");
        }
        if spec.type_params.is_some() {
            self.unsupported(Some(spec.node_id()), "type parameters");
        }
        let name = self.symbols.intern(&spec.name.name);
        let named = self.types.alloc(TypeKind::Named {
            object: ObjectId::INVALID,
            underlying: UnderlyingState::Unresolved,
            methods: Vec::new(),
        });
        let object = self.declare_source_object(
            &spec.name,
            ObjectKind::TypeName {
                named,
                is_alias: spec.assign.is_valid(),
            },
            name,
            named,
        );
        if let TypeKind::Named { object: slot, .. } = &mut self.types.get_mut(named).unwrap().kind {
            *slot = object;
        }
        self.type_specs.insert(
            named,
            ScopedTypeSpec {
                spec: spec.clone(),
                file,
            },
        );
    }

    fn collect_func_decl(&mut self, decl: &ast::FuncDecl, file: FileId) {
        if decl.recv.is_some() {
            self.unsupported(Some(decl.node_id()), "method declaration");
        }
        if decl.typ.type_params.is_some() {
            self.unsupported(Some(decl.node_id()), "type parameters");
        }
        if decl.body.is_none() {
            self.unsupported(Some(decl.node_id()), "function declaration without a body");
        }
        let params = self.types.alloc_tuple(Default::default());
        let results = self.types.alloc_tuple(Default::default());
        let signature = self.types.alloc(TypeKind::Signature {
            receiver: None,
            params,
            results,
            variadic: false,
        });
        let name = self.symbols.intern(&decl.name.name);
        self.declare_source_object(&decl.name, ObjectKind::Func { signature }, name, signature);
        self.func_decls.insert(
            signature,
            ScopedFuncDecl {
                decl: decl.clone(),
                file,
            },
        );
    }

    fn collect_value_spec(&mut self, spec: &ast::ValueSpec, is_const: bool, file: FileId) {
        for (index, ident) in spec.names.iter().enumerate() {
            let name = self.symbols.intern(&ident.name);
            let kind = if is_const {
                ObjectKind::Const {
                    value: ConstValue::Unknown,
                }
            } else {
                ObjectKind::Var { embedded: false }
            };
            let object = self.declare_source_object(ident, kind, name, TypeId::INVALID);
            self.pending_global_initializers.insert(
                object,
                PendingGlobalInitializer {
                    object,
                    is_const,
                    typ: spec.typ.clone(),
                    value: spec.values.get(index).cloned(),
                    anchor: Some(ident.node_id()),
                    file,
                },
            );
        }
    }

    fn resolve_type_headers(&mut self) {
        let named: Vec<TypeId> = self.type_specs.keys().copied().collect();
        for ty in named {
            self.resolve_named(ty, false);
        }

        let signatures: Vec<TypeId> = self.func_decls.keys().copied().collect();
        for signature in signatures {
            self.resolve_signature(signature);
        }
    }

    fn check_global_values(&mut self) {
        let objects: Vec<ObjectId> = self.pending_global_initializers.keys().copied().collect();
        for object in objects {
            self.resolve_global(object);
        }
    }

    fn resolve_global(&mut self, object: ObjectId) {
        match self.global_states.get(&object).copied() {
            Some(InitState::Done) => return,
            Some(InitState::Resolving) => {
                self.diagnostics.error(
                    TYPE_MISMATCH,
                    self.symbols.object(object).declaration,
                    "initialization cycle",
                );
                if let Some(global) = self.symbols.object_mut(object) {
                    global.typ = TypeId::INVALID;
                }
                return;
            }
            None => {}
        }
        let Some(initializer) = self.pending_global_initializers.get(&object).cloned() else {
            return;
        };
        self.global_states.insert(object, InitState::Resolving);
        let scope = self.file_scopes[&initializer.file];
        let explicit_type = initializer
            .typ
            .as_ref()
            .map(|typ| self.resolve_type_expr(typ, scope, false));
        let value = initializer
            .value
            .as_ref()
            .map(|value| self.check_expr(value, scope));
        let typ = match (explicit_type, value.as_ref()) {
            (Some(typ), Some(value)) => {
                self.require_assignable(value.typ, typ, initializer.anchor);
                typ
            }
            (Some(typ), None) if !initializer.is_const => typ,
            (None, Some(value)) if value.mode != ValueMode::Nil => value.typ,
            (None, Some(_)) => {
                self.diagnostics.error(
                    MISSING_VARIABLE_TYPE,
                    initializer.anchor,
                    "cannot infer a variable type from nil",
                );
                TypeId::INVALID
            }
            _ => {
                self.diagnostics.error(
                    TYPE_MISMATCH,
                    initializer.anchor,
                    "constant or variable requires an initializer or explicit type",
                );
                TypeId::INVALID
            }
        };
        if initializer.is_const && value.is_none() {
            self.diagnostics.error(
                TYPE_MISMATCH,
                initializer.anchor,
                "constant declaration requires an initializer",
            );
        }
        let initializer_is_valid =
            self.validate_v0_global_initializer(&initializer, typ, value.as_ref());
        let constant = value.as_ref().and_then(|value| value.constant.clone());
        if let Some(global) = self.symbols.object_mut(initializer.object) {
            global.typ = typ;
            if let ObjectKind::Const { value } = &mut global.kind {
                *value = constant.unwrap_or(ConstValue::Unknown);
            }
        }
        if !initializer.is_const && initializer_is_valid {
            let resolved = match value.as_ref().and_then(|value| value.constant.clone()) {
                Some(value) => GlobalInitializer::Scalar(value),
                None => GlobalInitializer::Zero,
            };
            self.resolved_global_initializers.insert(object, resolved);
        }
        self.global_states.insert(object, InitState::Done);
    }

    fn validate_v0_global_initializer(
        &mut self,
        initializer: &PendingGlobalInitializer,
        typ: TypeId,
        value: Option<&TypeAndValue>,
    ) -> bool {
        let Some(value) = value else {
            return typ != TypeId::INVALID;
        };
        if typ == TypeId::INVALID || value.mode == ValueMode::Invalid {
            return false;
        }
        let allowed = match self.types.get(self.types.underlying(typ)).kind {
            TypeKind::Basic(BasicType::Bool) => {
                matches!(value.constant, Some(ConstValue::Bool(_)))
            }
            TypeKind::Basic(BasicType::Int | BasicType::Byte) => {
                matches!(value.constant, Some(ConstValue::Int(_)))
            }
            TypeKind::Pointer { .. } => value.mode == ValueMode::Nil,
            TypeKind::Array { .. } | TypeKind::Struct { .. } => false,
            _ => false,
        };
        if !allowed {
            self.unsupported(
                initializer.anchor,
                "global initializer requiring runtime evaluation",
            );
        }
        allowed
    }

    /// Resolves a named type's underlying type. `indirect` records whether the
    /// use which re-enters an in-progress declaration passed through a pointer.
    fn resolve_named(&mut self, named: TypeId, indirect: bool) -> TypeId {
        let state = match &self.types.get(named).kind {
            TypeKind::Named { underlying, .. } => *underlying,
            _ => return named,
        };

        match state {
            UnderlyingState::Resolved(_) | UnderlyingState::Invalid => named,
            UnderlyingState::Resolving => {
                if !indirect {
                    self.diagnostics.error(
                        INVALID_RECURSIVE_TYPE,
                        None,
                        "invalid recursive type: cycle requires pointer indirection",
                    );
                    return TypeId::INVALID;
                }
                named
            }
            UnderlyingState::Unresolved => {
                let Some(declaration) = self.type_specs.get(&named).cloned() else {
                    return named;
                };
                if let TypeKind::Named { underlying, .. } =
                    &mut self.types.get_mut(named).unwrap().kind
                {
                    *underlying = UnderlyingState::Resolving;
                }
                let scope = self.file_scopes[&declaration.file];
                let underlying = self.resolve_type_expr(&declaration.spec.typ, scope, false);
                if let TypeKind::Named {
                    underlying: slot, ..
                } = &mut self.types.get_mut(named).unwrap().kind
                {
                    *slot = if underlying == TypeId::INVALID {
                        UnderlyingState::Invalid
                    } else {
                        UnderlyingState::Resolved(underlying)
                    };
                }
                named
            }
        }
    }

    fn resolve_type_expr(&mut self, expr: &ast::Expr, scope: ScopeId, indirect: bool) -> TypeId {
        match expr {
            ast::Expr::Ident(ident) => self.resolve_type_name(ident, scope, indirect),
            ast::Expr::ParenExpr(expr) => self.resolve_type_expr(&expr.x, scope, indirect),
            ast::Expr::StarExpr(expr) => {
                let base = self.resolve_type_expr(&expr.x, scope, true);
                if base == TypeId::INVALID {
                    TypeId::INVALID
                } else {
                    self.types.alloc(TypeKind::Pointer { base })
                }
            }
            ast::Expr::ArrayType(expr) => {
                let Some(length) = expr.len.as_ref().and_then(parse_array_length) else {
                    self.diagnostics.error(
                        UNSUPPORTED_TYPE,
                        None,
                        "MVP array length must be a non-negative decimal integer literal",
                    );
                    return TypeId::INVALID;
                };
                if matches!(&length, ConstValue::Int(value) if value.is_zero()) {
                    self.diagnostics.error(
                        UNSUPPORTED_TYPE,
                        Some(expr.node_id()),
                        "zero-length arrays are not supported by IR V0",
                    );
                    return TypeId::INVALID;
                }
                let element = self.resolve_type_expr(&expr.elt, scope, indirect);
                if element == TypeId::INVALID {
                    TypeId::INVALID
                } else {
                    self.types.alloc(TypeKind::Array {
                        len: length,
                        elem: element,
                    })
                }
            }
            ast::Expr::StructType(struct_type) => self.resolve_struct(struct_type, scope),
            _ => {
                self.diagnostics.error(
                    UNSUPPORTED_TYPE,
                    None,
                    "type syntax is not supported by the MVP",
                );
                TypeId::INVALID
            }
        }
    }

    fn resolve_type_name(&mut self, ident: &ast::Ident, scope: ScopeId, indirect: bool) -> TypeId {
        let name = self.symbols.intern(&ident.name);
        let Some(resolved) = self.scopes.lookup(scope, name) else {
            self.diagnostics.error(
                UNKNOWN_TYPE,
                Some(ident.node_id()),
                format!("undefined type `{}`", ident.name),
            );
            return TypeId::INVALID;
        };
        match self.symbols.object(resolved.object).kind {
            ObjectKind::TypeName { named, .. } => self.resolve_named(named, indirect),
            _ => {
                self.diagnostics.error(
                    UNKNOWN_TYPE,
                    Some(ident.node_id()),
                    format!("`{}` does not name a type", ident.name),
                );
                TypeId::INVALID
            }
        }
    }

    fn resolve_struct(&mut self, struct_type: &ast::StructType, scope: ScopeId) -> TypeId {
        let mut fields = Vec::new();
        let mut seen = HashSet::new();
        let mut invalid = false;
        let Some(field_list) = &struct_type.fields else {
            self.diagnostics.error(
                UNSUPPORTED_TYPE,
                Some(struct_type.node_id()),
                "empty structs are not supported by IR V0",
            );
            return TypeId::INVALID;
        };

        for field in &field_list.list {
            if field.tag.is_some() {
                self.diagnostics.error(
                    UNSUPPORTED_TYPE,
                    Some(field.node_id()),
                    "struct field tags are not supported by IR V0",
                );
            }
            let Some(typ_expr) = &field.typ else {
                self.diagnostics
                    .error(UNSUPPORTED_TYPE, None, "struct field has no type");
                continue;
            };
            let typ = self.resolve_type_expr(typ_expr, scope, false);
            invalid |= typ == TypeId::INVALID;
            if field.names.is_empty() {
                self.diagnostics.error(
                    UNSUPPORTED_TYPE,
                    Some(field.node_id()),
                    "embedded struct fields are not supported by the MVP",
                );
                continue;
            }
            for ident in &field.names {
                let name = self.symbols.intern(&ident.name);
                if !seen.insert(name) {
                    self.diagnostics.error(
                        DUPLICATE_FIELD,
                        Some(ident.node_id()),
                        format!("duplicate struct field `{}`", ident.name),
                    );
                }
                let index = fields.len() as u32;
                let object = self.declare_field(ident, name, index, typ);
                fields.push(object);
            }
        }
        if fields.is_empty() && !invalid {
            self.diagnostics.error(
                UNSUPPORTED_TYPE,
                Some(struct_type.node_id()),
                "empty structs are not supported by IR V0",
            );
            return TypeId::INVALID;
        }
        if invalid {
            TypeId::INVALID
        } else {
            self.types.alloc(TypeKind::Struct { fields })
        }
    }

    fn declare_field(
        &mut self,
        ident: &ast::Ident,
        name: crate::types::NameId,
        index: u32,
        typ: TypeId,
    ) -> ObjectId {
        let anchor = Some(ident.node_id());
        let object = self.symbols.alloc(declared_object(
            ObjectKind::Field {
                index,
                embedded: false,
            },
            name,
            Some(PackageId::from_raw(0)),
            self.package_scope.expect("package scope must exist"),
            anchor,
            typ,
        ));
        self.info.defs.insert(ident.node_id(), object);
        object
    }

    fn resolve_signature(&mut self, signature: TypeId) {
        let Some(declaration) = self.func_decls.get(&signature).cloned() else {
            return;
        };
        let decl = declaration.decl;
        let file_scope = self.file_scopes[&declaration.file];
        let function_scope =
            self.scopes
                .child(file_scope, ScopeKind::Function, Some(decl.node_id()));
        self.function_scopes.insert(signature, function_scope);
        let params = self.resolve_tuple(decl.typ.params.as_ref(), function_scope);
        let results = self.resolve_tuple(decl.typ.results.as_ref(), function_scope);
        let parameter_types = self
            .types
            .tuple(params)
            .map(|tuple| {
                tuple
                    .vars
                    .iter()
                    .map(|object| self.symbols.object(*object).typ)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let result_types = self
            .types
            .tuple(results)
            .map(|tuple| {
                tuple
                    .vars
                    .iter()
                    .map(|object| self.symbols.object(*object).typ)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if parameter_types
            .iter()
            .any(|typ| self.is_aggregate_type(*typ))
        {
            self.unsupported(Some(decl.node_id()), "aggregate function parameter");
        }
        if result_types.len() > 1 {
            self.unsupported(Some(decl.node_id()), "multiple function results");
        }
        if result_types.iter().any(|typ| self.is_aggregate_type(*typ)) {
            self.unsupported(Some(decl.node_id()), "aggregate function result");
        }
        if let TypeKind::Signature {
            params: params_slot,
            results: results_slot,
            ..
        } = &mut self.types.get_mut(signature).unwrap().kind
        {
            *params_slot = params;
            *results_slot = results;
        }
    }

    fn resolve_tuple(
        &mut self,
        fields: Option<&ast::FieldList>,
        scope: ScopeId,
    ) -> crate::types::TupleId {
        let mut vars = Vec::new();
        if let Some(fields) = fields {
            for field in &fields.list {
                let typ = field
                    .typ
                    .as_ref()
                    .map(|expr| self.resolve_type_expr(expr, scope, false))
                    .unwrap_or(TypeId::INVALID);
                if field.names.is_empty() {
                    vars.push(self.declare_unnamed_param(scope, field, vars.len() as u32, typ));
                } else {
                    for ident in &field.names {
                        let name = self.symbols.intern(&ident.name);
                        let anchor = Some(ident.node_id());
                        let object = self.declare_in_scope(
                            scope,
                            name,
                            ObjectKind::Param {
                                index: vars.len() as u32,
                            },
                            Some(PackageId::from_raw(0)),
                            anchor,
                            typ,
                            Some(ident.node_id()),
                        );
                        vars.push(object);
                    }
                }
            }
        }
        self.types.alloc_tuple(crate::types::Tuple { vars })
    }

    fn declare_unnamed_param(
        &mut self,
        scope: ScopeId,
        field: &ast::Field,
        index: u32,
        typ: TypeId,
    ) -> ObjectId {
        self.symbols.alloc(declared_object(
            ObjectKind::Param { index },
            crate::types::NameId::default(),
            Some(PackageId::from_raw(0)),
            scope,
            Some(field.node_id()),
            typ,
        ))
    }

    fn check_function_bodies(&mut self) {
        let functions: Vec<(TypeId, ast::FuncDecl)> = self
            .func_decls
            .iter()
            .map(|(&signature, declaration)| (signature, declaration.decl.clone()))
            .collect();
        for (signature, decl) in functions {
            let Some(body) = decl.body.as_deref() else {
                continue;
            };
            let scope = self.function_scopes[&signature];
            let results = match self.types.get(signature).kind {
                TypeKind::Signature { results, .. } => self
                    .types
                    .tuple(results)
                    .map(|tuple| {
                        tuple
                            .vars
                            .iter()
                            .map(|object| self.symbols.object(*object).typ)
                            .collect()
                    })
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            let mut control = ControlContext {
                results,
                loop_breaks: Vec::new(),
            };
            self.info.scopes.insert(decl.name.node_id(), scope);
            let returns = self.check_block(body, scope, &mut control);
            if !control.results.is_empty() && !returns {
                self.diagnostics.error(
                    MISSING_RETURN,
                    Some(decl.name.node_id()),
                    "function with results may reach the end without returning",
                );
            }
        }
    }

    fn check_block(
        &mut self,
        block: &ast::BlockStmt,
        parent: ScopeId,
        control: &mut ControlContext,
    ) -> bool {
        let scope = self
            .scopes
            .child(parent, ScopeKind::Block, Some(block.node_id()));
        self.info.scopes.insert(block.node_id(), scope);
        let mut terminates = false;
        for statement in &block.list {
            let loop_breaks = control.loop_breaks.clone();
            let statement_terminates = self.check_stmt(statement, scope, control);
            if !terminates {
                terminates = statement_terminates;
            } else {
                control.loop_breaks = loop_breaks;
            }
        }
        terminates
    }

    fn check_stmt(
        &mut self,
        statement: &ast::Stmt,
        scope: ScopeId,
        control: &mut ControlContext,
    ) -> bool {
        let mut guaranteed_return = false;
        match statement {
            ast::Stmt::DeclStmt(statement) => self.check_local_decl(&statement.decl, scope),
            ast::Stmt::BlockStmt(block) => {
                guaranteed_return = self.check_block(block, scope, control);
            }
            ast::Stmt::ExprStmt(statement) => {
                if !is_call_expression(&statement.x) {
                    self.unsupported(Some(statement.node_id()), "non-call expression statement");
                }
                self.check_expr(&statement.x, scope);
            }
            ast::Stmt::AssignStmt(statement) => {
                if statement.tok == Token::Define {
                    self.unsupported(Some(statement.node_id()), "short variable declaration");
                } else if statement.tok != Token::Assign {
                    self.unsupported(Some(statement.node_id()), "compound assignment");
                }
                self.check_assignment(&statement.lhs, &statement.rhs, scope);
            }
            ast::Stmt::IncDecStmt(statement) => {
                let value = self.check_expr(&statement.x, scope);
                if value.mode != ValueMode::Variable {
                    self.diagnostics.error(
                        EXPECTED_VARIABLE,
                        Some(statement.x.node_id()),
                        "increment or decrement target is not assignable",
                    );
                }
                self.require_integer(&value, Some(statement.x.node_id()));
            }
            ast::Stmt::ReturnStmt(statement) => {
                self.check_return(statement, scope, control);
                guaranteed_return = true;
            }
            ast::Stmt::IfStmt(statement) => {
                let if_scope =
                    self.scopes
                        .child(scope, ScopeKind::Block, Some(statement.node_id()));
                if let Some(init) = &statement.init {
                    self.unsupported(Some(init.node_id()), "if initializer");
                    self.check_stmt(init, if_scope, control);
                }
                let condition = self.check_expr(&statement.cond, if_scope);
                self.require_boolean(&condition, Some(statement.cond.node_id()));
                let then_returns = self.check_block(&statement.body, if_scope, control);
                let else_returns = statement
                    .else_
                    .as_ref()
                    .map(|else_| self.check_stmt(else_, if_scope, control))
                    .unwrap_or(false);
                guaranteed_return = then_returns && else_returns;
            }
            ast::Stmt::ForStmt(statement) => {
                let for_scope =
                    self.scopes
                        .child(scope, ScopeKind::Block, Some(statement.node_id()));
                if let Some(init) = &statement.init {
                    self.unsupported(Some(init.node_id()), "three-clause for initializer");
                    self.check_stmt(init, for_scope, control);
                }
                if let Some(condition) = &statement.cond {
                    let condition_value = self.check_expr(condition, for_scope);
                    self.require_boolean(&condition_value, Some(condition.node_id()));
                }
                if let Some(post) = &statement.post {
                    self.unsupported(Some(post.node_id()), "three-clause for post statement");
                    self.check_stmt(post, for_scope, control);
                }
                control.loop_breaks.push(false);
                self.check_block(&statement.body, for_scope, control);
                let has_break = control.loop_breaks.pop().expect("active loop");
                guaranteed_return = statement.cond.is_none() && !has_break;
            }
            ast::Stmt::LabeledStmt(statement) => {
                self.unsupported(Some(statement.node_id()), "labeled statement");
                guaranteed_return = self.check_stmt(&statement.stmt, scope, control);
            }
            ast::Stmt::SendStmt(statement) => {
                self.unsupported(Some(statement.node_id()), "send statement");
                self.check_expr(&statement.chan_, scope);
                self.check_expr(&statement.value, scope);
            }
            ast::Stmt::GoStmt(statement) => {
                self.unsupported(Some(statement.node_id()), "go statement");
                self.check_call(&statement.call, scope);
            }
            ast::Stmt::DeferStmt(statement) => {
                self.unsupported(Some(statement.node_id()), "defer statement");
                self.check_call(&statement.call, scope);
            }
            ast::Stmt::RangeStmt(statement) => {
                self.unsupported(Some(statement.node_id()), "range");
                if let Some(key) = &statement.key {
                    self.check_expr(key, scope);
                }
                if let Some(value) = &statement.value {
                    self.check_expr(value, scope);
                }
                self.check_expr(&statement.x, scope);
                self.check_block(&statement.body, scope, control);
            }
            ast::Stmt::SwitchStmt(statement) => {
                self.unsupported(Some(statement.node_id()), "switch");
                if let Some(init) = &statement.init {
                    self.check_stmt(init, scope, control);
                }
                if let Some(tag) = &statement.tag {
                    self.check_expr(tag, scope);
                }
                self.check_block(&statement.body, scope, control);
            }
            ast::Stmt::TypeSwitchStmt(statement) => {
                self.unsupported(Some(statement.node_id()), "type switch");
                if let Some(init) = &statement.init {
                    self.check_stmt(init, scope, control);
                }
                self.check_stmt(&statement.assign, scope, control);
                self.check_block(&statement.body, scope, control);
            }
            ast::Stmt::SelectStmt(statement) => {
                self.unsupported(Some(statement.node_id()), "select");
                self.check_block(&statement.body, scope, control);
            }
            ast::Stmt::CaseClause(statement) => {
                for expression in &statement.list {
                    self.check_expr(expression, scope);
                }
                for statement in &statement.body {
                    self.check_stmt(statement, scope, control);
                }
            }
            ast::Stmt::CommClause(statement) => {
                if let Some(statement) = &statement.comm {
                    self.check_stmt(statement, scope, control);
                }
                for statement in &statement.body {
                    self.check_stmt(statement, scope, control);
                }
            }
            ast::Stmt::BranchStmt(statement) => {
                guaranteed_return = self.check_branch(statement, control)
            }
            ast::Stmt::EmptyStmt(_) | ast::Stmt::BadStmt(_) => {}
        }
        guaranteed_return
    }

    fn check_return(
        &mut self,
        statement: &ast::ReturnStmt,
        scope: ScopeId,
        control: &ControlContext,
    ) {
        let values: Vec<TypeAndValue> = statement
            .results
            .iter()
            .map(|expr| self.check_expr(expr, scope))
            .collect();
        if values.len() != control.results.len() {
            self.diagnostics.error(
                INVALID_RETURN,
                Some(statement.node_id()),
                "return has an incorrect number of values",
            );
        }
        for (value, result) in values.iter().zip(&control.results) {
            self.require_assignable(value.typ, *result, Some(statement.node_id()));
        }
    }

    fn check_branch(&mut self, statement: &ast::BranchStmt, control: &mut ControlContext) -> bool {
        if statement.label.is_some() {
            self.unsupported(Some(statement.node_id()), "labeled branch statement");
            return false;
        }
        match statement.tok {
            Token::Break if !control.loop_breaks.is_empty() => {
                *control.loop_breaks.last_mut().expect("active loop") = true;
                true
            }
            Token::Continue if !control.loop_breaks.is_empty() => true,
            Token::Break | Token::Continue => {
                self.diagnostics.error(
                    INVALID_BRANCH,
                    Some(statement.node_id()),
                    "break or continue is only valid inside a for loop",
                );
                false
            }
            _ => {
                self.unsupported(Some(statement.node_id()), "labeled branch statement");
                false
            }
        }
    }

    fn unsupported(&mut self, anchor: Option<AstNodeId>, feature: &str) {
        self.diagnostics.error(
            UNSUPPORTED_FEATURE,
            anchor,
            format!("{feature} is not supported by the MVP"),
        );
    }

    fn validate_entry_point(&mut self) {
        if self.package_name.and_then(|name| self.symbols.name(name)) != Some("main") {
            return;
        }
        let name = self.symbols.intern("main");
        let package_scope = self.package_scope.expect("package scope must exist");
        let Some(main) = self.scopes.lookup_local(package_scope, name) else {
            self.diagnostics.error(
                INVALID_ENTRY_POINT,
                None,
                "MVP executable requires func main()",
            );
            return;
        };
        let object = self.symbols.object(main);
        let ObjectKind::Func { signature, .. } = object.kind else {
            self.diagnostics.error(
                INVALID_ENTRY_POINT,
                object.declaration,
                "main must be declared as a function",
            );
            return;
        };
        let TypeKind::Signature {
            params, results, ..
        } = self.types.get(signature).kind
        else {
            return;
        };
        let parameter_count = self
            .types
            .tuple(params)
            .map(|tuple| tuple.vars.len())
            .unwrap_or(0);
        let result_count = self
            .types
            .tuple(results)
            .map(|tuple| tuple.vars.len())
            .unwrap_or(0);
        if parameter_count != 0 || result_count != 0 {
            self.diagnostics.error(
                INVALID_ENTRY_POINT,
                object.declaration,
                "main must not have parameters or results",
            );
        }
    }

    fn check_local_decl(&mut self, declaration: &ast::Decl, scope: ScopeId) {
        let ast::Decl::GenDecl(declaration) = declaration else {
            return;
        };
        if declaration.tok != Token::Var {
            self.unsupported(Some(declaration.node_id()), "local declaration");
            return;
        }
        for spec in &declaration.specs {
            if let ast::Spec::ValueSpec(spec) = spec {
                self.check_local_var_spec(spec, scope);
            }
        }
    }

    fn check_local_var_spec(&mut self, spec: &ast::ValueSpec, scope: ScopeId) {
        // A variable's scope begins at the end of its declaration, so its
        // initializer resolves in the enclosing scope before insertion.
        let values: Vec<TypeAndValue> = spec
            .values
            .iter()
            .map(|value| self.check_expr(value, scope))
            .collect();
        let explicit_type = spec
            .typ
            .as_ref()
            .map(|expr| self.resolve_type_expr(expr, scope, false));
        let typ = match (explicit_type, values.first()) {
            (Some(typ), _) => typ,
            (None, Some(value)) if value.mode != ValueMode::Nil => value.typ,
            (None, Some(_)) => {
                self.diagnostics.error(
                    MISSING_VARIABLE_TYPE,
                    Some(spec.node_id()),
                    "cannot infer a variable type from nil",
                );
                TypeId::INVALID
            }
            (None, None) => {
                self.diagnostics.error(
                    MISSING_VARIABLE_TYPE,
                    Some(spec.node_id()),
                    "local variable requires an explicit type or an initializer",
                );
                TypeId::INVALID
            }
        };
        if let Some(explicit_type) = explicit_type {
            for value in &values {
                self.require_assignable(value.typ, explicit_type, Some(spec.node_id()));
            }
        }
        if spec.names.len() != values.len() && !values.is_empty() {
            self.diagnostics.error(
                TYPE_MISMATCH,
                Some(spec.node_id()),
                "variable declaration has a different number of names and values",
            );
        }
        for ident in &spec.names {
            let name = self.symbols.intern(&ident.name);
            let anchor = Some(ident.node_id());
            self.declare_in_scope(
                scope,
                name,
                ObjectKind::Var { embedded: false },
                Some(PackageId::from_raw(0)),
                anchor,
                typ,
                Some(ident.node_id()),
            );
        }
    }

    fn check_expr(&mut self, expr: &ast::Expr, scope: ScopeId) -> TypeAndValue {
        let result = match expr {
            ast::Expr::Ident(ident) => self.bind_value_name(ident, scope),
            ast::Expr::BasicLit(literal) if literal.kind == Token::Int => TypeAndValue {
                typ: self.predeclared.expect("universe must be initialized").int,
                mode: ValueMode::Value,
                constant: parse_array_length(expr),
            },
            ast::Expr::BasicLit(_) => {
                self.unsupported_value(Some(expr.node_id()), "non-integer literal")
            }
            ast::Expr::BadExpr(_) => self.invalid_value(),
            ast::Expr::ParenExpr(expr) => self.check_expr(&expr.x, scope),
            ast::Expr::StarExpr(expr) => {
                let operand = self.check_expr(&expr.x, scope);
                match self.types.deref(operand.typ) {
                    Some(typ) => TypeAndValue {
                        typ,
                        mode: ValueMode::Variable,
                        constant: None,
                    },
                    None => self.invalid_operation(
                        Some(expr.node_id()),
                        "cannot dereference a non-pointer",
                    ),
                }
            }
            ast::Expr::UnaryExpr(expr) => self.check_unary(expr, scope),
            ast::Expr::BinaryExpr(expr) => self.check_binary(expr, scope),
            ast::Expr::IndexExpr(expr) => {
                let array = self.check_expr(&expr.x, scope);
                let index = self.check_expr(&expr.index, scope);
                self.require_integer(&index, Some(expr.index.node_id()));
                if is_composite_literal(&expr.x) {
                    self.unsupported_value(Some(expr.node_id()), "composite literal indexing")
                } else {
                    match self.types.array_element(array.typ) {
                        Some(typ) => TypeAndValue {
                            typ,
                            mode: ValueMode::Variable,
                            constant: None,
                        },
                        None => self
                            .invalid_operation(Some(expr.node_id()), "indexing requires an array"),
                    }
                }
            }
            ast::Expr::SelectorExpr(expr) => self.check_selector(expr, scope),
            ast::Expr::CallExpr(expr) => self.check_call(expr, scope),
            ast::Expr::SliceExpr(expr) => {
                self.unsupported(Some(expr.node_id()), "slice expression");
                self.check_expr(&expr.x, scope);
                if let Some(low) = &expr.low {
                    self.check_expr(low, scope);
                }
                if let Some(high) = &expr.high {
                    self.check_expr(high, scope);
                }
                if let Some(max) = &expr.max {
                    self.check_expr(max, scope);
                }
                self.invalid_value()
            }
            ast::Expr::IndexListExpr(expr) => {
                self.unsupported(Some(expr.node_id()), "generic index list");
                self.check_expr(&expr.x, scope);
                for index in &expr.indices {
                    self.check_expr(index, scope);
                }
                self.invalid_value()
            }
            ast::Expr::KeyValueExpr(expr) => {
                self.unsupported(Some(expr.node_id()), "key-value expression");
                self.check_expr(&expr.key, scope);
                self.check_expr(&expr.value, scope);
                self.invalid_value()
            }
            ast::Expr::CompositeLit(expr) => match &expr.typ {
                Some(typ) => {
                    let typ = self.resolve_type_expr(typ, scope, false);
                    if !expr.elts.is_empty() {
                        self.unsupported(Some(expr.node_id()), "composite literal");
                        for element in &expr.elts {
                            self.check_expr(element, scope);
                        }
                        self.invalid_value()
                    } else if self.is_aggregate_type(typ) {
                        TypeAndValue {
                            typ,
                            mode: ValueMode::Value,
                            constant: None,
                        }
                    } else {
                        self.unsupported_value(Some(expr.node_id()), "composite literal")
                    }
                }
                None => self.unsupported_value(Some(expr.node_id()), "composite literal"),
            },
            ast::Expr::FuncLit(_) => {
                self.unsupported_value(Some(expr.node_id()), "function literal")
            }
            ast::Expr::TypeAssertExpr(expr) => {
                self.unsupported(Some(expr.node_id()), "type assertion");
                self.check_expr(&expr.x, scope);
                self.invalid_value()
            }
            ast::Expr::Ellipsis(_)
            | ast::Expr::ArrayType(_)
            | ast::Expr::StructType(_)
            | ast::Expr::FuncType(_)
            | ast::Expr::InterfaceType(_)
            | ast::Expr::MapType(_)
            | ast::Expr::ChanType(_) => {
                self.unsupported_value(Some(expr.node_id()), "type syntax in value expression")
            }
        };
        self.info.types.insert(expr.node_id(), result.clone());
        result
    }

    fn check_call(&mut self, call: &ast::CallExpr, scope: ScopeId) -> TypeAndValue {
        if call.ellipsis.is_valid() {
            self.unsupported(Some(call.node_id()), "ellipsis call argument");
        }
        let function = self.check_expr(&call.fun, scope);
        let TypeKind::Signature {
            params, results, ..
        } = self.types.get(function.typ).kind
        else {
            return self.invalid_operation(Some(call.node_id()), "call requires a function");
        };
        let parameter_count = self
            .types
            .tuple(params)
            .map(|tuple| tuple.vars.len())
            .unwrap_or(0);
        if call.args.len() != parameter_count {
            self.diagnostics.error(
                TYPE_MISMATCH,
                Some(call.node_id()),
                "function call has an incorrect number of arguments",
            );
        }
        for (index, argument) in call.args.iter().enumerate() {
            let argument = self.check_expr(argument, scope);
            if let Some(parameter) = self
                .types
                .tuple(params)
                .and_then(|tuple| tuple.vars.get(index))
            {
                self.require_assignable(
                    argument.typ,
                    self.symbols.object(*parameter).typ,
                    Some(call.node_id()),
                );
            }
        }
        let Some(results) = self.types.tuple(results) else {
            return self.invalid_value();
        };
        match results.vars.as_slice() {
            [] => TypeAndValue {
                typ: self.predeclared.expect("universe must be initialized").void,
                mode: ValueMode::NoValue,
                constant: None,
            },
            [result] => TypeAndValue {
                typ: self.symbols.object(*result).typ,
                mode: ValueMode::Value,
                constant: None,
            },
            _ => self.invalid_value(),
        }
    }

    fn check_unary(&mut self, expr: &ast::UnaryExpr, scope: ScopeId) -> TypeAndValue {
        let operand = self.check_expr(&expr.x, scope);
        match expr.op {
            Token::Add | Token::Sub if self.is_integer_type(operand.typ) => {
                let constant = match (&operand.constant, expr.op) {
                    (Some(ConstValue::Int(value)), Token::Add) => {
                        Some(ConstValue::Int(value.clone()))
                    }
                    (Some(ConstValue::Int(value)), Token::Sub) => value
                        .to_i128()
                        .and_then(|value| value.checked_neg())
                        .map(|value| ConstValue::Int(IntegerValue::from_i128(value))),
                    _ => None,
                };
                TypeAndValue {
                    constant,
                    ..operand
                }
            }
            Token::Not if self.types.is_basic(operand.typ, BasicType::Bool) => TypeAndValue {
                typ: operand.typ,
                mode: ValueMode::Value,
                constant: None,
            },
            Token::And if is_composite_literal(&expr.x) => {
                self.unsupported_value(Some(expr.node_id()), "address of composite literal")
            }
            Token::And if operand.mode == ValueMode::Variable => TypeAndValue {
                typ: self.types.alloc(TypeKind::Pointer { base: operand.typ }),
                mode: ValueMode::Value,
                constant: None,
            },
            _ => self.invalid_operation(Some(expr.node_id()), "invalid unary operation"),
        }
    }

    fn check_binary(&mut self, expr: &ast::BinaryExpr, scope: ScopeId) -> TypeAndValue {
        let left = self.check_expr(&expr.x, scope);
        let right = self.check_expr(&expr.y, scope);
        match expr.op {
            Token::Add | Token::Sub | Token::Mul | Token::Quo | Token::Rem => {
                self.require_integer(&left, Some(expr.x.node_id()));
                self.require_integer(&right, Some(expr.y.node_id()));
                self.require_assignable(right.typ, left.typ, Some(expr.node_id()));
                TypeAndValue {
                    typ: left.typ,
                    mode: ValueMode::Value,
                    constant: self.fold_integer_binary(expr.op, &left.constant, &right.constant),
                }
            }
            Token::Less | Token::Leq | Token::Greater | Token::Geq => {
                self.require_integer(&left, Some(expr.x.node_id()));
                self.require_integer(&right, Some(expr.y.node_id()));
                self.require_assignable(right.typ, left.typ, Some(expr.node_id()));
                TypeAndValue {
                    typ: self
                        .predeclared
                        .expect("universe must be initialized")
                        .bool_,
                    mode: ValueMode::Value,
                    constant: self.fold_integer_comparison(
                        expr.op,
                        &left.constant,
                        &right.constant,
                    ),
                }
            }
            Token::Equal | Token::Neq => {
                if left.typ != TypeId::INVALID && !self.types.comparable(left.typ) {
                    self.diagnostics.error(
                        INVALID_OPERATION,
                        Some(expr.x.node_id()),
                        "equality comparison requires a comparable operand",
                    );
                }
                self.require_assignable(right.typ, left.typ, Some(expr.node_id()));
                TypeAndValue {
                    typ: self
                        .predeclared
                        .expect("universe must be initialized")
                        .bool_,
                    mode: ValueMode::Value,
                    constant: self.fold_equality(&left.constant, &right.constant),
                }
            }
            Token::LAnd | Token::LOr => {
                self.require_boolean(&left, Some(expr.x.node_id()));
                self.require_boolean(&right, Some(expr.y.node_id()));
                TypeAndValue {
                    typ: self
                        .predeclared
                        .expect("universe must be initialized")
                        .bool_,
                    mode: ValueMode::Value,
                    constant: self.fold_boolean_binary(expr.op, &left.constant, &right.constant),
                }
            }
            _ => self.invalid_operation(Some(expr.node_id()), "invalid binary operation"),
        }
    }

    fn check_selector(&mut self, expr: &ast::SelectorExpr, scope: ScopeId) -> TypeAndValue {
        let receiver = self.check_expr(&expr.x, scope);
        if is_composite_literal(&expr.x) {
            self.unsupported_value(Some(expr.node_id()), "composite literal field selection")
        } else {
            let mut typ = receiver.typ;
            let mut indirect = false;
            if let Some(base) = self.types.deref(typ) {
                typ = base;
                indirect = true;
            }
            let TypeKind::Struct { fields } = &self.types.get(self.types.underlying(typ)).kind
            else {
                return self
                    .invalid_operation(Some(expr.node_id()), "field selection requires a struct");
            };
            let name = self.symbols.intern(&expr.sel.name);
            let Some((index, field)) = fields
                .iter()
                .enumerate()
                .find(|(_, field)| self.symbols.object(**field).name == name)
            else {
                return self.invalid_operation(Some(expr.sel.node_id()), "unknown struct field");
            };
            let field = *field;
            self.info.selections.insert(
                expr.node_id(),
                crate::types::Selection {
                    object: field,
                    kind: crate::types::SelectionKind::Field,
                    index: vec![index as u32],
                    indirect,
                },
            );
            TypeAndValue {
                typ: self.symbols.object(field).typ,
                mode: ValueMode::Variable,
                constant: None,
            }
        }
    }

    fn check_assignment(&mut self, lhs: &[ast::Expr], rhs: &[ast::Expr], scope: ScopeId) {
        if lhs.len() != rhs.len() {
            self.diagnostics.error(
                TYPE_MISMATCH,
                None,
                "assignment has a different number of left and right values",
            );
        }
        for (left_expr, right_expr) in lhs.iter().zip(rhs) {
            if is_blank_identifier(left_expr) {
                let right = self.check_expr(right_expr, scope);
                if right.mode == ValueMode::NoValue {
                    self.diagnostics.error(
                        INVALID_OPERATION,
                        Some(right_expr.node_id()),
                        "blank assignment requires a value",
                    );
                }
                continue;
            }
            let left = self.check_expr(left_expr, scope);
            let right = self.check_expr(right_expr, scope);
            if left.mode != ValueMode::Variable {
                self.diagnostics.error(
                    EXPECTED_VARIABLE,
                    None,
                    "assignment target is not assignable",
                );
            }
            self.require_assignable(right.typ, left.typ, None);
        }
    }

    fn bind_value_name(&mut self, ident: &ast::Ident, scope: ScopeId) -> TypeAndValue {
        if ident.name == "_" {
            return self.invalid_operation(
                Some(ident.node_id()),
                "blank identifier cannot be used as a value",
            );
        }
        let name = self.symbols.intern(&ident.name);
        let Some(resolved) = self.scopes.lookup(scope, name) else {
            self.diagnostics.error(
                UNDEFINED_NAME,
                Some(ident.node_id()),
                format!("undefined name `{}`", ident.name),
            );
            return self.invalid_value();
        };
        if self
            .pending_global_initializers
            .contains_key(&resolved.object)
        {
            self.resolve_global(resolved.object);
        }
        let object = self.symbols.object(resolved.object);
        if matches!(&object.kind, ObjectKind::TypeName { .. }) {
            self.diagnostics.error(
                TYPE_USED_AS_VALUE,
                Some(ident.node_id()),
                format!("type `{}` used as a value", ident.name),
            );
            return self.invalid_value();
        }
        self.info.uses.insert(ident.node_id(), resolved.object);
        let mode = match object.kind {
            ObjectKind::Var { .. } | ObjectKind::Param { .. } => ValueMode::Variable,
            ObjectKind::Nil => ValueMode::Nil,
            _ => ValueMode::Value,
        };
        let constant = match &object.kind {
            ObjectKind::Const { value } => Some(value.clone()),
            _ => None,
        };
        TypeAndValue {
            typ: object.typ,
            mode,
            constant,
        }
    }

    fn invalid_value(&self) -> TypeAndValue {
        TypeAndValue {
            typ: TypeId::INVALID,
            mode: ValueMode::Invalid,
            constant: None,
        }
    }

    fn invalid_operation(&mut self, anchor: Option<AstNodeId>, message: &str) -> TypeAndValue {
        self.diagnostics.error(INVALID_OPERATION, anchor, message);
        self.invalid_value()
    }

    fn unsupported_value(&mut self, anchor: Option<AstNodeId>, feature: &str) -> TypeAndValue {
        self.unsupported(anchor, feature);
        self.invalid_value()
    }

    fn is_aggregate_type(&self, typ: TypeId) -> bool {
        matches!(
            self.types.get(self.types.underlying(typ)).kind,
            TypeKind::Array { .. } | TypeKind::Struct { .. }
        )
    }

    fn require_assignable(&mut self, source: TypeId, target: TypeId, anchor: Option<AstNodeId>) {
        if !self.types.assignable_to(source, target) {
            self.diagnostics.error(
                TYPE_MISMATCH,
                anchor,
                "value is not assignable to the target type",
            );
        }
    }

    fn require_integer(&mut self, value: &TypeAndValue, anchor: Option<AstNodeId>) {
        if value.typ != TypeId::INVALID && !self.is_integer_type(value.typ) {
            self.diagnostics.error(
                INVALID_OPERATION,
                anchor,
                "operation requires an integer operand",
            );
        }
    }

    fn is_integer_type(&self, typ: TypeId) -> bool {
        self.types.is_basic(typ, BasicType::Int) || self.types.is_basic(typ, BasicType::Byte)
    }

    fn require_boolean(&mut self, value: &TypeAndValue, anchor: Option<AstNodeId>) {
        if value.typ != TypeId::INVALID && !self.types.is_basic(value.typ, BasicType::Bool) {
            self.diagnostics
                .error(EXPECTED_BOOLEAN, anchor, "condition must have type bool");
        }
    }

    fn fold_integer_binary(
        &self,
        op: Token,
        left: &Option<ConstValue>,
        right: &Option<ConstValue>,
    ) -> Option<ConstValue> {
        let (ConstValue::Int(left), ConstValue::Int(right)) = (left.as_ref()?, right.as_ref()?)
        else {
            return None;
        };
        let (left, right) = (left.to_i128()?, right.to_i128()?);
        let value = match op {
            Token::Add => left.checked_add(right),
            Token::Sub => left.checked_sub(right),
            Token::Mul => left.checked_mul(right),
            Token::Quo if right != 0 => left.checked_div(right),
            Token::Rem if right != 0 => left.checked_rem(right),
            _ => None,
        }?;
        Some(ConstValue::Int(IntegerValue::from_i128(value)))
    }

    fn fold_integer_comparison(
        &self,
        op: Token,
        left: &Option<ConstValue>,
        right: &Option<ConstValue>,
    ) -> Option<ConstValue> {
        let (ConstValue::Int(left), ConstValue::Int(right)) = (left.as_ref()?, right.as_ref()?)
        else {
            return None;
        };
        let (left, right) = (left.to_i128()?, right.to_i128()?);
        let value = match op {
            Token::Less => left < right,
            Token::Leq => left <= right,
            Token::Greater => left > right,
            Token::Geq => left >= right,
            _ => return None,
        };
        Some(ConstValue::Bool(value))
    }

    fn fold_equality(
        &self,
        left: &Option<ConstValue>,
        right: &Option<ConstValue>,
    ) -> Option<ConstValue> {
        Some(ConstValue::Bool(left.as_ref()? == right.as_ref()?))
    }

    fn fold_boolean_binary(
        &self,
        op: Token,
        left: &Option<ConstValue>,
        right: &Option<ConstValue>,
    ) -> Option<ConstValue> {
        let (ConstValue::Bool(left), ConstValue::Bool(right)) = (left.as_ref()?, right.as_ref()?)
        else {
            return None;
        };
        Some(ConstValue::Bool(match op {
            Token::LAnd => *left && *right,
            Token::LOr => *left || *right,
            _ => return None,
        }))
    }

    fn declare_predeclared_type(&mut self, spelling: &str, typ: TypeId) {
        let name = self.symbols.intern(spelling);
        self.declare_in_scope(
            self.scopes.universe(),
            name,
            ObjectKind::TypeName {
                named: typ,
                is_alias: false,
            },
            None,
            None,
            typ,
            None,
        );
    }

    fn declare_predeclared_const(&mut self, spelling: &str, typ: TypeId, value: ConstValue) {
        let name = self.symbols.intern(spelling);
        self.declare_in_scope(
            self.scopes.universe(),
            name,
            ObjectKind::Const { value },
            None,
            None,
            typ,
            None,
        );
    }

    fn declare_predeclared_nil(&mut self) {
        let name = self.symbols.intern("nil");
        self.declare_in_scope(
            self.scopes.universe(),
            name,
            ObjectKind::Nil,
            None,
            None,
            TypeId::INVALID,
            None,
        );
    }

    fn declare_source_object(
        &mut self,
        ident: &ast::Ident,
        kind: ObjectKind,
        name: crate::types::NameId,
        typ: TypeId,
    ) -> ObjectId {
        let anchor = Some(ident.node_id());
        self.declare_in_scope(
            self.package_scope.expect("package scope must exist"),
            name,
            kind,
            Some(PackageId::from_raw(0)),
            anchor,
            typ,
            Some(ident.node_id()),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn declare_in_scope(
        &mut self,
        scope: ScopeId,
        name: crate::types::NameId,
        kind: ObjectKind,
        package: Option<PackageId>,
        anchor: Option<AstNodeId>,
        typ: TypeId,
        definition: Option<AstNodeId>,
    ) -> ObjectId {
        let object = self
            .symbols
            .alloc(declared_object(kind, name, package, scope, anchor, typ));
        if let Some(node) = definition {
            self.info.defs.insert(node, object);
        }

        if let Err(error) = self.scopes.declare(scope, name, object) {
            match error {
                DeclareError::Duplicate(duplicate) => self.report_duplicate(anchor, duplicate),
                DeclareError::UnknownScope(_) => unreachable!("checker created an invalid scope"),
            }
        }
        object
    }

    fn report_duplicate(&mut self, anchor: Option<AstNodeId>, duplicate: DuplicateDeclaration) {
        let previous = self.symbols.object(duplicate.existing);
        self.diagnostics.push(
            Diagnostic::new(
                gane_diagnostics::Severity::Error,
                DUPLICATE_DECLARATION,
                label_from_node(anchor, "duplicate declaration in scope"),
            )
            .with_secondary(label_from_node(
                previous.declaration,
                "previous declaration is here",
            )),
        );
    }

    fn finish(self) -> AnalysisResult {
        let package = Package {
            id: PackageId::from_raw(0),
            name: self.package_name.expect("package name must be initialized"),
            path: self.input.path,
            scope: self
                .package_scope
                .expect("package scope must be initialized"),
            files: self.input.files.iter().map(|file| file.id).collect(),
        };
        AnalysisResult {
            package,
            predeclared: self.predeclared.expect("universe must be initialized"),
            types: self.types,
            symbols: self.symbols,
            scopes: self.scopes,
            file_scopes: self.file_scopes,
            info: self.info,
            global_initializers: self.resolved_global_initializers,
            diagnostics: self.diagnostics.finish(),
        }
    }
}

fn label_from_node(node: Option<AstNodeId>, message: impl Into<String>) -> Label {
    match node {
        Some(node) => Label::from_node(node, message),
        None => Label::from_position(gane_parser::token::Position::default(), message),
    }
}

fn parse_array_length(expr: &ast::Expr) -> Option<ConstValue> {
    let ast::Expr::BasicLit(literal) = expr else {
        return None;
    };
    if literal.kind != Token::Int {
        return None;
    }
    let digits = literal.value.replace('_', "");
    let (radix, digits) = if let Some(value) = digits.strip_prefix("0x") {
        (16, value)
    } else if let Some(value) = digits.strip_prefix("0X") {
        (16, value)
    } else {
        (10, digits.as_str())
    };
    u64::from_str_radix(digits, radix)
        .ok()
        .map(|value| ConstValue::Int(IntegerValue::from_u64(value)))
}

fn is_blank_identifier(expr: &ast::Expr) -> bool {
    matches!(expr, ast::Expr::Ident(ident) if ident.name == "_")
}

fn is_call_expression(mut expr: &ast::Expr) -> bool {
    while let ast::Expr::ParenExpr(paren) = expr {
        expr = &paren.x;
    }
    matches!(expr, ast::Expr::CallExpr(_))
}

fn is_composite_literal(mut expr: &ast::Expr) -> bool {
    while let ast::Expr::ParenExpr(paren) = expr {
        expr = &paren.x;
    }
    matches!(expr, ast::Expr::CompositeLit(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gane_parser::{parser::Mode, parser::parse_file, token::FileSet};

    fn analyze(source: &str) -> AnalysisResult {
        let mut files = FileSet::new();
        let source = if source.contains("func main(") {
            source.to_owned()
        } else {
            format!("{source}\nfunc main() {{}}\n")
        };
        let (ast, errors) = parse_file(&mut files, "main.go", source.as_bytes(), Mode::default());
        assert!(errors.is_none(), "fixture should parse: {errors:?}");
        analyze_package(PackageInput::single("main", FileId::from_raw(1), &ast))
    }

    fn analyze_files(sources: &[(&str, &str)]) -> AnalysisResult {
        let mut file_set = FileSet::new();
        let asts = sources
            .iter()
            .map(|(name, source)| {
                let (ast, errors) =
                    parse_file(&mut file_set, name, source.as_bytes(), Mode::default());
                assert!(errors.is_none(), "fixture should parse: {errors:?}");
                ast
            })
            .collect::<Vec<_>>();
        let files = asts
            .iter()
            .enumerate()
            .map(|(index, ast)| PackageFile {
                id: FileId::from_raw(index as u32 + 1),
                ast,
            })
            .collect();
        analyze_package(PackageInput {
            path: PackagePath("main".to_owned()),
            files,
        })
    }

    fn reports_error(
        result: &AnalysisResult,
        code: DiagnosticCode,
        message: impl AsRef<str>,
    ) -> bool {
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == code && diagnostic.message() == message.as_ref())
    }

    fn reports_unsupported(result: &AnalysisResult, feature: &str) -> bool {
        reports_error(
            result,
            UNSUPPORTED_FEATURE,
            format!("{feature} is not supported by the MVP"),
        )
    }

    #[test]
    fn collects_top_level_declarations_before_function_bodies() {
        let result = analyze(
            "package main\n\
             type Node struct { next *Node }\n\
             const Limit = 2\n\
             var root Node\n\
             func main() { later() }\n\
             func later() {}\n",
        );

        assert!(result.diagnostics.is_empty());
        assert_eq!(result.info.defs.len(), 6);
        assert_eq!(
            result.scopes.scope(result.package.scope).unwrap().kind,
            ScopeKind::Package
        );
        assert!(matches!(
            result.types.get(result.predeclared.int).kind,
            TypeKind::Basic(BasicType::Int)
        ));
    }

    #[test]
    fn creates_distinct_file_scopes_below_the_package_scope() {
        let result = analyze_files(&[
            (
                "types.go",
                "package main\ntype Shared int\nvar value Shared\n",
            ),
            (
                "main.go",
                "package main\nfunc main() { var local Shared; local = value }\n",
            ),
        ]);

        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let first = result.file_scope(FileId::from_raw(1)).unwrap();
        let second = result.file_scope(FileId::from_raw(2)).unwrap();
        assert_ne!(first, second);
        for scope in [first, second] {
            let scope = result.scopes.scope(scope).unwrap();
            assert_eq!(scope.kind, ScopeKind::File);
            assert_eq!(scope.parent, Some(result.package.scope));
            assert!(scope.names.is_empty());
        }

        let main = result.package_member("main").unwrap();
        let main_node = result.symbols.object(main).declaration.unwrap();
        let function_scope = result.info.scopes[&main_node];
        assert_eq!(
            result.scopes.scope(function_scope).unwrap().parent,
            Some(second)
        );
    }

    #[test]
    fn reports_duplicate_input_file_ids_without_replacing_the_first_scope() {
        let mut file_set = FileSet::new();
        let (first, first_errors) = parse_file(
            &mut file_set,
            "first.go",
            b"package main\nvar value int\n",
            Mode::default(),
        );
        let (second, second_errors) = parse_file(
            &mut file_set,
            "second.go",
            b"package main\nfunc main() {}\n",
            Mode::default(),
        );
        assert!(first_errors.is_none() && second_errors.is_none());
        let id = FileId::from_raw(7);
        let result = analyze_package(PackageInput {
            path: PackagePath("main".to_owned()),
            files: vec![
                PackageFile { id, ast: &first },
                PackageFile { id, ast: &second },
            ],
        });

        assert_eq!(
            result
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == DUPLICATE_FILE)
                .count(),
            1
        );
        let file_scope = result.file_scope(id).unwrap();
        assert_eq!(
            result.scopes.scope(file_scope).unwrap().parent,
            Some(result.package.scope)
        );
    }

    #[test]
    fn rejects_package_files_from_different_file_sets() {
        let mut first_set = FileSet::new();
        let (first, first_errors) = parse_file(
            &mut first_set,
            "first.go",
            b"package main\nvar value int\n",
            Mode::default(),
        );
        let mut second_set = FileSet::new();
        let (second, second_errors) = parse_file(
            &mut second_set,
            "second.go",
            b"package main\nfunc main() {}\n",
            Mode::default(),
        );
        assert!(first_errors.is_none() && second_errors.is_none());

        let result = analyze_package(PackageInput {
            path: PackagePath("main".to_owned()),
            files: vec![
                PackageFile {
                    id: FileId::from_raw(1),
                    ast: &first,
                },
                PackageFile {
                    id: FileId::from_raw(2),
                    ast: &second,
                },
            ],
        });

        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == MIXED_FILESET)
        );
    }

    #[test]
    fn reports_cross_file_package_duplicates() {
        let result = analyze_files(&[
            ("first.go", "package main\nvar value int\n"),
            ("second.go", "package main\nvar value int\nfunc main() {}\n"),
        ]);

        assert_eq!(
            result
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == DUPLICATE_DECLARATION)
                .count(),
            1
        );
    }

    #[test]
    fn leaves_file_scopes_empty_while_imports_are_unsupported() {
        let result = analyze_files(&[(
            "main.go",
            "package main\nimport \"foreign\"\nfunc main() {}\n",
        )]);

        assert!(reports_unsupported(&result, "import"));
        let scope = result.file_scope(FileId::from_raw(1)).unwrap();
        assert!(result.scopes.scope(scope).unwrap().names.is_empty());
    }

    #[test]
    fn reports_duplicate_top_level_declarations_without_losing_definitions() {
        let result = analyze("package main\nvar value int\nfunc value() {}\n");

        assert_eq!(result.info.defs.len(), 3);
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].code, DUPLICATE_DECLARATION);
    }

    #[test]
    fn rejects_a_non_main_package_for_the_mvp() {
        let result = analyze("package library\nvar value int\n");

        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].code, INVALID_PACKAGE);
    }

    #[test]
    fn resolves_struct_fields_arrays_and_function_signature() {
        let result = analyze(
            "package main\n\
             type Pair struct { left int; right [2]*Pair }\n\
             func add(a int, b int) int { return a + b }\n",
        );

        assert!(result.diagnostics.is_empty());
        let add = result
            .info
            .defs
            .values()
            .copied()
            .find(|object| result.symbols.name(result.symbols.object(*object).name) == Some("add"))
            .unwrap();
        let ObjectKind::Func { signature, .. } = result.symbols.object(add).kind else {
            panic!("add must be a function");
        };
        let TypeKind::Signature {
            params, results, ..
        } = result.types.get(signature).kind
        else {
            panic!("add must have a signature");
        };
        assert_eq!(result.types.tuple(params).unwrap().vars.len(), 2);
        assert_eq!(result.types.tuple(results).unwrap().vars.len(), 1);
    }

    #[test]
    fn rejects_recursive_value_types_but_allows_pointer_recursion() {
        let valid = analyze("package main\ntype Node struct { next *Node }\n");
        assert!(valid.diagnostics.is_empty());

        let invalid = analyze("package main\ntype Loop struct { next Loop }\n");
        assert!(
            invalid
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == INVALID_RECURSIVE_TYPE)
        );
    }

    #[test]
    fn binds_parameters_locals_and_outer_names_across_nested_blocks() {
        let result = analyze(
            "package main\n\
             func f(value int) {\n\
                 var outer int\n\
                 { var outer int; outer = value }\n\
                 outer = missing\n\
             }\n",
        );

        assert_eq!(result.info.defs.len(), 5);
        assert_eq!(result.info.uses.len(), 3);
        assert!(result.info.scopes.len() >= 3);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == UNDEFINED_NAME)
        );
    }

    #[test]
    fn reports_type_names_used_as_values() {
        let result = analyze("package main\nfunc f() { int = 1 }\n");

        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == TYPE_USED_AS_VALUE)
        );
    }

    #[test]
    fn infers_local_types_and_checks_operators_assignments_and_conditions() {
        let result = analyze(
            "package main\n\
             func f() {\n\
                 var count = 1\n\
                 var ok = count < 2\n\
                 if ok { count = count + 1 }\n\
                 for ok { ok = false }\n\
             }\n",
        );

        assert!(result.diagnostics.is_empty());
        assert!(result.info.types.len() >= 10);
    }

    #[test]
    fn checks_array_pointer_and_struct_field_access() {
        let result = analyze(
            "package main\n\
             type Point struct { x int }\n\
             func f() {\n\
                 var point Point\n\
                 var points [2]Point\n\
                 var ptr = &point\n\
                 points[0].x = ptr.x\n\
             }\n",
        );

        assert!(result.diagnostics.is_empty());
        assert!(!result.info.selections.is_empty());
    }

    #[test]
    fn reports_assignment_and_condition_type_errors() {
        let result = analyze(
            "package main\n\
             func f() {\n\
                 var count = 1\n\
                 var ok = true\n\
                 count = ok\n\
                 if count {}\n\
             }\n",
        );

        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == TYPE_MISMATCH)
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == EXPECTED_BOOLEAN)
        );
    }

    #[test]
    fn checks_main_signature_and_required_entry_point() {
        let mut files = FileSet::new();
        let (missing, errors) = parse_file(
            &mut files,
            "missing.go",
            b"package main\nfunc helper() {}\n",
            Mode::default(),
        );
        assert!(errors.is_none());
        let missing = analyze_package(PackageInput::single("main", FileId::from_raw(1), &missing));
        assert!(
            missing
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == INVALID_ENTRY_POINT)
        );

        let invalid = analyze("package main\nfunc main(value int) {}\n");
        assert!(
            invalid
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == INVALID_ENTRY_POINT)
        );
    }

    #[test]
    fn checks_return_values_and_all_paths_returning() {
        let missing = analyze(
            "package main\n\
             func choose(ok bool) int { if ok { return 1 } }\n",
        );
        assert!(
            missing
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == MISSING_RETURN)
        );

        let valid = analyze(
            "package main\n\
             func choose(ok bool) int { if ok { return 1 } else { return 2 } }\n",
        );
        assert!(valid.diagnostics.is_empty());

        let infinite = analyze("package main\nfunc choose() int { for {} }\nfunc main() {}\n");
        assert!(
            !infinite
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == MISSING_RETURN),
            "{:?}",
            infinite.diagnostics
        );

        let break_reaches_end = analyze(
            "package main\nfunc choose() int { for { break; return 1 } }\nfunc main() {}\n",
        );
        assert!(
            break_reaches_end
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == MISSING_RETURN)
        );

        let return_precedes_break = analyze(
            "package main\nfunc choose() int { for { return 1; break } }\nfunc main() {}\n",
        );
        assert!(
            !return_precedes_break
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == MISSING_RETURN),
            "{:?}",
            return_precedes_break.diagnostics
        );

        let invalid_return = analyze("package main\nfunc helper() { return 1 }\n");
        assert!(
            invalid_return
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == INVALID_RETURN)
        );
    }

    #[test]
    fn validates_branch_context_and_rejects_mvp_features() {
        let result = analyze("package main\nfunc main() { break; value := 1 }\n");

        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == INVALID_BRANCH)
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == UNSUPPORTED_FEATURE)
        );
    }

    #[test]
    fn accepts_blank_assignment_and_still_checks_its_value() {
        let valid = analyze(
            "package main\n\
             func sideEffect() int { return 1 }\n\
             func main() { _ = sideEffect() }\n",
        );
        assert!(valid.diagnostics.is_empty(), "{:?}", valid.diagnostics);

        let invalid = analyze(
            "package main\n\
             func noValue() {}\n\
             func main() { _ = noValue() }\n",
        );
        assert!(
            invalid
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == INVALID_OPERATION)
        );
    }

    #[test]
    fn rejects_send_before_validating_its_operands() {
        let result = analyze("package main\nfunc main() { var value int; value <- value }\n");

        assert!(
            reports_unsupported(&result, "send statement"),
            "{:?}",
            result.diagnostics
        );
    }

    #[test]
    fn rejects_each_unsupported_control_flow_statement() {
        for (feature, source) in [
            ("range", "package main\nfunc main() { for range 1 {} }"),
            ("switch", "package main\nfunc main() { switch {} }"),
            (
                "type switch",
                "package main\nfunc main() { var value interface{}; switch value := value.(type) {} }",
            ),
            (
                "select",
                "package main\nfunc main() { select { default: } }",
            ),
            (
                "defer statement",
                "package main\nfunc call() {}\nfunc main() { defer call() }",
            ),
            (
                "go statement",
                "package main\nfunc call() {}\nfunc main() { go call() }",
            ),
        ] {
            let result = analyze(source);
            assert!(
                reports_unsupported(&result, feature),
                "{feature}: {:?}",
                result.diagnostics
            );
        }
    }

    #[test]
    fn rejects_labeled_break_and_continue() {
        for source in [
            "package main\nfunc main() { outer: for { break outer } }",
            "package main\nfunc main() { outer: for { continue outer } }",
        ] {
            let result = analyze(source);
            assert!(
                reports_unsupported(&result, "labeled branch statement"),
                "{:?}",
                result.diagnostics
            );
        }
    }

    #[test]
    fn rejects_short_declarations_compound_assignments_and_non_call_statements() {
        let result = analyze(
            "package main\n\
             func main() {\n\
                 var value int\n\
                 value := 1\n\
                 value += 1\n\
                 1\n\
             }\n",
        );

        for feature in [
            "short variable declaration",
            "compound assignment",
            "non-call expression statement",
        ] {
            assert!(
                reports_unsupported(&result, feature),
                "{feature}: {:?}",
                result.diagnostics
            );
        }
    }

    #[test]
    fn rejects_three_clause_for_and_if_initializers() {
        let result = analyze(
            "package main\n\
             func main() {\n\
                 var value int\n\
                 for value = 0; value < 1; value++ {}\n\
                 if value = 0; value == 0 {}\n\
             }\n",
        );

        for feature in [
            "three-clause for initializer",
            "three-clause for post statement",
            "if initializer",
        ] {
            assert!(
                reports_unsupported(&result, feature),
                "{feature}: {:?}",
                result.diagnostics
            );
        }
    }

    #[test]
    fn rejects_each_unrepresentable_value_expression() {
        for (feature, source) in [
            (
                "non-integer literal",
                "package main\nfunc main() { _ = \"text\" }",
            ),
            (
                "function literal",
                "package main\nfunc main() { _ = func() {} }",
            ),
            (
                "slice expression",
                "package main\nfunc main() { var values [1]int; _ = values[:] }",
            ),
            (
                "generic index list",
                "package main\nfunc f() {}\nfunc main() { _ = f[int, int] }",
            ),
            (
                "type assertion",
                "package main\nfunc main() { var value int; _ = value.(int) }",
            ),
        ] {
            let result = analyze(source);
            assert!(
                reports_unsupported(&result, feature),
                "{feature}: {:?}",
                result.diagnostics
            );
        }

        let result = analyze(
            "package main\ntype Pair struct { value int }\nfunc main() { _ = Pair{value: 1} }",
        );
        for feature in ["key-value expression", "composite literal"] {
            assert!(
                reports_unsupported(&result, feature),
                "{feature}: {:?}",
                result.diagnostics
            );
        }
    }

    #[test]
    fn accepts_empty_aggregate_literals_and_rejects_other_aggregate_forms() {
        let accepted = analyze(
            "package main\n\
             type Pair struct { value int }\n\
             func main() {\n\
                 var pair Pair = Pair{}\n\
                 var values [2]int = [2]int{}\n\
                 pair = Pair{}\n\
                 values = [2]int{}\n\
             }\n",
        );
        assert!(
            accepted.diagnostics.is_empty(),
            "{:?}",
            accepted.diagnostics
        );

        for (feature, source) in [
            (
                "aggregate function parameter",
                "package main\ntype Pair struct { value int }\nfunc take(pair Pair) {}\nfunc main() {}\n",
            ),
            (
                "address of composite literal",
                "package main\ntype Pair struct { value int }\nfunc main() { _ = &Pair{} }\n",
            ),
            (
                "composite literal field selection",
                "package main\ntype Pair struct { value int }\nfunc main() { _ = (Pair{}).value }\n",
            ),
            (
                "composite literal indexing",
                "package main\nfunc main() { _ = ([2]int{})[0] }\n",
            ),
        ] {
            let result = analyze(source);
            assert!(
                reports_unsupported(&result, feature),
                "{feature}: {:?}",
                result.diagnostics
            );
        }
    }

    #[test]
    fn rejects_type_forms_without_ir_v0_representations() {
        for source in [
            "package main\nvar value []int\n",
            "package main\nvar value func()\n",
            "package main\nvar value interface{}\n",
            "package main\nvar value map[int]int\n",
            "package main\nvar value chan int\n",
            "package main\ntype Inner struct { value int }\ntype Outer struct { Inner }\n",
        ] {
            let result = analyze(source);
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == UNSUPPORTED_TYPE),
                "{source:?}: {:?}",
                result.diagnostics
            );
        }
    }

    #[test]
    fn rejects_zero_sized_aggregates_and_non_scalar_results() {
        let result = analyze(
            "package main\n\
             type Empty struct {}\n\
             type Zero [0]int\n\
             type Pair struct { value int }\n\
             func many() (int, int) { return 1, 2 }\n\
             func aggregate() Pair { var value Pair; return value }\n",
        );
        assert!(reports_error(
            &result,
            UNSUPPORTED_TYPE,
            "empty structs are not supported by IR V0"
        ));
        assert!(reports_error(
            &result,
            UNSUPPORTED_TYPE,
            "zero-length arrays are not supported by IR V0"
        ));
        assert!(reports_unsupported(&result, "multiple function results"));
        assert!(reports_unsupported(&result, "aggregate function result"));
    }

    #[test]
    fn rejects_aggregate_comparisons() {
        let result = analyze(
            "package main\n\
             type Pair struct { value int }\n\
             func main() { var left Pair; var right Pair; _ = left == right }\n",
        );

        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == INVALID_OPERATION
                && diagnostic.message() == "equality comparison requires a comparable operand"
        }));
    }

    #[test]
    fn rejects_function_declarations_without_a_body() {
        let result = analyze(
            "package main\n\
             func foreign(value int)\n\
             func main() {}\n",
        );
        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == UNSUPPORTED_FEATURE
                && diagnostic.message()
                    == "function declaration without a body is not supported by the MVP"
        }));
    }

    #[test]
    fn rejects_unrepresentable_declarations_and_ellipsis_calls() {
        for (feature, source) in [
            (
                "type alias",
                "package main\ntype Alias = int\nfunc main() {}\n",
            ),
            (
                "type parameters",
                "package main\ntype Box[T int] int\nfunc main() {}\n",
            ),
            (
                "type parameters",
                "package main\nfunc identity[T int](value int) int { return value }\nfunc main() {}\n",
            ),
            (
                "method declaration",
                "package main\ntype Pair struct { value int }\nfunc (pair Pair) method() {}\nfunc main() {}\n",
            ),
            (
                "local declaration",
                "package main\nfunc main() { const value = 1 }\n",
            ),
            (
                "ellipsis call argument",
                "package main\nfunc take(values [1]int) {}\nfunc main() { var values [1]int; take(values...) }\n",
            ),
        ] {
            let result = analyze(source);
            assert!(
                reports_unsupported(&result, feature),
                "{feature}: {:?}",
                result.diagnostics
            );
        }

        let tagged = analyze(
            "package main\ntype Tagged struct { value int `json:\"value\"` }\nfunc main() {}\n",
        );
        assert!(reports_error(
            &tagged,
            UNSUPPORTED_TYPE,
            "struct field tags are not supported by IR V0"
        ));
    }

    #[test]
    fn rejects_unrepresentable_global_initializers() {
        for (feature, source) in [
            (
                "global initializer requiring runtime evaluation",
                "package main\nvar value int\nvar pointer = &value\nfunc main() {}\n",
            ),
            (
                "global initializer requiring runtime evaluation",
                "package main\nvar computed = getValue()\nfunc getValue() int { return 1 }\nfunc main() {}\n",
            ),
            (
                "composite literal",
                "package main\ntype Pair struct { value int }\nvar pair Pair = Pair{value: 1}\nfunc main() {}\n",
            ),
        ] {
            let result = analyze(source);
            assert!(
                reports_unsupported(&result, feature),
                "{feature}: {:?}",
                result.diagnostics
            );
        }
    }

    #[test]
    fn supports_byte_operations_and_rejects_nil_type_inference() {
        let byte = analyze(
            "package main\n\
             func bump(value byte) byte { value++; return value + value }\n",
        );
        assert!(byte.diagnostics.is_empty(), "{:?}", byte.diagnostics);

        let nil = analyze(
            "package main\n\
             var global = nil\n\
             func main() { var local = nil }\n",
        );
        assert!(
            nil.diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == MISSING_VARIABLE_TYPE)
                .count()
                >= 2
        );
    }

    #[test]
    fn resolves_global_initializers_in_dependency_order_and_folds_constants() {
        let result = analyze(
            "package main\n\
             const limit = base + 3\n\
             const base = 2\n\
             var next = limit + 1\n\
             var ready bool = true\n\
             var zero int\n",
        );

        assert!(result.diagnostics.is_empty());
        let limit = result
            .info
            .defs
            .values()
            .copied()
            .find(|object| {
                result.symbols.name(result.symbols.object(*object).name) == Some("limit")
            })
            .unwrap();
        let ObjectKind::Const {
            value: ConstValue::Int(value),
        } = &result.symbols.object(limit).kind
        else {
            panic!("limit must be an integer constant");
        };
        assert_eq!(value.to_i128(), Some(5));
        let zero = result
            .info
            .defs
            .values()
            .copied()
            .find(|object| result.symbols.name(result.symbols.object(*object).name) == Some("zero"))
            .unwrap();
        assert_eq!(result.symbols.object(zero).typ, result.predeclared.int);
    }

    #[test]
    fn reports_global_initialization_cycles_and_type_mismatches() {
        let cycle = analyze("package main\nvar first = second\nvar second = first\n");
        assert!(
            cycle
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message() == "initialization cycle")
        );

        let mismatch = analyze("package main\nvar ready bool = 1\n");
        assert!(
            mismatch
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == TYPE_MISMATCH)
        );
    }
}
