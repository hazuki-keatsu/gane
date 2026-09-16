use crate::{
    BinaryOp, BlockId, BuildError, Callee, CallingConvention, ComparePredicate, Constant,
    FunctionAttributes, FunctionId, HirBuilder, HirParameter, HirSignature, HirTypeKind,
    IntCastKind, Linkage, PassingMode, Terminator, TrapReason, TypeId, UnaryOp,
    UnverifiedHirPackage, ValueId,
};
use gane_parser::{
    ast,
    token::{AstNodeId, Token},
};
use gane_sema::{
    AnalysisResult, BasicType, ConstValue, ObjectId, ObjectKind, PackageInput, SelectionKind,
    Severity, TypeKind,
};
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
};

#[derive(Debug)]
pub enum LowerError {
    SemanticErrors {
        node: AstNodeId,
        count: usize,
    },
    MissingSemanticFact {
        node: AstNodeId,
        fact: &'static str,
    },
    Unsupported {
        node: AstNodeId,
        construct: &'static str,
    },
    InvalidConstant {
        node: AstNodeId,
    },
    Build {
        node: AstNodeId,
        source: BuildError,
    },
}

impl LowerError {
    pub fn node(&self) -> AstNodeId {
        match *self {
            Self::SemanticErrors { node, .. }
            | Self::MissingSemanticFact { node, .. }
            | Self::Unsupported { node, .. }
            | Self::InvalidConstant { node }
            | Self::Build { node, .. } => node,
        }
    }
}

impl fmt::Display for LowerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SemanticErrors { count, .. } => {
                write!(formatter, "semantic analysis contains {count} error(s)")
            }
            Self::MissingSemanticFact { fact, .. } => {
                write!(formatter, "missing semantic fact: {fact}")
            }
            Self::Unsupported { construct, .. } => {
                write!(formatter, "unsupported lowering construct: {construct}")
            }
            Self::InvalidConstant { .. } => {
                formatter.write_str("constant cannot be represented by its HIR type")
            }
            Self::Build { source, .. } => write!(formatter, "HIR builder failed: {source}"),
        }
    }
}

impl Error for LowerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Build { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub fn lower_package(
    input: &PackageInput<'_>,
    analysis: &AnalysisResult,
    target: crate::TargetSpec,
) -> Result<UnverifiedHirPackage, LowerError> {
    let errors = analysis
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        let node = errors[0]
            .primary
            .node
            .or_else(|| input.files.first().map(|file| file.ast.node_id()))
            .unwrap_or(AstNodeId::INVALID);
        return Err(LowerError::SemanticErrors {
            node,
            count: errors.len(),
        });
    }

    Lowerer::new(analysis, target).lower(input)
}

#[derive(Clone, Copy)]
struct Place {
    pointer: ValueId,
    typ: TypeId,
}

#[derive(Clone, Copy)]
struct Local {
    place: Place,
}

#[derive(Clone)]
struct LoweredFunction {
    id: FunctionId,
    signature: HirSignature,
}

#[derive(Clone, Copy)]
struct Loop {
    header: BlockId,
    exit: Option<BlockId>,
}

struct Lowerer<'a> {
    analysis: &'a AnalysisResult,
    builder: HirBuilder,
    function: FunctionId,
    block: BlockId,
    target_width: u8,
    locals: HashMap<ObjectId, Local>,
    functions: HashMap<ObjectId, LoweredFunction>,
    results: Vec<TypeId>,
    pointer_types: HashMap<TypeId, TypeId>,
    type_map: HashMap<gane_sema::TypeId, TypeId>,
    known_non_null: HashSet<ValueId>,
    loops: Vec<Loop>,
}

impl<'a> Lowerer<'a> {
    fn new(analysis: &'a AnalysisResult, target: crate::TargetSpec) -> Self {
        let target_width = target.pointer_width();
        Self {
            analysis,
            builder: HirBuilder::new(target),
            function: FunctionId::INVALID,
            block: BlockId::INVALID,
            target_width,
            locals: HashMap::new(),
            functions: HashMap::new(),
            results: Vec::new(),
            pointer_types: HashMap::new(),
            type_map: HashMap::new(),
            known_non_null: HashSet::new(),
            loops: Vec::new(),
        }
    }

    fn lower(mut self, input: &PackageInput<'_>) -> Result<UnverifiedHirPackage, LowerError> {
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
                        return Err(self.unsupported(declaration.node_id(), "global variable"));
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
                            Linkage::Internal,
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

    fn function_signature(
        &mut self,
        function: ObjectId,
        node: AstNodeId,
    ) -> Result<HirSignature, LowerError> {
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
            parameters.push(HirParameter {
                typ,
                passing: PassingMode::Direct,
            });
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
        Ok(HirSignature {
            parameters,
            results,
            calling_convention: CallingConvention::Gane,
        })
    }

    fn lower_function(&mut self, declaration: &ast::FuncDecl) -> Result<(), LowerError> {
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

    fn initialize_parameters(
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
            self.locals.insert(object, Local { place });
            self.known_non_null.insert(pointer);
            self.store(parameter_node, place, value)?;
        }
        Ok(())
    }

    fn lower_block(&mut self, block: &ast::BlockStmt) -> Result<bool, LowerError> {
        for statement in &block.list {
            if self.lower_statement(statement)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn lower_statement(&mut self, statement: &ast::Stmt) -> Result<bool, LowerError> {
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

    fn lower_if(&mut self, statement: &ast::IfStmt) -> Result<bool, LowerError> {
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

    fn lower_for(&mut self, statement: &ast::ForStmt) -> Result<bool, LowerError> {
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

    fn lower_branch(&mut self, statement: &ast::BranchStmt) -> Result<bool, LowerError> {
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

    fn lower_expression_statement(
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

    fn lower_return(&mut self, statement: &ast::ReturnStmt) -> Result<bool, LowerError> {
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

    fn lower_declaration(&mut self, declaration: &ast::Decl) -> Result<(), LowerError> {
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

    fn lower_local_var(&mut self, spec: &ast::ValueSpec) -> Result<(), LowerError> {
        let values = spec
            .values
            .iter()
            .map(|expression| self.lower_expression(expression))
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
            self.locals.insert(object, Local { place });
            self.known_non_null.insert(pointer);

            if let Some(value) = values.get(index) {
                self.store(spec.node_id(), place, *value)?;
            }
        }
        Ok(())
    }

    fn lower_assignment(&mut self, statement: &ast::AssignStmt) -> Result<(), LowerError> {
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
            .map(|expression| self.lower_expression(expression))
            .collect::<Result<Vec<_>, _>>()?;

        for (destination, value) in destinations.into_iter().zip(values) {
            if let Some(destination) = destination {
                self.store(statement.node_id(), destination, value)?;
            }
        }
        Ok(())
    }

    fn lower_inc_dec(&mut self, statement: &ast::IncDecStmt) -> Result<(), LowerError> {
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

    fn lower_expression(&mut self, expression: &ast::Expr) -> Result<ValueId, LowerError> {
        let fact = self
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
                        let unsigned = self.analysis.is_basic_type(fact.typ, BasicType::Byte);
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

    fn lower_short_circuit(
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

    fn lower_call(&mut self, call: &ast::CallExpr) -> Result<Vec<ValueId>, LowerError> {
        let ast::Expr::Ident(callee) = &call.fun else {
            return Err(self.unsupported(call.fun.node_id(), "non-identifier call callee"));
        };
        let object = self.use_of(callee.node_id())?;
        let function = self
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

    fn compare_predicate(
        &self,
        operator: Token,
        typ: gane_sema::TypeId,
        node: AstNodeId,
    ) -> Result<ComparePredicate, LowerError> {
        let unsigned = self.analysis.is_basic_type(typ, BasicType::Byte);
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

    fn lower_place(&mut self, expression: &ast::Expr) -> Result<Place, LowerError> {
        let semantic_type = self
            .analysis
            .type_and_value(expression.node_id())
            .ok_or_else(|| self.missing(expression.node_id(), "expression type and value"))?
            .typ;
        let typ = self.lower_type(semantic_type, expression.node_id())?;
        match expression {
            ast::Expr::ParenExpr(expression) => self.lower_place(&expression.x),
            ast::Expr::Ident(identifier) => {
                let object = self.use_of(identifier.node_id())?;
                self.locals
                    .get(&object)
                    .copied()
                    .map(|local| local.place)
                    .ok_or_else(|| self.unsupported(identifier.node_id(), "non-local place"))
            }
            ast::Expr::StarExpr(expression) => {
                let pointer = self.lower_expression(&expression.x)?;
                let place = Place { pointer, typ };
                self.ensure_non_null(expression.node_id(), place)?;
                Ok(place)
            }
            ast::Expr::SelectorExpr(expression) => {
                let selection = self
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
                        .analysis
                        .type_and_value(expression.x.node_id())
                        .ok_or_else(|| self.missing(expression.x.node_id(), "field receiver type"))?
                        .typ;
                    let pointee = self.analysis.deref_type(receiver_type).ok_or_else(|| {
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

    fn lower_index_place(
        &mut self,
        expression: &ast::IndexExpr,
        element: TypeId,
    ) -> Result<Place, LowerError> {
        let base = self.lower_place(&expression.x)?;
        self.ensure_non_null(expression.x.node_id(), base)?;
        let length = match self.builder.types().get(base.typ) {
            Some(crate::HirType {
                kind: HirTypeKind::Array { length, .. },
            }) => *length,
            _ => return Err(self.unsupported(expression.x.node_id(), "indexing non-array place")),
        };
        let index_fact = self
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

        if self.analysis.is_basic_type(index_fact.typ, BasicType::Int) {
            let zero = self.integer_constant(expression.index.node_id(), index_type, 0)?;
            let non_negative = self.instruction(
                expression.index.node_id(),
                crate::InstructionKind::Compare {
                    predicate: ComparePredicate::SignedGreaterEqual,
                    left: index,
                    right: zero,
                },
                [self.builder.types().i1()],
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
        } else if !self.analysis.is_basic_type(index_fact.typ, BasicType::Byte) {
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
            [self.builder.types().i1()],
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

    fn lower_type(
        &mut self,
        typ: gane_sema::TypeId,
        node: AstNodeId,
    ) -> Result<TypeId, LowerError> {
        if let Some(hir_type) = self.type_map.get(&typ) {
            return Ok(*hir_type);
        }
        let underlying = self.analysis.underlying_type(typ);
        if let Some(hir_type) = self.type_map.get(&underlying).copied() {
            self.type_map.insert(typ, hir_type);
            return Ok(hir_type);
        }
        if let Some(hir_type) = self.type_map.iter().find_map(|(semantic, hir_type)| {
            self.analysis
                .identical_types(underlying, *semantic)
                .then_some(*hir_type)
        }) {
            self.type_map.insert(underlying, hir_type);
            self.type_map.insert(typ, hir_type);
            return Ok(hir_type);
        }
        let result = match self.analysis.type_of(underlying).kind.clone() {
            TypeKind::Basic(BasicType::Bool) => self.builder.types().i1(),
            TypeKind::Basic(BasicType::Byte) => self.builder.types().i8(),
            TypeKind::Basic(BasicType::Int) => match self.target_width {
                32 => self.builder.types().i32(),
                64 => self.builder.types().i64(),
                _ => return Err(self.unsupported(node, "target pointer width")),
            },
            TypeKind::Pointer { base } => {
                let pointee = self.lower_type(base, node)?;
                self.pointer_type(pointee)
            }
            TypeKind::Array { len, elem } => {
                let result = self.builder.reserve_type();
                self.type_map.insert(underlying, result);
                self.type_map.insert(typ, result);
                let length = self.array_length(len, node)?;
                let element = self.lower_type(elem, node)?;
                self.builder
                    .define_type(result, HirTypeKind::Array { length, element })
                    .map_err(|source| LowerError::Build { node, source })?;
                return Ok(result);
            }
            TypeKind::Struct { fields } => {
                let result = self.builder.reserve_type();
                self.type_map.insert(underlying, result);
                self.type_map.insert(typ, result);
                let fields = fields
                    .into_iter()
                    .map(|field| self.lower_type(self.analysis.object(field).typ, node))
                    .collect::<Result<Vec<_>, _>>()?;
                self.builder
                    .define_type(result, HirTypeKind::Struct { fields })
                    .map_err(|source| LowerError::Build { node, source })?;
                return Ok(result);
            }
            _ => return Err(self.unsupported(node, "type")),
        };
        self.type_map.insert(underlying, result);
        self.type_map.insert(typ, result);
        Ok(result)
    }

    fn array_length(&self, length: ConstValue, node: AstNodeId) -> Result<u64, LowerError> {
        let ConstValue::Int(length) = length else {
            return Err(LowerError::InvalidConstant { node });
        };
        let length = length
            .to_i128()
            .and_then(|length| u64::try_from(length).ok())
            .filter(|length| *length != 0)
            .ok_or(LowerError::InvalidConstant { node })?;
        if self.target_width == 32 && length > u32::MAX as u64 {
            return Err(LowerError::InvalidConstant { node });
        }
        Ok(length)
    }

    fn lower_constant(
        &self,
        constant: ConstValue,
        typ: gane_sema::TypeId,
        node: AstNodeId,
    ) -> Result<Constant, LowerError> {
        match constant {
            ConstValue::Bool(value) if self.analysis.is_basic_type(typ, BasicType::Bool) => {
                Ok(Constant::Bool(value))
            }
            ConstValue::Int(value) if self.analysis.is_basic_type(typ, BasicType::Byte) => value
                .to_i128()
                .filter(|value| (0..=u8::MAX as i128).contains(value))
                .map(|value| Constant::Integer(value as u64))
                .ok_or(LowerError::InvalidConstant { node }),
            ConstValue::Int(value) if self.analysis.is_basic_type(typ, BasicType::Int) => {
                let width = self.target_width as u32;
                let value = value
                    .to_i128()
                    .ok_or(LowerError::InvalidConstant { node })?;
                let minimum = -(1_i128 << (width - 1));
                let maximum = (1_i128 << (width - 1)) - 1;
                if !(minimum..=maximum).contains(&value) {
                    return Err(LowerError::InvalidConstant { node });
                }
                let mask = (1_u128 << width) - 1;
                Ok(Constant::Integer((value as u128 & mask) as u64))
            }
            _ => Err(LowerError::InvalidConstant { node }),
        }
    }

    fn pointer_type(&mut self, pointee: TypeId) -> TypeId {
        if let Some(pointer) = self.pointer_types.get(&pointee) {
            return *pointer;
        }
        let pointer = self.builder.add_type(HirTypeKind::Ptr {
            pointee,
            address_space: 0,
        });
        self.pointer_types.insert(pointee, pointer);
        pointer
    }

    fn pointer_integer_type(&self, node: AstNodeId) -> Result<TypeId, LowerError> {
        match self.target_width {
            32 => Ok(self.builder.types().i32()),
            64 => Ok(self.builder.types().i64()),
            _ => Err(self.unsupported(node, "target pointer width")),
        }
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

    fn integer_constant(
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

    fn ensure_non_null(&mut self, node: AstNodeId, place: Place) -> Result<(), LowerError> {
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
            [self.builder.types().i1()],
        )?[0];
        self.guard(node, non_zero, TrapReason::DivisionByZero)
    }

    fn guard(
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

    fn load(&mut self, node: AstNodeId, place: Place) -> Result<ValueId, LowerError> {
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

    fn store(&mut self, node: AstNodeId, place: Place, value: ValueId) -> Result<(), LowerError> {
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

    fn is_aggregate(&self, typ: TypeId) -> bool {
        matches!(
            self.builder.types().get(typ),
            Some(crate::HirType {
                kind: HirTypeKind::Array { .. } | HirTypeKind::Struct { .. },
            })
        )
    }

    fn instruction(
        &mut self,
        node: AstNodeId,
        kind: crate::InstructionKind,
        result_types: impl IntoIterator<Item = TypeId>,
    ) -> Result<Vec<ValueId>, LowerError> {
        self.build(node, |builder, function, block| {
            builder.append_instruction(function, block, kind, result_types, Some(node))
        })
    }

    fn build<T>(
        &mut self,
        node: AstNodeId,
        operation: impl FnOnce(&mut HirBuilder, FunctionId, BlockId) -> Result<T, BuildError>,
    ) -> Result<T, LowerError> {
        operation(&mut self.builder, self.function, self.block)
            .map_err(|source| LowerError::Build { node, source })
    }

    fn terminate(
        &mut self,
        node: AstNodeId,
        block: BlockId,
        terminator: Terminator,
    ) -> Result<(), LowerError> {
        self.builder
            .set_terminator(self.function, block, terminator)
            .map_err(|source| LowerError::Build { node, source })
    }

    fn definition(&self, node: AstNodeId) -> Result<ObjectId, LowerError> {
        self.analysis
            .definition(node)
            .ok_or_else(|| self.missing(node, "definition"))
    }

    fn use_of(&self, node: AstNodeId) -> Result<ObjectId, LowerError> {
        self.analysis
            .use_of(node)
            .ok_or_else(|| self.missing(node, "identifier use"))
    }

    fn missing(&self, node: AstNodeId, fact: &'static str) -> LowerError {
        LowerError::MissingSemanticFact { node, fact }
    }

    fn unsupported(&self, node: AstNodeId, construct: &'static str) -> LowerError {
        LowerError::Unsupported { node, construct }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InstructionKind, TargetSpec, verify};
    use gane_parser::{
        parser::{Mode, parse_file},
        token::FileSet,
    };
    use gane_sema::{FileId, analyze_package};

    fn lower(source: &str, target: TargetSpec) -> Result<UnverifiedHirPackage, LowerError> {
        let mut files = FileSet::new();
        let (ast, errors) = parse_file(&mut files, "main.go", source.as_bytes(), Mode::default());
        assert!(errors.is_none(), "{errors:?}");
        let input = PackageInput::single("example/main", FileId::from_raw(1), &ast);
        let analysis = analyze_package(input.clone());
        lower_package(&input, &analysis, target)
    }

    #[test]
    fn lowers_minimal_main_to_stable_verified_hir() {
        let package = lower(
            "package main\nfunc main() { var x int; x = 1 + 2 }\n",
            TargetSpec::for_test_64(),
        )
        .unwrap();
        verify(&package).unwrap();

        assert_eq!(
            package.to_string(),
            r#"target "x86_64-unknown-linux-gnu" {
  cpu = "generic"
  features = ""
  data_layout = "e-m:e-p:64:64-i64:64-n8:16:32:64-S128"
  pointer_width = 64
  endianness = Little
}
type !1
type !2
type !3
type !4
type !5
type !6
type !7
type !1 = void
type !2 = i1
type !3 = i8
type !4 = i16
type !5 = i32
type !6 = i64
type !7 = ptr(addrspace=0, !6)
func @1 "gane.main"() -> () internal [no_return=false, no_unwind=false, memory=unknown] entry ^1 {
  slot $1: !6
  ^1():
    %1 = stack_addr $1
    %2 = const !6 3
    store %1, %2
    return
}
entry @1
"#
        );
    }

    #[test]
    fn emits_runtime_scalar_operations_and_unsigned_byte_comparison() {
        let package = lower(
            "package main\nfunc main() { var x int; var y int = x + 2; var b byte; _ = b < b }\n",
            TargetSpec::for_test_64(),
        )
        .unwrap();
        verify(&package).unwrap();
        let instructions = &package.function(package.entry()).unwrap().blocks[0].instructions;

        assert!(instructions.iter().any(|instruction| matches!(
            instruction.kind,
            InstructionKind::Binary {
                op: BinaryOp::Add,
                ..
            }
        )));
        assert!(instructions.iter().any(|instruction| matches!(
            instruction.kind,
            InstructionKind::Compare {
                predicate: ComparePredicate::UnsignedLess,
                ..
            }
        )));
    }

    #[test]
    fn relies_on_slot_zero_value_and_discards_blank_assignment() {
        let package = lower(
            "package main\nfunc main() { var x int; _ = x }\n",
            TargetSpec::for_test_64(),
        )
        .unwrap();
        verify(&package).unwrap();
        let instructions = &package.function(package.entry()).unwrap().blocks[0].instructions;

        assert_eq!(
            instructions
                .iter()
                .filter(|instruction| matches!(instruction.kind, InstructionKind::Load { .. }))
                .count(),
            1
        );
        assert!(
            !instructions
                .iter()
                .any(|instruction| matches!(instruction.kind, InstructionKind::Store { .. }))
        );
    }

    #[test]
    fn keeps_shadowed_named_scalars_separate_and_reads_all_swap_values_first() {
        let package = lower(
            "package main\ntype Number int\nfunc main() { var x Number; var y Number; { var x Number; x = x + x }; x, y = y, x }\n",
            TargetSpec::for_test_64(),
        )
        .unwrap();
        verify(&package).unwrap();
        let function = package.function(package.entry()).unwrap();
        assert_eq!(function.stack_slots.len(), 3);
        assert!(
            function
                .stack_slots
                .iter()
                .all(|slot| slot.typ == package.types().i64())
        );

        let tail = &function.blocks[0].instructions;
        let first_swap_load = tail.len() - 4;
        assert!(matches!(
            tail[first_swap_load].kind,
            InstructionKind::Load { .. }
        ));
        assert!(matches!(
            tail[first_swap_load + 1].kind,
            InstructionKind::Load { .. }
        ));
        assert!(matches!(
            tail[first_swap_load + 2].kind,
            InstructionKind::Store { .. }
        ));
        assert!(matches!(
            tail[first_swap_load + 3].kind,
            InstructionKind::Store { .. }
        ));
    }

    #[test]
    fn maps_int_to_target_width_and_rejects_unrepresentable_constants() {
        for (target, expected) in [
            (TargetSpec::for_test_32(), 32),
            (TargetSpec::for_test_64(), 64),
        ] {
            let package =
                lower("package main\nfunc main() { var x int; _ = x }\n", target).unwrap();
            let slot = &package.function(package.entry()).unwrap().stack_slots[0];
            let width = match package.types().get(slot.typ).unwrap().kind {
                HirTypeKind::I32 => 32,
                HirTypeKind::I64 => 64,
                ref other => panic!("unexpected int type: {other:?}"),
            };
            assert_eq!(width, expected);
        }

        assert!(matches!(
            lower(
                "package main\nfunc main() { var x int; x = 2147483648 }\n",
                TargetSpec::for_test_32(),
            ),
            Err(LowerError::InvalidConstant { .. })
        ));
    }

    #[test]
    fn lowers_scalar_functions_calls_and_returns_to_verified_hir() {
        let package = lower(
            "package main\n\
             func add(x int, y int) int { x = x + y; return x }\n\
             func main() { var result int; result = add(1, 2); sink(result) }\n\
             func sink(value int) {}\n",
            TargetSpec::for_test_64(),
        )
        .unwrap();
        verify(&package).unwrap();

        assert_eq!(
            package.to_string(),
            r#"target "x86_64-unknown-linux-gnu" {
  cpu = "generic"
  features = ""
  data_layout = "e-m:e-p:64:64-i64:64-n8:16:32:64-S128"
  pointer_width = 64
  endianness = Little
}
type !1
type !2
type !3
type !4
type !5
type !6
type !7
type !1 = void
type !2 = i1
type !3 = i8
type !4 = i16
type !5 = i32
type !6 = i64
type !7 = ptr(addrspace=0, !6)
func @1 "gane.add"(direct !6, direct !6) -> (!6) internal [no_return=false, no_unwind=false, memory=unknown] entry ^1 {
  slot $1: !6
  slot $2: !6
  ^1(%1: !6, %2: !6):
    %3 = stack_addr $1
    store %3, %1
    %4 = stack_addr $2
    store %4, %2
    %5 = load %3
    %6 = load %4
    %7 = add %5, %6
    store %3, %7
    %8 = load %3
    return %8
}
func @2 "gane.main"() -> () internal [no_return=false, no_unwind=false, memory=unknown] entry ^1 {
  slot $1: !6
  ^1():
    %1 = stack_addr $1
    %2 = const !6 1
    %3 = const !6 2
    %4 = call @1(%2, %3)
    store %1, %4
    %5 = load %1
    call @3(%5)
    return
}
func @3 "gane.sink"(direct !6) -> () internal [no_return=false, no_unwind=false, memory=unknown] entry ^1 {
  slot $1: !6
  ^1(%1: !6):
    %2 = stack_addr $1
    store %2, %1
    return
}
entry @2
"#
        );

        let main = package.function(package.entry()).unwrap();
        let call = main.blocks[0]
            .instructions
            .iter()
            .find(|instruction| matches!(instruction.kind, InstructionKind::Call { .. }))
            .unwrap();
        assert_eq!(call.results.len(), 1);
        assert_eq!(
            main.value(call.results[0]).unwrap().typ,
            package.types().i64()
        );
        let InstructionKind::Call {
            callee: crate::Callee::Function(callee),
            ..
        } = call.kind
        else {
            unreachable!();
        };
        assert_eq!(
            package.function(callee).unwrap().signature.results,
            vec![package.types().i64()]
        );
    }

    #[test]
    fn resolves_forward_and_recursive_direct_calls() {
        let package = lower(
            "package main\n\
             func main() { later() }\n\
             func later() { later() }\n",
            TargetSpec::for_test_64(),
        )
        .unwrap();
        verify(&package).unwrap();

        let main = package.function(package.entry()).unwrap();
        let InstructionKind::Call {
            callee: crate::Callee::Function(later),
            ..
        } = main.blocks[0].instructions[0].kind
        else {
            panic!("main must call later directly");
        };
        let recursive = package.function(later).unwrap();
        assert!(matches!(
            recursive.blocks[0].instructions[0].kind,
            InstructionKind::Call {
                callee: crate::Callee::Function(callee),
                ..
            } if callee == later
        ));
    }

    #[test]
    fn retains_anonymous_parameters_without_creating_inaccessible_slots() {
        let package = lower(
            "package main\nfunc consume(int) { return }\nfunc main() { consume(1) }\n",
            TargetSpec::for_test_64(),
        )
        .unwrap();
        verify(&package).unwrap();

        let consume = package.function(crate::FunctionId::from_raw(1)).unwrap();
        assert_eq!(consume.blocks[0].parameters.len(), 1);
        assert!(consume.stack_slots.is_empty());
    }

    #[test]
    fn lowers_if_else_to_zero_parameter_join() {
        let package = lower(
            "package main\nfunc main() { var x int; if x == 0 { x = 1 } else { x = 2 } }\n",
            TargetSpec::for_test_64(),
        )
        .unwrap();
        verify(&package).unwrap();

        assert_eq!(
            package.to_string(),
            r#"target "x86_64-unknown-linux-gnu" {
  cpu = "generic"
  features = ""
  data_layout = "e-m:e-p:64:64-i64:64-n8:16:32:64-S128"
  pointer_width = 64
  endianness = Little
}
type !1
type !2
type !3
type !4
type !5
type !6
type !7
type !1 = void
type !2 = i1
type !3 = i8
type !4 = i16
type !5 = i32
type !6 = i64
type !7 = ptr(addrspace=0, !6)
func @1 "gane.main"() -> () internal [no_return=false, no_unwind=false, memory=unknown] entry ^1 {
  slot $1: !6
  ^1():
    %1 = stack_addr $1
    %2 = load %1
    %3 = const !6 0
    %4 = cmp.eq %2, %3
    condbr %4, ^2(), ^3()
  ^2():
    %5 = const !6 1
    store %1, %5
    br ^4()
  ^3():
    %6 = const !6 2
    store %1, %6
    br ^4()
  ^4():
    return
}
entry @1
"#
        );
    }

    #[test]
    fn lowers_conditional_for_to_a_zero_parameter_loop_cfg() {
        let package = lower(
            "package main\nfunc main() { var x int; for x < 1 { x = x + 1 } }\n",
            TargetSpec::for_test_64(),
        )
        .unwrap();
        verify(&package).unwrap();

        assert_eq!(
            package.to_string(),
            r#"target "x86_64-unknown-linux-gnu" {
  cpu = "generic"
  features = ""
  data_layout = "e-m:e-p:64:64-i64:64-n8:16:32:64-S128"
  pointer_width = 64
  endianness = Little
}
type !1
type !2
type !3
type !4
type !5
type !6
type !7
type !1 = void
type !2 = i1
type !3 = i8
type !4 = i16
type !5 = i32
type !6 = i64
type !7 = ptr(addrspace=0, !6)
func @1 "gane.main"() -> () internal [no_return=false, no_unwind=false, memory=unknown] entry ^1 {
  slot $1: !6
  ^1():
    %1 = stack_addr $1
    br ^2()
  ^2():
    %2 = load %1
    %3 = const !6 1
    %4 = cmp.slt %2, %3
    condbr %4, ^3(), ^4()
  ^3():
    %5 = load %1
    %6 = const !6 1
    %7 = add %5, %6
    store %1, %7
    br ^2()
  ^4():
    return
}
entry @1
"#
        );
    }

    #[test]
    fn lowers_short_circuit_to_cfg_with_boolean_join_parameters() {
        let package = lower(
            "package main\n\
             func left() bool { return true }\n\
             func right() bool { return false }\n\
             func main() { var value bool; value = left() && right(); value = left() || right() }\n",
            TargetSpec::for_test_64(),
        )
        .unwrap();
        verify(&package).unwrap();
        assert_eq!(
            package.to_string(),
            r#"target "x86_64-unknown-linux-gnu" {
  cpu = "generic"
  features = ""
  data_layout = "e-m:e-p:64:64-i64:64-n8:16:32:64-S128"
  pointer_width = 64
  endianness = Little
}
type !1
type !2
type !3
type !4
type !5
type !6
type !7
type !1 = void
type !2 = i1
type !3 = i8
type !4 = i16
type !5 = i32
type !6 = i64
type !7 = ptr(addrspace=0, !2)
func @1 "gane.left"() -> (!2) internal [no_return=false, no_unwind=false, memory=unknown] entry ^1 {
  ^1():
    %1 = const !2 true
    return %1
}
func @2 "gane.right"() -> (!2) internal [no_return=false, no_unwind=false, memory=unknown] entry ^1 {
  ^1():
    %1 = const !2 false
    return %1
}
func @3 "gane.main"() -> () internal [no_return=false, no_unwind=false, memory=unknown] entry ^1 {
  slot $1: !2
  ^1():
    %1 = stack_addr $1
    %2 = call @1()
    condbr %2, ^2(), ^3()
  ^2():
    %4 = call @2()
    br ^4(%4)
  ^3():
    %5 = const !2 false
    br ^4(%5)
  ^4(%3: !2):
    store %1, %3
    %6 = call @1()
    condbr %6, ^6(), ^5()
  ^5():
    %8 = call @2()
    br ^7(%8)
  ^6():
    %9 = const !2 true
    br ^7(%9)
  ^7(%7: !2):
    store %1, %7
    return
}
entry @3
"#
        );
    }

    #[test]
    fn nests_short_circuit_in_returns_conditions_and_loops() {
        let package = lower(
            "package main\n\
             func left() bool { return true }\n\
             func right() bool { return false }\n\
             func choose() bool { return left() && right() }\n\
             func main() {\n\
                 var value bool\n\
                 if value && (left() || right()) { value = false }\n\
                 for value && (left() || right()) { continue }\n\
             }\n",
            TargetSpec::for_test_64(),
        )
        .unwrap();
        verify(&package).unwrap();

        let choose = package.function(crate::FunctionId::from_raw(3)).unwrap();
        assert!(matches!(
            choose.blocks.last().unwrap().terminator,
            Terminator::Return { ref values } if values.len() == 1
        ));

        let main = package.function(package.entry()).unwrap();
        assert!(
            main.blocks
                .iter()
                .filter(|block| block.parameters.len() == 1)
                .count()
                >= 4
        );
        assert!(
            main.blocks
                .iter()
                .filter(|block| block.parameters.len() == 1)
                .all(|block| main.value(block.parameters[0]).unwrap().typ == package.types().i1())
        );
        assert!(main.blocks.iter().any(|block| matches!(
            block.terminator,
            Terminator::Branch { ref arguments, .. } if arguments.len() == 1
        )));
        assert!(main.blocks.iter().any(|block| matches!(
            block.terminator,
            Terminator::Branch { target, ref arguments }
                if arguments.is_empty()
                    && main.block(target).is_some_and(|target| matches!(
                        target.terminator,
                        Terminator::CondBranch { .. }
                    ))
        )));

        let folded = lower(
            "package main\nfunc main() { var value bool = true && false }\n",
            TargetSpec::for_test_64(),
        )
        .unwrap();
        verify(&folded).unwrap();
        assert_eq!(folded.function(folded.entry()).unwrap().blocks.len(), 1);
    }

    #[test]
    fn lowers_break_continue_and_nested_loops() {
        let package = lower(
            "package main\nfunc main() { for { if true { continue }; break } }\n",
            TargetSpec::for_test_64(),
        )
        .unwrap();
        verify(&package).unwrap();
        let main = package.function(package.entry()).unwrap();
        assert_eq!(main.blocks.len(), 6);
        assert!(matches!(
            main.blocks[3].terminator,
            Terminator::Branch { target, ref arguments }
                if target == crate::BlockId::from_raw(2) && arguments.is_empty()
        ));
        assert!(matches!(
            main.blocks[4].terminator,
            Terminator::Branch { target, ref arguments }
                if target == crate::BlockId::from_raw(6) && arguments.is_empty()
        ));

        let package = lower(
            "package main\nfunc main() { var ok bool; for ok { for { break }; continue } }\n",
            TargetSpec::for_test_64(),
        )
        .unwrap();
        verify(&package).unwrap();
        let main = package.function(package.entry()).unwrap();
        assert!(matches!(
            main.blocks[5].terminator,
            Terminator::Branch { target, ref arguments }
                if target == crate::BlockId::from_raw(7) && arguments.is_empty()
        ));
        assert!(matches!(
            main.blocks[6].terminator,
            Terminator::Branch { target, ref arguments }
                if target == crate::BlockId::from_raw(2) && arguments.is_empty()
        ));
    }

    #[test]
    fn lowers_infinite_returning_loop_without_an_exit_block() {
        let package = lower(
            "package main\nfunc choose() int { for { return 1 } }\nfunc main() {}\n",
            TargetSpec::for_test_64(),
        )
        .unwrap();
        verify(&package).unwrap();

        let choose = package.function(crate::FunctionId::from_raw(1)).unwrap();
        assert_eq!(choose.blocks.len(), 3);
        assert!(matches!(
            choose.blocks[2].terminator,
            Terminator::Return { .. }
        ));
    }

    #[test]
    fn lowers_if_control_flow_and_preserves_stack_locals() {
        let package = lower(
            "package main\n\
             func predicate() bool { return true }\n\
             func main() {\n\
                 var x int\n\
                 if predicate() { var x int; x = 1 } else if x == 0 { x = 2 }\n\
                 if x == 2 { return }\n\
             }\n",
            TargetSpec::for_test_64(),
        )
        .unwrap();
        verify(&package).unwrap();

        let main = package.function(package.entry()).unwrap();
        assert_eq!(main.stack_slots.len(), 2);
        assert_eq!(
            main.blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .filter(|instruction| matches!(instruction.kind, InstructionKind::Call { .. }))
                .count(),
            1
        );
        let Terminator::CondBranch { condition, .. } = main.blocks[0].terminator else {
            panic!("condition must terminate the entry block");
        };
        assert!(
            main.blocks[0]
                .instructions
                .iter()
                .any(|instruction| matches!(
                    instruction.kind,
                    InstructionKind::Call { .. } if instruction.results == [condition]
                ))
        );
        assert!(main.blocks.iter().any(|block| matches!(
            block.terminator,
            Terminator::Branch { ref arguments, .. } if arguments.is_empty()
        )));
    }

    #[test]
    fn lowers_no_else_with_a_direct_false_edge_to_the_join() {
        let package = lower(
            "package main\nfunc choose(value bool) int { if value { return 1 }; return 2 }\nfunc main() {}\n",
            TargetSpec::for_test_64(),
        )
        .unwrap();
        verify(&package).unwrap();

        let choose = package.function(crate::FunctionId::from_raw(1)).unwrap();
        assert_eq!(choose.blocks.len(), 3);
        let Terminator::CondBranch { else_target, .. } = choose.blocks[0].terminator else {
            panic!("if condition must branch");
        };
        assert_eq!(else_target, crate::BlockId::from_raw(3));
        assert!(matches!(
            choose.blocks[1].terminator,
            Terminator::Return { .. }
        ));
        assert!(matches!(
            choose.blocks[2].terminator,
            Terminator::Return { .. }
        ));
    }

    #[test]
    fn omits_join_when_both_if_branches_return() {
        let package = lower(
            "package main\nfunc choose(value bool) int { if value { return 1 } else { return 2 } }\nfunc main() {}\n",
            TargetSpec::for_test_64(),
        )
        .unwrap();
        verify(&package).unwrap();

        let choose = package.function(crate::FunctionId::from_raw(1)).unwrap();
        assert_eq!(choose.blocks.len(), 3);
        assert!(matches!(
            choose.blocks[1].terminator,
            Terminator::Return { .. }
        ));
        assert!(matches!(
            choose.blocks[2].terminator,
            Terminator::Return { .. }
        ));
    }

    #[test]
    fn rejects_unsupported_call_forms_and_signatures() {
        for source in [
            "package main\nfunc helper() {}\nfunc main() { (helper)() }\n",
            "package main\nfunc helper(value [1]int) {}\nfunc main() {}\n",
        ] {
            assert!(matches!(
                lower(source, TargetSpec::for_test_64()),
                Err(LowerError::Unsupported { .. })
            ));
        }
    }

    #[test]
    fn lowers_nested_struct_and_array_places_to_geps() {
        let package = lower(
            "package main\n\
             type Row struct { values [2]int }\n\
             type Matrix struct { row Row }\n\
             func main() { var matrix Matrix; var index int; matrix.row.values[index] = 1; _ = matrix.row.values[index] }\n",
            TargetSpec::for_test_64(),
        )
        .unwrap();
        verify(&package).unwrap();

        let function = package.function(package.entry()).unwrap();
        let instructions = function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .collect::<Vec<_>>();
        assert_eq!(
            instructions
                .iter()
                .filter(|instruction| matches!(instruction.kind, InstructionKind::GepField { .. }))
                .count(),
            4
        );
        assert_eq!(
            instructions
                .iter()
                .filter(|instruction| matches!(instruction.kind, InstructionKind::GepIndex { .. }))
                .count(),
            2
        );
        assert!(instructions.iter().any(|instruction| matches!(
            instruction.kind,
            InstructionKind::Compare {
                predicate: ComparePredicate::SignedGreaterEqual,
                ..
            }
        )));
        assert!(function.blocks.iter().any(|block| matches!(
            block.terminator,
            Terminator::Trap {
                reason: TrapReason::BoundsError
            }
        )));
        for instruction in instructions
            .iter()
            .filter(|instruction| matches!(instruction.kind, InstructionKind::GepIndex { .. }))
        {
            let InstructionKind::GepIndex { index, .. } = instruction.kind else {
                unreachable!();
            };
            assert_eq!(function.value(index).unwrap().typ, package.types().i64());
        }
    }

    #[test]
    fn lowers_local_struct_field_to_stable_hir() {
        let package = lower(
            "package main\ntype Pair struct { value int }\nfunc main() { var pair Pair; pair.value = 1 }\n",
            TargetSpec::for_test_64(),
        )
        .unwrap();
        verify(&package).unwrap();
        assert_eq!(
            package.to_string(),
            r#"target "x86_64-unknown-linux-gnu" {
  cpu = "generic"
  features = ""
  data_layout = "e-m:e-p:64:64-i64:64-n8:16:32:64-S128"
  pointer_width = 64
  endianness = Little
}
type !1
type !2
type !3
type !4
type !5
type !6
type !7
type !8
type !9
type !1 = void
type !2 = i1
type !3 = i8
type !4 = i16
type !5 = i32
type !6 = i64
type !7 = struct {!6}
type !8 = ptr(addrspace=0, !7)
type !9 = ptr(addrspace=0, !6)
func @1 "gane.main"() -> () internal [no_return=false, no_unwind=false, memory=unknown] entry ^1 {
  slot $1: !7
  ^1():
    %1 = stack_addr $1
    %2 = gep_field %1, 0
    %3 = const !6 1
    store %2, %3
    return
}
entry @1
"#
        );
    }

    #[test]
    fn normalizes_array_indexes_to_the_target_pointer_width() {
        for (target, expected) in [
            (TargetSpec::for_test_32(), HirTypeKind::I32),
            (TargetSpec::for_test_64(), HirTypeKind::I64),
        ] {
            let package = lower(
                "package main\nfunc main() { var values [2]int; var index byte; _ = values[index] }\n",
                target,
            )
            .unwrap();
            verify(&package).unwrap();
            let function = package.function(package.entry()).unwrap();
            let index = function
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .find_map(|instruction| match instruction.kind {
                    InstructionKind::GepIndex { index, .. } => Some(index),
                    _ => None,
                })
                .unwrap();
            assert!(matches!(
                package.types().get(function.value(index).unwrap().typ),
                Some(crate::HirType { kind }) if *kind == expected
            ));
        }
    }

    #[test]
    fn guards_pointer_dereferences_division_and_byte_indexes() {
        let package = lower(
            "package main\n\
             type Pair struct { value int }\n\
             func calculate(pair *Pair, divisor int, bytes *[2]int, index byte) int {\n\
                 pair.value = pair.value / divisor\n\
                 (*bytes)[index] = pair.value % divisor\n\
                 return (*bytes)[index]\n\
             }\n\
             func byte_math(left byte, right byte) byte { return left / right % right }\n\
             func main() {}\n",
            TargetSpec::for_test_64(),
        )
        .unwrap();
        verify(&package).unwrap();

        let calculate = package.function(crate::FunctionId::from_raw(1)).unwrap();
        let instructions = calculate
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .collect::<Vec<_>>();
        assert_eq!(
            calculate
                .blocks
                .iter()
                .filter(|block| matches!(
                    block.terminator,
                    Terminator::Trap {
                        reason: TrapReason::NullDereference
                    }
                ))
                .count(),
            5
        );
        assert_eq!(
            calculate
                .blocks
                .iter()
                .filter(|block| matches!(
                    block.terminator,
                    Terminator::Trap {
                        reason: TrapReason::DivisionByZero
                    }
                ))
                .count(),
            2
        );
        assert_eq!(
            calculate
                .blocks
                .iter()
                .filter(|block| matches!(
                    block.terminator,
                    Terminator::Trap {
                        reason: TrapReason::BoundsError
                    }
                ))
                .count(),
            2
        );
        assert_eq!(
            instructions
                .iter()
                .filter(|instruction| matches!(
                    instruction.kind,
                    InstructionKind::IntCast {
                        kind: IntCastKind::ZeroExtend,
                        ..
                    }
                ))
                .count(),
            2
        );
        assert!(instructions.iter().any(|instruction| matches!(
            instruction.kind,
            InstructionKind::Binary {
                op: BinaryOp::SignedDiv,
                ..
            }
        )));
        assert!(instructions.iter().any(|instruction| matches!(
            instruction.kind,
            InstructionKind::Binary {
                op: BinaryOp::SignedRem,
                ..
            }
        )));

        let byte_math = package.function(crate::FunctionId::from_raw(2)).unwrap();
        assert_eq!(
            byte_math
                .blocks
                .iter()
                .filter(|block| matches!(
                    block.terminator,
                    Terminator::Trap {
                        reason: TrapReason::DivisionByZero
                    }
                ))
                .count(),
            2
        );
        assert!(
            byte_math
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .any(|instruction| matches!(
                    instruction.kind,
                    InstructionKind::Binary {
                        op: BinaryOp::UnsignedDiv,
                        ..
                    }
                ))
        );
        assert!(
            byte_math
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .any(|instruction| matches!(
                    instruction.kind,
                    InstructionKind::Binary {
                        op: BinaryOp::UnsignedRem,
                        ..
                    }
                ))
        );
    }

    #[test]
    fn caches_identical_arrays_and_supports_pointer_recursive_structs() {
        let package = lower(
            "package main\n\
             type Node struct { next *Node; value int }\n\
             func main() { var first [2]int; var second [2]int; var node Node; var pointer *Node; pointer = &node; first[0] = second[0]; pointer.next.value = first[0] }\n",
            TargetSpec::for_test_64(),
        )
        .unwrap();
        verify(&package).unwrap();

        let function = package.function(package.entry()).unwrap();
        assert_eq!(function.stack_slots[0].typ, function.stack_slots[1].typ);
        assert!(package.types().iter().any(|(_, typ)| matches!(
            typ.kind,
            HirTypeKind::Struct { ref fields }
                if fields.iter().any(|field| matches!(
                    package.types().get(*field),
                    Some(crate::HirType {
                        kind: HirTypeKind::Ptr { pointee, .. }
                    }) if *pointee == function.stack_slots[2].typ
                ))
        )));
    }

    #[test]
    fn reports_sema_errors_missing_facts_and_unsupported_syntax() {
        assert!(matches!(
            lower(
                "package main\nfunc main() { missing = 1 }\n",
                TargetSpec::for_test_64(),
            ),
            Err(LowerError::SemanticErrors { .. })
        ));
        assert!(matches!(
            lower(
                "package main\nfunc main() { if x := true; x {} }\n",
                TargetSpec::for_test_64(),
            ),
            Err(LowerError::SemanticErrors { .. })
        ));
        for source in ["package main\nvar x int\nfunc main() {}\n"] {
            assert!(matches!(
                lower(source, TargetSpec::for_test_64()),
                Err(LowerError::Unsupported { .. })
            ));
        }

        let source = "package main\nfunc main() {}\n";
        let mut first_files = FileSet::new();
        let (first_ast, first_errors) = parse_file(
            &mut first_files,
            "first.go",
            source.as_bytes(),
            Mode::default(),
        );
        assert!(first_errors.is_none());
        let first_input = PackageInput::single("example/main", FileId::from_raw(1), &first_ast);
        let analysis = analyze_package(first_input);

        let mut second_files = FileSet::new();
        let (second_ast, second_errors) = parse_file(
            &mut second_files,
            "second.go",
            source.as_bytes(),
            Mode::default(),
        );
        assert!(second_errors.is_none());
        let second_input = PackageInput::single("example/main", FileId::from_raw(1), &second_ast);
        assert!(matches!(
            lower_package(&second_input, &analysis, TargetSpec::for_test_64()),
            Err(LowerError::MissingSemanticFact { .. })
        ));
    }
}
