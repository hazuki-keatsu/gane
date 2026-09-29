use crate::{
    BinaryOp, Callee, ComparePredicate, Constant, FunctionAttributes, GlobalInitializer,
    InstructionKind, IrBuilder, IrGlobal, IrParameter, IrSignature, IrTypeKind, TargetSpec,
    Terminator, TypeId, UnverifiedIrPackage, verify, verify_and_check_escape,
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

#[test]
fn rejects_escape_from_second_branch_argument_and_loop_backedge() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i1 = builder.types().i1();
    let i64 = builder.types().i64();
    let pointer = pointer_type(&mut builder, i64);
    let global = builder.add_global(IrGlobal {
        symbol: "gane.value".into(),
        typ: i64,
        initializer: GlobalInitializer::Zero,
    });
    let escape = builder.declare_function(
        "gane.escape".into(),
        signature(vec![], vec![pointer]),
        FunctionAttributes::default(),
    );
    let entry = builder.entry_block(escape).unwrap();
    let exit = builder.create_block(escape).unwrap();
    let exit_first = builder
        .append_block_parameter(escape, exit, pointer, None)
        .unwrap();
    let exit_second = builder
        .append_block_parameter(escape, exit, pointer, None)
        .unwrap();
    let header = builder.create_block(escape).unwrap();
    let header_first = builder
        .append_block_parameter(escape, header, pointer, None)
        .unwrap();
    let header_second = builder
        .append_block_parameter(escape, header, pointer, None)
        .unwrap();
    let body = builder.create_block(escape).unwrap();
    let slot = builder.add_stack_slot(escape, i64, None, None).unwrap();
    let local = builder
        .append_instruction(
            escape,
            entry,
            InstructionKind::StackAddr { slot },
            [pointer],
            None,
        )
        .unwrap()[0];
    let static_pointer = builder
        .append_instruction(
            escape,
            entry,
            InstructionKind::GlobalAddr { global },
            [pointer],
            None,
        )
        .unwrap()[0];
    let condition = builder
        .append_instruction(
            escape,
            entry,
            InstructionKind::Const {
                value: Constant::Bool(true),
                typ: i1,
            },
            [i1],
            None,
        )
        .unwrap()[0];
    builder
        .set_terminator(
            escape,
            entry,
            Terminator::Branch {
                target: header,
                arguments: vec![static_pointer, static_pointer],
            },
        )
        .unwrap();
    builder
        .set_terminator(
            escape,
            header,
            Terminator::CondBranch {
                condition,
                then_target: exit,
                then_arguments: vec![header_first, header_second],
                else_target: body,
                else_arguments: vec![],
            },
        )
        .unwrap();
    builder
        .set_terminator(
            escape,
            body,
            Terminator::Branch {
                target: header,
                arguments: vec![static_pointer, local],
            },
        )
        .unwrap();
    builder
        .set_terminator(
            escape,
            exit,
            Terminator::Return {
                values: vec![exit_second],
            },
        )
        .unwrap();
    let _ = exit_first;
    add_empty_main(&mut builder);

    assert!(escape_messages(builder.finish().unwrap()).contains("returned from function"));
}

#[test]
fn accepts_null_and_global_pointer_through_slot_and_block_parameter() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i1 = builder.types().i1();
    let i64 = builder.types().i64();
    let pointer = pointer_type(&mut builder, i64);
    let pointer_pointer = pointer_type(&mut builder, pointer);
    let global = builder.add_global(IrGlobal {
        symbol: "gane.value".into(),
        typ: i64,
        initializer: GlobalInitializer::Zero,
    });
    let source = builder.declare_function(
        "gane.source".into(),
        signature(vec![], vec![pointer]),
        FunctionAttributes::default(),
    );
    let entry = builder.entry_block(source).unwrap();
    let join = builder.create_block(source).unwrap();
    let joined = builder
        .append_block_parameter(source, join, pointer, None)
        .unwrap();
    let slot = builder.add_stack_slot(source, pointer, None, None).unwrap();
    let address = builder
        .append_instruction(
            source,
            entry,
            InstructionKind::StackAddr { slot },
            [pointer_pointer],
            None,
        )
        .unwrap()[0];
    let null = builder
        .append_instruction(
            source,
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
            source,
            entry,
            InstructionKind::Store {
                pointer: address,
                value: null,
            },
            [],
            None,
        )
        .unwrap();
    let loaded = builder
        .append_instruction(
            source,
            entry,
            InstructionKind::Load { pointer: address },
            [pointer],
            None,
        )
        .unwrap()[0];
    let static_pointer = builder
        .append_instruction(
            source,
            entry,
            InstructionKind::GlobalAddr { global },
            [pointer],
            None,
        )
        .unwrap()[0];
    let one = builder
        .append_instruction(
            source,
            entry,
            InstructionKind::Const {
                value: Constant::Integer(1),
                typ: i64,
            },
            [i64],
            None,
        )
        .unwrap()[0];
    let two = builder
        .append_instruction(
            source,
            entry,
            InstructionKind::Binary {
                op: BinaryOp::Add,
                left: one,
                right: one,
            },
            [i64],
            None,
        )
        .unwrap()[0];
    let condition = builder
        .append_instruction(
            source,
            entry,
            InstructionKind::Compare {
                predicate: ComparePredicate::Equal,
                left: one,
                right: two,
            },
            [i1],
            None,
        )
        .unwrap()[0];
    builder
        .set_terminator(
            source,
            entry,
            Terminator::CondBranch {
                condition,
                then_target: join,
                then_arguments: vec![loaded],
                else_target: join,
                else_arguments: vec![static_pointer],
            },
        )
        .unwrap();
    builder
        .set_terminator(
            source,
            join,
            Terminator::Return {
                values: vec![joined],
            },
        )
        .unwrap();
    add_empty_main(&mut builder);

    let package = builder.finish().unwrap();
    assert!(verify(&package).is_ok());
    assert!(verify_and_check_escape(package).is_ok());
}

#[test]
fn rejects_pointer_array_copy_through_dynamic_index_and_self_copy() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = pointer_type(&mut builder, i64);
    let pointer_pointer = pointer_type(&mut builder, pointer);
    let array = builder.add_type(IrTypeKind::Array {
        length: 2,
        element: pointer,
    });
    let array_pointer = pointer_type(&mut builder, array);
    let global = builder.add_global(IrGlobal {
        symbol: "gane.saved".into(),
        typ: array,
        initializer: GlobalInitializer::Zero,
    });
    let escape = builder.declare_function(
        "gane.escape".into(),
        signature(vec![IrParameter { typ: i64 }], vec![]),
        FunctionAttributes::default(),
    );
    let entry = builder.entry_block(escape).unwrap();
    let index = builder.entry_parameters(escape).unwrap()[0];
    let value_slot = builder.add_stack_slot(escape, i64, None, None).unwrap();
    let array_slot = builder.add_stack_slot(escape, array, None, None).unwrap();
    let value = builder
        .append_instruction(
            escape,
            entry,
            InstructionKind::StackAddr { slot: value_slot },
            [pointer],
            None,
        )
        .unwrap()[0];
    let source = builder
        .append_instruction(
            escape,
            entry,
            InstructionKind::StackAddr { slot: array_slot },
            [array_pointer],
            None,
        )
        .unwrap()[0];
    let element = builder
        .append_instruction(
            escape,
            entry,
            InstructionKind::GepIndex {
                base: source,
                index,
            },
            [pointer_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            escape,
            entry,
            InstructionKind::Store {
                pointer: element,
                value,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .append_instruction(
            escape,
            entry,
            InstructionKind::AggregateCopy {
                destination: source,
                source,
                typ: array,
            },
            [],
            None,
        )
        .unwrap();
    let destination = builder
        .append_instruction(
            escape,
            entry,
            InstructionKind::GlobalAddr { global },
            [array_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            escape,
            entry,
            InstructionKind::AggregateCopy {
                destination,
                source,
                typ: array,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(escape, entry, Terminator::Return { values: vec![] })
        .unwrap();
    add_empty_main(&mut builder);

    assert!(escape_messages(builder.finish().unwrap()).contains("copied into global"));
}

#[test]
fn rejects_nested_struct_copy_between_subobjects_and_to_global() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = pointer_type(&mut builder, i64);
    let pointer_pointer = pointer_type(&mut builder, pointer);
    let inner = builder.add_type(IrTypeKind::Struct {
        fields: vec![pointer],
    });
    let inner_pointer = pointer_type(&mut builder, inner);
    let inner_pointer_pointer = pointer_type(&mut builder, inner_pointer);
    let outer = builder.add_type(IrTypeKind::Struct {
        fields: vec![inner, inner],
    });
    let outer_pointer = pointer_type(&mut builder, outer);
    let global = builder.add_global(IrGlobal {
        symbol: "gane.saved".into(),
        typ: outer,
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
    let outer_slot = builder.add_stack_slot(main, outer, None, None).unwrap();
    let alias_slot = builder
        .add_stack_slot(main, inner_pointer, None, None)
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
    let source = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr { slot: outer_slot },
            [outer_pointer],
            None,
        )
        .unwrap()[0];
    let first = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::GepField {
                base: source,
                field: 0,
            },
            [inner_pointer],
            None,
        )
        .unwrap()[0];
    let second = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::GepField {
                base: source,
                field: 1,
            },
            [inner_pointer],
            None,
        )
        .unwrap()[0];
    let field = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::GepField {
                base: first,
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
            InstructionKind::AggregateCopy {
                destination: first,
                source: first,
                typ: inner,
            },
            [],
            None,
        )
        .unwrap();
    let alias_address = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr { slot: alias_slot },
            [inner_pointer_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Store {
                pointer: alias_address,
                value: first,
            },
            [],
            None,
        )
        .unwrap();
    let overlapping_alias = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Load {
                pointer: alias_address,
            },
            [inner_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::AggregateCopy {
                destination: overlapping_alias,
                source: first,
                typ: inner,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::AggregateCopy {
                destination: second,
                source: first,
                typ: inner,
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
            [outer_pointer],
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
                typ: outer,
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
fn accepts_copying_null_and_global_pointer_array_to_global() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = pointer_type(&mut builder, i64);
    let pointer_pointer = pointer_type(&mut builder, pointer);
    let array = builder.add_type(IrTypeKind::Array {
        length: 2,
        element: pointer,
    });
    let array_pointer = pointer_type(&mut builder, array);
    let value_global = builder.add_global(IrGlobal {
        symbol: "gane.value".into(),
        typ: i64,
        initializer: GlobalInitializer::Zero,
    });
    let array_global = builder.add_global(IrGlobal {
        symbol: "gane.saved".into(),
        typ: array,
        initializer: GlobalInitializer::Zero,
    });
    let main = builder.declare_function(
        "gane.main".into(),
        signature(vec![], vec![]),
        FunctionAttributes::default(),
    );
    builder.set_entry(main).unwrap();
    let entry = builder.entry_block(main).unwrap();
    let slot = builder.add_stack_slot(main, array, None, None).unwrap();
    let source = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr { slot },
            [array_pointer],
            None,
        )
        .unwrap()[0];
    let index = builder
        .append_instruction(
            main,
            entry,
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
            main,
            entry,
            InstructionKind::GepIndex {
                base: source,
                index,
            },
            [pointer_pointer],
            None,
        )
        .unwrap()[0];
    let static_pointer = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::GlobalAddr {
                global: value_global,
            },
            [pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Store {
                pointer: element,
                value: static_pointer,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::AggregateCopy {
                destination: source,
                source,
                typ: array,
            },
            [],
            None,
        )
        .unwrap();
    let destination = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::GlobalAddr {
                global: array_global,
            },
            [array_pointer],
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
                typ: array,
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
fn accepts_recursive_pointer_object_cycle_used_during_call() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let node = builder.reserve_type();
    let node_pointer = pointer_type(&mut builder, node);
    let node_pointer_pointer = pointer_type(&mut builder, node_pointer);
    builder
        .define_type(
            node,
            IrTypeKind::Struct {
                fields: vec![node_pointer],
            },
        )
        .unwrap();
    let inspect = builder.declare_function(
        "gane.inspect".into(),
        signature(vec![IrParameter { typ: node_pointer }], vec![]),
        FunctionAttributes::default(),
    );
    let inspect_entry = builder.entry_block(inspect).unwrap();
    builder
        .set_terminator(
            inspect,
            inspect_entry,
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
    let slot = builder.add_stack_slot(main, node, None, None).unwrap();
    let address = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::StackAddr { slot },
            [node_pointer],
            None,
        )
        .unwrap()[0];
    let next = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::GepField {
                base: address,
                field: 0,
            },
            [node_pointer_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Store {
                pointer: next,
                value: address,
            },
            [],
            None,
        )
        .unwrap();
    let loaded = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Load { pointer: next },
            [node_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Call {
                callee: Callee::Function(inspect),
                arguments: vec![loaded],
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
fn accepts_aliased_parameters_for_integer_only_helper() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = pointer_type(&mut builder, i64);
    let helper = builder.declare_function(
        "gane.copy_integer".into(),
        signature(
            vec![IrParameter { typ: pointer }, IrParameter { typ: pointer }],
            vec![],
        ),
        FunctionAttributes::default(),
    );
    let helper_entry = builder.entry_block(helper).unwrap();
    let parameters = builder.entry_parameters(helper).unwrap().to_vec();
    let value = builder
        .append_instruction(
            helper,
            helper_entry,
            InstructionKind::Load {
                pointer: parameters[0],
            },
            [i64],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            helper,
            helper_entry,
            InstructionKind::Store {
                pointer: parameters[1],
                value,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(helper, helper_entry, Terminator::Return { values: vec![] })
        .unwrap();
    let main = builder.declare_function(
        "gane.main".into(),
        signature(vec![], vec![]),
        FunctionAttributes::default(),
    );
    builder.set_entry(main).unwrap();
    let entry = builder.entry_block(main).unwrap();
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
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Call {
                callee: Callee::Function(helper),
                arguments: vec![address, address],
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
fn rejects_aliased_pointer_parameters_when_one_reads_and_one_writes() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = pointer_type(&mut builder, i64);
    let pointer_pointer = pointer_type(&mut builder, pointer);
    let relay = builder.declare_function(
        "gane.relay".into(),
        signature(
            vec![
                IrParameter {
                    typ: pointer_pointer,
                },
                IrParameter {
                    typ: pointer_pointer,
                },
            ],
            vec![],
        ),
        FunctionAttributes::default(),
    );
    let relay_entry = builder.entry_block(relay).unwrap();
    let parameters = builder.entry_parameters(relay).unwrap().to_vec();
    let value = builder
        .append_instruction(
            relay,
            relay_entry,
            InstructionKind::Load {
                pointer: parameters[1],
            },
            [pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            relay,
            relay_entry,
            InstructionKind::Store {
                pointer: parameters[0],
                value,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(relay, relay_entry, Terminator::Return { values: vec![] })
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
    let address = builder
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
                pointer: address,
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
                callee: Callee::Function(relay),
                arguments: vec![address, address],
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
fn rejects_pointer_written_by_callee_then_loaded_and_stored_globally() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = pointer_type(&mut builder, i64);
    let pointer_pointer = pointer_type(&mut builder, pointer);
    let global = builder.add_global(IrGlobal {
        symbol: "gane.saved".into(),
        typ: pointer,
        initializer: GlobalInitializer::Zero,
    });
    let write = builder.declare_function(
        "gane.write".into(),
        signature(
            vec![
                IrParameter {
                    typ: pointer_pointer,
                },
                IrParameter { typ: pointer },
            ],
            vec![],
        ),
        FunctionAttributes::default(),
    );
    let write_entry = builder.entry_block(write).unwrap();
    let parameters = builder.entry_parameters(write).unwrap().to_vec();
    builder
        .append_instruction(
            write,
            write_entry,
            InstructionKind::Store {
                pointer: parameters[0],
                value: parameters[1],
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(write, write_entry, Terminator::Return { values: vec![] })
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
    let address = builder
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
            InstructionKind::Call {
                callee: Callee::Function(write),
                arguments: vec![address, value],
            },
            [],
            None,
        )
        .unwrap();
    let loaded = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Load { pointer: address },
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
                value: loaded,
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
fn accepts_global_pointer_written_back_by_callee() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = pointer_type(&mut builder, i64);
    let pointer_pointer = pointer_type(&mut builder, pointer);
    let value_global = builder.add_global(IrGlobal {
        symbol: "gane.value".into(),
        typ: i64,
        initializer: GlobalInitializer::Zero,
    });
    let pointer_global = builder.add_global(IrGlobal {
        symbol: "gane.pointer".into(),
        typ: pointer,
        initializer: GlobalInitializer::Zero,
    });
    let write = builder.declare_function(
        "gane.write".into(),
        signature(
            vec![
                IrParameter {
                    typ: pointer_pointer,
                },
                IrParameter { typ: pointer },
            ],
            vec![],
        ),
        FunctionAttributes::default(),
    );
    let write_entry = builder.entry_block(write).unwrap();
    let parameters = builder.entry_parameters(write).unwrap().to_vec();
    builder
        .append_instruction(
            write,
            write_entry,
            InstructionKind::Store {
                pointer: parameters[0],
                value: parameters[1],
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(write, write_entry, Terminator::Return { values: vec![] })
        .unwrap();
    let wrapper = builder.declare_function(
        "gane.wrapper".into(),
        signature(vec![], vec![pointer]),
        FunctionAttributes::default(),
    );
    let wrapper_entry = builder.entry_block(wrapper).unwrap();
    let destination = builder
        .append_instruction(
            wrapper,
            wrapper_entry,
            InstructionKind::GlobalAddr {
                global: pointer_global,
            },
            [pointer_pointer],
            None,
        )
        .unwrap()[0];
    let value = builder
        .append_instruction(
            wrapper,
            wrapper_entry,
            InstructionKind::GlobalAddr {
                global: value_global,
            },
            [pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            wrapper,
            wrapper_entry,
            InstructionKind::Call {
                callee: Callee::Function(write),
                arguments: vec![destination, value],
            },
            [],
            None,
        )
        .unwrap();
    let loaded = builder
        .append_instruction(
            wrapper,
            wrapper_entry,
            InstructionKind::Load {
                pointer: destination,
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
                values: vec![loaded],
            },
        )
        .unwrap();
    add_empty_main(&mut builder);

    let package = builder.finish().unwrap();
    assert!(verify(&package).is_ok());
    assert!(verify_and_check_escape(package).is_ok());
}

#[test]
fn accepts_direct_recursive_noescape_parameter() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = pointer_type(&mut builder, i64);
    let recurse = builder.declare_function(
        "gane.recurse".into(),
        signature(vec![IrParameter { typ: pointer }], vec![]),
        FunctionAttributes::default(),
    );
    let recurse_entry = builder.entry_block(recurse).unwrap();
    let parameter = builder.entry_parameters(recurse).unwrap()[0];
    builder
        .append_instruction(
            recurse,
            recurse_entry,
            InstructionKind::Call {
                callee: Callee::Function(recurse),
                arguments: vec![parameter],
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(
            recurse,
            recurse_entry,
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
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Call {
                callee: Callee::Function(recurse),
                arguments: vec![value],
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
fn rejects_direct_recursion_publishing_each_frames_local_address() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = pointer_type(&mut builder, i64);
    let pointer_pointer = pointer_type(&mut builder, pointer);
    let global = builder.add_global(IrGlobal {
        symbol: "gane.saved".into(),
        typ: pointer,
        initializer: GlobalInitializer::Zero,
    });
    let recurse = builder.declare_function(
        "gane.recurse".into(),
        signature(vec![IrParameter { typ: pointer }], vec![]),
        FunctionAttributes::default(),
    );
    let entry = builder.entry_block(recurse).unwrap();
    let parameter = builder.entry_parameters(recurse).unwrap()[0];
    let slot = builder.add_stack_slot(recurse, i64, None, None).unwrap();
    let local = builder
        .append_instruction(
            recurse,
            entry,
            InstructionKind::StackAddr { slot },
            [pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            recurse,
            entry,
            InstructionKind::Call {
                callee: Callee::Function(recurse),
                arguments: vec![local],
            },
            [],
            None,
        )
        .unwrap();
    let destination = builder
        .append_instruction(
            recurse,
            entry,
            InstructionKind::GlobalAddr { global },
            [pointer_pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            recurse,
            entry,
            InstructionKind::Store {
                pointer: destination,
                value: parameter,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(recurse, entry, Terminator::Return { values: vec![] })
        .unwrap();
    add_empty_main(&mut builder);

    assert!(escape_messages(builder.finish().unwrap()).contains("escaping parameter"));
}

#[test]
fn rejects_global_store_even_when_overwritten_with_null() {
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
    let slot = builder.add_stack_slot(main, i64, None, None).unwrap();
    let local = builder
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
                value: local,
            },
            [],
            None,
        )
        .unwrap();
    let null = builder
        .append_instruction(
            main,
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
            main,
            entry,
            InstructionKind::Store {
                pointer: destination,
                value: null,
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
fn accepts_global_pointer_passed_to_borrowed_identity() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = pointer_type(&mut builder, i64);
    let global = builder.add_global(IrGlobal {
        symbol: "gane.value".into(),
        typ: i64,
        initializer: GlobalInitializer::Zero,
    });
    let identity = builder.declare_function(
        "gane.identity".into(),
        signature(vec![IrParameter { typ: pointer }], vec![pointer]),
        FunctionAttributes::default(),
    );
    let identity_entry = builder.entry_block(identity).unwrap();
    let parameter = builder.entry_parameters(identity).unwrap()[0];
    builder
        .set_terminator(
            identity,
            identity_entry,
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
    let argument = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::GlobalAddr { global },
            [pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Call {
                callee: Callee::Function(identity),
                arguments: vec![argument],
            },
            [pointer],
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
fn rejects_unknown_call_result_stored_globally() {
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let i64 = builder.types().i64();
    let pointer = pointer_type(&mut builder, i64);
    let pointer_pointer = pointer_type(&mut builder, pointer);
    let global = builder.add_global(IrGlobal {
        symbol: "gane.saved".into(),
        typ: pointer,
        initializer: GlobalInitializer::Zero,
    });
    let invalid = builder.declare_function(
        "gane.invalid".into(),
        signature(vec![], vec![pointer]),
        FunctionAttributes::default(),
    );
    let invalid_entry = builder.entry_block(invalid).unwrap();
    let slot = builder.add_stack_slot(invalid, i64, None, None).unwrap();
    let local = builder
        .append_instruction(
            invalid,
            invalid_entry,
            InstructionKind::StackAddr { slot },
            [pointer],
            None,
        )
        .unwrap()[0];
    builder
        .set_terminator(
            invalid,
            invalid_entry,
            Terminator::Return {
                values: vec![local],
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
    let result = builder
        .append_instruction(
            main,
            entry,
            InstructionKind::Call {
                callee: Callee::Function(invalid),
                arguments: vec![],
            },
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
                value: result,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(main, entry, Terminator::Return { values: vec![] })
        .unwrap();

    assert!(
        escape_messages(builder.finish().unwrap()).contains("unknown provenance stored in global")
    );
}

#[test]
fn call_escape_result_is_independent_of_function_declaration_order() {
    for callee_first in [false, true] {
        let mut builder = IrBuilder::new(TargetSpec::for_test_64());
        let i64 = builder.types().i64();
        let pointer = pointer_type(&mut builder, i64);
        let declare_main = |builder: &mut IrBuilder| {
            builder.declare_function(
                "gane.main".into(),
                signature(vec![], vec![]),
                FunctionAttributes::default(),
            )
        };
        let declare_identity = |builder: &mut IrBuilder| {
            builder.declare_function(
                "gane.identity".into(),
                signature(vec![IrParameter { typ: pointer }], vec![pointer]),
                FunctionAttributes::default(),
            )
        };
        let (main, identity) = if callee_first {
            let identity = declare_identity(&mut builder);
            (declare_main(&mut builder), identity)
        } else {
            let main = declare_main(&mut builder);
            (main, declare_identity(&mut builder))
        };
        builder.set_entry(main).unwrap();
        let identity_entry = builder.entry_block(identity).unwrap();
        let parameter = builder.entry_parameters(identity).unwrap()[0];
        builder
            .set_terminator(
                identity,
                identity_entry,
                Terminator::Return {
                    values: vec![parameter],
                },
            )
            .unwrap();
        let entry = builder.entry_block(main).unwrap();
        let slot = builder.add_stack_slot(main, i64, None, None).unwrap();
        let argument = builder
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
                    callee: Callee::Function(identity),
                    arguments: vec![argument],
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
}
