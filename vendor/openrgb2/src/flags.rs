// SPDX-License-Identifier: GPL-2.0-or-later
//! SDK u32 flag sets backed by the MIT-licensed bitflags implementation.

/// Compatibility name for an SDK flag set. Each flag type is itself a set.
pub type FlagSet<T> = T;

#[doc(hidden)]
#[macro_export]
macro_rules! sdk_flags {
    ($(#[$meta:meta])* $vis:vis enum $name:ident: u32 {
        $($(#[$($flag_meta:tt)*])* $flag:ident = $value:expr,)*
    }) => {
        bitflags::bitflags! {
            $(#[$meta])*
            #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
            $vis struct $name: u32 {
                $(
                    $(#[$($flag_meta)*])*
                    #[allow(non_upper_case_globals, reason = "Preserve upstream SDK flag names")]
                    const $flag = $value;
                )*
            }
        }

        impl $name {
            /// All known SDK flags, excluding future bits.
            pub const fn full() -> Self { Self::all() }

            /// Retain known SDK bits and ignore future bits during discovery.
            pub const fn new_truncated(value: u32) -> Self { Self::from_bits_truncate(value) }
        }

        impl $crate::protocol::DeserFromBuf for $name {
            fn deserialize(buf: &mut $crate::protocol::ReceivedMessage<'_>) -> $crate::OpenRgbResult<Self> {
                Ok(Self::from_bits_truncate(buf.read_value::<u32>()?))
            }
        }

        impl $crate::protocol::SerToBuf for $name {
            fn serialize(&self, buf: &mut $crate::protocol::WriteMessage) -> $crate::OpenRgbResult<()> {
                buf.write_u32(self.bits());
                Ok(())
            }
        }
    };
}
