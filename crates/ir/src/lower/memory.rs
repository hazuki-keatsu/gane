//! Lowering helpers for memory-backed values.
//!
//! A [`Place`] represents an address and the type stored at that address.
//! Scalar values use [`load`] and [`store`], while aggregate values remain in
//! memory and use [`aggregate_zero`] or [`aggregate_copy`]. These helpers also
//! insert the null-dereference guards required before accessing a place.

use super::*;

impl FunctionLowerer<'_, '_> {
    /// Ensures that a place's address is non-null before it is accessed.
    ///
    /// Already-proven addresses are cached in `known_non_null`. Otherwise this
    /// emits a comparison and a trapping guard, then continues in the success
    /// block.
    pub(super) fn ensure_non_null(
        &mut self,
        node: AstNodeId,
        place: Place,
    ) -> Result<(), LowerError> {
        if self.known_non_null.contains(&place.pointer) {
            return Ok(());
        }
        let pointer_type = self.pointer_type(place.typ);
        let null = self.instruction(
            node,
            crate::InstructionKind::Const {
                value: Constant::Null,
                typ: pointer_type,
            },
            [pointer_type],
        )?[0];
        let non_null = self.instruction(
            node,
            crate::InstructionKind::Compare {
                predicate: ComparePredicate::NotEqual,
                left: place.pointer,
                right: null,
            },
            [self.package.builder.types().i1()],
        )?[0];
        self.guard(node, non_null, TrapReason::NullDereference)?;
        self.known_non_null.insert(place.pointer);
        Ok(())
    }

    /// Loads a scalar value from a place.
    ///
    /// Aggregate values are intentionally rejected because V0 represents them
    /// in memory rather than as ordinary SSA values.
    pub(super) fn load(&mut self, node: AstNodeId, place: Place) -> Result<ValueId, LowerError> {
        if self.is_aggregate(place.typ) {
            return Err(self.unsupported(node, "aggregate value"));
        }
        self.ensure_non_null(node, place)?;
        Ok(self.instruction(
            node,
            crate::InstructionKind::Load {
                pointer: place.pointer,
            },
            [place.typ],
        )?[0])
    }

    /// Stores a scalar SSA value into a place after checking its address.
    ///
    /// Aggregate assignment is handled by [`aggregate_copy`] or
    /// [`aggregate_zero`] instead.
    pub(super) fn store(
        &mut self,
        node: AstNodeId,
        place: Place,
        value: ValueId,
    ) -> Result<(), LowerError> {
        if self.is_aggregate(place.typ) {
            return Err(self.unsupported(node, "aggregate assignment"));
        }
        self.ensure_non_null(node, place)?;
        self.instruction(
            node,
            crate::InstructionKind::Store {
                pointer: place.pointer,
                value,
            },
            [],
        )?;
        Ok(())
    }

    /// Applies the appropriate scalar or aggregate assignment operation.
    pub(super) fn assign(
        &mut self,
        node: AstNodeId,
        destination: Place,
        value: Rvalue,
    ) -> Result<(), LowerError> {
        match value {
            Rvalue::Scalar(value) => self.store(node, destination, value),
            Rvalue::AggregateCopy(source) if source.typ == destination.typ => {
                self.aggregate_copy(node, destination, source)
            }
            Rvalue::AggregateZero(typ) if typ == destination.typ => {
                self.aggregate_zero(node, destination)
            }
            _ => Err(self.unsupported(node, "assignment type")),
        }
    }

    /// Copies an aggregate into a fresh temporary stack slot and returns its
    /// place.
    ///
    /// This snapshots an aggregate RHS before a multiple assignment can
    /// overwrite one of the source locations.
    pub(super) fn aggregate_snapshot(
        &mut self,
        node: AstNodeId,
        source: Place,
    ) -> Result<Place, LowerError> {
        let slot = self.build(node, |builder, function, _| {
            builder.add_stack_slot(function, source.typ, None, Some(node))
        })?;
        let pointer_type = self.pointer_type(source.typ);
        let pointer = self.instruction(
            node,
            crate::InstructionKind::StackAddr { slot },
            [pointer_type],
        )?[0];
        let destination = Place {
            pointer,
            typ: source.typ,
        };
        self.known_non_null.insert(pointer);
        self.aggregate_copy(node, destination, source)?;
        Ok(destination)
    }

    /// Writes the zero value of an aggregate into a destination place.
    pub(super) fn aggregate_zero(
        &mut self,
        node: AstNodeId,
        destination: Place,
    ) -> Result<(), LowerError> {
        if !self.is_aggregate(destination.typ) {
            return Err(self.unsupported(node, "aggregate zero"));
        }
        self.ensure_non_null(node, destination)?;
        self.instruction(
            node,
            crate::InstructionKind::AggregateZero {
                destination: destination.pointer,
                typ: destination.typ,
            },
            [],
        )?;
        Ok(())
    }

    /// Copies an aggregate between two places of the same type.
    pub(super) fn aggregate_copy(
        &mut self,
        node: AstNodeId,
        destination: Place,
        source: Place,
    ) -> Result<(), LowerError> {
        if !self.is_aggregate(destination.typ) || destination.typ != source.typ {
            return Err(self.unsupported(node, "aggregate copy"));
        }
        self.ensure_non_null(node, destination)?;
        self.ensure_non_null(node, source)?;
        self.instruction(
            node,
            crate::InstructionKind::AggregateCopy {
                destination: destination.pointer,
                source: source.pointer,
                typ: destination.typ,
            },
            [],
        )?;
        Ok(())
    }
}
