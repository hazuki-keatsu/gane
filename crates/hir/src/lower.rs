use crate::{
    BinaryOp, BlockId, BuildError, CallingConvention, ComparePredicate, Constant,
    FunctionAttributes, FunctionId, HirBuilder, HirSignature, HirTypeKind, Linkage, Terminator,
    TypeId, UnaryOp, UnverifiedHirPackage, ValueId,
};
use gane_parser::{
    ast,
    token::{AstNodeId, Token},
};
use gane_sema::{AnalysisResult, BasicType, ConstValue, ObjectId, PackageInput, Severity};
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

struct Lowerer<'a> {
    analysis: &'a AnalysisResult,
    builder: HirBuilder,
    function: FunctionId,
    block: BlockId,
    target_width: u8,
    locals: HashMap<ObjectId, Local>,
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
        let mut main = None;

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
                        if object == main_object {
                            main = Some(declaration);
                        } else {
                            return Err(
                                self.unsupported(declaration.node_id(), "additional function")
                            );
                        }
                    }
                    ast::Decl::BadDecl(declaration) => {
                        return Err(self.unsupported(declaration.node_id(), "bad declaration"));
                    }
                }
            }
        }

        let main = main.ok_or_else(|| {
            self.missing(
                input
                    .files
                    .first()
                    .map(|file| file.ast.node_id())
                    .unwrap_or(AstNodeId::INVALID),
                "main declaration",
            )
        })?;
        let body = main
            .body
            .as_deref()
            .ok_or_else(|| self.unsupported(main.node_id(), "bodyless main"))?;

        self.function = self.builder.declare_function(
            "gane.main".to_owned(),
            HirSignature {
                parameters: Vec::new(),
                results: Vec::new(),
                calling_convention: CallingConvention::Gane,
            },
            Linkage::Internal,
            FunctionAttributes::default(),
        );
        self.build(main.node_id(), |builder, function, _| {
            builder.set_entry(function)
        })?;
        self.block = self.build(main.node_id(), |builder, function, _| {
            builder.entry_block(function)
        })?;

        self.lower_block(body)?;
        self.build(body.node_id(), |builder, function, block| {
            builder.set_terminator(function, block, Terminator::Return { values: Vec::new() })
        })?;
        self.builder.finish().map_err(|source| LowerError::Build {
            node: main.node_id(),
            source,
        })
    }

    fn lower_block(&mut self, block: &ast::BlockStmt) -> Result<(), LowerError> {
        for statement in &block.list {
            self.lower_statement(statement)?;
        }
        Ok(())
    }

    fn lower_statement(&mut self, statement: &ast::Stmt) -> Result<(), LowerError> {
        match statement {
            ast::Stmt::EmptyStmt(_) => Ok(()),
            ast::Stmt::BlockStmt(block) => self.lower_block(block),
            ast::Stmt::DeclStmt(statement) => self.lower_declaration(&statement.decl),
            ast::Stmt::AssignStmt(statement) if statement.tok == Token::Assign => {
                self.lower_assignment(statement)
            }
            ast::Stmt::AssignStmt(statement) => {
                Err(self.unsupported(statement.node_id(), "non-simple assignment"))
            }
            _ => Err(self.unsupported(statement.node_id(), "statement")),
        }
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
            "package main\nfunc helper() {}\nfunc main() {}\n",
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
