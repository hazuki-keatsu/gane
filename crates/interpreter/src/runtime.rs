#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeValue {
    Bits(u64),
    Pointer(Pointer),
}

impl RuntimeValue {
    pub(crate) fn bits(&self) -> u64 {
        match self {
            Self::Bits(bits) => *bits,
            Self::Pointer(_) => unreachable!("verified IR supplied a pointer as an integer"),
        }
    }

    pub(crate) fn pointer(&self) -> &Pointer {
        match self {
            Self::Pointer(pointer) => pointer,
            Self::Bits(_) => unreachable!("verified IR supplied an integer as a pointer"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Pointer {
    Null,
    Address {
        root: PointerRoot,
        projections: Vec<Projection>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PointerRoot {
    Global(usize),
    Stack { frame: usize, slot: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Projection {
    Field(usize),
    Index(usize),
}
