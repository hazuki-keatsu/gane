use super::*;

impl<'a> Lowerer<'a> {
    pub(super) fn lower_block(&mut self, block: &ast::BlockStmt) -> Result<bool, LowerError> {
        for statement in &block.list {
            if self.lower_statement(statement)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) fn lower_statement(&mut self, statement: &ast::Stmt) -> Result<bool, LowerError> {
        match statement {
            ast::Stmt::EmptyStmt(_) => Ok(false),
            ast::Stmt::BlockStmt(block) => self.lower_block(block),
            ast::Stmt::DeclStmt(statement) => {
                self.lower_declaration(&statement.decl).map(|_| false)
            }
            ast::Stmt::AssignStmt(statement) if statement.tok == Token::Assign => {
                self.lower_assignment(statement).map(|_| false)
            }
            ast::Stmt::IncDecStmt(statement) => self.lower_inc_dec(statement).map(|_| false),
            ast::Stmt::ExprStmt(statement) => self.lower_expression_statement(statement),
            ast::Stmt::ReturnStmt(statement) => self.lower_return(statement),
            ast::Stmt::IfStmt(statement) => self.lower_if(statement),
            ast::Stmt::ForStmt(statement) => self.lower_for(statement),
            ast::Stmt::BranchStmt(statement) => self.lower_branch(statement),
            ast::Stmt::AssignStmt(statement) => {
                Err(self.unsupported(statement.node_id(), "non-simple assignment"))
            }
            _ => Err(self.unsupported(statement.node_id(), "statement")),
        }
    }

    pub(super) fn lower_if(&mut self, statement: &ast::IfStmt) -> Result<bool, LowerError> {
        if statement.init.is_some() {
            return Err(self.unsupported(statement.node_id(), "if initializer"));
        }

        let condition = self.lower_expression(&statement.cond)?;
        let then_block = self.build(statement.node_id(), |builder, function, _| {
            builder.create_block(function)
        })?;
        let else_block = self.build(statement.node_id(), |builder, function, _| {
            builder.create_block(function)
        })?;
        self.terminate(
            statement.node_id(),
            self.block,
            Terminator::CondBranch {
                condition,
                then_target: then_block,
                then_arguments: Vec::new(),
                else_target: else_block,
                else_arguments: Vec::new(),
            },
        )?;

        self.block = then_block;
        let then_terminates = self.lower_block(&statement.body)?;
        let then_end = self.block;

        if statement.else_.is_none() {
            if !then_terminates {
                self.terminate(
                    statement.node_id(),
                    then_end,
                    Terminator::Branch {
                        target: else_block,
                        arguments: Vec::new(),
                    },
                )?;
            }
            self.block = else_block;
            return Ok(false);
        }

        self.block = else_block;
        let else_terminates = match &statement.else_ {
            Some(else_) => self.lower_statement(else_)?,
            None => false,
        };
        let else_end = self.block;

        if then_terminates && else_terminates {
            return Ok(true);
        }

        let join = self.build(statement.node_id(), |builder, function, _| {
            builder.create_block(function)
        })?;
        if !then_terminates {
            self.terminate(
                statement.node_id(),
                then_end,
                Terminator::Branch {
                    target: join,
                    arguments: Vec::new(),
                },
            )?;
        }
        if !else_terminates {
            self.terminate(
                statement.node_id(),
                else_end,
                Terminator::Branch {
                    target: join,
                    arguments: Vec::new(),
                },
            )?;
        }
        self.block = join;
        Ok(false)
    }

    pub(super) fn lower_for(&mut self, statement: &ast::ForStmt) -> Result<bool, LowerError> {
        if statement.init.is_some() {
            return Err(self.unsupported(statement.node_id(), "three-clause for initializer"));
        }
        if statement.post.is_some() {
            return Err(self.unsupported(statement.node_id(), "three-clause for post statement"));
        }

        let header = self.build(statement.node_id(), |builder, function, _| {
            builder.create_block(function)
        })?;
        let body = self.build(statement.node_id(), |builder, function, _| {
            builder.create_block(function)
        })?;
        let exit = if statement.cond.is_some() {
            Some(self.build(statement.node_id(), |builder, function, _| {
                builder.create_block(function)
            })?)
        } else {
            None
        };
        self.terminate(
            statement.node_id(),
            self.block,
            Terminator::Branch {
                target: header,
                arguments: Vec::new(),
            },
        )?;

        self.block = header;
        if let Some(condition) = &statement.cond {
            let condition = self.lower_expression(condition)?;
            self.terminate(
                statement.node_id(),
                self.block,
                Terminator::CondBranch {
                    condition,
                    then_target: body,
                    then_arguments: Vec::new(),
                    else_target: exit.expect("conditional loop exit"),
                    else_arguments: Vec::new(),
                },
            )?;
        } else {
            self.terminate(
                statement.node_id(),
                header,
                Terminator::Branch {
                    target: body,
                    arguments: Vec::new(),
                },
            )?;
        }

        self.loops.push(Loop { header, exit });
        self.block = body;
        let body_terminates = self.lower_block(&statement.body)?;
        if !body_terminates {
            self.terminate(
                statement.node_id(),
                self.block,
                Terminator::Branch {
                    target: header,
                    arguments: Vec::new(),
                },
            )?;
        }

        let loop_ = self.loops.pop().expect("active loop");
        if let Some(exit) = loop_.exit {
            self.block = exit;
            Ok(false)
        } else {
            Ok(true)
        }
    }

    pub(super) fn lower_branch(&mut self, statement: &ast::BranchStmt) -> Result<bool, LowerError> {
        if statement.label.is_some() {
            return Err(self.unsupported(statement.node_id(), "labeled branch statement"));
        }
        let loop_ = self
            .loops
            .last()
            .copied()
            .ok_or_else(|| self.unsupported(statement.node_id(), "branch outside loop"))?;
        let target = match statement.tok {
            Token::Continue => loop_.header,
            Token::Break => match loop_.exit {
                Some(exit) => exit,
                None => {
                    let exit = self.build(statement.node_id(), |builder, function, _| {
                        builder.create_block(function)
                    })?;
                    let loop_ = self.loops.last_mut().expect("active loop");
                    loop_.exit = Some(exit);
                    exit
                }
            },
            _ => return Err(self.unsupported(statement.node_id(), "branch statement")),
        };
        self.terminate(
            statement.node_id(),
            self.block,
            Terminator::Branch {
                target,
                arguments: Vec::new(),
            },
        )?;
        Ok(true)
    }

    pub(super) fn lower_expression_statement(
        &mut self,
        statement: &ast::ExprStmt,
    ) -> Result<bool, LowerError> {
        let ast::Expr::CallExpr(call) = &statement.x else {
            return Err(self.unsupported(statement.node_id(), "expression statement"));
        };
        if !self.lower_call(call)?.is_empty() {
            return Err(
                self.unsupported(statement.node_id(), "value-returning expression statement")
            );
        }
        Ok(false)
    }

    pub(super) fn lower_return(&mut self, statement: &ast::ReturnStmt) -> Result<bool, LowerError> {
        let values = statement
            .results
            .iter()
            .map(|expression| self.lower_expression(expression))
            .collect::<Result<Vec<_>, _>>()?;
        if values.len() != self.results.len() {
            return Err(self.unsupported(statement.node_id(), "return arity"));
        }
        self.build(statement.node_id(), |builder, function, block| {
            builder.set_terminator(function, block, Terminator::Return { values })
        })?;
        Ok(true)
    }

    pub(super) fn lower_declaration(&mut self, declaration: &ast::Decl) -> Result<(), LowerError> {
        let ast::Decl::GenDecl(declaration) = declaration else {
            return Err(self.unsupported(declaration.node_id(), "local declaration"));
        };
        if declaration.tok != Token::Var {
            return Err(self.unsupported(declaration.node_id(), "local declaration"));
        }
        for spec in &declaration.specs {
            let ast::Spec::ValueSpec(spec) = spec else {
                return Err(self.unsupported(spec.node_id(), "local declaration spec"));
            };
            self.lower_local_var(spec)?;
        }
        Ok(())
    }

    pub(super) fn lower_local_var(&mut self, spec: &ast::ValueSpec) -> Result<(), LowerError> {
        let values = spec
            .values
            .iter()
            .map(|expression| self.lower_rvalue(expression))
            .collect::<Result<Vec<_>, _>>()?;

        for (index, identifier) in spec.names.iter().enumerate() {
            if identifier.name == "_" {
                continue;
            }
            let object = self.definition(identifier.node_id())?;
            let semantic_type = self.analysis.object(object).typ;
            let typ = self.lower_type(semantic_type, identifier.node_id())?;
            let slot = self.build(identifier.node_id(), |builder, function, _| {
                builder.add_stack_slot(
                    function,
                    typ,
                    Some(identifier.name.clone()),
                    Some(identifier.node_id()),
                )
            })?;
            let pointer_type = self.pointer_type(typ);
            let pointer = self.instruction(
                identifier.node_id(),
                crate::InstructionKind::StackAddr { slot },
                [pointer_type],
            )?[0];
            let place = Place { pointer, typ };
            self.locals.insert(object, place);
            self.known_non_null.insert(pointer);

            if let Some(value) = values.get(index) {
                self.assign(spec.node_id(), place, *value)?;
            }
        }
        Ok(())
    }

    pub(super) fn lower_assignment(
        &mut self,
        statement: &ast::AssignStmt,
    ) -> Result<(), LowerError> {
        let destinations = statement
            .lhs
            .iter()
            .map(|expression| match expression {
                ast::Expr::Ident(identifier) if identifier.name == "_" => Ok(None),
                _ => self.lower_place(expression).map(Some),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let values = statement
            .rhs
            .iter()
            .map(|expression| {
                let value = self.lower_rvalue(expression)?;
                match (statement.lhs.len() > 1, value) {
                    (true, Rvalue::AggregateCopy(source)) => self
                        .aggregate_snapshot(statement.node_id(), source)
                        .map(Rvalue::AggregateCopy),
                    (_, value) => Ok(value),
                }
            })
            .collect::<Result<Vec<_>, _>>()?;

        for (destination, value) in destinations.into_iter().zip(values) {
            if let Some(destination) = destination {
                self.assign(statement.node_id(), destination, value)?;
            }
        }
        Ok(())
    }

    pub(super) fn lower_inc_dec(&mut self, statement: &ast::IncDecStmt) -> Result<(), LowerError> {
        let place = self.lower_place(&statement.x)?;
        let value = self.load(statement.x.node_id(), place)?;
        let one = self.instruction(
            statement.node_id(),
            crate::InstructionKind::Const {
                value: Constant::Integer(1),
                typ: place.typ,
            },
            [place.typ],
        )?[0];
        let op = match statement.tok {
            Token::Inc => BinaryOp::Add,
            Token::Dec => BinaryOp::Sub,
            _ => return Err(self.unsupported(statement.node_id(), "increment or decrement")),
        };
        let result = self.instruction(
            statement.node_id(),
            crate::InstructionKind::Binary {
                op,
                left: value,
                right: one,
            },
            [place.typ],
        )?[0];
        self.store(statement.node_id(), place, result)
    }
}
