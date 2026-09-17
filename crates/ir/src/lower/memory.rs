use super::*;

impl<'a> Lowerer<'a> {
    pub(super) fn ensure_non_null(
        &mut self,
        node: AstNodeId,
        place: Place,
    ) -> Result<(), LowerError> {
        if self.known_non_null.contains(&place.pointer) {
            return Ok(());
        }
        let pointer_type = self.pointer_type(place.typ);
        let null = self.instruction(
            node,
            crate::InstructionKind::Const {
                value: Constant::Null,
                typ: pointer_type,
            },
            [pointer_type],
        )?[0];
        let non_null = self.instruction(
            node,
            crate::InstructionKind::Compare {
                predicate: ComparePredicate::NotEqual,
                left: place.pointer,
                right: null,
            },
            [self.builder.types().i1()],
        )?[0];
        self.guard(node, non_null, TrapReason::NullDereference)?;
        self.known_non_null.insert(place.pointer);
        Ok(())
    }

    pub(super) fn guard_nonzero(
        &mut self,
        node: AstNodeId,
        divisor: ValueId,
        typ: TypeId,
    ) -> Result<(), LowerError> {
        let zero = self.integer_constant(node, typ, 0)?;
        let non_zero = self.instruction(
            node,
            crate::InstructionKind::Compare {
                predicate: ComparePredicate::NotEqual,
                left: divisor,
                right: zero,
            },
            [self.builder.types().i1()],
        )?[0];
        self.guard(node, non_zero, TrapReason::DivisionByZero)
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

    pub(super) fn load(&mut self, node: AstNodeId, place: Place) -> Result<ValueId, LowerError> {
        if self.is_aggregate(place.typ) {
            return Err(self.unsupported(node, "aggregate value"));
        }
        self.ensure_non_null(node, place)?;
        Ok(self.instruction(
            node,
            crate::InstructionKind::Load {
                pointer: place.pointer,
            },
            [place.typ],
        )?[0])
    }

    pub(super) fn store(
        &mut self,
        node: AstNodeId,
        place: Place,
        value: ValueId,
    ) -> Result<(), LowerError> {
        if self.is_aggregate(place.typ) {
            return Err(self.unsupported(node, "aggregate assignment"));
        }
        self.ensure_non_null(node, place)?;
        self.instruction(
            node,
            crate::InstructionKind::Store {
                pointer: place.pointer,
                value,
            },
            [],
        )?;
        Ok(())
    }

    pub(super) fn assign(
        &mut self,
        node: AstNodeId,
        destination: Place,
        value: Rvalue,
    ) -> Result<(), LowerError> {
        match value {
            Rvalue::Scalar(value) => self.store(node, destination, value),
            Rvalue::AggregateCopy(source) if source.typ == destination.typ => {
                self.aggregate_copy(node, destination, source)
            }
            Rvalue::AggregateZero(typ) if typ == destination.typ => {
                self.aggregate_zero(node, destination)
            }
            _ => Err(self.unsupported(node, "assignment type")),
        }
    }

    pub(super) fn aggregate_snapshot(
        &mut self,
        node: AstNodeId,
        source: Place,
    ) -> Result<Place, LowerError> {
        let slot = self.build(node, |builder, function, _| {
            builder.add_stack_slot(function, source.typ, None, Some(node))
        })?;
        let pointer_type = self.pointer_type(source.typ);
        let pointer = self.instruction(
            node,
            crate::InstructionKind::StackAddr { slot },
            [pointer_type],
        )?[0];
        let destination = Place {
            pointer,
            typ: source.typ,
        };
        self.known_non_null.insert(pointer);
        self.aggregate_copy(node, destination, source)?;
        Ok(destination)
    }

    pub(super) fn aggregate_zero(
        &mut self,
        node: AstNodeId,
        destination: Place,
    ) -> Result<(), LowerError> {
        if !self.is_aggregate(destination.typ) {
            return Err(self.unsupported(node, "aggregate zero"));
        }
        self.ensure_non_null(node, destination)?;
        self.instruction(
            node,
            crate::InstructionKind::AggregateZero {
                destination: destination.pointer,
                typ: destination.typ,
            },
            [],
        )?;
        Ok(())
    }

    pub(super) fn aggregate_copy(
        &mut self,
        node: AstNodeId,
        destination: Place,
        source: Place,
    ) -> Result<(), LowerError> {
        if !self.is_aggregate(destination.typ) || destination.typ != source.typ {
            return Err(self.unsupported(node, "aggregate copy"));
        }
        self.ensure_non_null(node, destination)?;
        self.ensure_non_null(node, source)?;
        self.instruction(
            node,
            crate::InstructionKind::AggregateCopy {
                destination: destination.pointer,
                source: source.pointer,
                typ: destination.typ,
            },
            [],
        )?;
        Ok(())
    }

    pub(super) fn is_aggregate(&self, typ: TypeId) -> bool {
        matches!(
            self.builder.types().get(typ),
            Some(crate::IrType {
                kind: IrTypeKind::Array { .. } | IrTypeKind::Struct { .. },
            })
        )
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
        operation(&mut self.builder, self.function, self.block)
            .map_err(|source| LowerError::Build { node, source })
    }

    pub(super) fn terminate(
        &mut self,
        node: AstNodeId,
        block: BlockId,
        terminator: Terminator,
    ) -> Result<(), LowerError> {
        self.builder
            .set_terminator(self.function, block, terminator)
            .map_err(|source| LowerError::Build { node, source })
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
}
