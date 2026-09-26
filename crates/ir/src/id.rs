//! Strongly typed identifiers used to refer to entities in Gane IR.
//!
//! Valid identifiers are one-based arena indices. The zero value is reserved
//! as [`INVALID`](TypeId::INVALID) for each identifier type.

macro_rules! define_id {
    ($name:ident) => {
        impl $name {
            /// The sentinel value for an absent or invalid identifier.
            pub const INVALID: Self = Self(0);

            /// Returns whether this identifier refers to an arena entry.
            pub const fn is_valid(self) -> bool {
                self.0 != 0
            }

            /// Returns the one-based arena index backing this identifier.
            pub const fn raw(self) -> u32 {
                self.0
            }

            /// Creates an identifier from its raw arena index.
            ///
            /// Callers must preserve the convention that zero is invalid and
            /// valid arena entries start at one.
            pub(crate) const fn from_raw(raw: u32) -> Self {
                Self(raw)
            }
        }
    };
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
/// Identifies a type in the IR type arena.
pub struct TypeId(u32);

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
/// Identifies a global variable in an IR package.
pub struct GlobalId(u32);

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
/// Identifies a function in an IR package.
pub struct FunctionId(u32);

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
/// Identifies a basic block within an IR function.
pub struct BlockId(u32);

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
/// Identifies an SSA value within an IR function.
pub struct ValueId(u32);

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
/// Identifies a stack slot within an IR function.
pub struct StackSlotId(u32);

define_id!(TypeId);
define_id!(GlobalId);
define_id!(FunctionId);
define_id!(BlockId);
define_id!(ValueId);
define_id!(StackSlotId);
