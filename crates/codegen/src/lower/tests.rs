// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Hazuki Keatsu

use std::{fs, process::Command};

use gane_ir::{
    BinaryOp, Constant, FunctionAttributes, InstructionKind, IrBuilder, IrSignature, IrTypeKind,
    TargetSpec, Terminator, TrapReason, verify_package,
};

use super::*;

fn verified_program(backend: &LlvmBackend) -> VerifiedIrPackage {
    let mut builder = IrBuilder::new(backend.target_spec().clone());
    let i64 = builder.types().i64();
    let pointer = builder.add_type(IrTypeKind::Ptr {
        pointee: i64,
        address_space: 0,
    });
    let function = builder.declare_function(
        "gane.main".into(),
        IrSignature {
            parameters: Vec::new(),
            results: Vec::new(),
        },
        FunctionAttributes::default(),
    );
    builder.set_entry(function).unwrap();
    let entry = builder.entry_block(function).unwrap();
    let join = builder.create_block(function).unwrap();
    let parameter = builder
        .append_block_parameter(function, join, i64, None)
        .unwrap();
    let slot = builder.add_stack_slot(function, i64, None, None).unwrap();
    let value = builder
        .append_instruction(
            function,
            entry,
            InstructionKind::Const {
                value: Constant::Integer(41),
                typ: i64,
            },
            [i64],
            None,
        )
        .unwrap()[0];
    let min = builder
        .append_instruction(
            function,
            entry,
            InstructionKind::Const {
                value: Constant::Integer(1_u64 << 63),
                typ: i64,
            },
            [i64],
            None,
        )
        .unwrap()[0];
    let minus_one = builder
        .append_instruction(
            function,
            entry,
            InstructionKind::Const {
                value: Constant::Integer(u64::MAX),
                typ: i64,
            },
            [i64],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            function,
            entry,
            InstructionKind::Binary {
                op: BinaryOp::SignedDiv,
                left: min,
                right: minus_one,
            },
            [i64],
            None,
        )
        .unwrap();
    builder
        .set_terminator(
            function,
            entry,
            Terminator::Branch {
                target: join,
                arguments: vec![value],
            },
        )
        .unwrap();
    let address = builder
        .append_instruction(
            function,
            join,
            InstructionKind::StackAddr { slot },
            [pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            function,
            join,
            InstructionKind::Store {
                pointer: address,
                value: parameter,
            },
            [],
            None,
        )
        .unwrap();
    builder
        .set_terminator(function, join, Terminator::Return { values: Vec::new() })
        .unwrap();
    verify_package(builder.finish().unwrap()).unwrap()
}

fn aggregate_program(backend: &LlvmBackend) -> VerifiedIrPackage {
    let mut builder = IrBuilder::new(backend.target_spec().clone());
    let i64 = builder.types().i64();
    let array = builder.add_type(IrTypeKind::Array {
        length: 2,
        element: i64,
    });
    let pointer = builder.add_type(IrTypeKind::Ptr {
        pointee: array,
        address_space: 0,
    });
    let function = builder.declare_function(
        "gane.main".into(),
        IrSignature {
            parameters: Vec::new(),
            results: Vec::new(),
        },
        FunctionAttributes::default(),
    );
    builder.set_entry(function).unwrap();
    let entry = builder.entry_block(function).unwrap();
    let source_slot = builder.add_stack_slot(function, array, None, None).unwrap();
    let destination_slot = builder.add_stack_slot(function, array, None, None).unwrap();
    let source = builder
        .append_instruction(
            function,
            entry,
            InstructionKind::StackAddr { slot: source_slot },
            [pointer],
            None,
        )
        .unwrap()[0];
    let destination = builder
        .append_instruction(
            function,
            entry,
            InstructionKind::StackAddr {
                slot: destination_slot,
            },
            [pointer],
            None,
        )
        .unwrap()[0];
    builder
        .append_instruction(
            function,
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
        .set_terminator(function, entry, Terminator::Return { values: Vec::new() })
        .unwrap();
    verify_package(builder.finish().unwrap()).unwrap()
}

fn trap_program(backend: &LlvmBackend) -> VerifiedIrPackage {
    let mut builder = IrBuilder::new(backend.target_spec().clone());
    let function = builder.declare_function(
        "gane.main".into(),
        IrSignature {
            parameters: Vec::new(),
            results: Vec::new(),
        },
        FunctionAttributes::default(),
    );
    builder.set_entry(function).unwrap();
    let entry = builder.entry_block(function).unwrap();
    builder
        .set_terminator(
            function,
            entry,
            Terminator::Trap {
                reason: TrapReason::ExplicitPanic,
            },
        )
        .unwrap();
    verify_package(builder.finish().unwrap()).unwrap()
}

fn run_lli(llvm_ir: &str) -> Option<std::process::ExitStatus> {
    let lli = std::env::var_os("LLVM_SYS_221_PREFIX")
        .map(|prefix| std::path::PathBuf::from(prefix).join("bin/lli"))
        .or_else(|| command_in_path("lli"))?;
    let path = std::env::temp_dir().join(format!(
        "gane-codegen-{}-{:?}.ll",
        std::process::id(),
        std::thread::current().id()
    ));
    fs::write(&path, llvm_ir).unwrap();
    let status = Command::new(lli).arg(&path).output().unwrap().status;
    fs::remove_file(path).unwrap();
    Some(status)
}

fn command_in_path(name: &str) -> Option<std::path::PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

#[test]
fn builds_phi_and_stack_slots_with_inkwell() {
    let backend = LlvmBackend::for_host().unwrap();
    let package = verified_program(&backend);
    assert_eq!(gane_interpreter::interpret(&package), Ok(()));
    let llvm_ir = backend.emit_llvm_ir(&package).unwrap();
    assert!(llvm_ir.contains("phi i64"));
    assert!(!llvm_ir.contains("nsw"));
    assert!(run_lli(&llvm_ir).is_none_or(|status| status.success()));
}

#[test]
fn builds_aggregate_copy_with_target_data_memmove() {
    let backend = LlvmBackend::for_host().unwrap();
    let package = aggregate_program(&backend);
    assert_eq!(gane_interpreter::interpret(&package), Ok(()));
    let llvm_ir = backend.emit_llvm_ir(&package).unwrap();
    assert!(llvm_ir.contains("llvm.memmove.p0.p0"));
    assert!(run_lli(&llvm_ir).is_none_or(|status| status.success()));
}

#[test]
fn emits_explicit_traps() {
    let backend = LlvmBackend::for_host().unwrap();
    let package = trap_program(&backend);
    assert!(matches!(
        gane_interpreter::interpret(&package),
        Err(gane_interpreter::InterpreterError::Trap {
            reason: TrapReason::ExplicitPanic,
            ..
        })
    ));
    let llvm_ir = backend.emit_llvm_ir(&package).unwrap();
    assert!(llvm_ir.contains("@__gane_trap"));
    assert!(run_lli(&llvm_ir).is_none_or(|status| !status.success()));
}

#[test]
fn rejects_a_non_host_target() {
    let backend = LlvmBackend::for_host().unwrap();
    let mut builder = IrBuilder::new(TargetSpec::for_test_64());
    let function = builder.declare_function(
        "gane.main".into(),
        IrSignature {
            parameters: Vec::new(),
            results: Vec::new(),
        },
        FunctionAttributes::default(),
    );
    builder.set_entry(function).unwrap();
    let entry = builder.entry_block(function).unwrap();
    builder
        .set_terminator(function, entry, Terminator::Return { values: Vec::new() })
        .unwrap();
    let package = verify_package(builder.finish().unwrap()).unwrap();
    assert!(matches!(
        backend.emit_llvm_ir(&package),
        Err(CodegenError::TargetMismatch { .. })
    ));
}
