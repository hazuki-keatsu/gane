use gane_ir::{IrTypeKind, TypeArena, TypeId};

use crate::runtime::{Pointer, Projection, RuntimeValue};

#[derive(Clone, Debug)]
pub(crate) enum Object {
    Scalar(RuntimeValue),
    Array(Vec<Object>),
    Struct(Vec<Object>),
}

/// Initializer for zero
pub(crate) fn zero_object(types: &TypeArena, typ: TypeId) -> Object {
    match types
        .get(typ)
        .expect("verified IR has valid types")
        .kind
        .clone()
    {
        IrTypeKind::I1 | IrTypeKind::I8 | IrTypeKind::I16 | IrTypeKind::I32 | IrTypeKind::I64 => {
            Object::Scalar(RuntimeValue::Bits(0))
        }
        IrTypeKind::Ptr { .. } => Object::Scalar(RuntimeValue::Pointer(Pointer::Null)),
        IrTypeKind::Array { length, element } => {
            Object::Array((0..length).map(|_| zero_object(types, element)).collect())
        }
        IrTypeKind::Struct { fields } => Object::Struct(
            fields
                .into_iter()
                .map(|field| zero_object(types, field))
                .collect(),
        ),
        IrTypeKind::Void => unreachable!("verified IR has no void objects"),
    }
}

/// Find out the [`Object`] by projection chain. [`Object`] is immutable reference.
pub(crate) fn project_object<'a>(mut object: &'a Object, projections: &[Projection]) -> &'a Object {
    for projection in projections {
        object = match (object, projection) {
            (Object::Struct(fields), Projection::Field(field)) => &fields[*field],
            (Object::Array(elements), Projection::Index(index)) => &elements[*index],
            _ => unreachable!("verified IR pointer projection matches its object"),
        };
    }
    object
}

/// Find out the [`Object`] by projection chain. [`Object`] is mutable reference.
pub(crate) fn project_object_mut<'a>(
    mut object: &'a mut Object,
    projections: &[Projection],
) -> &'a mut Object {
    for projection in projections {
        object = match (object, projection) {
            (Object::Struct(fields), Projection::Field(field)) => &mut fields[*field],
            (Object::Array(elements), Projection::Index(index)) => &mut elements[*index],
            _ => unreachable!("verified IR pointer projection matches its object"),
        };
    }
    object
}
