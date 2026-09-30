use crate::ir::IrPackage;
use crate::{
    BinaryOp, BlockId, Callee, ComparePredicate, Constant, FunctionId, GlobalInitializer,
    Instruction, InstructionKind, IntCastKind, IrFunction, IrParameter, IrTypeKind, Terminator,
    TypeId, UnaryOp, UnverifiedIrPackage, ValueId, ValueOrigin, VerifiedIrPackage,
};
use std::collections::{BTreeSet, HashSet, VecDeque};
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
/// A verifier failure located in deterministic textual IR coordinates.
pub struct IrDiagnostic {
    location: String,
    message: String,
}

impl fmt::Display for IrDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.location, self.message)
    }
}

impl IrDiagnostic {
    #[allow(dead_code)]
    pub(crate) fn new(location: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            location: location.into(),
            message: message.into(),
        }
    }

    /// Returns the package, function, block, or instruction containing the failure.
    pub fn location(&self) -> &str {
        &self.location
    }

    /// Returns the invariant violation.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::error::Error for IrDiagnostic {}

/// Checks all ordinary IR invariants without performing escape analysis.
pub fn verify(package: &UnverifiedIrPackage) -> Result<(), Vec<IrDiagnostic>> {
    let mut verifier = Verifier {
        package: package.inner(),
        diagnostics: Vec::new(),
    };
    verifier.verify_package();
    if verifier.diagnostics.is_empty() {
        Ok(())
    } else {
        Err(verifier.diagnostics)
    }
}

/// Verifies a raw package and makes it available to IR consumers.
pub fn verify_package(
    package: UnverifiedIrPackage,
) -> Result<VerifiedIrPackage, Vec<IrDiagnostic>> {
    verify(&package)?;
    Ok(VerifiedIrPackage::from_inner(package.into_inner()))
}

struct Verifier<'a> {
    package: &'a IrPackage,
    diagnostics: Vec<IrDiagnostic>,
}

impl Verifier<'_> {
    fn verify_package(&mut self) {
        self.verify_types();
        self.verify_symbols_and_globals();
        self.verify_entry();
        for (index, function) in self.package.functions.iter().enumerate() {
            self.verify_function(FunctionId::from_raw(index as u32 + 1), function);
        }
    }

    /// Verify types, including:
    ///
    /// - whether primitive types are canonical
    /// - duplicate definition of primitive types
    /// - whether Ptr's address space is zero (v0 required)
    /// - whether array's length is zero
    /// - whether the array's max length is more than pointer width
    /// - whether array's element is void
    /// - whether struct's fields are empty
    /// - whether struct's fields include void value
    fn verify_types(&mut self) {
        let primitive = [
            IrTypeKind::Void,
            IrTypeKind::I1,
            IrTypeKind::I8,
            IrTypeKind::I16,
            IrTypeKind::I32,
            IrTypeKind::I64,
        ];
        // Make sure all the primitive types have been embedded
        for (index, expected) in primitive.iter().enumerate() {
            let id = TypeId::from_raw(index as u32 + 1);
            if self.package.types.get(id).map(|typ| &typ.kind) != Some(expected) {
                self.error(
                    format!("type !{}", id.raw()),
                    "primitive type is not canonical",
                );
            }
        }
        for (id, typ) in self.package.types.iter() {
            if id.raw() > 6 && primitive.contains(&typ.kind) {
                self.error(format!("type !{}", id.raw()), "duplicate primitive type");
            }
            match &typ.kind {
                IrTypeKind::Ptr {
                    pointee,
                    address_space,
                } => {
                    self.type_exists(*pointee, format!("type !{}", id.raw()));
                    if *address_space != 0 {
                        self.error(
                            format!("type !{}", id.raw()),
                            "address space must be zero in IR V0",
                        );
                    }
                }
                IrTypeKind::Array { length, element } => {
                    self.type_exists(*element, format!("type !{}", id.raw()));
                    if *length == 0 {
                        self.error(
                            format!("type !{}", id.raw()),
                            "array length must be non-zero",
                        );
                    }
                    let max = if self.package.target.pointer_width() == 32 {
                        u32::MAX as u64
                    } else {
                        u64::MAX
                    };
                    if *length > max {
                        self.error(
                            format!("type !{}", id.raw()),
                            "array length does not fit pointer width",
                        );
                    }
                    if self.is_void(*element) {
                        self.error(
                            format!("type !{}", id.raw()),
                            "array element cannot be void",
                        );
                    }
                }
                IrTypeKind::Struct { fields } => {
                    if fields.is_empty() {
                        self.error(
                            format!("type !{}", id.raw()),
                            "struct must have non-zero size",
                        );
                    }
                    for field in fields {
                        self.type_exists(*field, format!("type !{}", id.raw()));
                        if self.is_void(*field) {
                            self.error(
                                format!("type !{}", id.raw()),
                                "struct field cannot be void",
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        let mut visiting = HashSet::new();
        let mut visited = HashSet::new();
        for (id, _) in self.package.types.iter() {
            if self.has_value_cycle(id, &mut visiting, &mut visited) {
                self.error(
                    format!("type !{}", id.raw()),
                    "type has recursive by-value layout",
                );
            }
        }
    }

    /// Use DFS and two sets to detect ir or not there is any value recursion.
    fn has_value_cycle(
        &self,
        id: TypeId,
        visiting: &mut HashSet<TypeId>,
        visited: &mut HashSet<TypeId>,
    ) -> bool {
        if visited.contains(&id) {
            return false;
        }
        if !visiting.insert(id) {
            return true;
        }
        let cycle = match self.type_kind(id) {
            Some(IrTypeKind::Array { element, .. }) => {
                self.has_value_cycle(*element, visiting, visited)
            }
            Some(IrTypeKind::Struct { fields }) => fields
                .iter()
                .any(|field| self.has_value_cycle(*field, visiting, visited)),
            _ => false,
        };
        visiting.remove(&id);
        if !cycle {
            visited.insert(id);
        }
        cycle
    }

    /// Verify the uniqueness of symbols and the validness of global variable
    ///
    /// - whether global variables are repeated
    /// - whether global variable's type is [`IrTypeKind::Void`] or [`None`]
    /// - whether global variable is without initializer
    /// - whether function name has the same name with global variable
    fn verify_symbols_and_globals(&mut self) {
        // A package-level symbol table
        let mut symbols = HashSet::new();
        for (index, global) in self.package.globals.iter().enumerate() {
            let location = format!("global @{}", index + 1);
            if !symbols.insert(global.symbol.as_str()) {
                self.error(location.clone(), "duplicate symbol");
            }
            self.non_void_type(global.typ, location.clone(), "global");
            match global.initializer {
                GlobalInitializer::Zero => {}
                GlobalInitializer::Scalar(value) => {
                    self.verify_constant(value, global.typ, location)
                }
            }
        }
        for (index, function) in self.package.functions.iter().enumerate() {
            if !symbols.insert(function.symbol.as_str()) {
                self.error(format!("function @{}", index + 1), "duplicate symbol");
            }
        }
    }

    fn verify_entry(&mut self) {
        let Some(entry) = self
            .package
            .entry
            .raw()
            .checked_sub(1)
            .and_then(|index| self.package.functions.get(index as usize))
        else {
            self.error("package", "entry function is invalid");
            return;
        };
        if entry.symbol != "gane.main"
            || !entry.signature.parameters.is_empty()
            || !entry.signature.results.is_empty()
        {
            self.error("package", "entry must be Gane ABI void gane.main()");
        }
    }

    fn verify_function(&mut self, id: FunctionId, function: &IrFunction) {
        let prefix = format!("function @{}", id.raw());
        if function
            .entry
            .raw()
            .checked_sub(1)
            .and_then(|index| function.blocks.get(index as usize))
            .is_none()
        {
            self.error(prefix.clone(), "function entry block is invalid");
            return;
        }
        if function.signature.results.len() > 1 {
            self.error(prefix.clone(), "IR V0 functions have at most one result");
        }
        if function.attributes.no_return && !function.signature.results.is_empty() {
            self.error(prefix.clone(), "no_return function cannot declare results");
        }
        for parameter in &function.signature.parameters {
            self.verify_parameter(parameter, prefix.clone());
        }
        for result in &function.signature.results {
            self.scalar_type(*result, prefix.clone(), "function result");
        }
        for (slot_index, slot) in function.stack_slots.iter().enumerate() {
            self.non_void_type(
                slot.typ,
                format!("{prefix} slot ${}", slot_index + 1),
                "stack slot",
            );
        }
        for (block_index, block) in function.blocks.iter().enumerate() {
            if targets(&block.terminator).contains(&function.entry) {
                self.error(
                    format!("{prefix} block ^{} terminator", block_index + 1),
                    "function entry block cannot be a branch target",
                );
            }
        }
        self.verify_definitions(id, function);
        let (reachable, dominators) = self.control_flow(id, function);
        for (block_index, block) in function.blocks.iter().enumerate() {
            let block_id = BlockId::from_raw(block_index as u32 + 1);
            if block_id != function.entry
                && !block.parameters.is_empty()
                && !function
                    .blocks
                    .iter()
                    .any(|predecessor| targets(&predecessor.terminator).contains(&block_id))
            {
                self.error(
                    format!("{prefix} block ^{}", block_id.raw()),
                    "non-entry block parameters have no incoming edge",
                );
            }
            for (instruction_index, instruction) in block.instructions.iter().enumerate() {
                let location = format!(
                    "{prefix} block ^{} instruction {instruction_index}",
                    block_id.raw()
                );
                self.verify_uses(
                    function,
                    block_id,
                    instruction_index,
                    &reachable,
                    &dominators,
                    operands(&instruction.kind),
                    location.clone(),
                );
                self.verify_instruction(
                    id,
                    function,
                    block_id,
                    instruction_index,
                    instruction,
                    location,
                );
            }
            let location = format!("{prefix} block ^{} terminator", block_id.raw());
            self.verify_uses(
                function,
                block_id,
                block.instructions.len(),
                &reachable,
                &dominators,
                terminator_operands(&block.terminator),
                location.clone(),
            );
            self.verify_terminator(function, &block.terminator, location);
        }
    }

    fn verify_parameter(&mut self, parameter: &IrParameter, location: String) {
        self.scalar_type(parameter.typ, location, "function parameter");
    }

    fn verify_definitions(&mut self, function_id: FunctionId, function: &IrFunction) {
        let prefix = format!("function @{}", function_id.raw());
        let mut seen = vec![false; function.values.len()];
        for (block_index, block_data) in function.blocks.iter().enumerate() {
            let block = BlockId::from_raw(block_index as u32 + 1);
            for (index, value) in block_data.parameters.iter().enumerate() {
                self.definition(
                    function,
                    &mut seen,
                    *value,
                    ValueOrigin::BlockParameter {
                        block,
                        parameter_index: index as u32,
                    },
                    format!("{prefix} block ^{}", block.raw()),
                );
            }
            for (instruction_index, instruction) in
                function.blocks[block_index].instructions.iter().enumerate()
            {
                for (result_index, value) in instruction.results.iter().enumerate() {
                    self.definition(
                        function,
                        &mut seen,
                        *value,
                        ValueOrigin::InstructionResult {
                            block,
                            instruction_index: instruction_index as u32,
                            result_index: result_index as u32,
                        },
                        format!(
                            "{prefix} block ^{} instruction {instruction_index}",
                            block.raw()
                        ),
                    );
                }
            }
        }
        for (index, was_seen) in seen.iter().enumerate() {
            if !was_seen {
                self.error(
                    prefix.clone(),
                    format!("value %{} has no definition", index + 1),
                );
            }
        }
        let entry = &function.blocks[function.entry.raw() as usize - 1];
        if entry.parameters.len() != function.signature.parameters.len() {
            self.error(
                prefix.clone(),
                "entry block parameters do not match signature",
            );
        } else {
            for (value, parameter) in entry.parameters.iter().zip(&function.signature.parameters) {
                let actual = function.value(*value).map(|value| value.typ);
                if actual != Some(parameter.typ) {
                    self.error(
                        prefix.clone(),
                        "entry block parameter type does not match signature",
                    );
                }
            }
        }
    }

    fn definition(
        &mut self,
        function: &IrFunction,
        seen: &mut [bool],
        value: ValueId,
        expected: ValueOrigin,
        location: String,
    ) {
        let Some(index) = value.raw().checked_sub(1).map(|index| index as usize) else {
            self.error(location, "definition uses invalid value ID");
            return;
        };
        let Some(was_seen) = seen.get_mut(index) else {
            self.error(location, "definition value ID is out of range");
            return;
        };
        if *was_seen {
            self.error(
                location.clone(),
                format!("value %{} is defined more than once", value.raw()),
            );
        }
        *was_seen = true;
        match function.value(value) {
            Some(definition) if definition.origin == expected => {
                self.non_void_type(definition.typ, location, "SSA value")
            }
            Some(_) => self.error(
                location,
                format!("value %{} origin does not match definition", value.raw()),
            ),
            None => self.error(location, "definition value ID is out of range"),
        }
    }

    fn control_flow(
        &mut self,
        function_id: FunctionId,
        function: &IrFunction,
    ) -> (Vec<bool>, Vec<BTreeSet<usize>>) {
        let count = function.blocks.len();
        let mut successors = vec![Vec::new(); count];
        let mut predecessors = vec![Vec::new(); count];
        for (index, block) in function.blocks.iter().enumerate() {
            for target in targets(&block.terminator) {
                let Some(target_index) = target
                    .raw()
                    .checked_sub(1)
                    .map(|value| value as usize)
                    .filter(|value| *value < count)
                else {
                    self.error(
                        format!(
                            "function @{} block ^{} terminator",
                            function_id.raw(),
                            index + 1
                        ),
                        "branch target is invalid",
                    );
                    continue;
                };
                successors[index].push(target_index);
                predecessors[target_index].push(index);
            }
        }
        let entry = function.entry.raw() as usize - 1;
        let mut reachable = vec![false; count];
        let mut queue = VecDeque::from([entry]);
        while let Some(block) = queue.pop_front() {
            if std::mem::replace(&mut reachable[block], true) {
                continue;
            }
            queue.extend(successors[block].iter().copied());
        }
        let all = (0..count)
            .filter(|index| reachable[*index])
            .collect::<BTreeSet<_>>();
        let mut dominators = vec![BTreeSet::new(); count];
        for index in 0..count {
            if reachable[index] {
                dominators[index] = if index == entry {
                    BTreeSet::from([entry])
                } else {
                    all.clone()
                };
            }
        }
        loop {
            let mut changed = false;
            for block in 0..count {
                if !reachable[block] || block == entry {
                    continue;
                }
                let mut incoming = predecessors[block]
                    .iter()
                    .copied()
                    .filter(|pred| reachable[*pred]);
                let mut next = incoming
                    .next()
                    .map(|pred| dominators[pred].clone())
                    .unwrap_or_default();
                for pred in incoming {
                    next = next.intersection(&dominators[pred]).copied().collect();
                }
                next.insert(block);
                if next != dominators[block] {
                    dominators[block] = next;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        (reachable, dominators)
    }

    fn verify_uses(
        &mut self,
        function: &IrFunction,
        block: BlockId,
        position: usize,
        reachable: &[bool],
        dominators: &[BTreeSet<usize>],
        uses: Vec<ValueId>,
        location: String,
    ) {
        let block_index = block.raw() as usize - 1;
        for value in uses {
            let Some(definition) = function.value(value) else {
                self.error(
                    location.clone(),
                    format!("value %{} is invalid", value.raw()),
                );
                continue;
            };
            let (definition_block, definition_position) = match definition.origin {
                ValueOrigin::BlockParameter { block, .. } => (block, None),
                ValueOrigin::InstructionResult {
                    block,
                    instruction_index,
                    ..
                } => (block, Some(instruction_index as usize)),
            };
            if definition_block == block {
                if definition_position.is_some_and(|defined| defined >= position) {
                    self.error(
                        location.clone(),
                        format!("value %{} is used before its definition", value.raw()),
                    );
                }
            } else if !reachable[block_index] {
                self.error(
                    location.clone(),
                    format!(
                        "unreachable block uses value %{} from another block",
                        value.raw()
                    ),
                );
            } else {
                let definition_index = definition_block.raw().saturating_sub(1) as usize;
                if definition_index >= reachable.len()
                    || !dominators[block_index].contains(&definition_index)
                {
                    self.error(
                        location.clone(),
                        format!("value %{} definition does not dominate use", value.raw()),
                    );
                }
            }
        }
    }

    fn verify_instruction(
        &mut self,
        function_id: FunctionId,
        function: &IrFunction,
        block: BlockId,
        index: usize,
        instruction: &Instruction,
        location: String,
    ) {
        let result_types = instruction
            .results
            .iter()
            .filter_map(|id| function.value(*id).map(|value| value.typ))
            .collect::<Vec<_>>();
        let value_type = |id| function.value(id).map(|value| value.typ);
        match &instruction.kind {
            InstructionKind::Const { value, typ } => {
                self.verify_constant(*value, *typ, location.clone());
                self.results(&result_types, &[*typ], location);
            }
            InstructionKind::Unary { op, operand } => {
                let typ = value_type(*operand);
                if typ.is_none_or(|typ| match op {
                    UnaryOp::LogicalNot => !self.is_i1(typ),
                    _ => !self.is_integer(typ),
                }) {
                    self.error(location.clone(), "invalid unary operand type");
                }
                self.results_option(&result_types, typ, location);
            }
            InstructionKind::Binary { op, left, right } => {
                let left = value_type(*left);
                let right = value_type(*right);
                let shift = matches!(
                    op,
                    BinaryOp::Shl | BinaryOp::ArithmeticShr | BinaryOp::LogicalShr
                );
                if left.is_none_or(|typ| !self.is_integer(typ))
                    || (!shift && left != right)
                    || (shift && right != Some(self.package.types.i64()))
                {
                    self.error(location.clone(), "invalid binary operand types");
                }
                self.results_option(&result_types, left, location);
            }
            InstructionKind::Compare {
                predicate,
                left,
                right,
            } => {
                let left = value_type(*left);
                let right = value_type(*right);
                let equality = matches!(
                    predicate,
                    ComparePredicate::Equal | ComparePredicate::NotEqual
                );
                let valid = left == right
                    && left.is_some_and(|typ| {
                        self.is_integer(typ)
                            || (equality && (self.is_i1(typ) || self.is_pointer(typ)))
                    });
                if !valid {
                    self.error(location.clone(), "invalid compare operand types");
                }
                self.results(&result_types, &[self.package.types.i1()], location);
            }
            InstructionKind::IntCast {
                kind,
                operand,
                target,
            } => {
                let source = value_type(*operand);
                let source_width = source.and_then(|typ| self.integer_width(typ));
                let target_width = self.integer_width(*target);
                let valid = match (kind, source_width, target_width) {
                    (IntCastKind::Truncate, Some(a), Some(b)) => b < a,
                    (IntCastKind::SignExtend | IntCastKind::ZeroExtend, Some(a), Some(b)) => b > a,
                    _ => false,
                };
                if !valid {
                    self.error(location.clone(), "invalid integer cast widths");
                }
                self.results(&result_types, &[*target], location);
            }
            InstructionKind::StackAddr { slot } => {
                let pointee = function.stack_slot(*slot).map(|slot| slot.typ);
                self.pointer_result(
                    &result_types,
                    pointee,
                    location,
                    "stack slot or pointer result type is invalid",
                );
            }
            InstructionKind::GlobalAddr { global } => {
                let pointee = global
                    .raw()
                    .checked_sub(1)
                    .and_then(|index| self.package.globals.get(index as usize))
                    .map(|global| global.typ);
                self.pointer_result(
                    &result_types,
                    pointee,
                    location,
                    "global or pointer result type is invalid",
                );
            }
            InstructionKind::GepField { base, field } => {
                let pointee = value_type(*base)
                    .and_then(|typ| self.pointee(typ))
                    .and_then(|typ| match self.type_kind(typ) {
                        Some(IrTypeKind::Struct { fields }) => fields.get(*field as usize).copied(),
                        _ => None,
                    });
                self.pointer_result(
                    &result_types,
                    pointee,
                    location,
                    "invalid gep_field base, field, or result",
                );
            }
            InstructionKind::GepIndex { base, index } => {
                let pointee = value_type(*base)
                    .and_then(|typ| self.pointee(typ))
                    .and_then(|typ| match self.type_kind(typ) {
                        Some(IrTypeKind::Array { element, .. }) => Some(*element),
                        _ => None,
                    });
                let index_type = value_type(*index);
                let wanted = if self.package.target.pointer_width() == 32 {
                    self.package.types.i32()
                } else {
                    self.package.types.i64()
                };
                if index_type != Some(wanted) {
                    self.error(location.clone(), "invalid gep_index index type");
                }
                self.pointer_result(
                    &result_types,
                    pointee,
                    location,
                    "invalid gep_index base or result",
                );
            }
            InstructionKind::Load { pointer } => {
                let expected = value_type(*pointer).and_then(|typ| self.pointee(typ));
                if expected.is_none_or(|typ| !self.is_scalar(typ)) {
                    self.error(location.clone(), "load requires a pointer to scalar");
                }
                self.results_option(&result_types, expected, location);
            }
            InstructionKind::Store { pointer, value } => {
                let expected = value_type(*pointer).and_then(|typ| self.pointee(typ));
                if expected != value_type(*value) || expected.is_none_or(|typ| !self.is_scalar(typ))
                {
                    self.error(
                        location.clone(),
                        "store types do not match a scalar pointee",
                    );
                }
                self.results(&result_types, &[], location);
            }
            InstructionKind::AggregateZero { destination, typ } => {
                if !self.is_aggregate(*typ)
                    || value_type(*destination).and_then(|value| self.pointee(value)) != Some(*typ)
                {
                    self.error(
                        location.clone(),
                        "aggregate_zero requires a matching aggregate pointer",
                    );
                }
                self.results(&result_types, &[], location);
            }
            InstructionKind::AggregateCopy {
                destination,
                source,
                typ,
            } => {
                if !self.is_aggregate(*typ)
                    || value_type(*destination).is_none_or(|value| !self.points_to(value, *typ))
                    || value_type(*source).is_none_or(|value| !self.points_to(value, *typ))
                {
                    self.error(
                        location.clone(),
                        "aggregate_copy requires matching aggregate pointers",
                    );
                }
                self.results(&result_types, &[], location);
            }
            InstructionKind::Call {
                callee: Callee::Function(callee),
                arguments,
            } => self.verify_call(
                function_id,
                function,
                block,
                index,
                *callee,
                arguments,
                &result_types,
                location,
            ),
        }
    }

    fn verify_call(
        &mut self,
        _function_id: FunctionId,
        function: &IrFunction,
        block: BlockId,
        index: usize,
        callee_id: FunctionId,
        arguments: &[ValueId],
        results: &[TypeId],
        location: String,
    ) {
        let Some(callee) = callee_id
            .raw()
            .checked_sub(1)
            .and_then(|index| self.package.functions.get(index as usize))
        else {
            self.error(location, "call target is invalid");
            return;
        };
        if arguments.len() != callee.signature.parameters.len() {
            self.error(
                location.clone(),
                "call argument count does not match signature",
            );
        }
        for (argument, parameter) in arguments.iter().zip(&callee.signature.parameters) {
            let actual = function.value(*argument).map(|value| value.typ);
            if actual != Some(parameter.typ) {
                self.error(
                    location.clone(),
                    "call argument type does not match signature",
                );
            }
        }
        self.results(results, &callee.signature.results, location.clone());
        if callee.attributes.no_return {
            let block = &function.blocks[block.raw() as usize - 1];
            if !results.is_empty()
                || index + 1 != block.instructions.len()
                || block.terminator != Terminator::Unreachable
            {
                self.error(
                    location,
                    "no_return call must be last, resultless, and end in unreachable",
                );
            }
        }
    }

    fn verify_terminator(
        &mut self,
        function: &IrFunction,
        terminator: &Terminator,
        location: String,
    ) {
        match terminator {
            Terminator::Branch { target, arguments } => {
                self.verify_edge(function, *target, arguments, location)
            }
            Terminator::CondBranch {
                condition,
                then_target,
                then_arguments,
                else_target,
                else_arguments,
            } => {
                if function.value(*condition).map(|value| value.typ)
                    != Some(self.package.types.i1())
                {
                    self.error(location.clone(), "condbr condition must be i1");
                }
                self.verify_edge(function, *then_target, then_arguments, location.clone());
                self.verify_edge(function, *else_target, else_arguments, location);
            }
            Terminator::Return { values } => {
                if function.attributes.no_return {
                    self.error(location.clone(), "no_return function cannot return");
                }
                if values.len() != function.signature.results.len()
                    || values
                        .iter()
                        .zip(&function.signature.results)
                        .any(|(value, typ)| {
                            function.value(*value).map(|value| value.typ) != Some(*typ)
                        })
                {
                    self.error(location, "return values do not match function signature");
                }
            }
            Terminator::Trap { .. } | Terminator::Unreachable => {}
        }
    }

    fn verify_edge(
        &mut self,
        function: &IrFunction,
        target: BlockId,
        arguments: &[ValueId],
        location: String,
    ) {
        let Some(block) = function.block(target) else {
            return;
        };
        if arguments.len() != block.parameters.len()
            || arguments
                .iter()
                .zip(&block.parameters)
                .any(|(argument, parameter)| {
                    function.value(*argument).map(|value| value.typ)
                        != function.value(*parameter).map(|value| value.typ)
                })
        {
            self.error(location, "branch arguments do not match target parameters");
        }
    }

    fn verify_constant(&mut self, value: Constant, typ: TypeId, location: String) {
        let valid = match value {
            Constant::Bool(_) => self.is_i1(typ),
            Constant::Integer(value) => self
                .integer_width(typ)
                .is_some_and(|width| width == 64 || value < (1_u64 << width)),
            Constant::Null => self.is_pointer(typ),
        };
        if !valid {
            self.error(location, "constant is not representable by its type");
        }
    }

    fn results(&mut self, actual: &[TypeId], expected: &[TypeId], location: String) {
        if actual != expected {
            self.error(location, "instruction result count or type is invalid");
        }
    }
    fn results_option(&mut self, actual: &[TypeId], expected: Option<TypeId>, location: String) {
        match expected {
            Some(typ) => self.results(actual, &[typ], location),
            None => {
                if actual.len() != 1 {
                    self.error(location, "instruction must have one result");
                }
            }
        }
    }
    fn pointer_result(
        &mut self,
        actual: &[TypeId],
        pointee: Option<TypeId>,
        location: String,
        message: &str,
    ) {
        if actual.len() != 1
            || pointee.is_none()
            || !pointee.is_some_and(|pointee| self.points_to(actual[0], pointee))
        {
            self.error(location, message);
        }
    }

    // ====== Helpers ======

    /// Test if or not [`TypeId`] is existing in [`TypeArena`](crate::types::TypeArena).
    /// If not, error will be put into [`Verifier::diagnostics`]
    fn type_exists(&mut self, typ: TypeId, location: String) {
        if self.type_kind(typ).is_none() {
            self.error(location, format!("type !{} is invalid", typ.raw()));
        }
    }
    /// Test if or not [`TypeId`] is [`IrTypeKind::Void`] or [`None`]
    fn non_void_type(&mut self, typ: TypeId, location: String, subject: &str) {
        if self.type_kind(typ).is_none() || self.is_void(typ) {
            self.error(
                location,
                format!("{subject} type must exist and cannot be void"),
            );
        }
    }
    fn scalar_type(&mut self, typ: TypeId, location: String, subject: &str) {
        if !self.is_scalar(typ) {
            self.error(location, format!("{subject} must have scalar type"));
        }
    }
    /// Get [`&IrTypeKind`](crate::types::IrTypeKind) by [`TypeId`]
    /// from [`TypeArena`](crate::types::TypeArena)
    fn type_kind(&self, typ: TypeId) -> Option<&IrTypeKind> {
        self.package.types.get(typ).map(|typ| &typ.kind)
    }
    /// Test if or not [`TypeId`] is [`IrTypeKind::Void`]
    fn is_void(&self, typ: TypeId) -> bool {
        matches!(self.type_kind(typ), Some(IrTypeKind::Void))
    }
    /// Test if or not [`TypeId`] is [`IrTypeKind::I1`]
    fn is_i1(&self, typ: TypeId) -> bool {
        matches!(self.type_kind(typ), Some(IrTypeKind::I1))
    }
    /// [`IrTypeKind::I8`], [`IrTypeKind::I16`], [`IrTypeKind::I32`] and [`IrTypeKind::I64`] are integer,
    /// but [`IrTypeKind::I1`].
    fn is_integer(&self, typ: TypeId) -> bool {
        self.integer_width(typ).is_some()
    }
    /// Get the integer width by [`TypeId`]
    fn integer_width(&self, typ: TypeId) -> Option<u32> {
        match self.type_kind(typ)? {
            IrTypeKind::I8 => Some(8),
            IrTypeKind::I16 => Some(16),
            IrTypeKind::I32 => Some(32),
            IrTypeKind::I64 => Some(64),
            _ => None,
        }
    }
    /// Test if or not [`TypeId`] is a [`IrTypeKind::Ptr`].
    fn is_pointer(&self, typ: TypeId) -> bool {
        matches!(self.type_kind(typ), Some(IrTypeKind::Ptr { .. }))
    }
    fn pointee(&self, typ: TypeId) -> Option<TypeId> {
        match self.type_kind(typ)? {
            IrTypeKind::Ptr { pointee, .. } => Some(*pointee),
            _ => None,
        }
    }
    fn is_aggregate(&self, typ: TypeId) -> bool {
        matches!(
            self.type_kind(typ),
            Some(IrTypeKind::Array { .. } | IrTypeKind::Struct { .. })
        )
    }
    /// Except for [`IrTypeKind::Void`], [`IrTypeKind::Array`] and [`IrTypeKind::Struct`],
    /// the rest of types are scalar.
    fn is_scalar(&self, typ: TypeId) -> bool {
        self.is_i1(typ) || self.is_integer(typ) || self.is_pointer(typ)
    }
    fn points_to(&self, pointer: TypeId, expected: TypeId) -> bool {
        self.pointee(pointer) == Some(expected)
    }
    fn error(&mut self, location: impl Into<String>, message: impl Into<String>) {
        self.diagnostics.push(IrDiagnostic {
            location: location.into(),
            message: message.into(),
        });
    }
}

fn operands(kind: &InstructionKind) -> Vec<ValueId> {
    match kind {
        InstructionKind::Const { .. }
        | InstructionKind::StackAddr { .. }
        | InstructionKind::GlobalAddr { .. } => vec![],
        InstructionKind::Unary { operand, .. }
        | InstructionKind::IntCast { operand, .. }
        | InstructionKind::Load { pointer: operand }
        | InstructionKind::GepField { base: operand, .. } => vec![*operand],
        InstructionKind::Binary { left, right, .. }
        | InstructionKind::Compare { left, right, .. } => vec![*left, *right],
        InstructionKind::GepIndex { base, index } => vec![*base, *index],
        InstructionKind::Store { pointer, value } => vec![*pointer, *value],
        InstructionKind::AggregateZero { destination, .. } => vec![*destination],
        InstructionKind::AggregateCopy {
            destination,
            source,
            ..
        } => vec![*destination, *source],
        InstructionKind::Call { arguments, .. } => arguments.clone(),
    }
}

fn terminator_operands(terminator: &Terminator) -> Vec<ValueId> {
    match terminator {
        Terminator::Branch { arguments, .. } => arguments.clone(),
        Terminator::CondBranch {
            condition,
            then_arguments,
            else_arguments,
            ..
        } => std::iter::once(*condition)
            .chain(then_arguments.iter().chain(else_arguments).copied())
            .collect(),
        Terminator::Return { values } => values.clone(),
        Terminator::Trap { .. } | Terminator::Unreachable => vec![],
    }
}
fn targets(terminator: &Terminator) -> Vec<BlockId> {
    match terminator {
        Terminator::Branch { target, .. } => vec![*target],
        Terminator::CondBranch {
            then_target,
            else_target,
            ..
        } => vec![*then_target, *else_target],
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FunctionAttributes, IrBuilder, IrGlobal, IrSignature, StackSlot};

    fn signature(parameters: Vec<IrParameter>, results: Vec<TypeId>) -> IrSignature {
        IrSignature {
            parameters,
            results,
        }
    }

    fn empty_main() -> UnverifiedIrPackage {
        let mut builder = IrBuilder::new(crate::TargetSpec::for_test_64());
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
        builder.finish().unwrap()
    }

    fn messages(package: &UnverifiedIrPackage) -> String {
        verify(package)
            .unwrap_err()
            .into_iter()
            .map(|diagnostic| diagnostic.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn accepts_well_typed_package() {
        assert_eq!(verify(&empty_main()), Ok(()));
    }

    #[test]
    fn verify_package_wraps_a_valid_package() {
        let package = verify_package(empty_main()).unwrap();
        assert_eq!(package.entry(), FunctionId::from_raw(1));
    }

    #[test]
    fn rejects_branch_to_function_entry() {
        let mut builder = IrBuilder::new(crate::TargetSpec::for_test_64());
        let main = builder.declare_function(
            "gane.main".into(),
            signature(vec![], vec![]),
            FunctionAttributes::default(),
        );
        builder.set_entry(main).unwrap();
        let entry = builder.entry_block(main).unwrap();
        builder
            .set_terminator(
                main,
                entry,
                Terminator::Branch {
                    target: entry,
                    arguments: vec![],
                },
            )
            .unwrap();

        let errors = messages(&builder.finish().unwrap());
        assert!(errors.contains("function entry block cannot be a branch target"));
    }

    #[test]
    fn rejects_invalid_entry_and_symbol_collision() {
        let mut package = empty_main();
        let main = package.inner_mut().functions[0].clone();
        package.inner_mut().functions.push(main);
        package.inner_mut().entry = FunctionId::INVALID;

        let errors = messages(&package);
        assert!(errors.contains("entry function is invalid"));
        assert!(errors.contains("duplicate symbol"));
    }

    #[test]
    fn rejects_void_uses_and_invalid_global_initializer() {
        let mut package = empty_main();
        let void = package.types().void();
        package.inner_mut().globals.push(IrGlobal {
            symbol: "gane.bad".into(),
            typ: void,
            initializer: GlobalInitializer::Scalar(Constant::Integer(1)),
        });
        package.inner_mut().functions[0]
            .stack_slots
            .push(StackSlot {
                typ: void,
                name: None,
                origin: None,
            });

        let errors = messages(&package);
        assert!(errors.contains("global type must exist and cannot be void"));
        assert!(errors.contains("constant is not representable"));
        assert!(errors.contains("stack slot type must exist and cannot be void"));
    }

    #[test]
    fn rejects_duplicate_primitive_and_by_value_recursion() {
        let mut builder = IrBuilder::new(crate::TargetSpec::for_test_64());
        builder.add_type(IrTypeKind::I32);
        let recursive = builder.reserve_type();
        builder
            .define_type(
                recursive,
                IrTypeKind::Struct {
                    fields: vec![recursive],
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
        builder
            .set_terminator(main, entry, Terminator::Return { values: vec![] })
            .unwrap();

        let errors = messages(&builder.finish().unwrap());
        assert!(errors.contains("duplicate primitive type"));
        assert!(errors.contains("recursive by-value layout"));
    }

    #[test]
    fn rejects_invalid_type_shapes_for_target() {
        let mut builder = IrBuilder::new(crate::TargetSpec::for_test_32());
        let i32 = builder.types().i32();
        builder.add_type(IrTypeKind::Array {
            length: 0,
            element: i32,
        });
        builder.add_type(IrTypeKind::Array {
            length: u32::MAX as u64 + 1,
            element: i32,
        });
        builder.add_type(IrTypeKind::Struct { fields: vec![] });
        builder.add_type(IrTypeKind::Ptr {
            pointee: i32,
            address_space: 1,
        });
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

        let errors = messages(&builder.finish().unwrap());
        assert!(errors.contains("array length must be non-zero"));
        assert!(errors.contains("array length does not fit pointer width"));
        assert!(errors.contains("struct must have non-zero size"));
        assert!(errors.contains("address space must be zero"));
    }

    #[test]
    fn rejects_wrong_definition_origin_and_use_before_definition() {
        let mut builder = IrBuilder::new(crate::TargetSpec::for_test_64());
        let i32 = builder.types().i32();
        let main = builder.declare_function(
            "gane.main".into(),
            signature(vec![], vec![]),
            FunctionAttributes::default(),
        );
        builder.set_entry(main).unwrap();
        let entry = builder.entry_block(main).unwrap();
        let first = builder
            .append_instruction(
                main,
                entry,
                InstructionKind::Unary {
                    op: UnaryOp::Neg,
                    operand: ValueId::from_raw(2),
                },
                [i32],
                None,
            )
            .unwrap()[0];
        builder
            .append_instruction(
                main,
                entry,
                InstructionKind::Const {
                    value: Constant::Integer(1),
                    typ: i32,
                },
                [i32],
                None,
            )
            .unwrap();
        builder
            .set_terminator(main, entry, Terminator::Return { values: vec![] })
            .unwrap();
        let mut package = builder.finish().unwrap();
        package.inner_mut().functions[0].values[first.raw() as usize - 1].origin =
            ValueOrigin::BlockParameter {
                block: entry,
                parameter_index: 0,
            };

        let errors = messages(&package);
        assert!(errors.contains("origin does not match definition"));
        assert!(errors.contains("used before its definition"));
    }

    #[test]
    fn rejects_non_dominating_use_and_wrong_block_arguments() {
        let mut builder = IrBuilder::new(crate::TargetSpec::for_test_64());
        let i1 = builder.types().i1();
        let i32 = builder.types().i32();
        let main = builder.declare_function(
            "gane.main".into(),
            signature(vec![], vec![]),
            FunctionAttributes::default(),
        );
        builder.set_entry(main).unwrap();
        let entry = builder.entry_block(main).unwrap();
        let then_block = builder.create_block(main).unwrap();
        let else_block = builder.create_block(main).unwrap();
        let join = builder.create_block(main).unwrap();
        builder
            .append_block_parameter(main, join, i32, None)
            .unwrap();
        let condition = builder
            .append_instruction(
                main,
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
                main,
                entry,
                Terminator::CondBranch {
                    condition,
                    then_target: then_block,
                    then_arguments: vec![],
                    else_target: else_block,
                    else_arguments: vec![],
                },
            )
            .unwrap();
        let value = builder
            .append_instruction(
                main,
                then_block,
                InstructionKind::Const {
                    value: Constant::Integer(1),
                    typ: i32,
                },
                [i32],
                None,
            )
            .unwrap()[0];
        builder
            .set_terminator(
                main,
                then_block,
                Terminator::Branch {
                    target: join,
                    arguments: vec![value],
                },
            )
            .unwrap();
        builder
            .set_terminator(
                main,
                else_block,
                Terminator::Branch {
                    target: join,
                    arguments: vec![value],
                },
            )
            .unwrap();
        builder
            .set_terminator(main, join, Terminator::Return { values: vec![] })
            .unwrap();

        let errors = messages(&builder.finish().unwrap());
        assert!(errors.contains("does not dominate use"));
    }

    #[test]
    fn rejects_instruction_type_matrix_errors() {
        let mut builder = IrBuilder::new(crate::TargetSpec::for_test_64());
        let i1 = builder.types().i1();
        let i8 = builder.types().i8();
        let i32 = builder.types().i32();
        let pair = builder.add_type(IrTypeKind::Struct { fields: vec![i32] });
        let pair_ptr = builder.add_type(IrTypeKind::Ptr {
            pointee: pair,
            address_space: 0,
        });
        let main = builder.declare_function(
            "gane.main".into(),
            signature(vec![], vec![]),
            FunctionAttributes::default(),
        );
        builder.set_entry(main).unwrap();
        let entry = builder.entry_block(main).unwrap();
        let boolean = builder
            .append_instruction(
                main,
                entry,
                InstructionKind::Const {
                    value: Constant::Bool(true),
                    typ: i1,
                },
                [i1],
                None,
            )
            .unwrap()[0];
        let integer = builder
            .append_instruction(
                main,
                entry,
                InstructionKind::Const {
                    value: Constant::Integer(1),
                    typ: i8,
                },
                [i8],
                None,
            )
            .unwrap()[0];
        let slot = builder.add_stack_slot(main, pair, None, None).unwrap();
        let pointer = builder
            .append_instruction(
                main,
                entry,
                InstructionKind::StackAddr { slot },
                [pair_ptr],
                None,
            )
            .unwrap()[0];
        builder
            .append_instruction(
                main,
                entry,
                InstructionKind::Binary {
                    op: BinaryOp::Add,
                    left: boolean,
                    right: integer,
                },
                [i1],
                None,
            )
            .unwrap();
        builder
            .append_instruction(
                main,
                entry,
                InstructionKind::GepField {
                    base: pointer,
                    field: 8,
                },
                [pair_ptr],
                None,
            )
            .unwrap();
        builder
            .append_instruction(
                main,
                entry,
                InstructionKind::Store {
                    pointer,
                    value: integer,
                },
                [],
                None,
            )
            .unwrap();
        builder
            .set_terminator(main, entry, Terminator::Return { values: vec![] })
            .unwrap();

        let errors = messages(&builder.finish().unwrap());
        assert!(errors.contains("invalid binary operand types"));
        assert!(errors.contains("invalid gep_field"));
        assert!(errors.contains("store types do not match"));
    }

    #[test]
    fn rejects_constant_cast_compare_and_aggregate_errors() {
        let mut builder = IrBuilder::new(crate::TargetSpec::for_test_64());
        let i8 = builder.types().i8();
        let i32 = builder.types().i32();
        let pair = builder.add_type(IrTypeKind::Struct { fields: vec![i32] });
        let pointer = builder.add_type(IrTypeKind::Ptr {
            pointee: pair,
            address_space: 0,
        });
        let main = builder.declare_function(
            "gane.main".into(),
            signature(vec![], vec![]),
            FunctionAttributes::default(),
        );
        builder.set_entry(main).unwrap();
        let entry = builder.entry_block(main).unwrap();
        let too_large = builder
            .append_instruction(
                main,
                entry,
                InstructionKind::Const {
                    value: Constant::Integer(256),
                    typ: i8,
                },
                [i8],
                None,
            )
            .unwrap()[0];
        let slot = builder.add_stack_slot(main, pair, None, None).unwrap();
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
                InstructionKind::IntCast {
                    kind: IntCastKind::Truncate,
                    operand: too_large,
                    target: i32,
                },
                [i32],
                None,
            )
            .unwrap();
        builder
            .append_instruction(
                main,
                entry,
                InstructionKind::Compare {
                    predicate: ComparePredicate::SignedLess,
                    left: address,
                    right: address,
                },
                [builder.types().i1()],
                None,
            )
            .unwrap();
        builder
            .append_instruction(
                main,
                entry,
                InstructionKind::AggregateZero {
                    destination: address,
                    typ: i32,
                },
                [],
                None,
            )
            .unwrap();
        builder
            .set_terminator(main, entry, Terminator::Return { values: vec![] })
            .unwrap();

        let errors = messages(&builder.finish().unwrap());
        assert!(errors.contains("constant is not representable"));
        assert!(errors.contains("invalid integer cast widths"));
        assert!(errors.contains("invalid compare operand types"));
        assert!(errors.contains("aggregate_zero requires"));
    }

    #[test]
    fn rejects_unreachable_cross_block_use_and_orphan_parameter() {
        let mut builder = IrBuilder::new(crate::TargetSpec::for_test_64());
        let i32 = builder.types().i32();
        let main = builder.declare_function(
            "gane.main".into(),
            signature(vec![], vec![]),
            FunctionAttributes::default(),
        );
        builder.set_entry(main).unwrap();
        let entry = builder.entry_block(main).unwrap();
        let value = builder
            .append_instruction(
                main,
                entry,
                InstructionKind::Const {
                    value: Constant::Integer(1),
                    typ: i32,
                },
                [i32],
                None,
            )
            .unwrap()[0];
        builder
            .set_terminator(main, entry, Terminator::Return { values: vec![] })
            .unwrap();
        let unreachable = builder.create_block(main).unwrap();
        builder
            .append_block_parameter(main, unreachable, i32, None)
            .unwrap();
        builder
            .append_instruction(
                main,
                unreachable,
                InstructionKind::Unary {
                    op: UnaryOp::Neg,
                    operand: value,
                },
                [i32],
                None,
            )
            .unwrap();
        builder
            .set_terminator(main, unreachable, Terminator::Return { values: vec![] })
            .unwrap();

        let errors = messages(&builder.finish().unwrap());
        assert!(errors.contains("unreachable block uses value"));
        assert!(errors.contains("parameters have no incoming edge"));
    }

    #[test]
    fn rejects_call_return_and_no_return_contract_errors() {
        let mut builder = IrBuilder::new(crate::TargetSpec::for_test_64());
        let i32 = builder.types().i32();
        let callee = builder.declare_function(
            "gane.callee".into(),
            signature(vec![IrParameter { typ: i32 }], vec![i32]),
            FunctionAttributes { no_return: true },
        );
        let callee_entry = builder.entry_block(callee).unwrap();
        builder
            .set_terminator(callee, callee_entry, Terminator::Return { values: vec![] })
            .unwrap();
        let main = builder.declare_function(
            "gane.main".into(),
            signature(vec![], vec![]),
            FunctionAttributes::default(),
        );
        builder.set_entry(main).unwrap();
        let entry = builder.entry_block(main).unwrap();
        builder
            .append_instruction(
                main,
                entry,
                InstructionKind::Call {
                    callee: Callee::Function(callee),
                    arguments: vec![],
                },
                [],
                None,
            )
            .unwrap();
        builder
            .set_terminator(main, entry, Terminator::Return { values: vec![] })
            .unwrap();

        let errors = messages(&builder.finish().unwrap());
        assert!(errors.contains("no_return function cannot declare results"));
        assert!(errors.contains("return values do not match"));
        assert!(errors.contains("call argument count"));
        assert!(errors.contains("no_return call must be last"));
    }
}
