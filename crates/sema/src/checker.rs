//! Package-level declaration collection.
//!
//! This is the first semantic checker slice: it creates the universe and
//! package scopes, records top-level objects, and fills definition facts. Type
//! headers, values, and function bodies deliberately remain later phases.

use std::collections::{HashMap, HashSet};

use gane_diagnostics::{Diagnostic, DiagnosticCode, Diagnostics, Label, Span};
use gane_parser::{
    ast::{self, Decl, Spec},
    token::Token,
};

use crate::{
    scope::{DeclareError, DuplicateDeclaration, ScopeArena},
    symbol_table::{declared_object, SymbolTable},
    types::{
        BasicType, ConstValue, FileId, IntegerValue, NodeId, ObjectId, ObjectKind, Package,
        PackageId, PackagePath, ScopeId, ScopeKind, SemanticInfo, TypeAndValue, TypeArena, TypeId,
        TypeKind, UnderlyingState, ValueMode,
    },
};

const DUPLICATE_DECLARATION: DiagnosticCode = DiagnosticCode("E2002");
const INVALID_PACKAGE: DiagnosticCode = DiagnosticCode("E2003");
const MIXED_PACKAGE: DiagnosticCode = DiagnosticCode("E2004");
const EMPTY_PACKAGE: DiagnosticCode = DiagnosticCode("E2005");
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

/// Stable mapping from source spans to checker-local node IDs.
#[derive(Clone, Debug, Default)]
pub struct NodeIndex {
    by_span: HashMap<Span, NodeId>,
    next: u32,
}

impl NodeIndex {
    pub fn id_for(&mut self, span: Span) -> NodeId {
        if let Some(&id) = self.by_span.get(&span) {
            return id;
        }
        self.next += 1;
        let id = NodeId::from_raw(self.next);
        self.by_span.insert(span, id);
        id
    }

    pub fn get(&self, span: Span) -> Option<NodeId> {
        self.by_span.get(&span).copied()
    }
}

/// Frozen output of package declaration collection and type-header resolution.
#[derive(Clone, Debug)]
pub struct AnalysisResult {
    pub package: Package,
    pub predeclared: PredeclaredTypes,
    pub(crate) types: TypeArena,
    pub(crate) symbols: SymbolTable,
    pub(crate) scopes: ScopeArena,
    pub(crate) nodes: NodeIndex,
    pub info: SemanticInfo,
    pub diagnostics: Vec<Diagnostic>,
}

/// Analyzes one package. Function bodies and declaration value expressions are
/// intentionally deferred, but type headers and function signatures are complete.
pub fn analyze_package(input: PackageInput<'_>) -> AnalysisResult {
    let mut checker = Checker::new(input);
    checker.check_package_clause();
    checker.create_universe();
    checker.create_package_scope();
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
    nodes: NodeIndex,
    info: SemanticInfo,
    diagnostics: Diagnostics,
    package_name: Option<crate::types::NameId>,
    package_scope: Option<ScopeId>,
    predeclared: Option<PredeclaredTypes>,
    type_specs: HashMap<TypeId, ast::TypeSpec>,
    func_decls: HashMap<TypeId, ast::FuncDecl>,
    function_scopes: HashMap<TypeId, ScopeId>,
    global_initializers: HashMap<ObjectId, GlobalInitializer>,
    global_states: HashMap<ObjectId, InitState>,
}

struct ControlContext {
    results: Vec<TypeId>,
    loop_depth: u32,
}

#[derive(Clone)]
struct GlobalInitializer {
    object: ObjectId,
    is_const: bool,
    typ: Option<ast::Expr>,
    value: Option<ast::Expr>,
    span: Span,
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
            nodes: NodeIndex::default(),
            info: SemanticInfo::default(),
            diagnostics: Diagnostics::default(),
            package_name: None,
            package_scope: None,
            predeclared: None,
            type_specs: HashMap::new(),
            func_decls: HashMap::new(),
            function_scopes: HashMap::new(),
            global_initializers: HashMap::new(),
            global_states: HashMap::new(),
        }
    }

    fn check_package_clause(&mut self) {
        let Some(first) = self.input.files.first() else {
            self.diagnostics.error(
                EMPTY_PACKAGE,
                Span::default(),
                "package contains no source files",
            );
            self.package_name = Some(self.symbols.intern(""));
            return;
        };

        let expected = first.ast.name.name.as_str();
        self.package_name = Some(self.symbols.intern(expected));
        if expected != "main" {
            self.diagnostics.error(
                INVALID_PACKAGE,
                ident_span(&first.ast.name),
                "MVP only supports `package main`",
            );
        }

        for file in self.input.files.iter().skip(1) {
            if file.ast.name.name != expected {
                self.diagnostics.error(
                    MIXED_PACKAGE,
                    ident_span(&file.ast.name),
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
        let span = self
            .input
            .files
            .first()
            .map(|file| Span::new(file.ast.pos(), file.ast.end()))
            .unwrap_or_default();
        self.package_scope = Some(self.scopes.child(
            self.scopes.universe(),
            ScopeKind::Package,
            span,
        ));
    }

    fn collect_top_level(&mut self) {
        // Clone declaration lists so collection can mutably update checker
        // arenas without holding an immutable borrow of self.input.
        let declarations: Vec<Decl> = self
            .input
            .files
            .iter()
            .flat_map(|file| file.ast.decls.clone())
            .collect();

        for declaration in &declarations {
            match declaration {
                Decl::GenDecl(decl) => self.collect_gen_decl(decl),
                Decl::FuncDecl(decl) => self.collect_func_decl(decl),
                Decl::BadDecl(_) => {}
            }
        }
    }

    fn collect_gen_decl(&mut self, decl: &ast::GenDecl) {
        match decl.tok {
            Token::Type => {
                for spec in &decl.specs {
                    if let Spec::TypeSpec(spec) = spec {
                        self.collect_type_spec(spec);
                    }
                }
            }
            Token::Const => {
                for spec in &decl.specs {
                    if let Spec::ValueSpec(spec) = spec {
                        self.collect_value_spec(spec, true);
                    }
                }
            }
            Token::Var => {
                for spec in &decl.specs {
                    if let Spec::ValueSpec(spec) = spec {
                        self.collect_value_spec(spec, false);
                    }
                }
            }
            Token::Import => self.unsupported(Span::new(decl.pos(), decl.end()), "import"),
            _ => {}
        }
    }

    fn collect_type_spec(&mut self, spec: &ast::TypeSpec) {
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
        self.type_specs.insert(named, spec.clone());
    }

    fn collect_func_decl(&mut self, decl: &ast::FuncDecl) {
        if decl.recv.is_some() {
            self.unsupported(Span::new(decl.pos(), decl.end()), "method declaration");
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
        self.declare_source_object(
            &decl.name,
            ObjectKind::Func {
                signature,
                is_extern: decl.body.is_none(),
            },
            name,
            signature,
        );
        self.func_decls.insert(signature, decl.clone());
    }

    fn collect_value_spec(&mut self, spec: &ast::ValueSpec, is_const: bool) {
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
            self.global_initializers.insert(
                object,
                GlobalInitializer {
                    object,
                    is_const,
                    typ: spec.typ.clone(),
                    value: spec.values.get(index).cloned(),
                    span: ident_span(ident),
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
        let objects: Vec<ObjectId> = self.global_initializers.keys().copied().collect();
        for object in objects {
            self.resolve_global(object);
        }
        let signatures: Vec<TypeId> = self.func_decls.keys().copied().collect();
        for signature in signatures {
            self.check_extern_abi(signature);
        }
    }

    fn resolve_global(&mut self, object: ObjectId) {
        match self.global_states.get(&object).copied() {
            Some(InitState::Done) => return,
            Some(InitState::Resolving) => {
                self.diagnostics.error(
                    TYPE_MISMATCH,
                    self.symbols.object(object).span,
                    "initialization cycle",
                );
                if let Some(global) = self.symbols.object_mut(object) {
                    global.typ = TypeId::INVALID;
                }
                return;
            }
            None => {}
        }
        let Some(initializer) = self.global_initializers.get(&object).cloned() else {
            return;
        };
        self.global_states.insert(object, InitState::Resolving);
        let explicit_type = initializer
            .typ
            .as_ref()
            .map(|typ| self.resolve_type_expr(typ, false));
        let value = initializer.value.as_ref().map(|value| {
            self.check_expr(value, self.package_scope.expect("package scope must exist"))
        });
        let typ = match (explicit_type, value.as_ref()) {
            (Some(typ), Some(value)) => {
                self.require_assignable(value.typ, typ, initializer.span);
                typ
            }
            (Some(typ), None) if !initializer.is_const => typ,
            (None, Some(value)) => value.typ,
            _ => {
                self.diagnostics.error(
                    TYPE_MISMATCH,
                    initializer.span,
                    "constant or variable requires an initializer or explicit type",
                );
                TypeId::INVALID
            }
        };
        if initializer.is_const && value.is_none() {
            self.diagnostics.error(
                TYPE_MISMATCH,
                initializer.span,
                "constant declaration requires an initializer",
            );
        }
        let constant = value.and_then(|value| value.constant);
        if let Some(global) = self.symbols.object_mut(initializer.object) {
            global.typ = typ;
            if let ObjectKind::Const { value } = &mut global.kind {
                *value = constant.unwrap_or(ConstValue::Unknown);
            }
        }
        self.global_states.insert(object, InitState::Done);
    }

    fn check_extern_abi(&mut self, signature: TypeId) {
        let Some(decl) = self.func_decls.get(&signature) else {
            return;
        };
        if decl.body.is_some() {
            return;
        }
        let TypeKind::Signature {
            params, results, ..
        } = self.types.get(signature).kind
        else {
            return;
        };
        let objects = self
            .types
            .tuple(params)
            .into_iter()
            .flat_map(|tuple| tuple.vars.iter())
            .chain(
                self.types
                    .tuple(results)
                    .into_iter()
                    .flat_map(|tuple| tuple.vars.iter()),
            )
            .copied()
            .collect::<Vec<_>>();
        for object in objects {
            let typ = self.symbols.object(object).typ;
            if !self.is_abi_type(typ) {
                self.diagnostics.error(
                    UNSUPPORTED_FEATURE,
                    self.symbols.object(object).span,
                    "extern function ABI only supports bool, int, byte, and pointers",
                );
            }
        }
    }

    fn is_abi_type(&self, typ: TypeId) -> bool {
        matches!(
            self.types.get(self.types.underlying(typ)).kind,
            TypeKind::Basic(BasicType::Bool | BasicType::Int | BasicType::Byte)
                | TypeKind::Pointer { .. }
        )
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
                        Span::default(),
                        "invalid recursive type: cycle requires pointer indirection",
                    );
                    return TypeId::INVALID;
                }
                named
            }
            UnderlyingState::Unresolved => {
                let Some(spec) = self.type_specs.get(&named).cloned() else {
                    return named;
                };
                if let TypeKind::Named { underlying, .. } =
                    &mut self.types.get_mut(named).unwrap().kind
                {
                    *underlying = UnderlyingState::Resolving;
                }
                let underlying = self.resolve_type_expr(&spec.typ, false);
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

    fn resolve_type_expr(&mut self, expr: &ast::Expr, indirect: bool) -> TypeId {
        match expr {
            ast::Expr::Ident(ident) => self.resolve_type_name(ident, indirect),
            ast::Expr::ParenExpr(expr) => self.resolve_type_expr(&expr.x, indirect),
            ast::Expr::StarExpr(expr) => {
                let base = self.resolve_type_expr(&expr.x, true);
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
                        Span::default(),
                        "MVP array length must be a non-negative decimal integer literal",
                    );
                    return TypeId::INVALID;
                };
                let element = self.resolve_type_expr(&expr.elt, indirect);
                if element == TypeId::INVALID {
                    TypeId::INVALID
                } else {
                    self.types.alloc(TypeKind::Array {
                        len: length,
                        elem: element,
                    })
                }
            }
            ast::Expr::StructType(struct_type) => self.resolve_struct(struct_type),
            _ => {
                self.diagnostics.error(
                    UNSUPPORTED_TYPE,
                    Span::default(),
                    "type syntax is not supported by the MVP",
                );
                TypeId::INVALID
            }
        }
    }

    fn resolve_type_name(&mut self, ident: &ast::Ident, indirect: bool) -> TypeId {
        let name = self.symbols.intern(&ident.name);
        let Some(resolved) = self
            .scopes
            .lookup(self.package_scope.expect("package scope must exist"), name)
        else {
            self.diagnostics.error(
                UNKNOWN_TYPE,
                ident_span(ident),
                format!("undefined type `{}`", ident.name),
            );
            return TypeId::INVALID;
        };
        match self.symbols.object(resolved.object).kind {
            ObjectKind::TypeName { named, .. } => self.resolve_named(named, indirect),
            _ => {
                self.diagnostics.error(
                    UNKNOWN_TYPE,
                    ident_span(ident),
                    format!("`{}` does not name a type", ident.name),
                );
                TypeId::INVALID
            }
        }
    }

    fn resolve_struct(&mut self, struct_type: &ast::StructType) -> TypeId {
        let mut fields = Vec::new();
        let mut seen = HashSet::new();
        let mut invalid = false;
        let Some(field_list) = &struct_type.fields else {
            return self.types.alloc(TypeKind::Struct { fields });
        };

        for field in &field_list.list {
            let Some(typ_expr) = &field.typ else {
                self.diagnostics.error(
                    UNSUPPORTED_TYPE,
                    Span::default(),
                    "struct field has no type",
                );
                continue;
            };
            let typ = self.resolve_type_expr(typ_expr, false);
            invalid |= typ == TypeId::INVALID;
            if field.names.is_empty() {
                self.diagnostics.error(
                    UNSUPPORTED_TYPE,
                    Span::new(field.pos(), field.end()),
                    "embedded struct fields are not supported by the MVP",
                );
                continue;
            }
            for ident in &field.names {
                let name = self.symbols.intern(&ident.name);
                if !seen.insert(name) {
                    self.diagnostics.error(
                        DUPLICATE_FIELD,
                        ident_span(ident),
                        format!("duplicate struct field `{}`", ident.name),
                    );
                }
                let index = fields.len() as u32;
                let object = self.declare_field(ident, name, index, typ);
                fields.push(object);
            }
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
        let span = ident_span(ident);
        let object = self.symbols.alloc(declared_object(
            ObjectKind::Field {
                index,
                embedded: false,
            },
            name,
            Some(PackageId::from_raw(0)),
            self.package_scope.expect("package scope must exist"),
            span,
            typ,
        ));
        self.info.defs.insert(self.nodes.id_for(span), object);
        object
    }

    fn resolve_signature(&mut self, signature: TypeId) {
        let Some(decl) = self.func_decls.get(&signature).cloned() else {
            return;
        };
        let function_scope = self.scopes.child(
            self.package_scope.expect("package scope must exist"),
            ScopeKind::Function,
            Span::new(decl.pos(), decl.end()),
        );
        self.function_scopes.insert(signature, function_scope);
        let params = self.resolve_tuple(decl.typ.params.as_ref(), function_scope);
        let results = self.resolve_tuple(decl.typ.results.as_ref(), function_scope);
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
                    .map(|expr| self.resolve_type_expr(expr, false))
                    .unwrap_or(TypeId::INVALID);
                if field.names.is_empty() {
                    vars.push(self.declare_unnamed_param(scope, field, vars.len() as u32, typ));
                } else {
                    for ident in &field.names {
                        let name = self.symbols.intern(&ident.name);
                        let span = ident_span(ident);
                        let node = self.nodes.id_for(span);
                        let object = self.declare_in_scope(
                            scope,
                            name,
                            ObjectKind::Param {
                                index: vars.len() as u32,
                            },
                            Some(PackageId::from_raw(0)),
                            span,
                            typ,
                            Some(node),
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
            Span::new(field.pos(), field.end()),
            typ,
        ))
    }

    fn check_function_bodies(&mut self) {
        let functions: Vec<(TypeId, ast::FuncDecl)> = self
            .func_decls
            .iter()
            .map(|(&signature, decl)| (signature, decl.clone()))
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
                loop_depth: 0,
            };
            self.info
                .scopes
                .insert(self.nodes.id_for(ident_span(&decl.name)), scope);
            let returns = self.check_block(body, scope, &mut control);
            if !control.results.is_empty() && !returns {
                self.diagnostics.error(
                    MISSING_RETURN,
                    ident_span(&decl.name),
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
        let scope = self.scopes.child(
            parent,
            ScopeKind::Block,
            Span::new(block.pos(), block.end()),
        );
        self.info.scopes.insert(
            self.nodes.id_for(Span::new(block.pos(), block.end())),
            scope,
        );
        let mut returns = false;
        for statement in &block.list {
            returns |= self.check_stmt(statement, scope, control);
        }
        returns
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
                self.check_expr(&statement.x, scope);
            }
            ast::Stmt::AssignStmt(statement) => {
                if statement.tok == Token::Define {
                    self.unsupported(
                        Span::new(statement.pos(), statement.end()),
                        "short variable declaration",
                    );
                }
                self.check_assignment(&statement.lhs, &statement.rhs, scope);
            }
            ast::Stmt::IncDecStmt(statement) => {
                self.check_expr(&statement.x, scope);
            }
            ast::Stmt::ReturnStmt(statement) => {
                self.check_return(statement, scope, control);
                guaranteed_return = true;
            }
            ast::Stmt::IfStmt(statement) => {
                let if_scope = self.scopes.child(
                    scope,
                    ScopeKind::Block,
                    Span::new(statement.pos(), statement.end()),
                );
                if let Some(init) = &statement.init {
                    self.unsupported(Span::new(init.pos(), init.end()), "if initializer");
                    self.check_stmt(init, if_scope, control);
                }
                let condition = self.check_expr(&statement.cond, if_scope);
                self.require_boolean(
                    &condition,
                    Span::new(statement.cond.pos(), statement.cond.end()),
                );
                let then_returns = self.check_block(&statement.body, if_scope, control);
                let else_returns = statement
                    .else_
                    .as_ref()
                    .map(|else_| self.check_stmt(else_, if_scope, control))
                    .unwrap_or(false);
                guaranteed_return = then_returns && else_returns;
            }
            ast::Stmt::ForStmt(statement) => {
                let for_scope = self.scopes.child(
                    scope,
                    ScopeKind::Block,
                    Span::new(statement.pos(), statement.end()),
                );
                if let Some(init) = &statement.init {
                    self.unsupported(
                        Span::new(init.pos(), init.end()),
                        "three-clause for initializer",
                    );
                    self.check_stmt(init, for_scope, control);
                }
                if let Some(condition) = &statement.cond {
                    let condition_value = self.check_expr(condition, for_scope);
                    self.require_boolean(
                        &condition_value,
                        Span::new(condition.pos(), condition.end()),
                    );
                }
                if let Some(post) = &statement.post {
                    self.unsupported(
                        Span::new(post.pos(), post.end()),
                        "three-clause for post statement",
                    );
                    self.check_stmt(post, for_scope, control);
                }
                control.loop_depth += 1;
                let body_returns = self.check_block(&statement.body, for_scope, control);
                control.loop_depth -= 1;
                guaranteed_return = statement.cond.is_none() && body_returns;
            }
            ast::Stmt::LabeledStmt(statement) => {
                guaranteed_return = self.check_stmt(&statement.stmt, scope, control);
            }
            ast::Stmt::SendStmt(statement) => {
                self.check_expr(&statement.chan_, scope);
                self.check_expr(&statement.value, scope);
            }
            ast::Stmt::GoStmt(statement) => {
                self.unsupported(Span::new(statement.pos(), statement.end()), "go statement");
                self.check_call(&statement.call, scope);
            }
            ast::Stmt::DeferStmt(statement) => {
                self.unsupported(
                    Span::new(statement.pos(), statement.end()),
                    "defer statement",
                );
                self.check_call(&statement.call, scope);
            }
            ast::Stmt::RangeStmt(statement) => {
                self.unsupported(Span::new(statement.pos(), statement.end()), "range");
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
                self.unsupported(Span::new(statement.pos(), statement.end()), "switch");
                if let Some(init) = &statement.init {
                    self.check_stmt(init, scope, control);
                }
                if let Some(tag) = &statement.tag {
                    self.check_expr(tag, scope);
                }
                self.check_block(&statement.body, scope, control);
            }
            ast::Stmt::TypeSwitchStmt(statement) => {
                self.unsupported(Span::new(statement.pos(), statement.end()), "type switch");
                if let Some(init) = &statement.init {
                    self.check_stmt(init, scope, control);
                }
                self.check_stmt(&statement.assign, scope, control);
                self.check_block(&statement.body, scope, control);
            }
            ast::Stmt::SelectStmt(statement) => {
                self.unsupported(Span::new(statement.pos(), statement.end()), "select");
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
            ast::Stmt::BranchStmt(statement) => self.check_branch(statement, control),
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
                Span::new(statement.pos(), statement.end()),
                "return has an incorrect number of values",
            );
        }
        for (value, result) in values.iter().zip(&control.results) {
            self.require_assignable(
                value.typ,
                *result,
                Span::new(statement.pos(), statement.end()),
            );
        }
    }

    fn check_branch(&mut self, statement: &ast::BranchStmt, control: &ControlContext) {
        match statement.tok {
            Token::Break | Token::Continue if control.loop_depth > 0 => {}
            Token::Break | Token::Continue => self.diagnostics.error(
                INVALID_BRANCH,
                Span::new(statement.pos(), statement.end()),
                "break or continue is only valid inside a for loop",
            ),
            _ => self.unsupported(
                Span::new(statement.pos(), statement.end()),
                "labeled branch statement",
            ),
        }
    }

    fn unsupported(&mut self, span: Span, feature: &str) {
        self.diagnostics.error(
            UNSUPPORTED_FEATURE,
            span,
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
                Span::default(),
                "MVP executable requires func main()",
            );
            return;
        };
        let object = self.symbols.object(main);
        let ObjectKind::Func { signature, .. } = object.kind else {
            self.diagnostics.error(
                INVALID_ENTRY_POINT,
                object.span,
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
                object.span,
                "main must not have parameters or results",
            );
        }
    }

    fn check_local_decl(&mut self, declaration: &ast::Decl, scope: ScopeId) {
        let ast::Decl::GenDecl(declaration) = declaration else {
            return;
        };
        if declaration.tok != Token::Var {
            self.unsupported(
                Span::new(declaration.pos(), declaration.end()),
                "local declaration",
            );
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
            .map(|expr| self.resolve_type_expr(expr, false));
        let typ = match (explicit_type, values.first()) {
            (Some(typ), _) => typ,
            (None, Some(value)) => value.typ,
            (None, None) => {
                self.diagnostics.error(
                    MISSING_VARIABLE_TYPE,
                    Span::new(spec.pos(), spec.end()),
                    "local variable requires an explicit type or an initializer",
                );
                TypeId::INVALID
            }
        };
        if let Some(explicit_type) = explicit_type {
            for value in &values {
                self.require_assignable(
                    value.typ,
                    explicit_type,
                    Span::new(spec.pos(), spec.end()),
                );
            }
        }
        if spec.names.len() != values.len() && !values.is_empty() {
            self.diagnostics.error(
                TYPE_MISMATCH,
                Span::new(spec.pos(), spec.end()),
                "variable declaration has a different number of names and values",
            );
        }
        for ident in &spec.names {
            let name = self.symbols.intern(&ident.name);
            let span = ident_span(ident);
            let node = self.nodes.id_for(span);
            self.declare_in_scope(
                scope,
                name,
                ObjectKind::Var { embedded: false },
                Some(PackageId::from_raw(0)),
                span,
                typ,
                Some(node),
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
            ast::Expr::BasicLit(_) | ast::Expr::BadExpr(_) => self.invalid_value(),
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
                        Span::new(expr.pos(), expr.end()),
                        "cannot dereference a non-pointer",
                    ),
                }
            }
            ast::Expr::UnaryExpr(expr) => self.check_unary(expr, scope),
            ast::Expr::BinaryExpr(expr) => self.check_binary(expr, scope),
            ast::Expr::IndexExpr(expr) => {
                let array = self.check_expr(&expr.x, scope);
                let index = self.check_expr(&expr.index, scope);
                self.require_integer(&index, Span::new(expr.index.pos(), expr.index.end()));
                match self.types.array_element(array.typ) {
                    Some(typ) => TypeAndValue {
                        typ,
                        mode: ValueMode::Variable,
                        constant: None,
                    },
                    None => self.invalid_operation(
                        Span::new(expr.pos(), expr.end()),
                        "indexing requires an array",
                    ),
                }
            }
            ast::Expr::SelectorExpr(expr) => self.check_selector(expr, scope),
            ast::Expr::CallExpr(expr) => self.check_call(expr, scope),
            ast::Expr::SliceExpr(expr) => {
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
                self.check_expr(&expr.x, scope);
                for index in &expr.indices {
                    self.check_expr(index, scope);
                }
                self.invalid_value()
            }
            ast::Expr::KeyValueExpr(expr) => {
                self.check_expr(&expr.key, scope);
                self.check_expr(&expr.value, scope);
                self.invalid_value()
            }
            ast::Expr::CompositeLit(expr) => {
                for element in &expr.elts {
                    self.check_expr(element, scope);
                }
                self.invalid_value()
            }
            ast::Expr::FuncLit(_) => self.invalid_value(),
            ast::Expr::TypeAssertExpr(expr) => {
                self.check_expr(&expr.x, scope);
                self.invalid_value()
            }
            ast::Expr::Ellipsis(_)
            | ast::Expr::ArrayType(_)
            | ast::Expr::StructType(_)
            | ast::Expr::FuncType(_)
            | ast::Expr::InterfaceType(_)
            | ast::Expr::MapType(_)
            | ast::Expr::ChanType(_) => self.invalid_value(),
        };
        self.info.types.insert(
            self.nodes.id_for(Span::new(expr.pos(), expr.end())),
            result.clone(),
        );
        result
    }

    fn check_call(&mut self, call: &ast::CallExpr, scope: ScopeId) -> TypeAndValue {
        let function = self.check_expr(&call.fun, scope);
        let TypeKind::Signature {
            params, results, ..
        } = self.types.get(function.typ).kind
        else {
            return self.invalid_operation(
                Span::new(call.pos(), call.end()),
                "call requires a function",
            );
        };
        let parameter_count = self
            .types
            .tuple(params)
            .map(|tuple| tuple.vars.len())
            .unwrap_or(0);
        if call.args.len() != parameter_count {
            self.diagnostics.error(
                TYPE_MISMATCH,
                Span::new(call.pos(), call.end()),
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
                    Span::new(call.pos(), call.end()),
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
            Token::Add | Token::Sub if self.types.is_basic(operand.typ, BasicType::Int) => {
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
            Token::And if operand.mode == ValueMode::Variable => TypeAndValue {
                typ: self.types.alloc(TypeKind::Pointer { base: operand.typ }),
                mode: ValueMode::Value,
                constant: None,
            },
            _ => {
                self.invalid_operation(Span::new(expr.pos(), expr.end()), "invalid unary operation")
            }
        }
    }

    fn check_binary(&mut self, expr: &ast::BinaryExpr, scope: ScopeId) -> TypeAndValue {
        let left = self.check_expr(&expr.x, scope);
        let right = self.check_expr(&expr.y, scope);
        match expr.op {
            Token::Add | Token::Sub | Token::Mul | Token::Quo | Token::Rem => {
                self.require_integer(&left, Span::new(expr.x.pos(), expr.x.end()));
                self.require_integer(&right, Span::new(expr.y.pos(), expr.y.end()));
                TypeAndValue {
                    typ: self.predeclared.expect("universe must be initialized").int,
                    mode: ValueMode::Value,
                    constant: self.fold_integer_binary(expr.op, &left.constant, &right.constant),
                }
            }
            Token::Less | Token::Leq | Token::Greater | Token::Geq => {
                self.require_integer(&left, Span::new(expr.x.pos(), expr.x.end()));
                self.require_integer(&right, Span::new(expr.y.pos(), expr.y.end()));
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
                        Span::new(expr.x.pos(), expr.x.end()),
                        "equality comparison requires a comparable operand",
                    );
                }
                self.require_assignable(right.typ, left.typ, Span::new(expr.pos(), expr.end()));
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
                self.require_boolean(&left, Span::new(expr.x.pos(), expr.x.end()));
                self.require_boolean(&right, Span::new(expr.y.pos(), expr.y.end()));
                TypeAndValue {
                    typ: self
                        .predeclared
                        .expect("universe must be initialized")
                        .bool_,
                    mode: ValueMode::Value,
                    constant: self.fold_boolean_binary(expr.op, &left.constant, &right.constant),
                }
            }
            _ => self.invalid_operation(
                Span::new(expr.pos(), expr.end()),
                "invalid binary operation",
            ),
        }
    }

    fn check_selector(&mut self, expr: &ast::SelectorExpr, scope: ScopeId) -> TypeAndValue {
        let receiver = self.check_expr(&expr.x, scope);
        let mut typ = receiver.typ;
        let mut indirect = false;
        if let Some(base) = self.types.deref(typ) {
            typ = base;
            indirect = true;
        }
        let TypeKind::Struct { fields } = &self.types.get(self.types.underlying(typ)).kind else {
            return self.invalid_operation(
                Span::new(expr.pos(), expr.end()),
                "field selection requires a struct",
            );
        };
        let name = self.symbols.intern(&expr.sel.name);
        let Some((index, field)) = fields
            .iter()
            .enumerate()
            .find(|(_, field)| self.symbols.object(**field).name == name)
        else {
            return self.invalid_operation(ident_span(&expr.sel), "unknown struct field");
        };
        let field = *field;
        self.info.selections.insert(
            self.nodes.id_for(Span::new(expr.pos(), expr.end())),
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

    fn check_assignment(&mut self, lhs: &[ast::Expr], rhs: &[ast::Expr], scope: ScopeId) {
        if lhs.len() != rhs.len() {
            self.diagnostics.error(
                TYPE_MISMATCH,
                Span::default(),
                "assignment has a different number of left and right values",
            );
        }
        for (left, right) in lhs.iter().zip(rhs) {
            let left = self.check_expr(left, scope);
            let right = self.check_expr(right, scope);
            if left.mode != ValueMode::Variable {
                self.diagnostics.error(
                    EXPECTED_VARIABLE,
                    Span::default(),
                    "assignment target is not assignable",
                );
            }
            self.require_assignable(right.typ, left.typ, Span::default());
        }
    }

    fn bind_value_name(&mut self, ident: &ast::Ident, scope: ScopeId) -> TypeAndValue {
        if ident.name == "_" {
            return self.invalid_value();
        }
        let name = self.symbols.intern(&ident.name);
        let Some(resolved) = self.scopes.lookup(scope, name) else {
            self.diagnostics.error(
                UNDEFINED_NAME,
                ident_span(ident),
                format!("undefined name `{}`", ident.name),
            );
            return self.invalid_value();
        };
        if self.global_initializers.contains_key(&resolved.object) {
            self.resolve_global(resolved.object);
        }
        let object = self.symbols.object(resolved.object);
        if matches!(&object.kind, ObjectKind::TypeName { .. }) {
            self.diagnostics.error(
                TYPE_USED_AS_VALUE,
                ident_span(ident),
                format!("type `{}` used as a value", ident.name),
            );
            return self.invalid_value();
        }
        self.info
            .uses
            .insert(self.nodes.id_for(ident_span(ident)), resolved.object);
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

    fn invalid_operation(&mut self, span: Span, message: &str) -> TypeAndValue {
        self.diagnostics.error(INVALID_OPERATION, span, message);
        self.invalid_value()
    }

    fn require_assignable(&mut self, source: TypeId, target: TypeId, span: Span) {
        if !self.types.assignable_to(source, target) {
            self.diagnostics.error(
                TYPE_MISMATCH,
                span,
                "value is not assignable to the target type",
            );
        }
    }

    fn require_integer(&mut self, value: &TypeAndValue, span: Span) {
        if value.typ != TypeId::INVALID && !self.types.is_basic(value.typ, BasicType::Int) {
            self.diagnostics
                .error(INVALID_OPERATION, span, "operation requires an int operand");
        }
    }

    fn require_boolean(&mut self, value: &TypeAndValue, span: Span) {
        if value.typ != TypeId::INVALID && !self.types.is_basic(value.typ, BasicType::Bool) {
            self.diagnostics
                .error(EXPECTED_BOOLEAN, span, "condition must have type bool");
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
            Span::default(),
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
            Span::default(),
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
            Span::default(),
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
        let span = ident_span(ident);
        let node = self.nodes.id_for(span);
        let object = self.declare_in_scope(
            self.package_scope.expect("package scope must exist"),
            name,
            kind,
            Some(PackageId::from_raw(0)),
            span,
            typ,
            Some(node),
        );
        object
    }

    #[allow(clippy::too_many_arguments)]
    fn declare_in_scope(
        &mut self,
        scope: ScopeId,
        name: crate::types::NameId,
        kind: ObjectKind,
        package: Option<PackageId>,
        span: Span,
        typ: TypeId,
        definition: Option<NodeId>,
    ) -> ObjectId {
        let object = self
            .symbols
            .alloc(declared_object(kind, name, package, scope, span, typ));
        if let Some(node) = definition {
            self.info.defs.insert(node, object);
        }

        if let Err(error) = self.scopes.declare(scope, name, object) {
            match error {
                DeclareError::Duplicate(duplicate) => self.report_duplicate(span, duplicate),
                DeclareError::UnknownScope(_) => unreachable!("checker created an invalid scope"),
            }
        }
        object
    }

    fn report_duplicate(&mut self, span: Span, duplicate: DuplicateDeclaration) {
        let previous = self.symbols.object(duplicate.existing);
        self.diagnostics.push(
            Diagnostic::new(
                gane_diagnostics::Severity::Error,
                DUPLICATE_DECLARATION,
                span,
                "duplicate declaration in scope",
            )
            .with_secondary(Label::new(previous.span, "previous declaration is here")),
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
            nodes: self.nodes,
            info: self.info,
            diagnostics: self.diagnostics.finish(),
        }
    }
}

fn ident_span(ident: &ast::Ident) -> Span {
    Span::new(ident.pos(), ident.end())
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

#[cfg(test)]
mod tests {
    use super::*;
    use gane_parser::{parser::parse_file, parser::Mode, token::FileSet};

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
        assert!(invalid
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == INVALID_RECURSIVE_TYPE));
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
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == UNDEFINED_NAME));
    }

    #[test]
    fn reports_type_names_used_as_values() {
        let result = analyze("package main\nfunc f() { int = 1 }\n");

        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == TYPE_USED_AS_VALUE));
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

        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == TYPE_MISMATCH));
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == EXPECTED_BOOLEAN));
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
        assert!(missing
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == INVALID_ENTRY_POINT));

        let invalid = analyze("package main\nfunc main(value int) {}\n");
        assert!(invalid
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == INVALID_ENTRY_POINT));
    }

    #[test]
    fn checks_return_values_and_all_paths_returning() {
        let missing = analyze(
            "package main\n\
             func choose(ok bool) int { if ok { return 1 } }\n",
        );
        assert!(missing
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == MISSING_RETURN));

        let valid = analyze(
            "package main\n\
             func choose(ok bool) int { if ok { return 1 } else { return 2 } }\n",
        );
        assert!(valid.diagnostics.is_empty());

        let invalid_return = analyze("package main\nfunc helper() { return 1 }\n");
        assert!(invalid_return
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == INVALID_RETURN));
    }

    #[test]
    fn validates_branch_context_and_rejects_mvp_features() {
        let result = analyze("package main\nfunc main() { break; value := 1 }\n");

        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == INVALID_BRANCH));
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == UNSUPPORTED_FEATURE));
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
        assert!(cycle
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message == "initialization cycle"));

        let mismatch = analyze("package main\nvar ready bool = 1\n");
        assert!(mismatch
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == TYPE_MISMATCH));
    }
}
