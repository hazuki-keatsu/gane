// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Hazuki Keatsu

use super::*;

impl FunctionLowerer<'_, '_> {
    fn guard_nonzero(
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
            [self.package.builder.types().i1()],
        )?[0];
        self.guard(node, non_zero, TrapReason::DivisionByZero)
    }

    pub(super) fn lower_rvalue(&mut self, expression: &ast::Expr) -> Result<Rvalue, LowerError> {
        let semantic_type = self
            .package
            .analysis
            .type_and_value(expression.node_id())
            .ok_or_else(|| self.missing(expression.node_id(), "expression type and value"))?
            .typ;
        let typ = self.lower_type(semantic_type, expression.node_id())?;
        if !self.is_aggregate(typ) {
            return self.lower_expression(expression).map(Rvalue::Scalar);
        }
        match expression {
            ast::Expr::ParenExpr(expression) => self.lower_rvalue(&expression.x),
            ast::Expr::Ident(_)
            | ast::Expr::StarExpr(_)
            | ast::Expr::SelectorExpr(_)
            | ast::Expr::IndexExpr(_) => self.lower_place(expression).map(Rvalue::AggregateCopy),
            ast::Expr::CompositeLit(literal) if literal.elts.is_empty() => {
                Ok(Rvalue::AggregateZero(typ))
            }
            _ => Err(self.unsupported(expression.node_id(), "aggregate rvalue")),
        }
    }

    pub(super) fn lower_expression(
        &mut self,
        expression: &ast::Expr,
    ) -> Result<ValueId, LowerError> {
        let fact = self
            .package
            .analysis
            .type_and_value(expression.node_id())
            .ok_or_else(|| self.missing(expression.node_id(), "expression type and value"))?
            .clone();
        if let ast::Expr::CallExpr(call) = expression {
            let values = self.lower_call(call)?;
            return match values.as_slice() {
                [value] => Ok(*value),
                _ => Err(self.unsupported(expression.node_id(), "void call used as value")),
            };
        }
        let result_type = self.lower_type(fact.typ, expression.node_id())?;
        if self.is_aggregate(result_type) {
            return Err(self.unsupported(expression.node_id(), "aggregate value"));
        }
        if let Some(constant) = fact.constant {
            let constant = self.lower_constant(constant, fact.typ, expression.node_id())?;
            return Ok(self.instruction(
                expression.node_id(),
                crate::InstructionKind::Const {
                    value: constant,
                    typ: result_type,
                },
                [result_type],
            )?[0]);
        }

        match expression {
            ast::Expr::ParenExpr(expression) => self.lower_expression(&expression.x),
            ast::Expr::Ident(_)
            | ast::Expr::StarExpr(_)
            | ast::Expr::SelectorExpr(_)
            | ast::Expr::IndexExpr(_) => {
                let place = self.lower_place(expression)?;
                self.load(expression.node_id(), place)
            }
            ast::Expr::UnaryExpr(expression) => {
                if expression.op == Token::And {
                    return Ok(self.lower_place(&expression.x)?.pointer);
                }
                let operand = self.lower_expression(&expression.x)?;
                let Some(operator) = (match expression.op {
                    Token::Add => return Ok(operand),
                    Token::Sub => Some(UnaryOp::Neg),
                    Token::Not => Some(UnaryOp::LogicalNot),
                    _ => None,
                }) else {
                    return Err(self.unsupported(expression.node_id(), "unary operator"));
                };
                Ok(self.instruction(
                    expression.node_id(),
                    crate::InstructionKind::Unary {
                        op: operator,
                        operand,
                    },
                    [result_type],
                )?[0])
            }
            ast::Expr::BinaryExpr(expression) => {
                if matches!(expression.op, Token::LAnd | Token::LOr) {
                    return self.lower_short_circuit(expression, result_type);
                }
                let left = self.lower_expression(&expression.x)?;
                let right = self.lower_expression(&expression.y)?;
                match expression.op {
                    Token::Add | Token::Sub | Token::Mul => {
                        let op = match expression.op {
                            Token::Add => BinaryOp::Add,
                            Token::Sub => BinaryOp::Sub,
                            Token::Mul => BinaryOp::Mul,
                            _ => {
                                return Err(
                                    self.unsupported(expression.node_id(), "binary operator")
                                );
                            }
                        };
                        Ok(self.instruction(
                            expression.node_id(),
                            crate::InstructionKind::Binary { op, left, right },
                            [result_type],
                        )?[0])
                    }
                    Token::Quo | Token::Rem => {
                        self.guard_nonzero(expression.node_id(), right, result_type)?;
                        let unsigned = self
                            .package
                            .analysis
                            .is_basic_type(fact.typ, BasicType::Byte);
                        let op = match (expression.op, unsigned) {
                            (Token::Quo, false) => BinaryOp::SignedDiv,
                            (Token::Quo, true) => BinaryOp::UnsignedDiv,
                            (Token::Rem, false) => BinaryOp::SignedRem,
                            (Token::Rem, true) => BinaryOp::UnsignedRem,
                            _ => return Err(self.unsupported(expression.node_id(), "division")),
                        };
                        Ok(self.instruction(
                            expression.node_id(),
                            crate::InstructionKind::Binary { op, left, right },
                            [result_type],
                        )?[0])
                    }
                    Token::Equal
                    | Token::Neq
                    | Token::Less
                    | Token::Leq
                    | Token::Greater
                    | Token::Geq => {
                        let left_fact = self
                            .package
                            .analysis
                            .type_and_value(expression.x.node_id())
                            .ok_or_else(|| {
                                self.missing(expression.x.node_id(), "left operand type")
                            })?;
                        let predicate = self.compare_predicate(
                            expression.op,
                            left_fact.typ,
                            expression.node_id(),
                        )?;
                        Ok(self.instruction(
                            expression.node_id(),
                            crate::InstructionKind::Compare {
                                predicate,
                                left,
                                right,
                            },
                            [result_type],
                        )?[0])
                    }
                    _ => Err(self.unsupported(expression.node_id(), "binary operator")),
                }
            }
            _ => Err(self.unsupported(expression.node_id(), "expression")),
        }
    }

    pub(super) fn lower_short_circuit(
        &mut self,
        expression: &ast::BinaryExpr,
        result_type: TypeId,
    ) -> Result<ValueId, LowerError> {
        let left = self.lower_expression(&expression.x)?;
        let right_block = self.build(expression.node_id(), |builder, function, _| {
            builder.create_block(function)
        })?;
        let short_block = self.build(expression.node_id(), |builder, function, _| {
            builder.create_block(function)
        })?;
        let join = self.build(expression.node_id(), |builder, function, _| {
            builder.create_block(function)
        })?;
        let result = self.build(expression.node_id(), |builder, function, _| {
            builder.append_block_parameter(function, join, result_type, Some(expression.node_id()))
        })?;

        let (then_target, else_target, short_value) = match expression.op {
            Token::LAnd => (right_block, short_block, false),
            Token::LOr => (short_block, right_block, true),
            _ => return Err(self.unsupported(expression.node_id(), "short-circuit operator")),
        };
        self.terminate(
            expression.node_id(),
            self.block,
            Terminator::CondBranch {
                condition: left,
                then_target,
                then_arguments: Vec::new(),
                else_target,
                else_arguments: Vec::new(),
            },
        )?;

        self.block = right_block;
        let right = self.lower_expression(&expression.y)?;
        self.terminate(
            expression.node_id(),
            self.block,
            Terminator::Branch {
                target: join,
                arguments: vec![right],
            },
        )?;

        self.block = short_block;
        let short = self.instruction(
            expression.node_id(),
            crate::InstructionKind::Const {
                value: Constant::Bool(short_value),
                typ: result_type,
            },
            [result_type],
        )?[0];
        self.terminate(
            expression.node_id(),
            short_block,
            Terminator::Branch {
                target: join,
                arguments: vec![short],
            },
        )?;
        self.block = join;
        Ok(result)
    }

    pub(super) fn lower_call(&mut self, call: &ast::CallExpr) -> Result<Vec<ValueId>, LowerError> {
        let ast::Expr::Ident(callee) = &call.fun else {
            return Err(self.unsupported(call.fun.node_id(), "non-identifier call callee"));
        };
        let object = self.use_of(callee.node_id())?;
        let function = self
            .package
            .functions
            .get(&object)
            .cloned()
            .ok_or_else(|| self.unsupported(callee.node_id(), "non-package function call"))?;
        let arguments = call
            .args
            .iter()
            .map(|argument| self.lower_expression(argument))
            .collect::<Result<Vec<_>, _>>()?;
        self.instruction(
            call.node_id(),
            crate::InstructionKind::Call {
                callee: Callee::Function(function.id),
                arguments,
            },
            function.signature.results,
        )
    }

    pub(super) fn compare_predicate(
        &self,
        operator: Token,
        typ: gane_sema::TypeId,
        node: AstNodeId,
    ) -> Result<ComparePredicate, LowerError> {
        let unsigned = self.package.analysis.is_basic_type(typ, BasicType::Byte);
        let predicate = match (operator, unsigned) {
            (Token::Equal, _) => ComparePredicate::Equal,
            (Token::Neq, _) => ComparePredicate::NotEqual,
            (Token::Less, false) => ComparePredicate::SignedLess,
            (Token::Leq, false) => ComparePredicate::SignedLessEqual,
            (Token::Greater, false) => ComparePredicate::SignedGreater,
            (Token::Geq, false) => ComparePredicate::SignedGreaterEqual,
            (Token::Less, true) => ComparePredicate::UnsignedLess,
            (Token::Leq, true) => ComparePredicate::UnsignedLessEqual,
            (Token::Greater, true) => ComparePredicate::UnsignedGreater,
            (Token::Geq, true) => ComparePredicate::UnsignedGreaterEqual,
            _ => return Err(self.unsupported(node, "comparison")),
        };
        Ok(predicate)
    }

    pub(super) fn lower_place(&mut self, expression: &ast::Expr) -> Result<Place, LowerError> {
        let semantic_type = self
            .package
            .analysis
            .type_and_value(expression.node_id())
            .ok_or_else(|| self.missing(expression.node_id(), "expression type and value"))?
            .typ;
        let typ = self.lower_type(semantic_type, expression.node_id())?;
        match expression {
            ast::Expr::ParenExpr(expression) => self.lower_place(&expression.x),
            ast::Expr::Ident(identifier) => {
                let object = self.use_of(identifier.node_id())?;
                if let Some(local) = self.locals.get(&object) {
                    return Ok(*local);
                }
                let global = self
                    .package
                    .globals
                    .get(&object)
                    .copied()
                    .ok_or_else(|| self.unsupported(identifier.node_id(), "non-local place"))?;
                let pointer_type = self.pointer_type(typ);
                let pointer = self.instruction(
                    identifier.node_id(),
                    crate::InstructionKind::GlobalAddr { global },
                    [pointer_type],
                )?[0];
                self.known_non_null.insert(pointer);
                Ok(Place { pointer, typ })
            }
            ast::Expr::StarExpr(expression) => {
                let pointer = self.lower_expression(&expression.x)?;
                let place = Place { pointer, typ };
                self.ensure_non_null(expression.node_id(), place)?;
                Ok(place)
            }
            ast::Expr::SelectorExpr(expression) => {
                let selection = self
                    .package
                    .analysis
                    .selection(expression.node_id())
                    .cloned()
                    .ok_or_else(|| self.missing(expression.node_id(), "field selection"))?;
                let SelectionKind::Field = selection.kind else {
                    return Err(self.unsupported(expression.node_id(), "non-field selection"));
                };
                let [field] = selection.index.as_slice() else {
                    return Err(self.unsupported(expression.node_id(), "multi-field selection"));
                };
                let base = if selection.indirect {
                    let pointer = self.lower_expression(&expression.x)?;
                    let receiver_type = self
                        .package
                        .analysis
                        .type_and_value(expression.x.node_id())
                        .ok_or_else(|| self.missing(expression.x.node_id(), "field receiver type"))?
                        .typ;
                    let pointee =
                        self.package
                            .analysis
                            .deref_type(receiver_type)
                            .ok_or_else(|| {
                                self.missing(expression.x.node_id(), "field receiver pointee")
                            })?;
                    Place {
                        pointer,
                        typ: self.lower_type(pointee, expression.x.node_id())?,
                    }
                } else {
                    self.lower_place(&expression.x)?
                };
                self.ensure_non_null(expression.node_id(), base)?;
                let pointer_type = self.pointer_type(typ);
                let pointer = self.instruction(
                    expression.node_id(),
                    crate::InstructionKind::GepField {
                        base: base.pointer,
                        field: *field,
                    },
                    [pointer_type],
                )?[0];
                self.known_non_null.insert(pointer);
                Ok(Place { pointer, typ })
            }
            ast::Expr::IndexExpr(expression) => self.lower_index_place(expression, typ),
            _ => Err(self.unsupported(expression.node_id(), "assignment target")),
        }
    }

    pub(super) fn lower_index_place(
        &mut self,
        expression: &ast::IndexExpr,
        element: TypeId,
    ) -> Result<Place, LowerError> {
        let base = self.lower_place(&expression.x)?;
        self.ensure_non_null(expression.x.node_id(), base)?;
        let length = match self.package.builder.types().get(base.typ) {
            Some(crate::IrType {
                kind: IrTypeKind::Array { length, .. },
            }) => *length,
            _ => return Err(self.unsupported(expression.x.node_id(), "indexing non-array place")),
        };
        let index_fact = self
            .package
            .analysis
            .type_and_value(expression.index.node_id())
            .ok_or_else(|| self.missing(expression.index.node_id(), "index type"))?
            .clone();
        let index = self.lower_expression(&expression.index)?;
        let index_type = self.lower_type(index_fact.typ, expression.index.node_id())?;
        let failure = self.build(expression.node_id(), |builder, function, _| {
            builder.create_block(function)
        })?;
        let access = self.build(expression.node_id(), |builder, function, _| {
            builder.create_block(function)
        })?;

        if self
            .package
            .analysis
            .is_basic_type(index_fact.typ, BasicType::Int)
        {
            let zero = self.integer_constant(expression.index.node_id(), index_type, 0)?;
            let non_negative = self.instruction(
                expression.index.node_id(),
                crate::InstructionKind::Compare {
                    predicate: ComparePredicate::SignedGreaterEqual,
                    left: index,
                    right: zero,
                },
                [self.package.builder.types().i1()],
            )?[0];
            let upper = self.build(expression.node_id(), |builder, function, _| {
                builder.create_block(function)
            })?;
            self.terminate(
                expression.node_id(),
                self.block,
                Terminator::CondBranch {
                    condition: non_negative,
                    then_target: upper,
                    then_arguments: Vec::new(),
                    else_target: failure,
                    else_arguments: Vec::new(),
                },
            )?;
            self.block = upper;
        } else if !self
            .package
            .analysis
            .is_basic_type(index_fact.typ, BasicType::Byte)
        {
            return Err(self.unsupported(expression.index.node_id(), "array index type"));
        }

        let normalized = self.normalize_index(expression.index.node_id(), index, index_type)?;
        let index_type = self.pointer_integer_type(expression.index.node_id())?;
        let length = self.integer_constant(expression.node_id(), index_type, length)?;
        let in_range = self.instruction(
            expression.node_id(),
            crate::InstructionKind::Compare {
                predicate: ComparePredicate::UnsignedLess,
                left: normalized,
                right: length,
            },
            [self.package.builder.types().i1()],
        )?[0];
        self.terminate(
            expression.node_id(),
            self.block,
            Terminator::CondBranch {
                condition: in_range,
                then_target: access,
                then_arguments: Vec::new(),
                else_target: failure,
                else_arguments: Vec::new(),
            },
        )?;
        self.terminate(
            expression.node_id(),
            failure,
            Terminator::Trap {
                reason: TrapReason::BoundsError,
            },
        )?;
        self.block = access;
        let pointer_type = self.pointer_type(element);
        let pointer = self.instruction(
            expression.node_id(),
            crate::InstructionKind::GepIndex {
                base: base.pointer,
                index: normalized,
            },
            [pointer_type],
        )?[0];
        self.known_non_null.insert(pointer);
        Ok(Place {
            pointer,
            typ: element,
        })
    }

    fn normalize_index(
        &mut self,
        node: AstNodeId,
        index: ValueId,
        source: TypeId,
    ) -> Result<ValueId, LowerError> {
        let target = self.pointer_integer_type(node)?;
        if source == target {
            return Ok(index);
        }
        Ok(self.instruction(
            node,
            crate::InstructionKind::IntCast {
                kind: IntCastKind::ZeroExtend,
                operand: index,
                target,
            },
            [target],
        )?[0])
    }
}
