use crate::{
    BlockId, Callee, FunctionId, InstructionKind, IrDiagnostic, IrFunction, IrTypeKind,
    StackSlotId, Terminator, TypeId, UnverifiedIrPackage, ValueId,
};
use std::collections::{BTreeSet, VecDeque};

#[cfg(test)]
mod tests;

pub(crate) fn check(package: &UnverifiedIrPackage) -> Result<(), Vec<IrDiagnostic>> {
    let summaries = summaries(package);
    let mut diagnostics = Vec::new();

    for (function_id, function) in package.functions() {
        let mut analysis = Analysis::new(package, function);
        analysis.solve(&summaries);
        diagnostics.extend(analysis.diagnostics(function_id, &summaries));
    }

    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(diagnostics)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Summary {
    captures: Vec<bool>,
    returns_from: Vec<bool>,
    returns_static: bool,
    writes_external: bool,
    invalid_local: bool,
    unknown_effect: bool,
}

impl Summary {
    fn new(parameter_count: usize) -> Self {
        Self {
            captures: vec![false; parameter_count],
            returns_from: vec![false; parameter_count],
            returns_static: false,
            writes_external: false,
            invalid_local: false,
            unknown_effect: false,
        }
    }

    fn join(&mut self, other: &Self) -> bool {
        let mut changed = false;
        for (current, incoming) in self.captures.iter_mut().zip(&other.captures) {
            if *incoming && !*current {
                *current = true;
                changed = true;
            }
        }
        for (current, incoming) in self.returns_from.iter_mut().zip(&other.returns_from) {
            if *incoming && !*current {
                *current = true;
                changed = true;
            }
        }
        changed |= join_bool(&mut self.returns_static, other.returns_static);
        changed |= join_bool(&mut self.writes_external, other.writes_external);
        changed |= join_bool(&mut self.invalid_local, other.invalid_local);
        changed |= join_bool(&mut self.unknown_effect, other.unknown_effect);
        changed
    }
}

fn join_bool(current: &mut bool, incoming: bool) -> bool {
    if incoming && !*current {
        *current = true;
        true
    } else {
        false
    }
}

fn summaries(package: &UnverifiedIrPackage) -> Vec<Summary> {
    let mut summaries = package
        .functions()
        .map(|(_, function)| Summary::new(function.signature.parameters.len()))
        .collect::<Vec<_>>();

    loop {
        let mut changed = false;
        let current = summaries.clone();
        for (function_id, function) in package.functions() {
            let mut analysis = Analysis::new(package, function);
            analysis.solve(&current);
            changed |= summaries[function_id.raw() as usize - 1]
                .join(&analysis.summary(function_id, &current));
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

#[derive(Clone, Copy)]
enum SinkKind {
    ExternalWrite(&'static str),
    Call,
    Return,
}

struct Sink {
    location: String,
    kind: SinkKind,
    targets: ObjectSet,
    sources: ObjectSet,
    writes_external: bool,
    unknown_effect: bool,
}

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

    fn solve(&mut self, summaries: &[Summary]) {
        loop {
            let mut changed = false;
            for block_index in 0..self.function.blocks.len() {
                if !self.reachable[block_index] {
                    continue;
                }
                let instructions = self.function.blocks[block_index].instructions.clone();
                for instruction in instructions {
                    changed |= self.transfer(&instruction.kind, &instruction.results, summaries);
                }
                let terminator = self.function.blocks[block_index].terminator.clone();
                changed |= self.transfer_terminator(&terminator);
            }
            if !changed {
                break;
            }
        }
    }

    fn transfer(
        &mut self,
        kind: &InstructionKind,
        results: &[ValueId],
        summaries: &[Summary],
    ) -> bool {
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
            InstructionKind::Call {
                callee: Callee::Function(callee),
                arguments,
            } => self.transfer_call(*callee, arguments, results, summaries),
        }
    }

    fn transfer_call(
        &mut self,
        callee: FunctionId,
        arguments: &[ValueId],
        results: &[ValueId],
        summaries: &[Summary],
    ) -> bool {
        let Some(summary) = function_summary(summaries, callee) else {
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
            return changed;
        };
        let argument_sources = arguments
            .iter()
            .map(|argument| self.reach(self.points(*argument)))
            .collect::<Vec<_>>();
        let mut returned = ObjectSet::new();
        for (index, sources) in argument_sources.iter().enumerate() {
            if summary.returns_from.get(index).copied().unwrap_or(true) {
                extend(&mut returned, sources);
            }
        }
        if summary.returns_static {
            returned.insert(Object::Static);
        }
        if summary.invalid_local || summary.unknown_effect {
            returned.insert(Object::Unknown);
        }
        let mut changed = false;
        for result in results {
            if self
                .function
                .value(*result)
                .is_some_and(|value| is_pointer(self.package, value.typ))
            {
                changed |= self.extend_points(*result, &returned);
            }
        }
        if summary.writes_external {
            let mut targets = ObjectSet::new();
            let mut sources = ObjectSet::from([Object::Static]);
            for (index, reachable) in argument_sources.iter().enumerate() {
                extend(&mut targets, reachable);
                if summary.captures.get(index).copied().unwrap_or(true) {
                    extend(&mut sources, reachable);
                }
            }
            changed |= self.update_memory(&targets, &sources);
            changed |= extend(&mut self.external_memory, &sources);
        }
        changed
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

    fn sinks(&self, function_id: FunctionId, summaries: &[Summary]) -> Vec<Sink> {
        let mut sinks = Vec::new();
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
                            &mut sinks,
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
                            &mut sinks,
                            location,
                            self.points(*destination),
                            &sources,
                            "copied",
                        );
                    }
                    InstructionKind::AggregateZero { destination, typ }
                        if contains_pointer(self.package, *typ) =>
                    {
                        self.push_external_sink(
                            &mut sinks,
                            location,
                            self.points(*destination),
                            &ObjectSet::new(),
                            "zeroed",
                        );
                    }
                    InstructionKind::Call {
                        callee: Callee::Function(callee),
                        arguments,
                    } => {
                        let summary = function_summary(summaries, *callee);
                        let mut sources = ObjectSet::new();
                        for (index, argument) in arguments.iter().enumerate() {
                            if summary
                                .and_then(|summary| summary.captures.get(index))
                                .copied()
                                .unwrap_or(true)
                            {
                                extend(&mut sources, self.points(*argument));
                            }
                        }
                        sinks.push(Sink {
                            location,
                            kind: SinkKind::Call,
                            targets: ObjectSet::new(),
                            sources,
                            writes_external: summary.is_none_or(|summary| summary.writes_external),
                            unknown_effect: summary.is_none_or(|summary| summary.unknown_effect),
                        });
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
                sinks.push(Sink {
                    location: format!(
                        "function @{} block ^{} terminator",
                        function_id.raw(),
                        block_index + 1
                    ),
                    kind: SinkKind::Return,
                    targets: ObjectSet::new(),
                    sources,
                    writes_external: false,
                    unknown_effect: false,
                });
            }
        }
        sinks
    }

    fn push_external_sink(
        &self,
        sinks: &mut Vec<Sink>,
        location: String,
        targets: &ObjectSet,
        sources: &ObjectSet,
        action: &str,
    ) {
        if external_destination(targets).is_none() {
            return;
        }
        sinks.push(Sink {
            location,
            kind: SinkKind::ExternalWrite(match action {
                "stored" => "stored",
                "copied" => "copied",
                "zeroed" => "zeroed",
                _ => unreachable!(),
            }),
            targets: targets.clone(),
            sources: sources.clone(),
            writes_external: true,
            unknown_effect: targets.contains(&Object::Unknown),
        });
    }

    fn summary(&self, function_id: FunctionId, summaries: &[Summary]) -> Summary {
        let mut summary = Summary::new(self.function.signature.parameters.len());
        for sink in self.sinks(function_id, summaries) {
            summary.writes_external |= sink.writes_external;
            summary.unknown_effect |= sink.unknown_effect;
            let reached = self.reach(&sink.sources);
            for object in reached {
                match object {
                    Object::Local(_) => summary.invalid_local = true,
                    Object::Borrowed(index) => {
                        if let Some(captures) = summary.captures.get_mut(index) {
                            *captures = true;
                        }
                        if matches!(sink.kind, SinkKind::Return)
                            && let Some(returns_from) = summary.returns_from.get_mut(index)
                        {
                            *returns_from = true;
                        }
                    }
                    Object::Static if matches!(sink.kind, SinkKind::Return) => {
                        summary.returns_static = true;
                    }
                    Object::Unknown => summary.unknown_effect = true,
                    Object::Static => {}
                }
            }
        }
        summary
    }

    fn diagnostics(&self, function_id: FunctionId, summaries: &[Summary]) -> Vec<IrDiagnostic> {
        let mut diagnostics = Vec::new();
        for sink in self.sinks(function_id, summaries) {
            if sink.unknown_effect {
                diagnostics.push(IrDiagnostic::new(
                    sink.location.clone(),
                    "callee has unknown escape effects",
                ));
            }
            let reached = self.reach(&sink.sources);
            let slots = reached
                .iter()
                .filter_map(|object| match object {
                    Object::Local(slot) => Some(format!("${}", slot.raw())),
                    Object::Borrowed(_) | Object::Static | Object::Unknown => None,
                })
                .collect::<Vec<_>>();
            let unknown = reached.contains(&Object::Unknown);
            if slots.is_empty() && !unknown {
                continue;
            }
            let action = match sink.kind {
                SinkKind::ExternalWrite(action) => external_action(action, &sink.targets),
                SinkKind::Call => "passed to an escaping parameter",
                SinkKind::Return => "returned from function",
            };
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
            diagnostics.push(IrDiagnostic::new(sink.location, message));
        }
        diagnostics
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

fn function_summary(summaries: &[Summary], function: FunctionId) -> Option<&Summary> {
    summaries.get(function.raw().checked_sub(1)? as usize)
}

fn external_destination(targets: &ObjectSet) -> Option<&'static str> {
    if targets.contains(&Object::Static) {
        Some("global")
    } else if targets
        .iter()
        .any(|target| matches!(target, Object::Borrowed(_)))
    {
        Some("external memory")
    } else if targets.contains(&Object::Unknown) {
        Some("unknown memory")
    } else {
        None
    }
}

fn external_action(action: &str, targets: &ObjectSet) -> &'static str {
    match (action, external_destination(targets)) {
        ("stored", Some("global")) => "stored in global",
        ("copied", Some("global")) => "copied into global",
        ("zeroed", Some("global")) => "zeroed in global",
        ("stored", Some("external memory")) => "stored outside current stack",
        ("copied", Some("external memory")) => "copied outside current stack",
        ("zeroed", Some("external memory")) => "zeroed outside current stack",
        ("stored", Some("unknown memory")) => "stored through unknown pointer",
        ("copied", Some("unknown memory")) => "copied through unknown pointer",
        ("zeroed", Some("unknown memory")) => "zeroed through unknown pointer",
        _ => unreachable!(),
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
