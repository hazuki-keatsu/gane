use super::*;

impl<'a> Lowerer<'a> {
    pub(super) fn lower(
        mut self,
        input: &PackageInput<'_>,
    ) -> Result<UnverifiedIrPackage, LowerError> {
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
                    ast::Decl::GenDecl(declaration)
                        if matches!(declaration.tok, Token::Const | Token::Type) => {}
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

    pub(super) fn function_signature(
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
        self.function = function.id;
        self.block = self
            .builder
            .entry_block(function.id)
            .map_err(|source| LowerError::Build {
                node: declaration.node_id(),
                source,
            })?;
        self.results = function.signature.results.clone();
        self.locals.clear();
        self.known_non_null.clear();
        self.loops.clear();
        self.initialize_parameters(object, declaration.node_id())?;
        if !self.lower_block(body)? {
            if self.results.is_empty() {
                self.build(body.node_id(), |builder, function, block| {
                    builder.set_terminator(
                        function,
                        block,
                        Terminator::Return { values: Vec::new() },
                    )
                })?;
            } else {
                return Err(self.unsupported(body.node_id(), "non-void function reaches end"));
            }
        }
        Ok(())
    }

    pub(super) fn lower_global_declaration(
        &mut self,
        declaration: &ast::GenDecl,
    ) -> Result<(), LowerError> {
        for spec in &declaration.specs {
            let ast::Spec::ValueSpec(spec) = spec else {
                return Err(self.unsupported(spec.node_id(), "global variable declaration"));
            };
            for identifier in &spec.names {
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
                    mutable: true,
                    initializer,
                });
                self.globals.insert(object, global);
            }
        }
        Ok(())
    }

    pub(super) fn initialize_parameters(
        &mut self,
        function: ObjectId,
        node: AstNodeId,
    ) -> Result<(), LowerError> {
        let ObjectKind::Func { signature } = self.analysis.object(function).kind else {
            return Err(self.missing(node, "function signature"));
        };
        let TypeKind::Signature { params, .. } = &self.analysis.type_of(signature).kind else {
            return Err(self.missing(node, "function signature type"));
        };
        let parameters = self
            .analysis
            .tuple(*params)
            .ok_or_else(|| self.missing(node, "function parameters"))?;
        let parameter_objects = parameters.vars.clone();
        let entry_parameters = self
            .builder
            .entry_parameters(self.function)
            .map_err(|source| LowerError::Build { node, source })?;
        for (object, value) in parameter_objects.into_iter().zip(entry_parameters) {
            let parameter = self.analysis.object(object);
            let Some(name) = self.analysis.name(parameter.name) else {
                continue;
            };
            if name.is_empty() || name == "_" {
                continue;
            }
            let parameter_node = parameter.declaration.unwrap_or(node);
            let typ = self.lower_type(parameter.typ, parameter_node)?;
            let slot = self.build(parameter_node, |builder, function, _| {
                builder.add_stack_slot(function, typ, Some(name.to_owned()), Some(parameter_node))
            })?;
            let pointer_type = self.pointer_type(typ);
            let pointer = self.instruction(
                parameter_node,
                crate::InstructionKind::StackAddr { slot },
                [pointer_type],
            )?[0];
            let place = Place { pointer, typ };
            self.locals.insert(object, place);
            self.known_non_null.insert(pointer);
            self.store(parameter_node, place, value)?;
        }
        Ok(())
    }
}
