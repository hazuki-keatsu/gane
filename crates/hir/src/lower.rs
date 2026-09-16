use crate::{
    BinaryOp, BlockId, BuildError, Callee, CallingConvention, ComparePredicate, Constant,
    FunctionAttributes, FunctionId, HirBuilder, HirParameter, HirSignature, HirTypeKind, Linkage,
    PassingMode, Terminator, TypeId, UnaryOp, UnverifiedHirPackage, ValueId,
};
use gane_parser::{
    ast,
    token::{AstNodeId, Token},
};
use gane_sema::{
    AnalysisResult, BasicType, ConstValue, ObjectId, ObjectKind, PackageInput, Severity, TypeKind,
};
use std::{collections::HashMap, error::Error, fmt};

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
struct Local {
    pointer: ValueId,
}

#[derive(Clone)]
struct LoweredFunction {
    id: FunctionId,
    signature: HirSignature,
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
        &self,
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
        let parameters = self
            .analysis
            .tuple(*params)
            .ok_or_else(|| self.missing(node, "function parameters"))?
            .vars
            .iter()
            .map(|object| {
                self.lower_type(self.analysis.object(*object).typ, node)
                    .map(|typ| HirParameter {
                        typ,
                        passing: PassingMode::Direct,
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let results = self
            .analysis
            .tuple(*results)
            .ok_or_else(|| self.missing(node, "function results"))?
            .vars
            .iter()
            .map(|object| self.lower_type(self.analysis.object(*object).typ, node))
            .collect::<Result<Vec<_>, _>>()?;
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
        let entry_parameters = self
            .builder
            .entry_parameters(self.function)
            .map_err(|source| LowerError::Build { node, source })?;
        for (object, value) in parameters.vars.iter().zip(entry_parameters) {
            let parameter = self.analysis.object(*object);
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
            self.locals.insert(*object, Local { pointer });
            self.store(parameter_node, pointer, value)?;
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
            ast::Stmt::ExprStmt(statement) => self.lower_expression_statement(statement),
            ast::Stmt::ReturnStmt(statement) => self.lower_return(statement),
            ast::Stmt::AssignStmt(statement) => {
                Err(self.unsupported(statement.node_id(), "non-simple assignment"))
            }
            _ => Err(self.unsupported(statement.node_id(), "statement")),
        }
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
            self.locals.insert(object, Local { pointer });

            if let Some(value) = values.get(index) {
                self.store(spec.node_id(), pointer, *value)?;
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
                ast::Expr::Ident(identifier) => {
                    let object = self.use_of(identifier.node_id())?;
                    self.locals.get(&object).copied().map(Some).ok_or_else(|| {
                        self.unsupported(identifier.node_id(), "non-local assignment")
                    })
                }
                _ => Err(self.unsupported(expression.node_id(), "assignment target")),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let values = statement
            .rhs
            .iter()
            .map(|expression| self.lower_expression(expression))
            .collect::<Result<Vec<_>, _>>()?;

        for (destination, value) in destinations.into_iter().zip(values) {
            if let Some(destination) = destination {
                self.store(statement.node_id(), destination.pointer, value)?;
            }
        }
        Ok(())
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
            ast::Expr::Ident(identifier) => {
                let object = self.use_of(identifier.node_id())?;
                let local = self
                    .locals
                    .get(&object)
                    .copied()
                    .ok_or_else(|| self.unsupported(identifier.node_id(), "non-local value"))?;
                Ok(self.instruction(
                    identifier.node_id(),
                    crate::InstructionKind::Load {
                        pointer: local.pointer,
                    },
                    [result_type],
                )?[0])
            }
            ast::Expr::UnaryExpr(expression) => {
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

    fn lower_type(&self, typ: gane_sema::TypeId, node: AstNodeId) -> Result<TypeId, LowerError> {
        if self.analysis.is_basic_type(typ, BasicType::Bool) {
            Ok(self.builder.types().i1())
        } else if self.analysis.is_basic_type(typ, BasicType::Byte) {
            Ok(self.builder.types().i8())
        } else if self.analysis.is_basic_type(typ, BasicType::Int) {
            Ok(match self.target_width {
                32 => self.builder.types().i32(),
                64 => self.builder.types().i64(),
                _ => return Err(self.unsupported(node, "target pointer width")),
            })
        } else {
            Err(self.unsupported(node, "type"))
        }
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

    fn store(
        &mut self,
        node: AstNodeId,
        pointer: ValueId,
        value: ValueId,
    ) -> Result<(), LowerError> {
        self.instruction(node, crate::InstructionKind::Store { pointer, value }, [])?;
        Ok(())
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
    fn reports_sema_errors_missing_facts_and_unsupported_syntax() {
        assert!(matches!(
            lower(
                "package main\nfunc main() { missing = 1 }\n",
                TargetSpec::for_test_64(),
            ),
            Err(LowerError::SemanticErrors { .. })
        ));
        for source in [
            "package main\nfunc main() { if true {} }\n",
            "package main\nvar x int\nfunc main() {}\n",
        ] {
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
