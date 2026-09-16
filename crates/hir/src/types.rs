use crate::id::TypeId;
use gane_parser::token::AstNodeId;

pub type Symbol = String;
pub type SourceOrigin = Option<AstNodeId>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirType {
    pub kind: HirTypeKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HirTypeKind {
    Void,
    I1,
    I8,
    I16,
    I32,
    I64,
    Ptr { pointee: TypeId, address_space: u32 },
    Array { length: u64, element: TypeId },
    Struct { fields: Vec<TypeId> },
}

#[derive(Clone, Debug)]
pub struct TypeArena {
    types: Vec<Option<HirType>>,
    void: TypeId,
    i1: TypeId,
    i8: TypeId,
    i16: TypeId,
    i32: TypeId,
    i64: TypeId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeArenaError {
    InvalidTypeId(TypeId),
    TypeAlreadyDefined(TypeId),
}

impl TypeArena {
    pub(crate) fn new() -> Self {
        let mut arena = Self {
            types: Vec::new(),
            void: TypeId::INVALID,
            i1: TypeId::INVALID,
            i8: TypeId::INVALID,
            i16: TypeId::INVALID,
            i32: TypeId::INVALID,
            i64: TypeId::INVALID,
        };
        arena.void = arena.alloc(HirTypeKind::Void);
        arena.i1 = arena.alloc(HirTypeKind::I1);
        arena.i8 = arena.alloc(HirTypeKind::I8);
        arena.i16 = arena.alloc(HirTypeKind::I16);
        arena.i32 = arena.alloc(HirTypeKind::I32);
        arena.i64 = arena.alloc(HirTypeKind::I64);
        arena
    }

    pub fn void(&self) -> TypeId {
        self.void
    }

    pub fn i1(&self) -> TypeId {
        self.i1
    }

    pub fn i8(&self) -> TypeId {
        self.i8
    }

    pub fn i16(&self) -> TypeId {
        self.i16
    }

    pub fn i32(&self) -> TypeId {
        self.i32
    }

    pub fn i64(&self) -> TypeId {
        self.i64
    }

    pub fn get(&self, id: TypeId) -> Option<&HirType> {
        let index = id.raw().checked_sub(1)? as usize;
        self.types.get(index)?.as_ref()
    }

    pub fn iter(&self) -> impl Iterator<Item = (TypeId, &HirType)> {
        self.types.iter().enumerate().filter_map(|(index, typ)| {
            typ.as_ref()
                .map(|typ| (TypeId::from_raw(index as u32 + 1), typ))
        })
    }

    pub(crate) fn alloc(&mut self, kind: HirTypeKind) -> TypeId {
        self.types.push(Some(HirType { kind }));
        TypeId::from_raw(self.types.len() as u32)
    }

    pub(crate) fn reserve(&mut self) -> TypeId {
        self.types.push(None);
        TypeId::from_raw(self.types.len() as u32)
    }

    pub(crate) fn define(&mut self, id: TypeId, kind: HirTypeKind) -> Result<(), TypeArenaError> {
        let index = id
            .raw()
            .checked_sub(1)
            .map(|index| index as usize)
            .ok_or(TypeArenaError::InvalidTypeId(id))?;
        let Some(slot) = self.types.get_mut(index) else {
            return Err(TypeArenaError::InvalidTypeId(id));
        };
        if slot.is_some() {
            return Err(TypeArenaError::TypeAlreadyDefined(id));
        }
        *slot = Some(HirType { kind });
        Ok(())
    }

    pub(crate) fn unfinished(&self) -> Option<TypeId> {
        self.types.iter().position(Option::is_none).map(|index| {
            TypeId::from_raw(
                u32::try_from(index + 1).expect("HIR type arena cannot exceed u32 entries"),
            )
        })
    }
}
