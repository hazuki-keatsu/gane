//! Gane IR's public API.

pub use crate::builder::{BuildError, IrBuilder};
pub use crate::id::{BlockId, FunctionId, GlobalId, StackSlotId, TypeId, ValueId};
pub use crate::ir::{
    BinaryOp, Callee, ComparePredicate, Constant, FunctionAttributes, GlobalInitializer,
    Instruction, InstructionKind, IntCastKind, IrBlock, IrFunction, IrGlobal, IrParameter,
    IrSignature, StackSlot, Terminator, TrapReason, UnaryOp, UnverifiedIrPackage, ValueDef,
    ValueOrigin, VerifiedIrPackage,
};
pub use crate::lower::{LowerError, lower_package};
pub use crate::target::{Endianness, TargetSpec, TargetSpecError};
pub use crate::types::{IrType, IrTypeKind, SourceOrigin, Symbol, TypeArena, TypeArenaError};
pub use crate::verify::{IrDiagnostic, verify, verify_and_check_escape};

#[cfg(test)]
mod tests {
    use super::*;

    fn signature(parameters: Vec<IrParameter>) -> IrSignature {
        IrSignature {
            parameters,
            results: Vec::new(),
        }
    }

    fn declare_function(builder: &mut IrBuilder, parameters: Vec<IrParameter>) -> FunctionId {
        builder.declare_function(
            "gane.main".to_owned(),
            signature(parameters),
            FunctionAttributes::default(),
        )
    }

    #[test]
    fn ids_are_invalid_at_zero_and_primitives_are_canonical() {
        let builder = IrBuilder::new(TargetSpec::for_test_64());

        assert_eq!(TypeId::INVALID.raw(), 0);
        assert_eq!(FunctionId::INVALID.raw(), 0);
        assert_eq!(BlockId::INVALID.raw(), 0);
        assert_eq!(ValueId::INVALID.raw(), 0);
        assert_eq!(GlobalId::INVALID.raw(), 0);
        assert_eq!(StackSlotId::INVALID.raw(), 0);
        assert_eq!(builder.types().void().raw(), 1);
        assert_eq!(builder.types().i64().raw(), 6);
        assert_eq!(builder.types().i32(), builder.types().i32());
    }

    #[test]
    fn target_spec_accepts_only_complete_32_or_64_bit_targets() {
        assert_eq!(TargetSpec::for_test_32().pointer_width(), 32);
        assert_eq!(TargetSpec::for_test_64().pointer_width(), 64);
        assert_eq!(
            TargetSpec::new("", "generic", "", "layout", 64, Endianness::Little),
            Err(TargetSpecError::MissingTriple)
        );
        assert_eq!(
            TargetSpec::new("triple", "generic", "", "", 64, Endianness::Little),
            Err(TargetSpecError::MissingDataLayout)
        );
        assert_eq!(
            TargetSpec::new("triple", "generic", "", "layout", 16, Endianness::Little),
            Err(TargetSpecError::InvalidPointerWidth(16))
        );
    }

    #[test]
    fn builder_records_value_origins_and_block_parameters() {
        let mut builder = IrBuilder::new(TargetSpec::for_test_64());
        let i32 = builder.types().i32();
        let global = builder.add_global(IrGlobal {
            symbol: "gane.global".to_owned(),
            typ: i32,
            initializer: GlobalInitializer::Zero,
        });
        assert_eq!(global.raw(), 1);
        let function = declare_function(&mut builder, vec![IrParameter { typ: i32 }]);
        builder.set_entry(function).unwrap();
        let entry = builder.entry_block(function).unwrap();
        assert_eq!(
            builder.entry_parameters(function).unwrap(),
            vec![ValueId::from_raw(1)]
        );
        let slot = builder
            .add_stack_slot(function, i32, Some("value".to_owned()), None)
            .unwrap();
        let pointer = builder.add_type(IrTypeKind::Ptr {
            pointee: i32,
            address_space: 0,
        });
        let value = builder
            .append_instruction(
                function,
                entry,
                InstructionKind::StackAddr { slot },
                [pointer],
                None,
            )
            .unwrap();
        assert!(matches!(
            builder.append_block_parameter(function, entry, i32, None),
            Err(BuildError::BlockParametersSealed { .. })
        ));
        assert!(
            builder
                .append_instruction(
                    function,
                    entry,
                    InstructionKind::Store {
                        pointer: ValueId::INVALID,
                        value: ValueId::INVALID,
                    },
                    [],
                    None,
                )
                .unwrap()
                .is_empty()
        );
        let join = builder.create_block(function).unwrap();
        let parameter = builder
            .append_block_parameter(function, join, i32, None)
            .unwrap();
        builder
            .set_terminator(
                function,
                entry,
                Terminator::Branch {
                    target: join,
                    arguments: Vec::new(),
                },
            )
            .unwrap();
        builder
            .set_terminator(function, join, Terminator::Return { values: Vec::new() })
            .unwrap();

        let package = builder.finish().unwrap();
        let function = package.function(function).unwrap();
        assert_eq!(function.blocks[0].parameters[0].raw(), 1);
        assert_eq!(value[0].raw(), 2);
        assert_eq!(function.blocks[1].parameters, vec![parameter]);
        assert_eq!(
            function.values[1].origin,
            ValueOrigin::InstructionResult {
                block: entry,
                instruction_index: 0,
                result_index: 0,
            }
        );
        assert_eq!(
            function.values[2].origin,
            ValueOrigin::BlockParameter {
                block: join,
                parameter_index: 0,
            }
        );
    }

    #[test]
    fn builder_requires_terminated_blocks_and_complete_types() {
        let mut builder = IrBuilder::new(TargetSpec::for_test_64());
        let unfinished = builder.reserve_type();
        let function = declare_function(&mut builder, Vec::new());
        builder.set_entry(function).unwrap();

        assert!(matches!(
            builder.finish(),
            Err(BuildError::UnfinishedType(typ)) if typ == unfinished
        ));

        let mut builder = IrBuilder::new(TargetSpec::for_test_64());
        let function = declare_function(&mut builder, Vec::new());
        builder.set_entry(function).unwrap();
        let entry = builder.entry_block(function).unwrap();
        assert!(matches!(
            builder.finish(),
            Err(BuildError::UnterminatedBlock {
                function: actual_function,
                block: actual_block,
            }) if actual_function == function && actual_block == entry
        ));
    }

    #[test]
    fn builder_seals_terminated_blocks_and_supports_pointer_recursion() {
        let mut builder = IrBuilder::new(TargetSpec::for_test_64());
        let node = builder.reserve_type();
        let node_pointer = builder.add_type(IrTypeKind::Ptr {
            pointee: node,
            address_space: 0,
        });
        builder
            .define_type(
                node,
                IrTypeKind::Struct {
                    fields: vec![node_pointer],
                },
            )
            .unwrap();
        let function = declare_function(&mut builder, Vec::new());
        builder.set_entry(function).unwrap();
        let entry = builder.entry_block(function).unwrap();
        builder
            .set_terminator(function, entry, Terminator::Unreachable)
            .unwrap();
        let i32 = builder.types().i32();
        assert_eq!(
            builder.append_instruction(
                function,
                entry,
                InstructionKind::Const {
                    value: Constant::Integer(0),
                    typ: i32,
                },
                [i32],
                None,
            ),
            Err(BuildError::BlockTerminated {
                function,
                block: entry,
            })
        );
        assert!(builder.finish().unwrap().types().get(node).is_some());
    }
}
