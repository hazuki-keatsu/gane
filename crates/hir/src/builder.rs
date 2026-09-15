use crate::{
    id::{BlockId, FunctionId, GlobalId, StackSlotId, TypeId, ValueId},
    ir::{
        FunctionAttributes, HirBlock, HirFunction, HirGlobal, HirPackage, HirSignature,
        Instruction, InstructionKind, Linkage, PassingMode, StackSlot, Terminator,
        UnverifiedHirPackage, ValueDef, ValueOrigin,
    },
    target::TargetSpec,
    types::{HirTypeKind, SourceOrigin, Symbol, TypeArena, TypeArenaError},
};
use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub struct HirBuilder {
    package: HirPackage,
    functions: Vec<FunctionBuildState>,
}

#[derive(Debug)]
struct FunctionBuildState {
    blocks: Vec<BlockBuildState>,
}

#[derive(Debug, Default)]
struct BlockBuildState {
    terminated: bool,
    has_instructions: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildError {
    InvalidFunction(FunctionId),
    InvalidBlock {
        function: FunctionId,
        block: BlockId,
    },
    BlockParametersSealed {
        function: FunctionId,
        block: BlockId,
    },
    BlockTerminated {
        function: FunctionId,
        block: BlockId,
    },
    MissingEntry,
    UnterminatedBlock {
        function: FunctionId,
        block: BlockId,
    },
    UnfinishedType(TypeId),
    Type(TypeArenaError),
}

impl HirBuilder {
    pub fn new(target: TargetSpec) -> Self {
        Self {
            package: HirPackage {
                target,
                types: TypeArena::new(),
                globals: Vec::new(),
                functions: Vec::new(),
                entry: FunctionId::INVALID,
            },
            functions: Vec::new(),
        }
    }

    pub fn types(&self) -> &TypeArena {
        &self.package.types
    }

    pub fn add_type(&mut self, kind: HirTypeKind) -> TypeId {
        self.package.types.alloc(kind)
    }

    pub fn reserve_type(&mut self) -> TypeId {
        self.package.types.reserve()
    }

    pub fn define_type(&mut self, id: TypeId, kind: HirTypeKind) -> Result<(), BuildError> {
        self.package
            .types
            .define(id, kind)
            .map_err(BuildError::Type)
    }

    pub fn add_global(&mut self, global: HirGlobal) -> GlobalId {
        self.package.globals.push(global);
        GlobalId::from_raw(self.package.globals.len() as u32)
    }

    pub fn declare_function(
        &mut self,
        symbol: Symbol,
        signature: HirSignature,
        linkage: Linkage,
        attributes: FunctionAttributes,
    ) -> FunctionId {
        let entry = BlockId::from_raw(1);
        let mut function = HirFunction {
            symbol,
            signature,
            linkage,
            attributes,
            stack_slots: Vec::new(),
            values: Vec::new(),
            blocks: vec![HirBlock {
                parameters: Vec::new(),
                instructions: Vec::new(),
                terminator: Terminator::Unreachable,
            }],
            entry,
        };
        let parameter_types = function
            .signature
            .parameters
            .iter()
            .map(|parameter| match parameter.passing {
                PassingMode::Direct => parameter.typ,
                PassingMode::IndirectByValue => self.add_type(HirTypeKind::Ptr {
                    pointee: parameter.typ,
                    address_space: 0,
                }),
            })
            .collect::<Vec<_>>();
        for typ in parameter_types {
            let index = function.blocks[0].parameters.len() as u32;
            let value = ValueId::from_raw(function.values.len() as u32 + 1);
            function.values.push(ValueDef {
                typ,
                origin: ValueOrigin::BlockParameter {
                    block: entry,
                    index,
                },
                source: None,
            });
            function.blocks[0].parameters.push(value);
        }

        self.package.functions.push(function);
        self.functions.push(FunctionBuildState {
            blocks: vec![BlockBuildState::default()],
        });
        FunctionId::from_raw(self.package.functions.len() as u32)
    }

    pub fn set_entry(&mut self, function: FunctionId) -> Result<(), BuildError> {
        self.function(function)?;
        self.package.entry = function;
        Ok(())
    }

    pub fn entry_block(&self, function: FunctionId) -> Result<BlockId, BuildError> {
        Ok(self.function(function)?.entry)
    }

    pub fn add_stack_slot(
        &mut self,
        function: FunctionId,
        typ: TypeId,
        name: Option<Symbol>,
        origin: SourceOrigin,
    ) -> Result<StackSlotId, BuildError> {
        let function = self.function_mut(function)?;
        function.stack_slots.push(StackSlot { typ, name, origin });
        Ok(StackSlotId::from_raw(function.stack_slots.len() as u32))
    }

    pub fn create_block(&mut self, function: FunctionId) -> Result<BlockId, BuildError> {
        let block = {
            let function_data = self.function_mut(function)?;
            function_data.blocks.push(HirBlock {
                parameters: Vec::new(),
                instructions: Vec::new(),
                terminator: Terminator::Unreachable,
            });
            BlockId::from_raw(function_data.blocks.len() as u32)
        };
        self.function_state_mut(function)?
            .blocks
            .push(BlockBuildState::default());
        Ok(block)
    }

    pub fn append_block_parameter(
        &mut self,
        function: FunctionId,
        block: BlockId,
        typ: TypeId,
        source: SourceOrigin,
    ) -> Result<ValueId, BuildError> {
        let state = self.block_state(function, block)?;
        if state.has_instructions || state.terminated {
            return Err(BuildError::BlockParametersSealed { function, block });
        }
        let block_index = Self::block_index(function, block)?;
        let function_data = self.function_mut(function)?;
        let index = function_data.blocks[block_index].parameters.len() as u32;
        let value = ValueId::from_raw(function_data.values.len() as u32 + 1);
        function_data.values.push(ValueDef {
            typ,
            origin: ValueOrigin::BlockParameter { block, index },
            source,
        });
        function_data.blocks[block_index].parameters.push(value);
        Ok(value)
    }

    pub fn append_instruction(
        &mut self,
        function: FunctionId,
        block: BlockId,
        kind: InstructionKind,
        result_types: impl IntoIterator<Item = TypeId>,
        source: SourceOrigin,
    ) -> Result<Vec<ValueId>, BuildError> {
        if self.block_state(function, block)?.terminated {
            return Err(BuildError::BlockTerminated { function, block });
        }
        let block_index = Self::block_index(function, block)?;
        let function_data = self.function_mut(function)?;
        let instruction = function_data.blocks[block_index].instructions.len() as u32;
        let results = result_types
            .into_iter()
            .enumerate()
            .map(|(index, typ)| {
                let value = ValueId::from_raw(function_data.values.len() as u32 + 1);
                function_data.values.push(ValueDef {
                    typ,
                    origin: ValueOrigin::InstructionResult {
                        block,
                        instruction,
                        index: index as u32,
                    },
                    source,
                });
                value
            })
            .collect::<Vec<_>>();
        function_data.blocks[block_index]
            .instructions
            .push(Instruction {
                results: results.clone(),
                kind,
                source,
            });
        self.block_state_mut(function, block)?.has_instructions = true;
        Ok(results)
    }

    pub fn set_terminator(
        &mut self,
        function: FunctionId,
        block: BlockId,
        terminator: Terminator,
    ) -> Result<(), BuildError> {
        if self.block_state(function, block)?.terminated {
            return Err(BuildError::BlockTerminated { function, block });
        }
        let block_index = Self::block_index(function, block)?;
        self.function_mut(function)?.blocks[block_index].terminator = terminator;
        self.block_state_mut(function, block)?.terminated = true;
        Ok(())
    }

    pub fn finish(self) -> Result<UnverifiedHirPackage, BuildError> {
        if let Some(typ) = self.package.types.unfinished() {
            return Err(BuildError::UnfinishedType(typ));
        }
        if !self.package.entry.is_valid() {
            return Err(BuildError::MissingEntry);
        }
        for (function_index, state) in self.functions.iter().enumerate() {
            for (block_index, block) in state.blocks.iter().enumerate() {
                if !block.terminated {
                    return Err(BuildError::UnterminatedBlock {
                        function: FunctionId::from_raw(function_index as u32 + 1),
                        block: BlockId::from_raw(block_index as u32 + 1),
                    });
                }
            }
        }
        Ok(UnverifiedHirPackage::from_inner(self.package))
    }

    fn function(&self, id: FunctionId) -> Result<&HirFunction, BuildError> {
        let index = id
            .raw()
            .checked_sub(1)
            .ok_or(BuildError::InvalidFunction(id))? as usize;
        self.package
            .functions
            .get(index)
            .ok_or(BuildError::InvalidFunction(id))
    }

    fn function_mut(&mut self, id: FunctionId) -> Result<&mut HirFunction, BuildError> {
        let index = id
            .raw()
            .checked_sub(1)
            .ok_or(BuildError::InvalidFunction(id))? as usize;
        self.package
            .functions
            .get_mut(index)
            .ok_or(BuildError::InvalidFunction(id))
    }

    fn function_state_mut(
        &mut self,
        id: FunctionId,
    ) -> Result<&mut FunctionBuildState, BuildError> {
        let index = id
            .raw()
            .checked_sub(1)
            .ok_or(BuildError::InvalidFunction(id))? as usize;
        self.functions
            .get_mut(index)
            .ok_or(BuildError::InvalidFunction(id))
    }

    fn block_state(
        &self,
        function: FunctionId,
        block: BlockId,
    ) -> Result<&BlockBuildState, BuildError> {
        let function_index = function
            .raw()
            .checked_sub(1)
            .ok_or(BuildError::InvalidFunction(function))? as usize;
        let block_index = Self::block_index(function, block)?;
        self.functions
            .get(function_index)
            .and_then(|function_state| function_state.blocks.get(block_index))
            .ok_or(BuildError::InvalidBlock { function, block })
    }

    fn block_state_mut(
        &mut self,
        function: FunctionId,
        block: BlockId,
    ) -> Result<&mut BlockBuildState, BuildError> {
        let function_index = function
            .raw()
            .checked_sub(1)
            .ok_or(BuildError::InvalidFunction(function))? as usize;
        let block_index = Self::block_index(function, block)?;
        self.functions
            .get_mut(function_index)
            .and_then(|function_state| function_state.blocks.get_mut(block_index))
            .ok_or(BuildError::InvalidBlock { function, block })
    }

    fn block_index(function: FunctionId, block: BlockId) -> Result<usize, BuildError> {
        if !function.is_valid() {
            return Err(BuildError::InvalidFunction(function));
        }
        block
            .raw()
            .checked_sub(1)
            .map(|index| index as usize)
            .ok_or(BuildError::InvalidBlock { function, block })
    }
}

impl fmt::Display for BuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl Error for BuildError {}
