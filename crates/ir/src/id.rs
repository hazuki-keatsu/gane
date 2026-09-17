macro_rules! define_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(u32);

        impl $name {
            pub const INVALID: Self = Self(0);

            pub const fn is_valid(self) -> bool {
                self.0 != 0
            }

            pub const fn raw(self) -> u32 {
                self.0
            }

            pub(crate) const fn from_raw(raw: u32) -> Self {
                Self(raw)
            }
        }
    };
}

define_id!(TypeId);
define_id!(GlobalId);
define_id!(FunctionId);
define_id!(BlockId);
define_id!(ValueId);
define_id!(StackSlotId);
