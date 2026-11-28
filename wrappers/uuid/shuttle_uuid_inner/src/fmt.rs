//! Adapters for alternative string formats: uuid's, for this crate's [`Uuid`].

use crate::{Error, Uuid};
use core::{borrow::Borrow, fmt, mem::MaybeUninit, str::FromStr};

impl fmt::Debug for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::LowerHex for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, f)
    }
}

impl fmt::UpperHex for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::UpperHex::fmt(&self.0, f)
    }
}

#[cfg(feature = "std")]
impl From<Uuid> for String {
    fn from(uuid: Uuid) -> Self {
        uuid.0.into()
    }
}

macro_rules! format_types {
    ($($(#[$doc:meta])* $T:ident, $as_T:ident, $T_lower:ident;)+) => {$(
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[cfg_attr(
            all(uuid_unstable, feature = "zerocopy"),
            derive(
                zerocopy::IntoBytes,
                zerocopy::FromBytes,
                zerocopy::KnownLayout,
                zerocopy::Immutable,
                zerocopy::Unaligned
            )
        )]
        #[repr(transparent)]
        pub struct $T(Uuid);

        impl Uuid {
            #[doc = concat!("Get a [`", stringify!($T), "`] formatter.")]
            pub const fn $T_lower(self) -> $T {
                $T(self)
            }

            #[doc = concat!("Get a borrowed [`", stringify!($T), "`] formatter.")]
            pub fn $as_T(&self) -> &$T {
                // SAFETY: the formatter is `repr(transparent)` over `Uuid`.
                unsafe { &*(self as *const Uuid as *const $T) }
            }
        }

        impl $T {
            /// The length of the formatted string, in bytes.
            pub const LENGTH: usize = uuid_orig::fmt::$T::LENGTH;

            #[doc = concat!("Creates a [`", stringify!($T), "`] from a [`Uuid`].")]
            pub const fn from_uuid(uuid: Uuid) -> Self {
                $T(uuid)
            }

            /// Writes the UUID as a lower-case string to `buffer`, and returns the subslice of the
            /// buffer that contains the encoded UUID.
            pub fn encode_lower<'buf>(&self, buffer: &'buf mut [u8]) -> &'buf mut str {
                uuid_orig::fmt::$T::from_uuid(self.0.0).encode_lower(buffer)
            }

            /// Like `encode_lower`, into a buffer that may be uninitialized.
            pub fn encode_lower_uninit<'buf>(&self, buffer: &'buf mut [MaybeUninit<u8>]) -> &'buf mut str {
                uuid_orig::fmt::$T::from_uuid(self.0.0).encode_lower_uninit(buffer)
            }

            /// Writes the UUID as an upper-case string to `buffer`, and returns the subslice of the
            /// buffer that contains the encoded UUID.
            pub fn encode_upper<'buf>(&self, buffer: &'buf mut [u8]) -> &'buf mut str {
                uuid_orig::fmt::$T::from_uuid(self.0.0).encode_upper(buffer)
            }

            /// Like `encode_upper`, into a buffer that may be uninitialized.
            pub fn encode_upper_uninit<'buf>(&self, buffer: &'buf mut [MaybeUninit<u8>]) -> &'buf mut str {
                uuid_orig::fmt::$T::from_uuid(self.0.0).encode_upper_uninit(buffer)
            }

            /// Get a reference to the underlying [`Uuid`].
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            /// Consumes the formatter, returning the underlying [`Uuid`].
            pub const fn into_uuid(self) -> Uuid {
                self.0
            }
        }

        impl FromStr for $T {
            type Err = Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                uuid_orig::fmt::$T::from_str(s).map(Self::from)
            }
        }

        impl fmt::Display for $T {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&uuid_orig::fmt::$T::from_uuid(self.0.0), f)
            }
        }

        impl fmt::LowerHex for $T {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::LowerHex::fmt(&uuid_orig::fmt::$T::from_uuid(self.0.0), f)
            }
        }

        impl fmt::UpperHex for $T {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::UpperHex::fmt(&uuid_orig::fmt::$T::from_uuid(self.0.0), f)
            }
        }

        impl From<Uuid> for $T {
            fn from(f: Uuid) -> Self {
                $T(f)
            }
        }

        impl From<$T> for Uuid {
            fn from(f: $T) -> Self {
                f.into_uuid()
            }
        }

        impl AsRef<Uuid> for $T {
            fn as_ref(&self) -> &Uuid {
                &self.0
            }
        }

        impl Borrow<Uuid> for $T {
            fn borrow(&self) -> &Uuid {
                &self.0
            }
        }

        impl From<uuid_orig::fmt::$T> for $T {
            fn from(f: uuid_orig::fmt::$T) -> Self {
                $T(Uuid(f.into_uuid()))
            }
        }

        impl From<$T> for uuid_orig::fmt::$T {
            fn from(f: $T) -> Self {
                uuid_orig::fmt::$T::from_uuid(f.0.0)
            }
        }
    )+};
}

format_types! {
    /// Format a [`Uuid`] as a hyphenated string, like `67e55044-10b1-426f-9247-bb680e5fe0c8`.
    Hyphenated, as_hyphenated, hyphenated;
    /// Format a [`Uuid`] as a simple string, like `67e5504410b1426f9247bb680e5fe0c8`.
    Simple, as_simple, simple;
    /// Format a [`Uuid`] as a URN string, like `urn:uuid:67e55044-10b1-426f-9247-bb680e5fe0c8`.
    Urn, as_urn, urn;
    /// Format a [`Uuid`] as a braced hyphenated string, like
    /// `{67e55044-10b1-426f-9247-bb680e5fe0c8}`.
    Braced, as_braced, braced;
}
