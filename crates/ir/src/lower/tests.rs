use super::*;
use crate::{InstructionKind, TargetSpec, verify};
use gane_parser::{
    parser::{Mode, parse_file},
    token::FileSet,
};
use gane_sema::{FileId, analyze_package};

fn lower(source: &str, target: TargetSpec) -> Result<UnverifiedIrPackage, LowerError> {
    let mut files = FileSet::new();
    let (ast, errors) = parse_file(&mut files, "main.go", source.as_bytes(), Mode::default());
    assert!(errors.is_none(), "{errors:?}");
    let input = PackageInput::single("example/main", FileId::from_raw(1), &ast);
    let analysis = analyze_package(input.clone());
    lower_package(&input, &analysis, target)
}

#[test]
fn lowers_minimal_main_to_stable_verified_ir() {
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
type !1 = void
type !2 = i1
type !3 = i8
type !4 = i16
type !5 = i32
type !6 = i64
type !7 = ptr(addrspace=0, !6)
func @1 "gane.main"() -> () [no_return=false] entry ^1 {
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
        let package = lower("package main\nfunc main() { var x int; _ = x }\n", target).unwrap();
        let slot = &package.function(package.entry()).unwrap().stack_slots[0];
        let width = match package.types().get(slot.typ).unwrap().kind {
            IrTypeKind::I32 => 32,
            IrTypeKind::I64 => 64,
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
fn lowers_scalar_functions_calls_and_returns_to_verified_ir() {
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
type !1 = void
type !2 = i1
type !3 = i8
type !4 = i16
type !5 = i32
type !6 = i64
type !7 = ptr(addrspace=0, !6)
func @1 "gane.add"(!6, !6) -> (!6) [no_return=false] entry ^1 {
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
func @2 "gane.main"() -> () [no_return=false] entry ^1 {
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
func @3 "gane.sink"(!6) -> () [no_return=false] entry ^1 {
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
type !1 = void
type !2 = i1
type !3 = i8
type !4 = i16
type !5 = i32
type !6 = i64
type !7 = ptr(addrspace=0, !6)
func @1 "gane.main"() -> () [no_return=false] entry ^1 {
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
type !1 = void
type !2 = i1
type !3 = i8
type !4 = i16
type !5 = i32
type !6 = i64
type !7 = ptr(addrspace=0, !6)
func @1 "gane.main"() -> () [no_return=false] entry ^1 {
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
type !1 = void
type !2 = i1
type !3 = i8
type !4 = i16
type !5 = i32
type !6 = i64
type !7 = ptr(addrspace=0, !2)
func @1 "gane.left"() -> (!2) [no_return=false] entry ^1 {
  ^1():
    %1 = const !2 true
    return %1
}
func @2 "gane.right"() -> (!2) [no_return=false] entry ^1 {
  ^1():
    %1 = const !2 false
    return %1
}
func @3 "gane.main"() -> () [no_return=false] entry ^1 {
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
    assert!(matches!(
        lower(
            "package main\nfunc helper() {}\nfunc main() { (helper)() }\n",
            TargetSpec::for_test_64(),
        ),
        Err(LowerError::Unsupported { .. })
    ));
    assert!(matches!(
        lower(
            "package main\nfunc helper(value [1]int) {}\nfunc main() {}\n",
            TargetSpec::for_test_64(),
        ),
        Err(LowerError::SemanticErrors { .. })
    ));
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
fn lowers_local_struct_field_to_stable_ir() {
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
type !1 = void
type !2 = i1
type !3 = i8
type !4 = i16
type !5 = i32
type !6 = i64
type !7 = struct {!6}
type !8 = ptr(addrspace=0, !7)
type !9 = ptr(addrspace=0, !6)
func @1 "gane.main"() -> () [no_return=false] entry ^1 {
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
fn lowers_local_aggregate_copy_and_zero() {
    let package = lower(
        "package main\n\
         type Pair struct { value int; values [2]int }\n\
         func main() {\n\
             var source Pair\n\
             var copy Pair = source\n\
             copy = Pair{}\n\
             var input [2]int\n\
             var output [2]int = input\n\
             output = [2]int{}\n\
         }\n",
        TargetSpec::for_test_64(),
    )
    .unwrap();
    verify(&package).unwrap();

    let function = package.function(package.entry()).unwrap();
    assert_eq!(function.stack_slots.len(), 4);
    let instructions = function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();
    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| matches!(instruction.kind, InstructionKind::AggregateCopy { .. }))
            .count(),
        2
    );
    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| matches!(instruction.kind, InstructionKind::AggregateZero { .. }))
            .count(),
        2
    );
    for instruction in instructions {
        match instruction.kind {
            InstructionKind::AggregateCopy {
                destination,
                source,
                typ,
            } => {
                for pointer in [destination, source] {
                    assert!(matches!(
                        package.types().get(function.value(pointer).unwrap().typ),
                        Some(crate::IrType {
                            kind: IrTypeKind::Ptr { pointee, .. }
                        }) if *pointee == typ
                    ));
                }
            }
            InstructionKind::AggregateZero { destination, typ } => {
                assert!(matches!(
                    package.types().get(function.value(destination).unwrap().typ),
                    Some(crate::IrType {
                        kind: IrTypeKind::Ptr { pointee, .. }
                    }) if *pointee == typ
                ));
            }
            _ => {}
        }
    }
    assert_eq!(
        package.to_string(),
        r#"target "x86_64-unknown-linux-gnu" {
  cpu = "generic"
  features = ""
  data_layout = "e-m:e-p:64:64-i64:64-n8:16:32:64-S128"
  pointer_width = 64
  endianness = Little
}
type !1 = void
type !2 = i1
type !3 = i8
type !4 = i16
type !5 = i32
type !6 = i64
type !7 = struct {!6, !8}
type !8 = array 2 x !6
type !9 = ptr(addrspace=0, !7)
type !10 = ptr(addrspace=0, !8)
func @1 "gane.main"() -> () [no_return=false] entry ^1 {
  slot $1: !7
  slot $2: !7
  slot $3: !8
  slot $4: !8
  ^1():
    %1 = stack_addr $1
    %2 = stack_addr $2
    aggregate_copy %2, %1, !7
    aggregate_zero %2, !7
    %3 = stack_addr $3
    %4 = stack_addr $4
    aggregate_copy %4, %3, !8
    aggregate_zero %4, !8
    return
}
entry @1
"#
    );
}

#[test]
fn snapshots_aggregate_rhs_and_copies_through_aggregate_places() {
    let package = lower(
        "package main\n\
         type Pair struct { value int }\n\
         type Container struct { first Pair; entries [2]Pair }\n\
         func main() {\n\
             var left Pair\n\
             var right Pair\n\
             left, right = right, left\n\
             var source Container\n\
             var destination Container\n\
             var pointer *Container\n\
             pointer = &destination\n\
             (*pointer).first = source.first\n\
             (*pointer).entries[0] = source.entries[1]\n\
         }\n",
        TargetSpec::for_test_64(),
    )
    .unwrap();
    verify(&package).unwrap();

    let function = package.function(package.entry()).unwrap();
    assert_eq!(function.stack_slots.len(), 7);
    let instructions = function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();
    let copies = instructions
        .iter()
        .filter_map(|instruction| match instruction.kind {
            InstructionKind::AggregateCopy {
                destination,
                source,
                ..
            } => Some((destination, source)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(copies.len(), 6);
    assert_eq!(copies[0].0, copies[2].1);
    assert_eq!(copies[1].0, copies[3].1);
    assert!(
        instructions
            .iter()
            .filter(|instruction| matches!(instruction.kind, InstructionKind::GepField { .. }))
            .count()
            >= 4
    );
    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| matches!(instruction.kind, InstructionKind::GepIndex { .. }))
            .count(),
        2
    );
}

#[test]
fn snapshots_an_aggregate_before_a_later_rhs_call() {
    let package = lower(
        "package main\n\
         type Pair struct { value int }\n\
         func reset(pair *Pair) int { *pair = Pair{}; return 0 }\n\
         func main() {\n\
             var pair Pair\n\
             var number int\n\
             pair, number = pair, reset(&pair)\n\
         }\n",
        TargetSpec::for_test_64(),
    )
    .unwrap();
    verify(&package).unwrap();

    let main = package.function(package.entry()).unwrap();
    let instructions = main.blocks[0].instructions.iter().collect::<Vec<_>>();
    let copies = instructions
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| {
            matches!(instruction.kind, InstructionKind::AggregateCopy { .. }).then_some(index)
        })
        .collect::<Vec<_>>();
    let call = instructions
        .iter()
        .position(|instruction| matches!(instruction.kind, InstructionKind::Call { .. }))
        .unwrap();
    assert_eq!(copies.len(), 2);
    assert!(copies[0] < call && call < copies[1]);
}

#[test]
fn normalizes_array_indexes_to_the_target_pointer_width() {
    for (target, expected) in [
        (TargetSpec::for_test_32(), IrTypeKind::I32),
        (TargetSpec::for_test_64(), IrTypeKind::I64),
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
            Some(crate::IrType { kind }) if *kind == expected
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
fn keeps_non_null_facts_isolated_between_functions() {
    let package = lower(
        "package main\n\
         func seed() { var a int; var b int; var c int }\n\
         func check(p *int) { _ = *p }\n\
         func main() { var value int; check(&value) }\n",
        TargetSpec::for_test_64(),
    )
    .unwrap();
    verify(&package).unwrap();

    let check = package
        .functions()
        .find_map(|(_, function)| (function.symbol == "gane.check").then_some(function))
        .unwrap();
    assert!(check.blocks.iter().any(|block| matches!(
        block.terminator,
        Terminator::Trap {
            reason: TrapReason::NullDereference
        }
    )));
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
        IrTypeKind::Struct { ref fields }
            if fields.iter().any(|field| matches!(
                package.types().get(*field),
                Some(crate::IrType {
                    kind: IrTypeKind::Ptr { pointee, .. }
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
    assert!(
        lower(
            "package main\nvar x int\nfunc main() {}\n",
            TargetSpec::for_test_64(),
        )
        .is_ok()
    );
    assert!(matches!(
        lower(
            "package main\nfunc foreign()\nfunc main() {}\n",
            TargetSpec::for_test_64(),
        ),
        Err(LowerError::SemanticErrors { .. })
    ));

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

#[test]
fn lowers_scalar_globals_to_stable_ir_and_reuses_global_ids() {
    let package = lower(
        "package main\n\
         const initial = 1\n\
         var count int = initial\n\
         var ready bool\n\
         var none *int = nil\n\
         func tick() { count++ }\n\
         func main() { count = count + 1; tick(); _ = count }\n",
        TargetSpec::for_test_64(),
    )
    .unwrap();
    verify(&package).unwrap();

    let globals = package.globals().collect::<Vec<_>>();
    assert_eq!(globals.len(), 3);
    assert!(matches!(
        globals[0].1.initializer,
        crate::GlobalInitializer::Scalar(Constant::Integer(1))
    ));
    assert!(matches!(
        globals[1].1.initializer,
        crate::GlobalInitializer::Zero
    ));
    assert!(matches!(
        globals[2].1.initializer,
        crate::GlobalInitializer::Zero
    ));
    assert!(
        globals
            .iter()
            .all(|(_, global)| { global.symbol.starts_with("gane.") })
    );

    let count = crate::GlobalId::from_raw(1);
    let addresses = package
        .functions()
        .flat_map(|(_, function)| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction.kind {
            InstructionKind::GlobalAddr { global } => Some(global),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(addresses, vec![count, count, count, count]);
    assert_eq!(
        package.to_string(),
        r#"target "x86_64-unknown-linux-gnu" {
  cpu = "generic"
  features = ""
  data_layout = "e-m:e-p:64:64-i64:64-n8:16:32:64-S128"
  pointer_width = 64
  endianness = Little
}
type !1 = void
type !2 = i1
type !3 = i8
type !4 = i16
type !5 = i32
type !6 = i64
type !7 = ptr(addrspace=0, !6)
global @1 "gane.count": !6 = 1
global @2 "gane.ready": !2 = zero
global @3 "gane.none": !7 = zero
func @1 "gane.tick"() -> () [no_return=false] entry ^1 {
  ^1():
    %1 = global_addr @1
    %2 = load %1
    %3 = const !6 1
    %4 = add %2, %3
    store %1, %4
    return
}
func @2 "gane.main"() -> () [no_return=false] entry ^1 {
  ^1():
    %1 = global_addr @1
    %2 = global_addr @1
    %3 = load %2
    %4 = const !6 1
    %5 = add %3, %4
    store %1, %5
    call @1()
    %6 = global_addr @1
    %7 = load %6
    return
}
entry @2
"#
    );
}

#[test]
fn lowers_global_aggregate_places_copy_and_zero() {
    let package = lower(
        "package main\n\
         type Pair struct { value int; values [2]int }\n\
         var source Pair\n\
         var destination Pair\n\
         func update(index int) {\n\
             destination.value = source.values[index]\n\
             destination = source\n\
             source = Pair{}\n\
         }\n\
         func main() { update(0) }\n",
        TargetSpec::for_test_64(),
    )
    .unwrap();
    verify(&package).unwrap();

    let update = package.function(crate::FunctionId::from_raw(1)).unwrap();
    let instructions = update
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();
    assert_eq!(package.globals().count(), 2);
    assert!(
        instructions
            .iter()
            .any(|instruction| matches!(instruction.kind, InstructionKind::GepField { .. }))
    );
    assert!(
        instructions
            .iter()
            .any(|instruction| matches!(instruction.kind, InstructionKind::GepIndex { .. }))
    );
    assert!(
        instructions
            .iter()
            .any(|instruction| matches!(instruction.kind, InstructionKind::AggregateCopy { .. }))
    );
    assert!(
        instructions
            .iter()
            .any(|instruction| matches!(instruction.kind, InstructionKind::AggregateZero { .. }))
    );
    assert!(instructions.iter().any(|instruction| matches!(
        instruction.kind,
        InstructionKind::GlobalAddr { global }
            if global == crate::GlobalId::from_raw(1) || global == crate::GlobalId::from_raw(2)
    )));
    assert!(!update.blocks.iter().any(|block| matches!(
        block.terminator,
        Terminator::Trap {
            reason: TrapReason::NullDereference
        }
    )));
}
