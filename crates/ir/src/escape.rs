use crate::{
    BlockId, Callee, FunctionId, InstructionKind, IrDiagnostic, IrFunction, IrTypeKind,
    StackSlotId, Terminator, TypeId, UnverifiedIrPackage, ValueId, ValueOrigin,
};
use std::collections::{BTreeSet, VecDeque};

pub(crate) fn check(package: &UnverifiedIrPackage) -> Result<(), Vec<IrDiagnostic>> {
    let summaries = summaries(package);
    let mut diagnostics = Vec::new();

    for (function_id, function) in package.functions() {
        let mut analysis = Analysis::new(package, function);
        analysis.solve();
        diagnostics.extend(analysis.diagnostics(function_id, &summaries));
    }

    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(diagnostics)
    }
}

// ponytail: phase B keeps the bool-only call summary; phase C replaces it with return and
// external-write effects while reusing `Analysis` for each function.
/// Return a [`Vec<Vec<bool>>`] to describe that if
/// `summaries[FunctionId][IrParameter as index] == true`,
/// the [`IrParameter`](crate::ir::IrParameter) of the [`IrFunction`] may escape.
///
/// 1. Assume that all the parameter won't escape.
/// 2. Mark every pointer parameter with taint.
/// 3. [`propagate()`] will track the taint pointer by Gep, stack slot, branch parameter, load/store and etc.
/// 4. [`escapes()`] will check if or not the taint would escape by returning, out-of-memory writing or passing to escaping calling function.
/// 5. If found, the bool will be set to `true`.
/// 6. Repeat until there is no new result. To process the recursive calling.
fn summaries(package: &UnverifiedIrPackage) -> Vec<Vec<bool>> {
    let mut summaries = package
        .functions()
        .map(|(_, function)| vec![false; function.signature.parameters.len()])
        .collect::<Vec<Vec<bool>>>();

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

/// Find out the pointer in types recursively
fn contains_pointer(package: &UnverifiedIrPackage, typ: TypeId) -> bool {
    match package.types().get(typ).map(|typ| &typ.kind) {
        Some(IrTypeKind::Ptr { .. }) => true,
        Some(IrTypeKind::Array { element, .. }) => contains_pointer(package, *element),
        Some(IrTypeKind::Struct { fields }) => {
            fields.iter().any(|field| contains_pointer(package, *field))
        }
        Some(
            IrTypeKind::Void
            | IrTypeKind::I1
            | IrTypeKind::I8
            | IrTypeKind::I16
            | IrTypeKind::I32
            | IrTypeKind::I64,
        )
        | None => false,
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Object {
    /// The local object on the current call stack
    Local(StackSlotId),
    /// The external object borrowed by the usize-th argument
    Borrowed(usize),
    /// All the global objects and its sub-objects
    Static,
    Unknown,
}

type ObjectSet = BTreeSet<Object>;

struct Analysis<'a> {
    package: &'a UnverifiedIrPackage,
    function: &'a IrFunction,
    reachable: Vec<bool>,
    points_to: Vec<ObjectSet>,
    // ponytail: fields and array elements share their root slot; split paths only if false
    // positives show that root-object precision is insufficient.
    local_memory: Vec<ObjectSet>,
    // ponytail: all borrowed objects and globals share one external view; add per-parameter
    // views only when this conservative aliasing rejects valid programs.
    external_memory: ObjectSet,
}

impl<'a> Analysis<'a> {
    fn new(package: &'a UnverifiedIrPackage, function: &'a IrFunction) -> Self {
        let mut analysis = Self {
            package,
            function,
            reachable: reachable_blocks(function),
            points_to: vec![ObjectSet::new(); function.values.len()],
            local_memory: vec![ObjectSet::new(); function.stack_slots.len()],
            external_memory: ObjectSet::from([Object::Static]),
        };
        if let Some(entry) = function.block(function.entry) {
            for (index, parameter) in function.signature.parameters.iter().enumerate() {
                if is_pointer(package, parameter.typ)
                    && let Some(value) = entry.parameters.get(index).copied()
                {
                    analysis.insert_point(value, Object::Borrowed(index));
                    analysis.external_memory.insert(Object::Borrowed(index));
                }
            }
        }
        analysis
    }

    fn solve(&mut self) {
        loop {
            let mut changed = false;
            for block_index in 0..self.function.blocks.len() {
                if !self.reachable[block_index] {
                    continue;
                }
                let instructions = self.function.blocks[block_index].instructions.clone();
                for instruction in instructions {
                    changed |= self.transfer(&instruction.kind, &instruction.results);
                }
                let terminator = self.function.blocks[block_index].terminator.clone();
                changed |= self.transfer_terminator(&terminator);
            }
            if !changed {
                break;
            }
        }
    }

    fn transfer(&mut self, kind: &InstructionKind, results: &[ValueId]) -> bool {
        match kind {
            InstructionKind::Const { .. }
            | InstructionKind::Unary { .. }
            | InstructionKind::Binary { .. }
            | InstructionKind::Compare { .. }
            | InstructionKind::IntCast { .. } => false,
            // ponytail: weak updates retain old pointer facts across zeroing; add
            // CFG-sensitive strong updates only when valid programs need them.
            InstructionKind::AggregateZero { .. } => false,
            InstructionKind::StackAddr { slot } => results
                .first()
                .is_some_and(|result| self.insert_point(*result, Object::Local(*slot))),
            InstructionKind::GlobalAddr { .. } => results
                .first()
                .is_some_and(|result| self.insert_point(*result, Object::Static)),
            InstructionKind::GepField { base, .. } | InstructionKind::GepIndex { base, .. } => {
                let sources = self.points(*base).clone();
                results
                    .first()
                    .is_some_and(|result| self.extend_points(*result, &sources))
            }
            InstructionKind::Load { pointer } => {
                let Some(result) = results.first().copied().filter(|result| {
                    self.function
                        .value(*result)
                        .is_some_and(|value| is_pointer(self.package, value.typ))
                }) else {
                    return false;
                };
                let sources = self.contents(self.points(*pointer));
                self.extend_points(result, &sources)
            }
            InstructionKind::Store { pointer, value } => {
                if !self
                    .function
                    .value(*value)
                    .is_some_and(|value| is_pointer(self.package, value.typ))
                {
                    return false;
                }
                let targets = self.points(*pointer).clone();
                let sources = self.points(*value).clone();
                self.update_memory(&targets, &sources)
            }
            InstructionKind::AggregateCopy {
                destination,
                source,
                typ,
            } => {
                if !contains_pointer(self.package, *typ) {
                    return false;
                }
                let targets = self.points(*destination).clone();
                let sources = self.contents(self.points(*source));
                self.update_memory(&targets, &sources)
            }
            InstructionKind::Call { .. } => {
                // ponytail: pointer call results remain Unknown until phase C provides return
                // effect summaries.
                let mut changed = false;
                for result in results {
                    if self
                        .function
                        .value(*result)
                        .is_some_and(|value| is_pointer(self.package, value.typ))
                    {
                        changed |= self.insert_point(*result, Object::Unknown);
                    }
                }
                changed
            }
        }
    }

    fn transfer_terminator(&mut self, terminator: &Terminator) -> bool {
        match terminator {
            Terminator::Branch { target, arguments } => self.transfer_edge(*target, arguments),
            Terminator::CondBranch {
                then_target,
                then_arguments,
                else_target,
                else_arguments,
                ..
            } => {
                self.transfer_edge(*then_target, then_arguments)
                    | self.transfer_edge(*else_target, else_arguments)
            }
            Terminator::Return { .. } | Terminator::Trap { .. } | Terminator::Unreachable => false,
        }
    }

    fn transfer_edge(&mut self, target: BlockId, arguments: &[ValueId]) -> bool {
        let Some(parameters) = self
            .function
            .block(target)
            .map(|block| block.parameters.clone())
        else {
            return false;
        };
        let mut changed = false;
        for (parameter, argument) in parameters.into_iter().zip(arguments) {
            let sources = self.points(*argument).clone();
            changed |= self.extend_points(parameter, &sources);
        }
        changed
    }

    fn update_memory(&mut self, targets: &ObjectSet, sources: &ObjectSet) -> bool {
        let mut changed = false;
        let unknown = targets.contains(&Object::Unknown);
        for target in targets {
            match target {
                Object::Local(slot) => {
                    if let Some(memory) = self.local_memory.get_mut(slot.raw() as usize - 1) {
                        changed |= extend(memory, sources);
                    }
                }
                Object::Borrowed(_) | Object::Static => {
                    changed |= extend(&mut self.external_memory, sources);
                }
                Object::Unknown => {}
            }
        }
        if unknown {
            for memory in &mut self.local_memory {
                changed |= extend(memory, sources);
            }
            changed |= extend(&mut self.external_memory, sources);
        }
        changed
    }

    fn contents(&self, objects: &ObjectSet) -> ObjectSet {
        let mut contents = ObjectSet::new();
        for object in objects {
            match object {
                Object::Local(slot) => {
                    if let Some(memory) = self.local_memory.get(slot.raw() as usize - 1) {
                        extend(&mut contents, memory);
                    }
                }
                Object::Borrowed(_) | Object::Static => {
                    extend(&mut contents, &self.external_memory);
                }
                Object::Unknown => {
                    contents.insert(Object::Unknown);
                    extend(&mut contents, &self.external_memory);
                    for memory in &self.local_memory {
                        extend(&mut contents, memory);
                    }
                }
            }
        }
        contents
    }

    fn reach(&self, seeds: &ObjectSet) -> ObjectSet {
        let mut reached = ObjectSet::new();
        let mut pending = VecDeque::from_iter(seeds.iter().copied());
        while let Some(object) = pending.pop_front() {
            if !reached.insert(object) {
                continue;
            }
            pending.extend(self.contents(&ObjectSet::from([object])));
        }
        reached
    }

    fn diagnostics(&self, function_id: FunctionId, summaries: &[Vec<bool>]) -> Vec<IrDiagnostic> {
        let mut diagnostics = Vec::new();
        for (block_index, block) in self.function.blocks.iter().enumerate() {
            if !self.reachable[block_index] {
                continue;
            }
            for (instruction_index, instruction) in block.instructions.iter().enumerate() {
                let location = format!(
                    "function @{} block ^{} instruction #{}",
                    function_id.raw(),
                    block_index + 1,
                    instruction_index + 1
                );
                match &instruction.kind {
                    InstructionKind::Store { pointer, value }
                        if self
                            .function
                            .value(*value)
                            .is_some_and(|value| is_pointer(self.package, value.typ)) =>
                    {
                        self.push_external_sink(
                            &mut diagnostics,
                            location,
                            self.points(*pointer),
                            self.points(*value),
                            "stored",
                        );
                    }
                    InstructionKind::AggregateCopy {
                        destination,
                        source,
                        typ,
                    } if contains_pointer(self.package, *typ) => {
                        let sources = self.contents(self.points(*source));
                        self.push_external_sink(
                            &mut diagnostics,
                            location,
                            self.points(*destination),
                            &sources,
                            "copied",
                        );
                    }
                    InstructionKind::Call {
                        callee: Callee::Function(callee),
                        arguments,
                    } => {
                        let mut sources = ObjectSet::new();
                        for (index, argument) in arguments.iter().enumerate() {
                            if parameter_escapes(summaries, *callee, index) {
                                extend(&mut sources, self.points(*argument));
                            }
                        }
                        self.push_sink(
                            &mut diagnostics,
                            location,
                            &sources,
                            "passed to an escaping parameter",
                        );
                    }
                    InstructionKind::Const { .. }
                    | InstructionKind::Unary { .. }
                    | InstructionKind::Binary { .. }
                    | InstructionKind::Compare { .. }
                    | InstructionKind::IntCast { .. }
                    | InstructionKind::StackAddr { .. }
                    | InstructionKind::GlobalAddr { .. }
                    | InstructionKind::GepField { .. }
                    | InstructionKind::GepIndex { .. }
                    | InstructionKind::Load { .. }
                    | InstructionKind::Store { .. }
                    | InstructionKind::AggregateZero { .. }
                    | InstructionKind::AggregateCopy { .. } => {}
                }
            }
            if let Terminator::Return { values } = &block.terminator {
                let mut sources = ObjectSet::new();
                for value in values {
                    extend(&mut sources, self.points(*value));
                }
                self.push_sink(
                    &mut diagnostics,
                    format!(
                        "function @{} block ^{} terminator",
                        function_id.raw(),
                        block_index + 1
                    ),
                    &sources,
                    "returned from function",
                );
            }
        }
        diagnostics
    }

    fn push_external_sink(
        &self,
        diagnostics: &mut Vec<IrDiagnostic>,
        location: String,
        targets: &ObjectSet,
        sources: &ObjectSet,
        action: &str,
    ) {
        let destination = if targets.contains(&Object::Static) {
            "global"
        } else if targets
            .iter()
            .any(|target| matches!(target, Object::Borrowed(_)))
        {
            "external memory"
        } else if targets.contains(&Object::Unknown) {
            "unknown memory"
        } else {
            return;
        };
        let action = match (action, destination) {
            ("stored", "global") => "stored in global",
            ("copied", "global") => "copied into global",
            ("stored", "external memory") => "stored outside current stack",
            ("copied", "external memory") => "copied outside current stack",
            ("stored", "unknown memory") => "stored through unknown pointer",
            ("copied", "unknown memory") => "copied through unknown pointer",
            _ => unreachable!(),
        };
        self.push_sink(diagnostics, location, sources, action);
    }

    fn push_sink(
        &self,
        diagnostics: &mut Vec<IrDiagnostic>,
        location: String,
        sources: &ObjectSet,
        action: &str,
    ) {
        let reached = self.reach(sources);
        let slots = reached
            .iter()
            .filter_map(|object| match object {
                Object::Local(slot) => Some(format!("${}", slot.raw())),
                Object::Borrowed(_) | Object::Static | Object::Unknown => None,
            })
            .collect::<Vec<_>>();
        let unknown = reached.contains(&Object::Unknown);
        if slots.is_empty() && !unknown {
            return;
        }
        let message = if slots.is_empty() {
            format!("pointer with unknown provenance {action}")
        } else if unknown {
            format!(
                "stack-derived pointer {action} (source stack slots {}; also unknown)",
                slots.join(", ")
            )
        } else {
            format!(
                "stack-derived pointer {action} (source stack slots {})",
                slots.join(", ")
            )
        };
        diagnostics.push(IrDiagnostic::new(location, message));
    }

    fn points(&self, value: ValueId) -> &ObjectSet {
        &self.points_to[value.raw() as usize - 1]
    }

    fn insert_point(&mut self, value: ValueId, object: Object) -> bool {
        self.points_to[value.raw() as usize - 1].insert(object)
    }

    fn extend_points(&mut self, value: ValueId, sources: &ObjectSet) -> bool {
        extend(&mut self.points_to[value.raw() as usize - 1], sources)
    }
}

fn extend(target: &mut ObjectSet, sources: &ObjectSet) -> bool {
    let mut changed = false;
    for source in sources {
        changed |= target.insert(*source);
    }
    changed
}

fn reachable_blocks(function: &IrFunction) -> Vec<bool> {
    let mut reachable = vec![false; function.blocks.len()];
    let mut pending = VecDeque::from([function.entry]);
    while let Some(block) = pending.pop_front() {
        let index = block.raw() as usize - 1;
        if std::mem::replace(&mut reachable[index], true) {
            continue;
        }
        match &function.blocks[index].terminator {
            Terminator::Branch { target, .. } => pending.push_back(*target),
            Terminator::CondBranch {
                then_target,
                else_target,
                ..
            } => {
                pending.push_back(*then_target);
                pending.push_back(*else_target);
            }
            Terminator::Return { .. } | Terminator::Trap { .. } | Terminator::Unreachable => {}
        }
    }
    reachable
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
    let reachable = reachable_blocks(function);

    loop {
        let mut changed = false;
        for (block_index, block) in function.blocks.iter().enumerate() {
            if !reachable[block_index] {
                continue;
            }
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
    let mut changed = false;
    for value in values {
        changed |= taint.values.insert(*value);
    }
    changed
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
    let reachable = reachable_blocks(function);
    function
        .blocks
        .iter()
        .enumerate()
        .filter(|(index, _)| reachable[*index])
        .any(|(_, block)| {
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

/// Used for distinct a SSA value in a function is how to store.
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

    fn assert_current_escape_check_misses(package: UnverifiedIrPackage) {
        assert!(verify(&package).is_ok());
        assert!(verify_and_check_escape(package).is_ok());
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
    fn currently_misses_callee_publishing_pointer_loaded_from_parameter_memory() {
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

        assert_current_escape_check_misses(builder.finish().unwrap());
    }

    #[test]
    fn currently_misses_callee_publishing_pointer_through_multiple_loads() {
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

        assert_current_escape_check_misses(builder.finish().unwrap());
    }

    #[test]
    fn rejects_returning_pointer_call_result_until_return_effects_are_modeled() {
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

        assert!(
            escape_messages(builder.finish().unwrap())
                .contains("unknown provenance returned from function")
        );
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
