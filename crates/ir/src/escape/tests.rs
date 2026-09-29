use super::summaries;
use crate::{
    Callee, Constant, FunctionAttributes, GlobalInitializer, InstructionKind, IrBuilder,
    IrGlobal, IrParameter, IrSignature, IrTypeKind, TargetSpec, Terminator, TypeId,
    UnverifiedIrPackage, verify, verify_and_check_escape,
};

fn signature(parameters: Vec<IrParameter>, results: Vec<TypeId>) -> IrSignature {
    IrSignature {
        parameters,
        results,
    }
}

fn pointer_type(builder: &mut IrBuilder, pointee: TypeId) -> TypeId {
    builder.add_type(IrTypeKind::Ptr {
        pointee,
        address_space: 0,
    })
}

fn add_empty_main(builder: &mut IrBuilder) {
    let main = builder.declare_function(
        "gane.main".into(),
        signature(vec![], vec![]),
        FunctionAttributes::default(),
    );
    builder.set_entry(main).unwrap();
    let entry = builder.entry_block(main).unwrap();
    builder
        .set_terminator(main, entry, Terminator::Return { values: vec![] })
        .unwrap();
}

fn empty_main() -> UnverifiedIrPackage {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i32 = builder.types().i32();
    let pointer = builder.add_type(IrTypeKind::Ptr {
        pointee: i32,
        address_space: 0,
    });
    let pointer_pointer = pointer_type(&mut builder, pointer);
    let main = builder.declare_function(
        "gane.main".into(),
        signature(vec![], vec![]),
        FunctionAttributes::default(),
    );
    builder.set_entry(main).unwrap();
    let entry = builder.entry_block(main).unwrap();
    let value_slot = builder.add_stack_slot(main, i32, None, None).unwrap();
    let pointer_slot = builder.add_stack_slot(main, pointer, None, None).unwrap();
    let value = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr { slot: value_slot },
            [pointer],
            None,
        )
        .unwrap()[0];
    let destination = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr { slot: pointer_slot },
            [pointer_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Store {
                pointer: destination,
                value,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Load {
                pointer: destination,
            },
            [pointer],
            None,
        )
        .unwrap();
    builder
        .set_terminator(main, entry, Terminator::Return { values: vec![] })
        .unwrap();
    builder.finish().unwrap()
}

fn escape_messages(package: UnverifiedIrPackage) -> String {
    verify(&package).unwrap();
    verify_and_check_escape(package)
        .unwrap_err()
        .into_iter()
        .map(|diagnostic| diagnostic.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn accepts_non_escaping_stack_addresses() {
    assert!(verify_and_check_escape(empty_main()).is_ok());
}

#[test]
fn rejects_returning_stack_address_through_gep() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i32 = builder.types().i32();
    let i64 = builder.types().i64();
    let array = builder.add_type(IrTypeKind::Array {
        length: 1,
        element: i32,
    });
    let array_pointer = builder.add_type(IrTypeKind::Ptr {
        pointee: array,
        address_space: 0,
    });
    let pointer = builder.add_type(IrTypeKind::Ptr {
        pointee: i32,
        address_space: 0,
    });
    let escape = builder.declare_function(
        "gane.escape".into(),
        signature(vec![], vec![pointer]),
        FunctionAttributes::default(),
    );
    let escape_entry = builder.entry_block(escape).unwrap();
    let slot = builder.add_stack_slot(escape, array, None, None).unwrap();
    let address = builder
        .append_instruction(
            escape,
            escape_entry,
            InstructionKind::StackAddr { slot },
            [array_pointer],
            None,
        )
        .unwrap()[0];
    let index = builder
        .append_instruction(
            escape,
            escape_entry,
            InstructionKind::Const {
                value: Constant::Integer(0),
                typ: i64,
            },
            [i64],
            None,
        )
        .unwrap()[0];
    let element = builder
        .append_instruction(
            escape,
            escape_entry,
            InstructionKind::GepIndex {
                base: address,
                index,
            },
            [pointer],
            None,
        )
        .unwrap()[0];
    builder
        .set_terminator(
            escape,
            escape_entry,
            Terminator::Return {
                values: vec![element],
            },
        )
        .unwrap();

    let main = builder.declare_function(
        "gane.main".into(),
        signature(vec![], vec![]),
        FunctionAttributes::default(),
    );
    builder.set_entry(main).unwrap();
    let main_entry = builder.entry_block(main).unwrap();
    builder
        .set_terminator(main, main_entry, Terminator::Return { values: vec![] })
        .unwrap();

    assert!(escape_messages(builder.finish().unwrap()).contains("returned from function"));
}

#[test]
fn rejects_stack_pointer_roundtrip_through_slot() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i32 = builder.types().i32();
    let pointer = builder.add_type(IrTypeKind::Ptr {
        pointee: i32,
        address_space: 0,
    });
    let pointer_pointer = builder.add_type(IrTypeKind::Ptr {
        pointee: pointer,
        address_space: 0,
    });
    let escape = builder.declare_function(
        "gane.escape".into(),
        signature(vec![], vec![pointer]),
        FunctionAttributes::default(),
    );
    let escape_entry = builder.entry_block(escape).unwrap();
    let value_slot = builder.add_stack_slot(escape, i32, None, None).unwrap();
    let pointer_slot = builder.add_stack_slot(escape, pointer, None, None).unwrap();
    let value = builder
        .append_instruction(
            escape,
            escape_entry,
            InstructionKind::StackAddr { slot: value_slot },
            [pointer],
            None,
        )
        .unwrap()[0];
    let destination = builder
        .append_instruction(
            escape,
            escape_entry,
            InstructionKind::StackAddr { slot: pointer_slot },
            [pointer_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            escape,
            escape_entry,
            InstructionKind::Store {
                pointer: destination,
                value,
            },
            [],
            None,
        )
        .unwrap();
    let loaded = builder
        .append_instruction(
            escape,
            escape_entry,
            InstructionKind::Load {
                pointer: destination,
            },
            [pointer],
            None,
        )
        .unwrap()[0];
    builder
        .set_terminator(
            escape,
            escape_entry,
            Terminator::Return {
                values: vec![loaded],
            },
        )
        .unwrap();

    let main = builder.declare_function(
        "gane.main".into(),
        signature(vec![], vec![]),
        FunctionAttributes::default(),
    );
    builder.set_entry(main).unwrap();
    let main_entry = builder.entry_block(main).unwrap();
    builder
        .set_terminator(main, main_entry, Terminator::Return { values: vec![] })
        .unwrap();

    assert!(escape_messages(builder.finish().unwrap()).contains("returned from function"));
}

#[test]
fn rejects_storing_stack_pointer_in_global() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i32 = builder.types().i32();
    let pointer = builder.add_type(IrTypeKind::Ptr {
        pointee: i32,
        address_space: 0,
    });
    let pointer_pointer = builder.add_type(IrTypeKind::Ptr {
        pointee: pointer,
        address_space: 0,
    });
    let global = builder.add_global(IrGlobal {
        symbol: "gane.global".into(),
        typ: pointer,
        initializer: GlobalInitializer::Zero,
    });
    let main = builder.declare_function(
        "gane.main".into(),
        signature(vec![], vec![]),
        FunctionAttributes::default(),
    );
    builder.set_entry(main).unwrap();
    let entry = builder.entry_block(main).unwrap();
    let slot = builder.add_stack_slot(main, i32, None, None).unwrap();
    let value = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr { slot },
            [pointer],
            None,
        )
        .unwrap()[0];
    let destination = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::GlobalAddr { global },
            [pointer_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Store {
                pointer: destination,
                value,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(main, entry, Terminator::Return { values: vec![] })
        .unwrap();

    assert!(escape_messages(builder.finish().unwrap()).contains("stored in global"));
}

#[test]
fn rejects_stack_pointer_passed_to_borrowed_identity_when_result_is_discarded() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i32 = builder.types().i32();
    let pointer = builder.add_type(IrTypeKind::Ptr {
        pointee: i32,
        address_space: 0,
    });
    let callee = builder.declare_function(
        "gane.callee".into(),
        signature(vec![IrParameter { typ: pointer }], vec![pointer]),
        FunctionAttributes::default(),
    );
    let callee_entry = builder.entry_block(callee).unwrap();
    let parameter = builder.entry_parameters(callee).unwrap()[0];
    builder
        .set_terminator(
            callee,
            callee_entry,
            Terminator::Return {
                values: vec![parameter],
            },
        )
        .unwrap();

    let main = builder.declare_function(
        "gane.main".into(),
        signature(vec![], vec![]),
        FunctionAttributes::default(),
    );
    builder.set_entry(main).unwrap();
    let entry = builder.entry_block(main).unwrap();
    let slot = builder.add_stack_slot(main, i32, None, None).unwrap();
    let value = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr { slot },
            [pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Call {
                callee: Callee::Function(callee),
                arguments: vec![value],
            },
            [pointer],
            None,
        )
        .unwrap();
    builder
        .set_terminator(main, entry, Terminator::Return { values: vec![] })
        .unwrap();

    assert!(escape_messages(builder.finish().unwrap()).contains("escaping parameter"));
}

#[test]
fn rejects_pointer_aggregate_copy_through_block_parameter() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = pointer_type(&mut builder, i64);
    let pointer_pointer = pointer_type(&mut builder, pointer);
    let aggregate = builder.add_type(IrTypeKind::Struct {
        fields: vec![pointer],
    });
    let aggregate_pointer = pointer_type(&mut builder, aggregate);
    let global = builder.add_global(IrGlobal {
        symbol: "gane.saved".into(),
        typ: aggregate,
        initializer: GlobalInitializer::Zero,
    });
    let main = builder.declare_function(
        "gane.main".into(),
        signature(vec![], vec![]),
        FunctionAttributes::default(),
    );
    builder.set_entry(main).unwrap();
    let entry = builder.entry_block(main).unwrap();
    let copy = builder.create_block(main).unwrap();
    let source = builder
        .append_block_parameter(main, copy, aggregate_pointer, None)
        .unwrap();
    let value_slot = builder.add_stack_slot(main, i64, None, None).unwrap();
    let aggregate_slot = builder.add_stack_slot(main, aggregate, None, None).unwrap();
    let value = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr { slot: value_slot },
            [pointer],
            None,
        )
        .unwrap()[0];
    let aggregate_address = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr {
                slot: aggregate_slot,
            },
            [aggregate_pointer],
            None,
        )
        .unwrap()[0];
    let field = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::GepField {
                base: aggregate_address,
                field: 0,
            },
            [pointer_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Store {
                pointer: field,
                value,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(
            main,
            entry,
            Terminator::Branch {
                target: copy,
                arguments: vec![aggregate_address],
            },
        )
        .unwrap();
    let destination = builder
        .append_instruction(
            main,
            copy,
            InstructionKind::GlobalAddr { global },
            [aggregate_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            copy,
            InstructionKind::AggregateCopy {
                destination,
                source,
                typ: aggregate,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(main, copy, Terminator::Return { values: vec![] })
        .unwrap();

    assert!(escape_messages(builder.finish().unwrap()).contains("copied into global"));
}

#[test]
fn rejects_pointer_aggregate_copy_through_loaded_alias_after_zeroing() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = pointer_type(&mut builder, i64);
    let pointer_pointer = pointer_type(&mut builder, pointer);
    let aggregate = builder.add_type(IrTypeKind::Struct {
        fields: vec![pointer],
    });
    let aggregate_pointer = pointer_type(&mut builder, aggregate);
    let aggregate_pointer_pointer = pointer_type(&mut builder, aggregate_pointer);
    let global = builder.add_global(IrGlobal {
        symbol: "gane.saved".into(),
        typ: aggregate,
        initializer: GlobalInitializer::Zero,
    });
    let main = builder.declare_function(
        "gane.main".into(),
        signature(vec![], vec![]),
        FunctionAttributes::default(),
    );
    builder.set_entry(main).unwrap();
    let entry = builder.entry_block(main).unwrap();
    let value_slot = builder.add_stack_slot(main, i64, None, None).unwrap();
    let aggregate_slot = builder.add_stack_slot(main, aggregate, None, None).unwrap();
    let alias_slot = builder
        .add_stack_slot(main, aggregate_pointer, None, None)
        .unwrap();
    let value = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr { slot: value_slot },
            [pointer],
            None,
        )
        .unwrap()[0];
    let aggregate_address = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr {
                slot: aggregate_slot,
            },
            [aggregate_pointer],
            None,
        )
        .unwrap()[0];
    let field = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::GepField {
                base: aggregate_address,
                field: 0,
            },
            [pointer_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Store {
                pointer: field,
                value,
            },
            [],
            None,
        )
        .unwrap();
    let alias = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr { slot: alias_slot },
            [aggregate_pointer_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Store {
                pointer: alias,
                value: aggregate_address,
            },
            [],
            None,
        )
        .unwrap();
    let source = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Load { pointer: alias },
            [aggregate_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::AggregateZero {
                destination: aggregate_address,
                typ: aggregate,
            },
            [],
            None,
        )
        .unwrap();
    let destination = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::GlobalAddr { global },
            [aggregate_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::AggregateCopy {
                destination,
                source,
                typ: aggregate,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(main, entry, Terminator::Return { values: vec![] })
        .unwrap();

    assert!(escape_messages(builder.finish().unwrap()).contains("copied into global"));
}

#[test]
fn rejects_escape_when_sink_block_precedes_forwarding_block() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = pointer_type(&mut builder, i64);
    let pointer_pointer = pointer_type(&mut builder, pointer);
    let global = builder.add_global(IrGlobal {
        symbol: "gane.saved".into(),
        typ: pointer,
        initializer: GlobalInitializer::Zero,
    });
    let main = builder.declare_function(
        "gane.main".into(),
        signature(vec![], vec![]),
        FunctionAttributes::default(),
    );
    builder.set_entry(main).unwrap();
    let entry = builder.entry_block(main).unwrap();
    let sink = builder.create_block(main).unwrap();
    let sink_value = builder
        .append_block_parameter(main, sink, pointer, None)
        .unwrap();
    let forwarding = builder.create_block(main).unwrap();
    let forwarding_value = builder
        .append_block_parameter(main, forwarding, pointer, None)
        .unwrap();
    let slot = builder.add_stack_slot(main, i64, None, None).unwrap();
    let value = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr { slot },
            [pointer],
            None,
        )
        .unwrap()[0];
    let destination = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::GlobalAddr { global },
            [pointer_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .set_terminator(
            main,
            entry,
            Terminator::Branch {
                target: forwarding,
                arguments: vec![value],
            },
        )
        .unwrap();
    builder
        .append_instruction(
            main,
            sink,
            InstructionKind::Store {
                pointer: destination,
                value: sink_value,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(main, sink, Terminator::Return { values: vec![] })
        .unwrap();
    builder
        .set_terminator(
            main,
            forwarding,
            Terminator::Branch {
                target: sink,
                arguments: vec![forwarding_value],
            },
        )
        .unwrap();

    assert!(escape_messages(builder.finish().unwrap()).contains("stored in global"));
}

#[test]
fn rejects_callee_publishing_pointer_loaded_from_parameter_memory() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = pointer_type(&mut builder, i64);
    let pointer_pointer = pointer_type(&mut builder, pointer);
    let global = builder.add_global(IrGlobal {
        symbol: "gane.saved".into(),
        typ: pointer,
        initializer: GlobalInitializer::Zero,
    });
    let publish = builder.declare_function(
        "gane.publish".into(),
        signature(
            vec![IrParameter {
                typ: pointer_pointer,
            }],
            vec![],
        ),
        FunctionAttributes::default(),
    );
    let publish_entry = builder.entry_block(publish).unwrap();
    let parameter = builder.entry_parameters(publish).unwrap()[0];
    let loaded = builder
        .append_instruction(
            publish,
            publish_entry,
            InstructionKind::Load { pointer: parameter },
            [pointer],
            None,
        )
        .unwrap()[0];
    let destination = builder
        .append_instruction(
            publish,
            publish_entry,
            InstructionKind::GlobalAddr { global },
            [pointer_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            publish,
            publish_entry,
            InstructionKind::Store {
                pointer: destination,
                value: loaded,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(
            publish,
            publish_entry,
            Terminator::Return { values: vec![] },
        )
        .unwrap();

    let main = builder.declare_function(
        "gane.main".into(),
        signature(vec![], vec![]),
        FunctionAttributes::default(),
    );
    builder.set_entry(main).unwrap();
    let entry = builder.entry_block(main).unwrap();
    let value_slot = builder.add_stack_slot(main, i64, None, None).unwrap();
    let pointer_slot = builder.add_stack_slot(main, pointer, None, None).unwrap();
    let value = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr { slot: value_slot },
            [pointer],
            None,
        )
        .unwrap()[0];
    let argument = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr { slot: pointer_slot },
            [pointer_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Store {
                pointer: argument,
                value,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Call {
                callee: Callee::Function(publish),
                arguments: vec![argument],
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(main, entry, Terminator::Return { values: vec![] })
        .unwrap();

    assert!(escape_messages(builder.finish().unwrap()).contains("escaping parameter"));
}

#[test]
fn rejects_callee_copying_pointer_aggregate_to_global() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = pointer_type(&mut builder, i64);
    let pointer_pointer = pointer_type(&mut builder, pointer);
    let aggregate = builder.add_type(IrTypeKind::Struct {
        fields: vec![pointer],
    });
    let aggregate_pointer = pointer_type(&mut builder, aggregate);
    let global = builder.add_global(IrGlobal {
        symbol: "gane.saved".into(),
        typ: aggregate,
        initializer: GlobalInitializer::Zero,
    });
    let publish = builder.declare_function(
        "gane.publish".into(),
        signature(
            vec![IrParameter {
                typ: aggregate_pointer,
            }],
            vec![],
        ),
        FunctionAttributes::default(),
    );
    let publish_entry = builder.entry_block(publish).unwrap();
    let source = builder.entry_parameters(publish).unwrap()[0];
    let destination = builder
        .append_instruction(
            publish,
            publish_entry,
            InstructionKind::GlobalAddr { global },
            [aggregate_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            publish,
            publish_entry,
            InstructionKind::AggregateCopy {
                destination,
                source,
                typ: aggregate,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(
            publish,
            publish_entry,
            Terminator::Return { values: vec![] },
        )
        .unwrap();

    let main = builder.declare_function(
        "gane.main".into(),
        signature(vec![], vec![]),
        FunctionAttributes::default(),
    );
    builder.set_entry(main).unwrap();
    let entry = builder.entry_block(main).unwrap();
    let value_slot = builder.add_stack_slot(main, i64, None, None).unwrap();
    let aggregate_slot = builder.add_stack_slot(main, aggregate, None, None).unwrap();
    let value = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr { slot: value_slot },
            [pointer],
            None,
        )
        .unwrap()[0];
    let argument = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr {
                slot: aggregate_slot,
            },
            [aggregate_pointer],
            None,
        )
        .unwrap()[0];
    let field = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::GepField {
                base: argument,
                field: 0,
            },
            [pointer_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Store {
                pointer: field,
                value,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Call {
                callee: Callee::Function(publish),
                arguments: vec![argument],
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(main, entry, Terminator::Return { values: vec![] })
        .unwrap();

    assert!(escape_messages(builder.finish().unwrap()).contains("escaping parameter"));
}

#[test]
fn rejects_callee_publishing_pointer_through_multiple_loads() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = pointer_type(&mut builder, i64);
    let pointer_pointer = pointer_type(&mut builder, pointer);
    let pointer_pointer_pointer = pointer_type(&mut builder, pointer_pointer);
    let global = builder.add_global(IrGlobal {
        symbol: "gane.saved".into(),
        typ: pointer,
        initializer: GlobalInitializer::Zero,
    });
    let publish = builder.declare_function(
        "gane.publish".into(),
        signature(
            vec![IrParameter {
                typ: pointer_pointer_pointer,
            }],
            vec![],
        ),
        FunctionAttributes::default(),
    );
    let publish_entry = builder.entry_block(publish).unwrap();
    let parameter = builder.entry_parameters(publish).unwrap()[0];
    let inner = builder
        .append_instruction(
            publish,
            publish_entry,
            InstructionKind::Load { pointer: parameter },
            [pointer_pointer],
            None,
        )
        .unwrap()[0];
    let loaded = builder
        .append_instruction(
            publish,
            publish_entry,
            InstructionKind::Load { pointer: inner },
            [pointer],
            None,
        )
        .unwrap()[0];
    let destination = builder
        .append_instruction(
            publish,
            publish_entry,
            InstructionKind::GlobalAddr { global },
            [pointer_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            publish,
            publish_entry,
            InstructionKind::Store {
                pointer: destination,
                value: loaded,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(
            publish,
            publish_entry,
            Terminator::Return { values: vec![] },
        )
        .unwrap();

    let main = builder.declare_function(
        "gane.main".into(),
        signature(vec![], vec![]),
        FunctionAttributes::default(),
    );
    builder.set_entry(main).unwrap();
    let entry = builder.entry_block(main).unwrap();
    let value_slot = builder.add_stack_slot(main, i64, None, None).unwrap();
    let pointer_slot = builder.add_stack_slot(main, pointer, None, None).unwrap();
    let pointer_pointer_slot = builder
        .add_stack_slot(main, pointer_pointer, None, None)
        .unwrap();
    let value = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr { slot: value_slot },
            [pointer],
            None,
        )
        .unwrap()[0];
    let inner = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr { slot: pointer_slot },
            [pointer_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Store {
                pointer: inner,
                value,
            },
            [],
            None,
        )
        .unwrap();
    let argument = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr {
                slot: pointer_pointer_slot,
            },
            [pointer_pointer_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Store {
                pointer: argument,
                value: inner,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Call {
                callee: Callee::Function(publish),
                arguments: vec![argument],
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(main, entry, Terminator::Return { values: vec![] })
        .unwrap();

    assert!(escape_messages(builder.finish().unwrap()).contains("escaping parameter"));
}

#[test]
fn accepts_returning_static_pointer_through_call_summary() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = pointer_type(&mut builder, i64);
    let global = builder.add_global(IrGlobal {
        symbol: "gane.value".into(),
        typ: i64,
        initializer: GlobalInitializer::Zero,
    });
    let source = builder.declare_function(
        "gane.global_pointer".into(),
        signature(vec![], vec![pointer]),
        FunctionAttributes::default(),
    );
    let source_entry = builder.entry_block(source).unwrap();
    let value = builder
        .append_instruction(
            source,
            source_entry,
            InstructionKind::GlobalAddr { global },
            [pointer],
            None,
        )
        .unwrap()[0];
    builder
        .set_terminator(
            source,
            source_entry,
            Terminator::Return {
                values: vec![value],
            },
        )
        .unwrap();
    let wrapper = builder.declare_function(
        "gane.wrapper".into(),
        signature(vec![], vec![pointer]),
        FunctionAttributes::default(),
    );
    let wrapper_entry = builder.entry_block(wrapper).unwrap();
    let result = builder
        .append_instruction(
            wrapper,
            wrapper_entry,
            InstructionKind::Call {
                callee: Callee::Function(source),
                arguments: vec![],
            },
            [pointer],
            None,
        )
        .unwrap()[0];
    builder
        .set_terminator(
            wrapper,
            wrapper_entry,
            Terminator::Return {
                values: vec![result],
            },
        )
        .unwrap();
    add_empty_main(&mut builder);

    let package = builder.finish().unwrap();
    assert!(verify(&package).is_ok());
    assert!(verify_and_check_escape(package).is_ok());
}

#[test]
fn call_write_havoc_propagates_into_caller_memory() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = pointer_type(&mut builder, i64);
    let pointer_pointer = pointer_type(&mut builder, pointer);
    let clear = builder.declare_function(
        "gane.clear".into(),
        signature(
            vec![IrParameter {
                typ: pointer_pointer,
            }],
            vec![],
        ),
        FunctionAttributes::default(),
    );
    let clear_entry = builder.entry_block(clear).unwrap();
    let destination = builder.entry_parameters(clear).unwrap()[0];
    let null = builder
        .append_instruction(
            clear,
            clear_entry,
            InstructionKind::Const {
                value: Constant::Null,
                typ: pointer,
            },
            [pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            clear,
            clear_entry,
            InstructionKind::Store {
                pointer: destination,
                value: null,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(clear, clear_entry, Terminator::Return { values: vec![] })
        .unwrap();

    let wrapper = builder.declare_function(
        "gane.wrapper".into(),
        signature(vec![], vec![pointer]),
        FunctionAttributes::default(),
    );
    let wrapper_entry = builder.entry_block(wrapper).unwrap();
    let slot = builder
        .add_stack_slot(wrapper, pointer, None, None)
        .unwrap();
    let address = builder
        .append_instruction(
            wrapper,
            wrapper_entry,
            InstructionKind::StackAddr { slot },
            [pointer_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            wrapper,
            wrapper_entry,
            InstructionKind::Call {
                callee: Callee::Function(clear),
                arguments: vec![address],
            },
            [],
            None,
        )
        .unwrap();
    let loaded = builder
        .append_instruction(
            wrapper,
            wrapper_entry,
            InstructionKind::Load { pointer: address },
            [pointer],
            None,
        )
        .unwrap()[0];
    builder
        .set_terminator(
            wrapper,
            wrapper_entry,
            Terminator::Return {
                values: vec![loaded],
            },
        )
        .unwrap();
    add_empty_main(&mut builder);

    let package = builder.finish().unwrap();
    assert!(verify(&package).is_ok());
    let effects = summaries(&package);
    assert!(effects[clear.raw() as usize - 1].writes_external);
    assert!(effects[wrapper.raw() as usize - 1].returns_static);
    assert!(verify_and_check_escape(package).is_ok());
}

#[test]
fn accepts_copying_integer_aggregate_from_stack_to_global() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let aggregate = builder.add_type(IrTypeKind::Struct { fields: vec![i64] });
    let aggregate_pointer = pointer_type(&mut builder, aggregate);
    let global = builder.add_global(IrGlobal {
        symbol: "gane.saved".into(),
        typ: aggregate,
        initializer: GlobalInitializer::Zero,
    });
    let main = builder.declare_function(
        "gane.main".into(),
        signature(vec![], vec![]),
        FunctionAttributes::default(),
    );
    builder.set_entry(main).unwrap();
    let entry = builder.entry_block(main).unwrap();
    let slot = builder.add_stack_slot(main, aggregate, None, None).unwrap();
    let source = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr { slot },
            [aggregate_pointer],
            None,
        )
        .unwrap()[0];
    let destination = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::GlobalAddr { global },
            [aggregate_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::AggregateCopy {
                destination,
                source,
                typ: aggregate,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(main, entry, Terminator::Return { values: vec![] })
        .unwrap();

    let package = builder.finish().unwrap();
    assert!(verify(&package).is_ok());
    assert!(verify_and_check_escape(package).is_ok());
}

#[test]
fn rejects_return_after_overwriting_stack_pointer_with_null() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = pointer_type(&mut builder, i64);
    let pointer_pointer = pointer_type(&mut builder, pointer);
    let escape = builder.declare_function(
        "gane.escape".into(),
        signature(vec![], vec![pointer]),
        FunctionAttributes::default(),
    );
    let entry = builder.entry_block(escape).unwrap();
    let value_slot = builder.add_stack_slot(escape, i64, None, None).unwrap();
    let pointer_slot = builder.add_stack_slot(escape, pointer, None, None).unwrap();
    let value = builder
        .append_instruction(
            escape,
            entry,
            InstructionKind::StackAddr { slot: value_slot },
            [pointer],
            None,
        )
        .unwrap()[0];
    let destination = builder
        .append_instruction(
            escape,
            entry,
            InstructionKind::StackAddr { slot: pointer_slot },
            [pointer_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            escape,
            entry,
            InstructionKind::Store {
                pointer: destination,
                value,
            },
            [],
            None,
        )
        .unwrap();
    let null = builder
        .append_instruction(
            escape,
            entry,
            InstructionKind::Const {
                value: Constant::Null,
                typ: pointer,
            },
            [pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            escape,
            entry,
            InstructionKind::Store {
                pointer: destination,
                value: null,
            },
            [],
            None,
        )
        .unwrap();
    let loaded = builder
        .append_instruction(
            escape,
            entry,
            InstructionKind::Load {
                pointer: destination,
            },
            [pointer],
            None,
        )
        .unwrap()[0];
    builder
        .set_terminator(
            escape,
            entry,
            Terminator::Return {
                values: vec![loaded],
            },
        )
        .unwrap();
    add_empty_main(&mut builder);

    assert!(escape_messages(builder.finish().unwrap()).contains("returned from function"));
}

#[test]
fn rejects_escaping_recursive_parameter_cycle() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i32 = builder.types().i32();
    let pointer = builder.add_type(IrTypeKind::Ptr {
        pointee: i32,
        address_space: 0,
    });
    let first = builder.declare_function(
        "gane.first".into(),
        signature(vec![IrParameter { typ: pointer }], vec![pointer]),
        FunctionAttributes::default(),
    );
    let second = builder.declare_function(
        "gane.second".into(),
        signature(vec![IrParameter { typ: pointer }], vec![pointer]),
        FunctionAttributes::default(),
    );
    let first_entry = builder.entry_block(first).unwrap();
    let first_parameter = builder.entry_parameters(first).unwrap()[0];
    let returned = builder
        .append_instruction(
            first,
            first_entry,
            InstructionKind::Call {
                callee: Callee::Function(second),
                arguments: vec![first_parameter],
            },
            [pointer],
            None,
        )
        .unwrap()[0];
    builder
        .set_terminator(
            first,
            first_entry,
            Terminator::Return {
                values: vec![returned],
            },
        )
        .unwrap();
    let second_entry = builder.entry_block(second).unwrap();
    let second_parameter = builder.entry_parameters(second).unwrap()[0];
    builder
        .append_instruction(
            second,
            second_entry,
            InstructionKind::Call {
                callee: Callee::Function(first),
                arguments: vec![second_parameter],
            },
            [pointer],
            None,
        )
        .unwrap();
    builder
        .set_terminator(
            second,
            second_entry,
            Terminator::Return {
                values: vec![second_parameter],
            },
        )
        .unwrap();

    let main = builder.declare_function(
        "gane.main".into(),
        signature(vec![], vec![]),
        FunctionAttributes::default(),
    );
    builder.set_entry(main).unwrap();
    let entry = builder.entry_block(main).unwrap();
    let slot = builder.add_stack_slot(main, i32, None, None).unwrap();
    let value = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr { slot },
            [pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Call {
                callee: Callee::Function(first),
                arguments: vec![value],
            },
            [pointer],
            None,
        )
        .unwrap();
    builder
        .set_terminator(main, entry, Terminator::Return { values: vec![] })
        .unwrap();

    assert!(escape_messages(builder.finish().unwrap()).contains("escaping parameter"));
}

#[test]
fn accepts_non_escaping_recursive_parameter_cycle() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i32 = builder.types().i32();
    let pointer = builder.add_type(IrTypeKind::Ptr {
        pointee: i32,
        address_space: 0,
    });
    let first = builder.declare_function(
        "gane.first".into(),
        signature(vec![IrParameter { typ: pointer }], vec![]),
        FunctionAttributes::default(),
    );
    let second = builder.declare_function(
        "gane.second".into(),
        signature(vec![IrParameter { typ: pointer }], vec![]),
        FunctionAttributes::default(),
    );
    let first_entry = builder.entry_block(first).unwrap();
    let first_parameter = builder.entry_parameters(first).unwrap()[0];
    builder
        .append_instruction(
            first,
            first_entry,
            InstructionKind::Call {
                callee: Callee::Function(second),
                arguments: vec![first_parameter],
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(first, first_entry, Terminator::Return { values: vec![] })
        .unwrap();
    let second_entry = builder.entry_block(second).unwrap();
    let second_parameter = builder.entry_parameters(second).unwrap()[0];
    builder
        .append_instruction(
            second,
            second_entry,
            InstructionKind::Call {
                callee: Callee::Function(first),
                arguments: vec![second_parameter],
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(second, second_entry, Terminator::Return { values: vec![] })
        .unwrap();

    let main = builder.declare_function(
        "gane.main".into(),
        signature(vec![], vec![]),
        FunctionAttributes::default(),
    );
    builder.set_entry(main).unwrap();
    let entry = builder.entry_block(main).unwrap();
    let slot = builder.add_stack_slot(main, i32, None, None).unwrap();
    let value = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr { slot },
            [pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Call {
                callee: Callee::Function(first),
                arguments: vec![value],
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(main, entry, Terminator::Return { values: vec![] })
        .unwrap();

    assert!(verify_and_check_escape(builder.finish().unwrap()).is_ok());
}