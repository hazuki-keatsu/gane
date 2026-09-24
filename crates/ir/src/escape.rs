use crate::{
    Callee, FunctionId, InstructionKind, IrDiagnostic, IrFunction, StackSlotId, Terminator,
    UnverifiedIrPackage, ValueId, ValueOrigin,
};
use std::collections::BTreeSet;

pub(crate) fn check(package: &UnverifiedIrPackage) -> Result<(), Vec<IrDiagnostic>> {
    let summaries = summaries(package);
    let mut diagnostics = Vec::new();

    for (function_id, function) in package.functions() {
        let taint = propagate(function, stack_addresses(function));
        report_escapes(function_id, function, &taint, &summaries, &mut diagnostics);
    }

    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(diagnostics)
    }
}

fn summaries(package: &UnverifiedIrPackage) -> Vec<Vec<bool>> {
    let mut summaries = package
        .functions()
        .map(|(_, function)| vec![false; function.signature.parameters.len()])
        .collect::<Vec<_>>();

    loop {
        let mut changed = false;
        for (function_id, function) in package.functions() {
            let Some(entry) = function.block(function.entry) else {
                continue;
            };
            for (index, parameter) in function.signature.parameters.iter().enumerate() {
                if summaries[function_id.raw() as usize - 1][index]
                    || !is_pointer(package, parameter.typ)
                {
                    continue;
                }
                let Some(value) = entry.parameters.get(index).copied() else {
                    continue;
                };
                let taint = propagate(function, [value]);
                if escapes(function, &taint, &summaries) {
                    summaries[function_id.raw() as usize - 1][index] = true;
                    changed = true;
                }
            }
        }
        if !changed {
            return summaries;
        }
    }
}

fn is_pointer(package: &UnverifiedIrPackage, typ: crate::TypeId) -> bool {
    package
        .types()
        .get(typ)
        .is_some_and(|typ| matches!(typ.kind, crate::IrTypeKind::Ptr { .. }))
}

#[derive(Default)]
struct Taint {
    values: BTreeSet<ValueId>,
    slots: BTreeSet<StackSlotId>,
}

fn propagate(function: &IrFunction, values: impl IntoIterator<Item = ValueId>) -> Taint {
    // ponytail: flow-insensitive slot taint may reject after a clearing overwrite; add CFG-sensitive
    // memory state only when valid source programs need that precision.
    let mut taint = Taint {
        values: values.into_iter().collect(),
        slots: BTreeSet::new(),
    };

    loop {
        let mut changed = false;
        for block in &function.blocks {
            for instruction in &block.instructions {
                match &instruction.kind {
                    InstructionKind::GepField { base, .. }
                    | InstructionKind::GepIndex { base, .. }
                        if taint.values.contains(base) =>
                    {
                        changed |= extend_values(&mut taint, &instruction.results);
                    }
                    InstructionKind::Load { pointer } => {
                        if let Storage::Stack(slot) = storage(function, *pointer)
                            && taint.slots.contains(&slot)
                        {
                            changed |= extend_values(&mut taint, &instruction.results);
                        }
                    }
                    InstructionKind::Store { pointer, value } if taint.values.contains(value) => {
                        if let Storage::Stack(slot) = storage(function, *pointer) {
                            changed |= taint.slots.insert(slot);
                        }
                    }
                    InstructionKind::AggregateCopy {
                        destination,
                        source,
                        ..
                    } if storage(function, *source)
                        .stack_slot()
                        .is_some_and(|slot| taint.slots.contains(&slot)) =>
                    {
                        if let Storage::Stack(slot) = storage(function, *destination) {
                            changed |= taint.slots.insert(slot);
                        }
                    }
                    _ => {}
                }
            }
            changed |= propagate_terminator(function, &block.terminator, &mut taint);
        }
        if !changed {
            return taint;
        }
    }
}

fn extend_values(taint: &mut Taint, values: &[ValueId]) -> bool {
    values
        .iter()
        .copied()
        .any(|value| taint.values.insert(value))
}

fn propagate_terminator(function: &IrFunction, terminator: &Terminator, taint: &mut Taint) -> bool {
    match terminator {
        Terminator::Branch { target, arguments } => {
            propagate_edge(function, *target, arguments, taint)
        }
        Terminator::CondBranch {
            then_target,
            then_arguments,
            else_target,
            else_arguments,
            ..
        } => {
            propagate_edge(function, *then_target, then_arguments, taint)
                | propagate_edge(function, *else_target, else_arguments, taint)
        }
        Terminator::Return { .. } | Terminator::Trap { .. } | Terminator::Unreachable => false,
    }
}

fn propagate_edge(
    function: &IrFunction,
    target: crate::BlockId,
    arguments: &[ValueId],
    taint: &mut Taint,
) -> bool {
    let Some(block) = function.block(target) else {
        return false;
    };
    let values = block
        .parameters
        .iter()
        .zip(arguments)
        .filter(|(_, argument)| taint.values.contains(argument))
        .map(|(parameter, _)| *parameter)
        .collect::<Vec<_>>();
    extend_values(taint, &values)
}

fn escapes(function: &IrFunction, taint: &Taint, summaries: &[Vec<bool>]) -> bool {
    function.blocks.iter().any(|block| {
        instructions_escape(function, &block.instructions, taint, summaries)
            || matches!(
                &block.terminator,
                Terminator::Return { values } if values.iter().any(|value| taint.values.contains(value))
            )
    })
}

fn instructions_escape(
    function: &IrFunction,
    instructions: &[crate::Instruction],
    taint: &Taint,
    summaries: &[Vec<bool>],
) -> bool {
    instructions
        .iter()
        .any(|instruction| match &instruction.kind {
            InstructionKind::Store { pointer, value } => {
                taint.values.contains(value)
                    && !matches!(storage(function, *pointer), Storage::Stack(_))
            }
            InstructionKind::AggregateCopy {
                destination,
                source,
                ..
            } => {
                storage(function, *source)
                    .stack_slot()
                    .is_some_and(|slot| taint.slots.contains(&slot))
                    && !matches!(storage(function, *destination), Storage::Stack(_))
            }
            InstructionKind::Call {
                callee: Callee::Function(callee),
                arguments,
            } => arguments.iter().enumerate().any(|(index, argument)| {
                taint.values.contains(argument) && parameter_escapes(summaries, *callee, index)
            }),
            _ => false,
        })
}

fn report_escapes(
    function_id: FunctionId,
    function: &IrFunction,
    taint: &Taint,
    summaries: &[Vec<bool>],
    diagnostics: &mut Vec<IrDiagnostic>,
) {
    for (block_index, block) in function.blocks.iter().enumerate() {
        for (instruction_index, instruction) in block.instructions.iter().enumerate() {
            let location = format!(
                "function @{} block ^{} instruction #{}",
                function_id.raw(),
                block_index + 1,
                instruction_index + 1
            );
            match &instruction.kind {
                InstructionKind::Store { pointer, value } if taint.values.contains(value) => {
                    match storage(function, *pointer) {
                        Storage::Stack(_) => {}
                        Storage::Global => diagnostics.push(IrDiagnostic::new(
                            location,
                            "stack-derived pointer stored in global",
                        )),
                        Storage::Unknown => diagnostics.push(IrDiagnostic::new(
                            location,
                            "stack-derived pointer stored outside current stack",
                        )),
                    }
                }
                InstructionKind::AggregateCopy {
                    destination,
                    source,
                    ..
                } if storage(function, *source)
                    .stack_slot()
                    .is_some_and(|slot| taint.slots.contains(&slot)) =>
                {
                    match storage(function, *destination) {
                        Storage::Stack(_) => {}
                        Storage::Global => diagnostics.push(IrDiagnostic::new(
                            location,
                            "stack-derived pointer copied into global",
                        )),
                        Storage::Unknown => diagnostics.push(IrDiagnostic::new(
                            location,
                            "stack-derived pointer copied outside current stack",
                        )),
                    }
                }
                InstructionKind::Call {
                    callee: Callee::Function(callee),
                    arguments,
                } if arguments.iter().enumerate().any(|(index, argument)| {
                    taint.values.contains(argument) && parameter_escapes(summaries, *callee, index)
                }) =>
                {
                    diagnostics.push(IrDiagnostic::new(
                        location,
                        "stack-derived pointer passed to an escaping parameter",
                    ))
                }
                _ => {}
            }
        }
        if let Terminator::Return { values } = &block.terminator
            && values.iter().any(|value| taint.values.contains(value))
        {
            diagnostics.push(IrDiagnostic::new(
                format!(
                    "function @{} block ^{} terminator",
                    function_id.raw(),
                    block_index + 1
                ),
                "stack-derived pointer returned from function",
            ));
        }
    }
}

fn parameter_escapes(summaries: &[Vec<bool>], callee: FunctionId, index: usize) -> bool {
    callee
        .raw()
        .checked_sub(1)
        .and_then(|index| summaries.get(index as usize))
        .and_then(|parameters| parameters.get(index))
        .copied()
        .unwrap_or(true)
}

#[derive(Clone, Copy)]
enum Storage {
    Stack(StackSlotId),
    Global,
    Unknown,
}

impl Storage {
    fn stack_slot(self) -> Option<StackSlotId> {
        match self {
            Self::Stack(slot) => Some(slot),
            Self::Global | Self::Unknown => None,
        }
    }
}

fn storage(function: &IrFunction, value: ValueId) -> Storage {
    let Some(definition) = function.value(value) else {
        return Storage::Unknown;
    };
    let ValueOrigin::InstructionResult {
        block,
        instruction_index,
        ..
    } = definition.origin
    else {
        return Storage::Unknown;
    };
    let Some(instruction) = function
        .block(block)
        .and_then(|block| block.instructions.get(instruction_index as usize))
    else {
        return Storage::Unknown;
    };
    match &instruction.kind {
        InstructionKind::StackAddr { slot } => Storage::Stack(*slot),
        InstructionKind::GlobalAddr { .. } => Storage::Global,
        InstructionKind::GepField { base, .. } | InstructionKind::GepIndex { base, .. } => {
            storage(function, *base)
        }
        _ => Storage::Unknown,
    }
}

fn stack_addresses(function: &IrFunction) -> Vec<ValueId> {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| matches!(instruction.kind, InstructionKind::StackAddr { .. }))
        .flat_map(|instruction| instruction.results.iter().copied())
        .collect()
}

#[cfg(test)]
mod tests {
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

    fn empty_main() -> UnverifiedIrPackage {
        let mut builder = IrBuilder::new(TargetSpec::for_test_64());
        let i32 = builder.types().i32();
        let pointer = builder.add_type(IrTypeKind::Ptr {
            pointee: i32,
            address_space: 0,
        });
        let main = builder.declare_function(
            "gane.main".into(),
            signature(vec![], vec![]),
            FunctionAttributes::default(),
        );
        builder.set_entry(main).unwrap();
        let entry = builder.entry_block(main).unwrap();
        let slot = builder.add_stack_slot(main, i32, None, None).unwrap();
        builder
            .append_instruction(
                main,
                entry,
                InstructionKind::StackAddr { slot },
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
    fn rejects_stack_pointer_passed_to_escaping_parameter() {
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
    fn rejects_escaping_recursive_parameter_cycle() {
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
            signature(vec![IrParameter { typ: pointer }], vec![pointer]),
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
                [pointer],
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
}
