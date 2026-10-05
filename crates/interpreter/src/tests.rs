// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Hazuki Keatsu

use crate::*;
use gane_ir::{
    BinaryOp, BlockId, Callee, ComparePredicate, Constant, FunctionAttributes, FunctionId,
    GlobalInitializer, InstructionKind, IntCastKind, IrBuilder, IrGlobal, IrParameter, IrSignature,
    IrTypeKind, TargetSpec, Terminator, TrapReason, TypeId, ValueId, VerifiedIrPackage,
    verify_package,
};

fn signature(parameters: Vec<IrParameter>, results: Vec<TypeId>) -> IrSignature {
    IrSignature {
        parameters,
        results,
    }
}

fn main_function(builder: &mut IrBuilder) -> (FunctionId, BlockId) {
    let main = builder.declare_function(
        "gane.main".into(),
        signature(Vec::new(), Vec::new()),
        FunctionAttributes::default(),
    );
    builder.set_entry(main).unwrap();
    (main, builder.entry_block(main).unwrap())
}

fn verified(builder: IrBuilder) -> VerifiedIrPackage {
    verify_package(builder.finish().unwrap()).unwrap()
}

fn trap(reason: TrapReason) -> InterpreterError {
    InterpreterError::Trap {
        reason,
        trace: None,
    }
}

fn constant(
    builder: &mut IrBuilder,
    function: FunctionId,
    block: BlockId,
    typ: TypeId,
    value: Constant,
) -> ValueId {
    builder
        .append_instruction(
            function,
            block,
            InstructionKind::Const { value, typ },
            [typ],
            None,
        )
        .unwrap()[0]
}

fn compare(
    builder: &mut IrBuilder,
    function: FunctionId,
    block: BlockId,
    predicate: ComparePredicate,
    left: ValueId,
    right: ValueId,
) -> ValueId {
    let i1 = builder.types().i1();
    builder
        .append_instruction(
            function,
            block,
            InstructionKind::Compare {
                predicate,
                left,
                right,
            },
            [i1],
            None,
        )
        .unwrap()[0]
}

fn require(
    builder: &mut IrBuilder,
    function: FunctionId,
    block: BlockId,
    condition: ValueId,
) -> BlockId {
    let success = builder.create_block(function).unwrap();
    let failure = builder.create_block(function).unwrap();
    builder
        .set_terminator(
            function,
            block,
            Terminator::CondBranch {
                condition,
                then_target: success,
                then_arguments: Vec::new(),
                else_target: failure,
                else_arguments: Vec::new(),
            },
        )
        .unwrap();
    builder
        .set_terminator(
            function,
            failure,
            Terminator::Trap {
                reason: TrapReason::ExplicitPanic,
            },
        )
        .unwrap();
    success
}

fn require_compare(
    builder: &mut IrBuilder,
    function: FunctionId,
    block: BlockId,
    predicate: ComparePredicate,
    left: ValueId,
    right: ValueId,
) -> BlockId {
    let condition = compare(builder, function, block, predicate, left, right);
    require(builder, function, block, condition)
}

#[test]
fn interprets_wrapping_integer_operations_casts_and_shifts() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i8 = builder.types().i8();
    let i64 = builder.types().i64();
    let (main, mut block) = main_function(&mut builder);

    let max = constant(&mut builder, main, block, i8, Constant::Integer(255));
    let one = constant(&mut builder, main, block, i8, Constant::Integer(1));
    let wrapped = builder
        .append_instruction(
            main,
            block,
            InstructionKind::Binary {
                op: BinaryOp::Add,
                left: max,
                right: one,
            },
            [i8],
            None,
        )
        .unwrap()[0];
    let zero = constant(&mut builder, main, block, i8, Constant::Integer(0));
    block = require_compare(
        &mut builder,
        main,
        block,
        ComparePredicate::Equal,
        wrapped,
        zero,
    );

    let negative_one = constant(&mut builder, main, block, i8, Constant::Integer(255));
    let zero = constant(&mut builder, main, block, i8, Constant::Integer(0));
    let signed_less = compare(
        &mut builder,
        main,
        block,
        ComparePredicate::SignedLess,
        negative_one,
        zero,
    );
    block = require(&mut builder, main, block, signed_less);
    let unsigned_greater = compare(
        &mut builder,
        main,
        block,
        ComparePredicate::UnsignedGreater,
        negative_one,
        zero,
    );
    block = require(&mut builder, main, block, unsigned_greater);

    let sign_extended = builder
        .append_instruction(
            main,
            block,
            InstructionKind::IntCast {
                kind: IntCastKind::SignExtend,
                operand: negative_one,
                target: i64,
            },
            [i64],
            None,
        )
        .unwrap()[0];
    let zero64 = constant(&mut builder, main, block, i64, Constant::Integer(0));
    let is_negative = compare(
        &mut builder,
        main,
        block,
        ComparePredicate::SignedLess,
        sign_extended,
        zero64,
    );
    block = require(&mut builder, main, block, is_negative);
    let zero_extended = builder
        .append_instruction(
            main,
            block,
            InstructionKind::IntCast {
                kind: IntCastKind::ZeroExtend,
                operand: negative_one,
                target: i64,
            },
            [i64],
            None,
        )
        .unwrap()[0];
    let two_fifty_five = constant(&mut builder, main, block, i64, Constant::Integer(255));
    block = require_compare(
        &mut builder,
        main,
        block,
        ComparePredicate::Equal,
        zero_extended,
        two_fifty_five,
    );

    let minimum = constant(
        &mut builder,
        main,
        block,
        i64,
        Constant::Integer(i64::MIN as u64),
    );
    let negative_one = constant(&mut builder, main, block, i64, Constant::Integer(u64::MAX));
    let division = builder
        .append_instruction(
            main,
            block,
            InstructionKind::Binary {
                op: BinaryOp::SignedDiv,
                left: minimum,
                right: negative_one,
            },
            [i64],
            None,
        )
        .unwrap()[0];
    block = require_compare(
        &mut builder,
        main,
        block,
        ComparePredicate::Equal,
        division,
        minimum,
    );
    let remainder = builder
        .append_instruction(
            main,
            block,
            InstructionKind::Binary {
                op: BinaryOp::SignedRem,
                left: minimum,
                right: negative_one,
            },
            [i64],
            None,
        )
        .unwrap()[0];
    let zero64 = constant(&mut builder, main, block, i64, Constant::Integer(0));
    block = require_compare(
        &mut builder,
        main,
        block,
        ComparePredicate::Equal,
        remainder,
        zero64,
    );

    let one8 = constant(&mut builder, main, block, i8, Constant::Integer(1));
    let negative8 = constant(&mut builder, main, block, i8, Constant::Integer(128));
    let wide = constant(&mut builder, main, block, i64, Constant::Integer(8));
    let shifted_left = builder
        .append_instruction(
            main,
            block,
            InstructionKind::Binary {
                op: BinaryOp::Shl,
                left: one8,
                right: wide,
            },
            [i8],
            None,
        )
        .unwrap()[0];
    let logical_right = builder
        .append_instruction(
            main,
            block,
            InstructionKind::Binary {
                op: BinaryOp::LogicalShr,
                left: one8,
                right: wide,
            },
            [i8],
            None,
        )
        .unwrap()[0];
    let arithmetic_right = builder
        .append_instruction(
            main,
            block,
            InstructionKind::Binary {
                op: BinaryOp::ArithmeticShr,
                left: negative8,
                right: wide,
            },
            [i8],
            None,
        )
        .unwrap()[0];
    let zero = constant(&mut builder, main, block, i8, Constant::Integer(0));
    block = require_compare(
        &mut builder,
        main,
        block,
        ComparePredicate::Equal,
        shifted_left,
        zero,
    );
    block = require_compare(
        &mut builder,
        main,
        block,
        ComparePredicate::Equal,
        logical_right,
        zero,
    );
    let all_ones = constant(&mut builder, main, block, i8, Constant::Integer(255));
    block = require_compare(
        &mut builder,
        main,
        block,
        ComparePredicate::Equal,
        arithmetic_right,
        all_ones,
    );
    builder
        .set_terminator(main, block, Terminator::Return { values: Vec::new() })
        .unwrap();

    assert_eq!(interpret(&verified(builder)), Ok(()));
}

#[test]
fn interprets_calls_loops_block_parameters_and_zeroed_slots() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = builder.add_type(IrTypeKind::Ptr {
        pointee: i64,
        address_space: 0,
    });
    let increment = builder.declare_function(
        "gane.increment".into(),
        signature(vec![IrParameter { typ: i64 }], vec![i64]),
        FunctionAttributes::default(),
    );
    let increment_entry = builder.entry_block(increment).unwrap();
    let parameter = builder.entry_parameters(increment).unwrap()[0];
    let one = constant(
        &mut builder,
        increment,
        increment_entry,
        i64,
        Constant::Integer(1),
    );
    let result = builder
        .append_instruction(
            increment,
            increment_entry,
            InstructionKind::Binary {
                op: BinaryOp::Add,
                left: parameter,
                right: one,
            },
            [i64],
            None,
        )
        .unwrap()[0];
    builder
        .set_terminator(
            increment,
            increment_entry,
            Terminator::Return {
                values: vec![result],
            },
        )
        .unwrap();

    let (main, entry) = main_function(&mut builder);
    let header = builder.create_block(main).unwrap();
    let counter = builder
        .append_block_parameter(main, header, i64, None)
        .unwrap();
    let body = builder.create_block(main).unwrap();
    let exit = builder.create_block(main).unwrap();
    let slot = builder.add_stack_slot(main, i64, None, None).unwrap();
    let address = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr { slot },
            [pointer],
            None,
        )
        .unwrap()[0];
    let initial = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Load { pointer: address },
            [i64],
            None,
        )
        .unwrap()[0];
    let zero = constant(&mut builder, main, entry, i64, Constant::Integer(0));
    let mut block = require_compare(
        &mut builder,
        main,
        entry,
        ComparePredicate::Equal,
        initial,
        zero,
    );
    builder
        .set_terminator(
            main,
            block,
            Terminator::Branch {
                target: header,
                arguments: vec![zero],
            },
        )
        .unwrap();

    let limit = constant(&mut builder, main, header, i64, Constant::Integer(3));
    let before_limit = compare(
        &mut builder,
        main,
        header,
        ComparePredicate::SignedLess,
        counter,
        limit,
    );
    builder
        .set_terminator(
            main,
            header,
            Terminator::CondBranch {
                condition: before_limit,
                then_target: body,
                then_arguments: Vec::new(),
                else_target: exit,
                else_arguments: Vec::new(),
            },
        )
        .unwrap();
    let next = builder
        .append_instruction(
            main,
            body,
            InstructionKind::Call {
                callee: Callee::Function(increment),
                arguments: vec![counter],
            },
            [i64],
            None,
        )
        .unwrap()[0];
    builder
        .set_terminator(
            main,
            body,
            Terminator::Branch {
                target: header,
                arguments: vec![next],
            },
        )
        .unwrap();
    let expected = constant(&mut builder, main, exit, i64, Constant::Integer(3));
    let reached_limit = compare(
        &mut builder,
        main,
        exit,
        ComparePredicate::Equal,
        counter,
        expected,
    );
    block = require(&mut builder, main, exit, reached_limit);
    builder
        .set_terminator(main, block, Terminator::Return { values: Vec::new() })
        .unwrap();

    assert_eq!(interpret(&verified(builder)), Ok(()));
}

#[test]
fn interprets_globals_structured_memory_zero_and_copy() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let array = builder.add_type(IrTypeKind::Array {
        length: 2,
        element: i64,
    });
    let record = builder.add_type(IrTypeKind::Struct {
        fields: vec![i64, array],
    });
    let integer_pointer = builder.add_type(IrTypeKind::Ptr {
        pointee: i64,
        address_space: 0,
    });
    let array_pointer = builder.add_type(IrTypeKind::Ptr {
        pointee: array,
        address_space: 0,
    });
    let record_pointer = builder.add_type(IrTypeKind::Ptr {
        pointee: record,
        address_space: 0,
    });
    let global = builder.add_global(IrGlobal {
        symbol: "gane.counter".into(),
        typ: i64,
        initializer: GlobalInitializer::Scalar(Constant::Integer(4)),
    });
    let (main, mut block) = main_function(&mut builder);
    let first_slot = builder.add_stack_slot(main, record, None, None).unwrap();
    let second_slot = builder.add_stack_slot(main, record, None, None).unwrap();
    let first = builder
        .append_instruction(
            main,
            block,
            InstructionKind::StackAddr { slot: first_slot },
            [record_pointer],
            None,
        )
        .unwrap()[0];
    let second = builder
        .append_instruction(
            main,
            block,
            InstructionKind::StackAddr { slot: second_slot },
            [record_pointer],
            None,
        )
        .unwrap()[0];
    let distinct = compare(
        &mut builder,
        main,
        block,
        ComparePredicate::NotEqual,
        first,
        second,
    );
    block = require(&mut builder, main, block, distinct);
    let address = builder
        .append_instruction(
            main,
            block,
            InstructionKind::GlobalAddr { global },
            [integer_pointer],
            None,
        )
        .unwrap()[0];
    let global_value = builder
        .append_instruction(
            main,
            block,
            InstructionKind::Load { pointer: address },
            [i64],
            None,
        )
        .unwrap()[0];
    let four = constant(&mut builder, main, block, i64, Constant::Integer(4));
    block = require_compare(
        &mut builder,
        main,
        block,
        ComparePredicate::Equal,
        global_value,
        four,
    );
    let seven = constant(&mut builder, main, block, i64, Constant::Integer(7));
    builder
        .append_instruction(
            main,
            block,
            InstructionKind::Store {
                pointer: address,
                value: seven,
            },
            [],
            None,
        )
        .unwrap();
    let global_value = builder
        .append_instruction(
            main,
            block,
            InstructionKind::Load { pointer: address },
            [i64],
            None,
        )
        .unwrap()[0];
    block = require_compare(
        &mut builder,
        main,
        block,
        ComparePredicate::Equal,
        global_value,
        seven,
    );

    builder
        .append_instruction(
            main,
            block,
            InstructionKind::AggregateZero {
                destination: first,
                typ: record,
            },
            [],
            None,
        )
        .unwrap();
    let first_field = builder
        .append_instruction(
            main,
            block,
            InstructionKind::GepField {
                base: first,
                field: 0,
            },
            [integer_pointer],
            None,
        )
        .unwrap()[0];
    let initial = builder
        .append_instruction(
            main,
            block,
            InstructionKind::Load {
                pointer: first_field,
            },
            [i64],
            None,
        )
        .unwrap()[0];
    let zero = constant(&mut builder, main, block, i64, Constant::Integer(0));
    block = require_compare(
        &mut builder,
        main,
        block,
        ComparePredicate::Equal,
        initial,
        zero,
    );
    let first_array = builder
        .append_instruction(
            main,
            block,
            InstructionKind::GepField {
                base: first,
                field: 1,
            },
            [array_pointer],
            None,
        )
        .unwrap()[0];
    let index_zero = constant(&mut builder, main, block, i64, Constant::Integer(0));
    let index_one = constant(&mut builder, main, block, i64, Constant::Integer(1));
    let first_element = builder
        .append_instruction(
            main,
            block,
            InstructionKind::GepIndex {
                base: first_array,
                index: index_zero,
            },
            [integer_pointer],
            None,
        )
        .unwrap()[0];
    let second_element = builder
        .append_instruction(
            main,
            block,
            InstructionKind::GepIndex {
                base: first_array,
                index: index_one,
            },
            [integer_pointer],
            None,
        )
        .unwrap()[0];
    let one = constant(&mut builder, main, block, i64, Constant::Integer(1));
    let two = constant(&mut builder, main, block, i64, Constant::Integer(2));
    let three = constant(&mut builder, main, block, i64, Constant::Integer(3));
    for (pointer, value) in [
        (first_field, one),
        (first_element, two),
        (second_element, three),
    ] {
        builder
            .append_instruction(
                main,
                block,
                InstructionKind::Store { pointer, value },
                [],
                None,
            )
            .unwrap();
    }
    builder
        .append_instruction(
            main,
            block,
            InstructionKind::AggregateCopy {
                destination: second,
                source: first,
                typ: record,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .append_instruction(
            main,
            block,
            InstructionKind::AggregateCopy {
                destination: second,
                source: second,
                typ: record,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .append_instruction(
            main,
            block,
            InstructionKind::AggregateZero {
                destination: first,
                typ: record,
            },
            [],
            None,
        )
        .unwrap();
    let reset = builder
        .append_instruction(
            main,
            block,
            InstructionKind::Load {
                pointer: first_field,
            },
            [i64],
            None,
        )
        .unwrap()[0];
    block = require_compare(
        &mut builder,
        main,
        block,
        ComparePredicate::Equal,
        reset,
        zero,
    );
    let second_field = builder
        .append_instruction(
            main,
            block,
            InstructionKind::GepField {
                base: second,
                field: 0,
            },
            [integer_pointer],
            None,
        )
        .unwrap()[0];
    let second_array = builder
        .append_instruction(
            main,
            block,
            InstructionKind::GepField {
                base: second,
                field: 1,
            },
            [array_pointer],
            None,
        )
        .unwrap()[0];
    let copied_first = builder
        .append_instruction(
            main,
            block,
            InstructionKind::GepIndex {
                base: second_array,
                index: index_zero,
            },
            [integer_pointer],
            None,
        )
        .unwrap()[0];
    let copied_second = builder
        .append_instruction(
            main,
            block,
            InstructionKind::GepIndex {
                base: second_array,
                index: index_one,
            },
            [integer_pointer],
            None,
        )
        .unwrap()[0];
    for (pointer, value) in [
        (second_field, one),
        (copied_first, two),
        (copied_second, three),
    ] {
        let loaded = builder
            .append_instruction(main, block, InstructionKind::Load { pointer }, [i64], None)
            .unwrap()[0];
        block = require_compare(
            &mut builder,
            main,
            block,
            ComparePredicate::Equal,
            loaded,
            value,
        );
    }
    builder
        .set_terminator(main, block, Terminator::Return { values: Vec::new() })
        .unwrap();

    assert_eq!(interpret(&verified(builder)), Ok(()));
}

fn binary_error(op: BinaryOp, left: u64, right: u64) -> InterpreterError {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let (main, block) = main_function(&mut builder);
    let left = constant(&mut builder, main, block, i64, Constant::Integer(left));
    let right = constant(&mut builder, main, block, i64, Constant::Integer(right));
    builder
        .append_instruction(
            main,
            block,
            InstructionKind::Binary { op, left, right },
            [i64],
            None,
        )
        .unwrap();
    builder
        .set_terminator(main, block, Terminator::Return { values: Vec::new() })
        .unwrap();
    interpret(&verified(builder)).unwrap_err()
}

#[test]
fn reports_total_operation_and_terminator_traps() {
    assert_eq!(
        binary_error(BinaryOp::SignedDiv, 1, 0),
        trap(TrapReason::DivisionByZero)
    );
    assert_eq!(
        binary_error(BinaryOp::Shl, 1, u64::MAX),
        trap(TrapReason::NegativeShift)
    );

    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let array = builder.add_type(IrTypeKind::Array {
        length: 1,
        element: i64,
    });
    let array_pointer = builder.add_type(IrTypeKind::Ptr {
        pointee: array,
        address_space: 0,
    });
    let integer_pointer = builder.add_type(IrTypeKind::Ptr {
        pointee: i64,
        address_space: 0,
    });
    let (main, block) = main_function(&mut builder);
    let slot = builder.add_stack_slot(main, array, None, None).unwrap();
    let array = builder
        .append_instruction(
            main,
            block,
            InstructionKind::StackAddr { slot },
            [array_pointer],
            None,
        )
        .unwrap()[0];
    let out_of_bounds = constant(&mut builder, main, block, i64, Constant::Integer(1));
    builder
        .append_instruction(
            main,
            block,
            InstructionKind::GepIndex {
                base: array,
                index: out_of_bounds,
            },
            [integer_pointer],
            None,
        )
        .unwrap();
    builder
        .set_terminator(main, block, Terminator::Return { values: Vec::new() })
        .unwrap();
    assert_eq!(
        interpret(&verified(builder)),
        Err(trap(TrapReason::BoundsError))
    );

    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = builder.add_type(IrTypeKind::Ptr {
        pointee: i64,
        address_space: 0,
    });
    let (main, block) = main_function(&mut builder);
    let null = constant(&mut builder, main, block, pointer, Constant::Null);
    builder
        .append_instruction(
            main,
            block,
            InstructionKind::Load { pointer: null },
            [i64],
            None,
        )
        .unwrap();
    builder
        .set_terminator(main, block, Terminator::Return { values: Vec::new() })
        .unwrap();
    assert_eq!(
        interpret(&verified(builder)),
        Err(trap(TrapReason::NullDereference))
    );

    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let (main, block) = main_function(&mut builder);
    builder
        .set_terminator(
            main,
            block,
            Terminator::Trap {
                reason: TrapReason::ExplicitPanic,
            },
        )
        .unwrap();
    assert_eq!(
        interpret(&verified(builder)),
        Err(trap(TrapReason::ExplicitPanic))
    );

    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let (main, block) = main_function(&mut builder);
    builder
        .set_terminator(main, block, Terminator::Unreachable)
        .unwrap();
    let package = verified(builder);
    assert_eq!(
        interpret(&package),
        Err(InterpreterError::ReachedUnreachable { trace: None })
    );
    let error = interpret_with_stack_details(&package).unwrap_err();
    let InterpreterError::ReachedUnreachable { trace: Some(trace) } = error else {
        panic!("expected unreachable with stack details");
    };
    assert!(trace.contains("#0 gane.main"));
}

#[test]
fn traps_when_a_reused_stack_frame_dereferences_a_dangling_pointer() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = builder.add_type(IrTypeKind::Ptr {
        pointee: i64,
        address_space: 0,
    });

    let producer = builder.declare_function(
        "gane.producer".into(),
        signature(Vec::new(), vec![pointer]),
        FunctionAttributes::default(),
    );
    let producer_entry = builder.entry_block(producer).unwrap();
    let producer_slot = builder.add_stack_slot(producer, i64, None, None).unwrap();
    let local = builder
        .append_instruction(
            producer,
            producer_entry,
            InstructionKind::StackAddr {
                slot: producer_slot,
            },
            [pointer],
            None,
        )
        .unwrap()[0];
    builder
        .set_terminator(
            producer,
            producer_entry,
            Terminator::Return {
                values: vec![local],
            },
        )
        .unwrap();

    let consumer = builder.declare_function(
        "gane.consumer".into(),
        signature(vec![IrParameter { typ: pointer }], Vec::new()),
        FunctionAttributes::default(),
    );
    let consumer_entry = builder.entry_block(consumer).unwrap();
    let parameter = builder.entry_parameters(consumer).unwrap()[0];
    builder.add_stack_slot(consumer, i64, None, None).unwrap();
    builder
        .append_instruction(
            consumer,
            consumer_entry,
            InstructionKind::Load { pointer: parameter },
            [i64],
            None,
        )
        .unwrap();
    builder
        .set_terminator(
            consumer,
            consumer_entry,
            Terminator::Return { values: Vec::new() },
        )
        .unwrap();

    let (main, entry) = main_function(&mut builder);
    let escaped = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Call {
                callee: Callee::Function(producer),
                arguments: Vec::new(),
            },
            [pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Call {
                callee: Callee::Function(consumer),
                arguments: vec![escaped],
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(main, entry, Terminator::Return { values: Vec::new() })
        .unwrap();

    assert_eq!(
        interpret(&verified(builder)),
        Err(InterpreterError::DanglingStackPointer { trace: None })
    );
}

#[test]
fn reports_nested_stack_details_when_a_callee_traps() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let callee = builder.declare_function(
        "gane.fail".into(),
        signature(Vec::new(), Vec::new()),
        FunctionAttributes::default(),
    );
    let callee_entry = builder.entry_block(callee).unwrap();
    let callee_body = builder.create_block(callee).unwrap();
    builder
        .set_terminator(
            callee,
            callee_entry,
            Terminator::Branch {
                target: callee_body,
                arguments: Vec::new(),
            },
        )
        .unwrap();
    let one = constant(&mut builder, callee, callee_body, i64, Constant::Integer(1));
    let zero = constant(&mut builder, callee, callee_body, i64, Constant::Integer(0));
    builder
        .append_instruction(
            callee,
            callee_body,
            InstructionKind::Binary {
                op: BinaryOp::SignedDiv,
                left: one,
                right: zero,
            },
            [i64],
            None,
        )
        .unwrap();
    builder
        .set_terminator(
            callee,
            callee_body,
            Terminator::Return { values: Vec::new() },
        )
        .unwrap();

    let (main, main_entry) = main_function(&mut builder);
    builder
        .append_instruction(
            main,
            main_entry,
            InstructionKind::Call {
                callee: Callee::Function(callee),
                arguments: Vec::new(),
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(main, main_entry, Terminator::Return { values: Vec::new() })
        .unwrap();

    let error = interpret_with_stack_details(&verified(builder)).unwrap_err();
    let rendered = error.to_string();
    assert!(rendered.starts_with("IR trapped: DivisionByZero\n#0 gane.fail"));
    assert!(!rendered.ends_with('\n'));
    let InterpreterError::Trap {
        reason,
        trace: Some(trace),
    } = error
    else {
        panic!("expected a trap with stack details");
    };
    assert_eq!(reason, TrapReason::DivisionByZero);
    assert!(trace.contains(&format!("#0 gane.fail bb{}:2", callee_body.raw())));
    assert!(trace.contains("#1 gane.main"));
    assert!(trace.contains("  values:"));
    assert!(trace.contains("  slots:"));
}
