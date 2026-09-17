use gane_ir::{Endianness, TargetSpec};
use inkwell::{
    OptimizationLevel,
    targets::{ByteOrdering, CodeModel, InitializationConfig, RelocMode, Target, TargetMachine},
};

use crate::{CodegenError, LlvmBackend};

pub(crate) fn for_host() -> Result<LlvmBackend, CodegenError> {
    Target::initialize_native(&InitializationConfig::default())
        .map_err(CodegenError::TargetInitialization)?;
    let triple = TargetMachine::normalize_triple(&TargetMachine::get_default_triple());
    let triple_text = triple.as_str().to_string_lossy().into_owned();
    let cpu = TargetMachine::get_host_cpu_name().to_string();
    let features = TargetMachine::get_host_cpu_features().to_string();
    let target = Target::from_triple(&triple)
        .map_err(|error| CodegenError::TargetInitialization(error.to_string()))?;
    let machine = target
        .create_target_machine(
            &triple,
            &cpu,
            &features,
            OptimizationLevel::None,
            RelocMode::Default,
            CodeModel::Default,
        )
        .ok_or_else(|| {
            CodegenError::TargetInitialization("could not create TargetMachine".into())
        })?;
    let data = machine.get_target_data();
    let data_layout = data
        .get_data_layout()
        .as_str()
        .to_string_lossy()
        .into_owned();
    let pointer_width = u8::try_from(data.get_pointer_byte_size(None) * 8)
        .map_err(|error| CodegenError::TargetInitialization(error.to_string()))?;
    let endianness = match data.get_byte_ordering() {
        ByteOrdering::LittleEndian => Endianness::Little,
        ByteOrdering::BigEndian => Endianness::Big,
    };
    let target = TargetSpec::new(
        triple_text,
        cpu,
        features,
        data_layout,
        pointer_width,
        endianness,
    )
    .map_err(|error| CodegenError::TargetInitialization(error.to_string()))?;
    Ok(LlvmBackend {
        _machine: machine,
        data,
        target,
    })
}
