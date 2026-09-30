/// A scalar value held by the interpreter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeValue {
    /// The raw bit pattern of an integer or boolean value.
    Bits(u64),
    /// A null pointer or an address into interpreter-managed memory.
    Pointer(Pointer),
}

impl RuntimeValue {
    /// Returns the raw bits of an integer or boolean value.
    pub(crate) fn bits(&self) -> u64 {
        match self {
            Self::Bits(bits) => *bits,
            Self::Pointer(_) => unreachable!("verified IR supplied a pointer as an integer"),
        }
    }

    /// Returns the represented pointer.
    pub(crate) fn pointer(&self) -> &Pointer {
        match self {
            Self::Pointer(pointer) => pointer,
            Self::Bits(_) => unreachable!("verified IR supplied an integer as a pointer"),
        }
    }
}

/// A pointer into the interpreter's structured memory model.
///
/// An address identifies a root object and a path through its aggregate
/// children. An empty projection path addresses the root object itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Pointer {
    /// The null pointer.
    Null,
    /// An address rooted in a global or stack object.
    Address {
        /// The object from which address resolution starts.
        root: PointerRoot,
        /// The field and array-element path from the root to the addressed object.
        projections: Vec<Projection>,
    },
}

/// The interpreter-owned object at the base of an address.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PointerRoot {
    /// A package global, indexed in the interpreter's global storage.
    Global(usize),
    /// A stack slot in a specific call frame.
    Stack {
        generation: u64,
        frame: usize,
        slot: usize,
    },
}

/// One step through an aggregate object while resolving an address.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Projection {
    /// Selects a field from a struct object.
    Field(usize),
    /// Selects an element from an array object.
    Index(usize),
}
