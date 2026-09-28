//! Package-level AST-to-IR lowering orchestration.
//!
//! This module owns the parts of lowering that require package-wide state:
//! discovering the `main` object, creating IR globals, declaring every
//! function before any function body is lowered, and maintaining the mappings
//! from sema objects to their IR global/function IDs.
//!
//! The lowering flow is:
//!
//! 1. Require a package-level `main` function. (v0 required)
//! 2. Visit all input files. Constants are already folded by sema and have no
//!    runtime declaration here; named types are lowered lazily by
//!    [`lower_type()`]; global variables are converted to IR globals; and every
//!    function is declared with its lowered signature.
//! 3. Mark the IR function corresponding to `main` as the package entry.
//! 4. Require the V0 `main()` signature: no parameters and no results.
//! 5. Lower every function body using `FunctionLowerer`, after all function
//!    declarations are known so calls can refer to functions declared later in
//!    the source package.
//! 6. Finish the shared `IrBuilder` and return the unverified IR package.
//!
//! This pass does not verify the resulting IR. Verification happens after
//! lowering, and the resulting package is not suitable for the interpreter or
//! codegen until it passes the verifier and the separate escape check.

use super::*;

impl<'a> PackageLowerer<'a> {
    /// Lowers all package-level declarations and function bodies into raw IR.
    ///
    /// The first declaration pass creates globals and function declarations,
    /// while the second pass lowers function bodies.
    pub(super) fn lower(
        mut self,
        input: &PackageInput<'_>,
    ) -> Result<UnverifiedIrPackage, LowerError> {
        // Guard for main package
        let main_object = self.analysis.package_member("main").ok_or_else(|| {
            self.unsupported(
                input
                    .files
                    .first()
                    .map(|file| file.ast.node_id())
                    .unwrap_or(AstNodeId::INVALID),
                "package without main",
            )
        })?;
        for file in &input.files {
            for declaration in &file.ast.decls {
                match declaration {
                    ast::Decl::GenDecl(declaration) if declaration.tok == Token::Const => {
                        // Constants are calculated in `sema` and related to identities relatively.
                        // Constants will be inlined into every reference site.
                    }
                    ast::Decl::GenDecl(declaration) if declaration.tok == Token::Type => {
                        // Named types are lowered lazily through `lower_type()`.
                    }
                    ast::Decl::GenDecl(declaration) if declaration.tok == Token::Var => {
                        self.lower_global_declaration(declaration)?;
                    }
                    ast::Decl::GenDecl(declaration) => {
                        return Err(
                            self.unsupported(declaration.node_id(), "top-level declaration")
                        );
                    }
                    ast::Decl::FuncDecl(declaration) => {
                        let object = self.definition(declaration.name.node_id())?;
                        let signature = self.function_signature(object, declaration.node_id())?;
                        let function = self.builder.declare_function(
                            format!("gane.{}", declaration.name.name),
                            signature.clone(),
                            FunctionAttributes::default(),
                        );
                        if object == main_object {
                            self.builder.set_entry(function).map_err(|source| {
                                LowerError::Build {
                                    node: declaration.node_id(),
                                    source,
                                }
                            })?;
                        }
                        self.functions.insert(
                            object,
                            LoweredFunction {
                                id: function,
                                signature,
                            },
                        );
                    }
                    ast::Decl::BadDecl(declaration) => {
                        return Err(self.unsupported(declaration.node_id(), "bad declaration"));
                    }
                }
            }
        }

        let main = self.functions.get(&main_object).cloned().ok_or_else(|| {
            self.missing(
                input
                    .files
                    .first()
                    .map(|file| file.ast.node_id())
                    .unwrap_or(AstNodeId::INVALID),
                "main declaration",
            )
        })?;
        if main.signature.parameters.is_empty() && main.signature.results.is_empty() {
            for file in &input.files {
                for declaration in &file.ast.decls {
                    if let ast::Decl::FuncDecl(declaration) = declaration {
                        self.lower_function(declaration)?;
                    }
                }
            }
        } else {
            return Err(self.unsupported(
                input
                    .files
                    .first()
                    .map(|file| file.ast.node_id())
                    .unwrap_or(AstNodeId::INVALID),
                "main signature",
            ));
        }
        self.builder.finish().map_err(|source| LowerError::Build {
            node: AstNodeId::INVALID,
            source,
        })
    }

    /// Converts a sema function object into an IR signature.
    ///
    /// V0 accepts only non-method, non-variadic functions with scalar
    /// parameters/results and at most one result. The function body is not
    /// touched here.
    fn function_signature(
        &mut self,
        function: ObjectId,
        node: AstNodeId,
    ) -> Result<IrSignature, LowerError> {
        let ObjectKind::Func { signature } = self.analysis.object(function).kind else {
            return Err(self.missing(node, "function signature"));
        };
        let TypeKind::Signature {
            receiver,
            params,
            results,
            variadic,
        } = &self.analysis.type_of(signature).kind
        else {
            return Err(self.missing(node, "function signature type"));
        };
        if receiver.is_some() || *variadic {
            return Err(self.unsupported(node, "function signature"));
        }
        let parameter_objects = self
            .analysis
            .tuple(*params)
            .ok_or_else(|| self.missing(node, "function parameters"))?
            .vars
            .clone();
        let mut parameters = Vec::with_capacity(parameter_objects.len());
        for object in parameter_objects {
            let typ = self.lower_type(self.analysis.object(object).typ, node)?;
            if self.is_aggregate(typ) {
                return Err(self.unsupported(node, "aggregate parameter"));
            }
            parameters.push(IrParameter { typ });
        }
        let result_objects = self
            .analysis
            .tuple(*results)
            .ok_or_else(|| self.missing(node, "function results"))?
            .vars
            .clone();
        let mut results = Vec::with_capacity(result_objects.len());
        for object in result_objects {
            let typ = self.lower_type(self.analysis.object(object).typ, node)?;
            if self.is_aggregate(typ) {
                return Err(self.unsupported(node, "aggregate result"));
            }
            results.push(typ);
        }
        if results.len() > 1 {
            return Err(self.unsupported(node, "multiple function results"));
        }
        Ok(IrSignature {
            parameters,
            results,
        })
    }

    /// Lowers one function body after its IR declaration already exists.
    pub(super) fn lower_function(&mut self, declaration: &ast::FuncDecl) -> Result<(), LowerError> {
        let object = self.definition(declaration.name.node_id())?;
        let function = self
            .functions
            .get(&object)
            .cloned()
            .ok_or_else(|| self.missing(declaration.node_id(), "declared function"))?;
        let body = declaration
            .body
            .as_deref()
            .ok_or_else(|| self.unsupported(declaration.node_id(), "bodyless function"))?;
        FunctionLowerer::new(self, function, object, declaration.node_id())?.lower(body)
    }

    /// Lowers package-level variable declarations into IR globals.
    ///
    /// The initializer has already been classified by sema as either zero
    /// initialization or a supported scalar constant. This method only
    /// translates that result and records the sema-object-to-IR-global mapping.
    fn lower_global_declaration(&mut self, declaration: &ast::GenDecl) -> Result<(), LowerError> {
        for spec in &declaration.specs {
            let ast::Spec::ValueSpec(spec) = spec else {
                return Err(self.unsupported(spec.node_id(), "global variable declaration"));
            };
            for identifier in &spec.names {
                // Find the definition by the `AstNodeId` of the identifier.
                let object = self.definition(identifier.node_id())?;
                let typ =
                    self.lower_type(self.analysis.object(object).typ, identifier.node_id())?;
                let initializer = match self
                    .analysis
                    .global_initializer(object)
                    .ok_or_else(|| self.missing(identifier.node_id(), "global initializer"))?
                {
                    gane_sema::GlobalInitializer::Zero => GlobalInitializer::Zero,
                    gane_sema::GlobalInitializer::Scalar(value) => {
                        GlobalInitializer::Scalar(self.lower_constant(
                            value.clone(),
                            self.analysis.object(object).typ,
                            identifier.node_id(),
                        )?)
                    }
                };
                let global = self.builder.add_global(IrGlobal {
                    symbol: format!("gane.{}", identifier.name),
                    typ,
                    initializer,
                });
                self.globals.insert(object, global);
            }
        }
        Ok(())
    }

    pub(super) fn definition(&self, node: AstNodeId) -> Result<ObjectId, LowerError> {
        self.analysis
            .definition(node)
            .ok_or_else(|| self.missing(node, "definition"))
    }

    pub(super) fn use_of(&self, node: AstNodeId) -> Result<ObjectId, LowerError> {
        self.analysis
            .use_of(node)
            .ok_or_else(|| self.missing(node, "identifier use"))
    }

    pub(super) fn missing(&self, node: AstNodeId, fact: &'static str) -> LowerError {
        LowerError::MissingSemanticFact { node, fact }
    }

    pub(super) fn unsupported(&self, node: AstNodeId, construct: &'static str) -> LowerError {
        LowerError::Unsupported { node, construct }
    }

    pub(super) fn is_aggregate(&self, typ: TypeId) -> bool {
        matches!(
            self.builder.types().get(typ),
            Some(crate::IrType {
                kind: IrTypeKind::Array { .. } | IrTypeKind::Struct { .. },
            })
        )
    }
}
