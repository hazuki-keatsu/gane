use crate::ir::IrPackage;
use crate::{
    BinaryOp, Callee, ComparePredicate, Constant, GlobalInitializer, InstructionKind, IntCastKind,
    IrTypeKind, Terminator, UnaryOp, UnverifiedIrPackage, VerifiedIrPackage,
};
use std::fmt::{self, Write};

impl fmt::Display for UnverifiedIrPackage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        print_package(self.inner(), f)
    }
}

impl fmt::Display for VerifiedIrPackage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        print_package(self.inner(), f)
    }
}

fn print_package(package: &IrPackage, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    writeln!(f, "target {:?} {{", package.target.triple())?;
    writeln!(f, "  cpu = {:?}", package.target.cpu())?;
    writeln!(f, "  features = {:?}", package.target.features())?;
    writeln!(f, "  data_layout = {:?}", package.target.data_layout())?;
    writeln!(f, "  pointer_width = {}", package.target.pointer_width())?;
    writeln!(f, "  endianness = {:?}", package.target.endianness())?;
    writeln!(f, "}}")?;

    for (id, _) in package.types.iter() {
        writeln!(f, "type !{}", id.raw())?;
    }
    for (id, typ) in package.types.iter() {
        writeln!(f, "type !{} = {}", id.raw(), TypeDisplay(&typ.kind))?;
    }

    for (index, global) in package.globals.iter().enumerate() {
        write!(
            f,
            "global @{} {:?}: !{} {}",
            index + 1,
            global.symbol,
            global.typ.raw(),
            if global.mutable {
                "mutable"
            } else {
                "constant"
            }
        )?;
        match global.initializer {
            GlobalInitializer::Zero => writeln!(f, " = zero")?,
            GlobalInitializer::Scalar(value) => writeln!(f, " = {}", constant(value))?,
        }
    }

    for (index, function) in package.functions.iter().enumerate() {
        write!(f, "func @{} {:?}(", index + 1, function.symbol)?;
        for (parameter_index, parameter) in function.signature.parameters.iter().enumerate() {
            if parameter_index != 0 {
                f.write_str(", ")?;
            }
            write!(f, "!{}", parameter.typ.raw())?;
        }
        f.write_str(") -> (")?;
        ids(f, function.signature.results.iter().map(|id| id.raw()), "!")?;
        writeln!(
            f,
            ") [no_return={}] entry ^{} {{",
            function.attributes.no_return,
            function.entry.raw()
        )?;

        for (slot_index, slot) in function.stack_slots.iter().enumerate() {
            writeln!(f, "  slot ${}: !{}", slot_index + 1, slot.typ.raw())?;
        }
        for (block_index, block) in function.blocks.iter().enumerate() {
            write!(f, "  ^{}(", block_index + 1)?;
            for (parameter_index, value) in block.parameters.iter().enumerate() {
                if parameter_index != 0 {
                    f.write_str(", ")?;
                }
                let typ = function
                    .value(*value)
                    .map(|value| value.typ.raw())
                    .unwrap_or(0);
                write!(f, "%{}: !{}", value.raw(), typ)?;
            }
            writeln!(f, "):")?;
            for instruction in &block.instructions {
                f.write_str("    ")?;
                if !instruction.results.is_empty() {
                    ids(f, instruction.results.iter().map(|id| id.raw()), "%")?;
                    f.write_str(" = ")?;
                }
                print_instruction(&instruction.kind, f)?;
                writeln!(f)?;
            }
            f.write_str("    ")?;
            print_terminator(&block.terminator, f)?;
            writeln!(f)?;
        }
        writeln!(f, "}}")?;
    }
    writeln!(f, "entry @{}", package.entry.raw())
}

struct TypeDisplay<'a>(&'a IrTypeKind);

impl fmt::Display for TypeDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            IrTypeKind::Void => f.write_str("void"),
            IrTypeKind::I1 => f.write_str("i1"),
            IrTypeKind::I8 => f.write_str("i8"),
            IrTypeKind::I16 => f.write_str("i16"),
            IrTypeKind::I32 => f.write_str("i32"),
            IrTypeKind::I64 => f.write_str("i64"),
            IrTypeKind::Ptr {
                pointee,
                address_space,
            } => {
                write!(f, "ptr(addrspace={address_space}, !{})", pointee.raw())
            }
            IrTypeKind::Array { length, element } => {
                write!(f, "array {length} x !{}", element.raw())
            }
            IrTypeKind::Struct { fields } => {
                f.write_str("struct {")?;
                ids(f, fields.iter().map(|id| id.raw()), "!")?;
                f.write_char('}')
            }
        }
    }
}

fn print_instruction(kind: &InstructionKind, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match kind {
        InstructionKind::Const { value, typ } => {
            write!(f, "const !{} {}", typ.raw(), constant(*value))
        }
        InstructionKind::Unary { op, operand } => write!(f, "{} %{}", unary(*op), operand.raw()),
        InstructionKind::Binary { op, left, right } => {
            write!(f, "{} %{}, %{}", binary(*op), left.raw(), right.raw())
        }
        InstructionKind::Compare {
            predicate,
            left,
            right,
        } => write!(
            f,
            "cmp.{} %{}, %{}",
            predicate_name(*predicate),
            left.raw(),
            right.raw()
        ),
        InstructionKind::IntCast {
            kind,
            operand,
            target,
        } => write!(f, "{} %{} to !{}", cast(*kind), operand.raw(), target.raw()),
        InstructionKind::StackAddr { slot } => write!(f, "stack_addr ${}", slot.raw()),
        InstructionKind::GlobalAddr { global } => write!(f, "global_addr @{}", global.raw()),
        InstructionKind::GepField { base, field } => {
            write!(f, "gep_field %{}, {field}", base.raw())
        }
        InstructionKind::GepIndex { base, index } => {
            write!(f, "gep_index %{}, %{}", base.raw(), index.raw())
        }
        InstructionKind::Load { pointer } => write!(f, "load %{}", pointer.raw()),
        InstructionKind::Store { pointer, value } => {
            write!(f, "store %{}, %{}", pointer.raw(), value.raw())
        }
        InstructionKind::AggregateZero { destination, typ } => {
            write!(f, "aggregate_zero %{}, !{}", destination.raw(), typ.raw())
        }
        InstructionKind::AggregateCopy {
            destination,
            source,
            typ,
        } => write!(
            f,
            "aggregate_copy %{}, %{}, !{}",
            destination.raw(),
            source.raw(),
            typ.raw()
        ),
        InstructionKind::Call {
            callee: Callee::Function(function),
            arguments,
        } => {
            write!(f, "call @{}(", function.raw())?;
            ids(f, arguments.iter().map(|id| id.raw()), "%")?;
            f.write_char(')')
        }
    }
}

fn print_terminator(terminator: &Terminator, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match terminator {
        Terminator::Branch { target, arguments } => {
            write!(f, "br ^{}(", target.raw())?;
            ids(f, arguments.iter().map(|id| id.raw()), "%")?;
            f.write_char(')')
        }
        Terminator::CondBranch {
            condition,
            then_target,
            then_arguments,
            else_target,
            else_arguments,
        } => {
            write!(f, "condbr %{}, ^{}(", condition.raw(), then_target.raw())?;
            ids(f, then_arguments.iter().map(|id| id.raw()), "%")?;
            write!(f, "), ^{}(", else_target.raw())?;
            ids(f, else_arguments.iter().map(|id| id.raw()), "%")?;
            f.write_char(')')
        }
        Terminator::Return { values } => {
            f.write_str("return")?;
            if !values.is_empty() {
                f.write_char(' ')?;
                ids(f, values.iter().map(|id| id.raw()), "%")?;
            }
            Ok(())
        }
        Terminator::Trap { reason } => write!(f, "trap {reason:?}"),
        Terminator::Unreachable => f.write_str("unreachable"),
    }
}

fn ids(f: &mut fmt::Formatter<'_>, values: impl Iterator<Item = u32>, prefix: &str) -> fmt::Result {
    for (index, value) in values.enumerate() {
        if index != 0 {
            f.write_str(", ")?;
        }
        write!(f, "{prefix}{value}")?;
    }
    Ok(())
}

fn constant(value: Constant) -> String {
    match value {
        Constant::Bool(value) => value.to_string(),
        Constant::Integer(value) => value.to_string(),
        Constant::Null => "null".into(),
    }
}
fn unary(value: UnaryOp) -> &'static str {
    match value {
        UnaryOp::Neg => "neg",
        UnaryOp::BitNot => "bit_not",
        UnaryOp::LogicalNot => "logical_not",
    }
}
fn cast(value: IntCastKind) -> &'static str {
    match value {
        IntCastKind::Truncate => "trunc",
        IntCastKind::SignExtend => "sext",
        IntCastKind::ZeroExtend => "zext",
    }
}
fn binary(value: BinaryOp) -> &'static str {
    match value {
        BinaryOp::Add => "add",
        BinaryOp::Sub => "sub",
        BinaryOp::Mul => "mul",
        BinaryOp::SignedDiv => "sdiv",
        BinaryOp::UnsignedDiv => "udiv",
        BinaryOp::SignedRem => "srem",
        BinaryOp::UnsignedRem => "urem",
        BinaryOp::Shl => "shl",
        BinaryOp::ArithmeticShr => "ashr",
        BinaryOp::LogicalShr => "lshr",
        BinaryOp::BitAnd => "and",
        BinaryOp::BitOr => "or",
        BinaryOp::BitXor => "xor",
        BinaryOp::BitClear => "bit_clear",
    }
}
fn predicate_name(value: ComparePredicate) -> &'static str {
    match value {
        ComparePredicate::Equal => "eq",
        ComparePredicate::NotEqual => "ne",
        ComparePredicate::SignedLess => "slt",
        ComparePredicate::SignedLessEqual => "sle",
        ComparePredicate::SignedGreater => "sgt",
        ComparePredicate::SignedGreaterEqual => "sge",
        ComparePredicate::UnsignedLess => "ult",
        ComparePredicate::UnsignedLessEqual => "ule",
        ComparePredicate::UnsignedGreater => "ugt",
        ComparePredicate::UnsignedGreaterEqual => "uge",
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        Constant, FunctionAttributes, InstructionKind, IrBuilder, IrSignature, TargetSpec,
        Terminator,
    };

    #[test]
    fn package_text_is_stable_and_ignores_debug_metadata() {
        let mut builder = IrBuilder::new(TargetSpec::for_test_64());
        let i32 = builder.types().i32();
        let main = builder.declare_function(
            "gane.main".into(),
            IrSignature {
                parameters: vec![],
                results: vec![],
            },
            FunctionAttributes::default(),
        );
        builder.set_entry(main).unwrap();
        let entry = builder.entry_block(main).unwrap();
        builder
            .add_stack_slot(main, i32, Some("ignored".into()), None)
            .unwrap();
        builder
            .append_instruction(
                main,
                entry,
                InstructionKind::Const {
                    value: Constant::Integer(7),
                    typ: i32,
                },
                [i32],
                None,
            )
            .unwrap();
        builder
            .set_terminator(main, entry, Terminator::Return { values: vec![] })
            .unwrap();
        let package = builder.finish().unwrap();

        let first = package.to_string();
        assert_eq!(
            first,
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
type !1 = void
type !2 = i1
type !3 = i8
type !4 = i16
type !5 = i32
type !6 = i64
func @1 "gane.main"() -> () [no_return=false] entry ^1 {
  slot $1: !5
  ^1():
    %1 = const !5 7
    return
}
entry @1
"#
        );
        assert!(!first.contains("ignored"));
    }
}
