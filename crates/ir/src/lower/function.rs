use super::*;

impl<'package, 'analysis> FunctionLowerer<'package, 'analysis> {
    pub(super) fn lower_type(
        &mut self,
        typ: gane_sema::TypeId,
        node: AstNodeId,
    ) -> Result<TypeId, LowerError> {
        self.package.lower_type(typ, node)
    }

    pub(super) fn lower_constant(
        &self,
        constant: ConstValue,
        typ: gane_sema::TypeId,
        node: AstNodeId,
    ) -> Result<Constant, LowerError> {
        self.package.lower_constant(constant, typ, node)
    }

    pub(super) fn pointer_type(&mut self, pointee: TypeId) -> TypeId {
        self.package.pointer_type(pointee)
    }

    pub(super) fn pointer_integer_type(&self, node: AstNodeId) -> Result<TypeId, LowerError> {
        self.package.pointer_integer_type(node)
    }

    pub(super) fn is_aggregate(&self, typ: TypeId) -> bool {
        self.package.is_aggregate(typ)
    }

    pub(super) fn definition(&self, node: AstNodeId) -> Result<ObjectId, LowerError> {
        self.package.definition(node)
    }

    pub(super) fn use_of(&self, node: AstNodeId) -> Result<ObjectId, LowerError> {
        self.package.use_of(node)
    }

    pub(super) fn missing(&self, node: AstNodeId, fact: &'static str) -> LowerError {
        self.package.missing(node, fact)
    }

    pub(super) fn unsupported(&self, node: AstNodeId, construct: &'static str) -> LowerError {
        self.package.unsupported(node, construct)
    }

    pub(super) fn new(
        package: &'package mut PackageLowerer<'analysis>,
        lowered: LoweredFunction,
        object: ObjectId,
        node: AstNodeId,
    ) -> Result<Self, LowerError> {
        // Get the entry of the function
        let block = package
            .builder
            .entry_block(lowered.id)
            .map_err(|source| LowerError::Build { node, source })?;
        // Build FunctionLowerer
        let mut lowerer = Self {
            package,
            function: lowered.id,
            block,
            results: lowered.signature.results,
            locals: HashMap::new(),
            known_non_null: HashSet::new(),
            loops: Vec::new(),
        };
        // Use the information from `sema` to initialize lowerer
        lowerer.initialize_parameters(object, node)?;
        Ok(lowerer)
    }

    pub(super) fn lower(&mut self, body: &ast::BlockStmt) -> Result<(), LowerError> {
        if !self.lower_block(body)? {
            if self.results.is_empty() {
                self.terminate(
                    body.node_id(),
                    self.block,
                    Terminator::Return { values: Vec::new() },
                )?;
            } else {
                return Err(self.unsupported(body.node_id(), "non-void function reaches end"));
            }
        }
        Ok(())
    }

    fn initialize_parameters(
        &mut self,
        function: ObjectId,
        node: AstNodeId,
    ) -> Result<(), LowerError> {
        // Get function signature
        let ObjectKind::Func { signature } = self.package.analysis.object(function).kind else {
            return Err(self.missing(node, "function signature"));
        };
        // Get parameters of the function
        let TypeKind::Signature { params, .. } = &self.package.analysis.type_of(signature).kind
        else {
            return Err(self.missing(node, "function signature type"));
        };
        // Get Object whose kind is ObjectKind::Param
        let parameter_objects = self
            .package
            .analysis
            .tuple(*params)
            .ok_or_else(|| self.missing(node, "function parameters"))?
            .vars
            .clone();
        // Get the entry block's parameter
        let entry_parameters = self
            .package
            .builder
            .entry_parameters(self.function)
            .map_err(|source| LowerError::Build { node, source })?;
        for (object, value) in parameter_objects.into_iter().zip(entry_parameters) {
            let parameter = self.package.analysis.object(object);
            let Some(name) = self.package.analysis.name(parameter.name) else {
                continue;
            };
            // Non-named parameter and _ parameter cannot be referred in block
            if name.is_empty() || name == "_" {
                continue;
            }
            // Used for diagnostics
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

    pub(super) fn instruction(
        &mut self,
        node: AstNodeId,
        kind: crate::InstructionKind,
        result_types: impl IntoIterator<Item = TypeId>,
    ) -> Result<Vec<ValueId>, LowerError> {
        self.build(node, |builder, function, block| {
            builder.append_instruction(function, block, kind, result_types, Some(node))
        })
    }

    pub(super) fn build<T>(
        &mut self,
        node: AstNodeId,
        operation: impl FnOnce(&mut IrBuilder, FunctionId, BlockId) -> Result<T, BuildError>,
    ) -> Result<T, LowerError> {
        let function = self.function;
        let block = self.block;
        operation(&mut self.package.builder, function, block)
            .map_err(|source| LowerError::Build { node, source })
    }

    pub(super) fn terminate(
        &mut self,
        node: AstNodeId,
        block: BlockId,
        terminator: Terminator,
    ) -> Result<(), LowerError> {
        let function = self.function;
        self.package
            .builder
            .set_terminator(function, block, terminator)
            .map_err(|source| LowerError::Build { node, source })
    }

    pub(super) fn guard(
        &mut self,
        node: AstNodeId,
        condition: ValueId,
        reason: TrapReason,
    ) -> Result<(), LowerError> {
        let success = self.build(node, |builder, function, _| builder.create_block(function))?;
        let failure = self.build(node, |builder, function, _| builder.create_block(function))?;
        self.terminate(
            node,
            self.block,
            Terminator::CondBranch {
                condition,
                then_target: success,
                then_arguments: Vec::new(),
                else_target: failure,
                else_arguments: Vec::new(),
            },
        )?;
        self.terminate(node, failure, Terminator::Trap { reason })?;
        self.block = success;
        Ok(())
    }

    pub(super) fn integer_constant(
        &mut self,
        node: AstNodeId,
        typ: TypeId,
        value: u64,
    ) -> Result<ValueId, LowerError> {
        Ok(self.instruction(
            node,
            crate::InstructionKind::Const {
                value: Constant::Integer(value),
                typ,
            },
            [typ],
        )?[0])
    }
}
