//! uuid's implementations of other crates' traits, each behind the same feature as in uuid, and
//! delegating to uuid's.

#[cfg(feature = "serde")]
mod serde_support {
    use crate::fmt::{Braced, Hyphenated, Simple, Urn};
    use crate::{NonNilUuid, Uuid};
    use serde_core::{Deserialize, Deserializer, Serialize, Serializer};

    macro_rules! delegate_serde {
        ($($T:ty => $Orig:ty;)+) => {$(
            impl Serialize for $T {
                fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                    <$Orig>::from(*self).serialize(serializer)
                }
            }

            impl<'de> Deserialize<'de> for $T {
                fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                    <$Orig>::deserialize(deserializer).map(<$T>::from)
                }
            }
        )+};
    }

    delegate_serde! {
        Uuid => uuid_orig::Uuid;
        NonNilUuid => uuid_orig::NonNilUuid;
        Hyphenated => uuid_orig::fmt::Hyphenated;
        Simple => uuid_orig::fmt::Simple;
        Urn => uuid_orig::fmt::Urn;
        Braced => uuid_orig::fmt::Braced;
    }
}

/// Adapters for alternative `serde` formats, for use with `#[serde(with)]`: uuid's, for this
/// crate's [`Uuid`](crate::Uuid).
#[cfg(feature = "serde")]
pub mod serde_adapters {
    macro_rules! adapters {
        ($($(#[$doc:meta])* $name:ident;)+) => {$(
            $(#[$doc])*
            pub mod $name {
                use crate::Uuid;

                /// Serialize from a [`Uuid`].
                pub fn serialize<S>(u: &Uuid, serializer: S) -> Result<S::Ok, S::Error>
                where
                    S: serde_core::Serializer,
                {
                    uuid_orig::serde::$name::serialize(&(*u).into(), serializer)
                }

                /// Deserialize into a [`Uuid`].
                pub fn deserialize<'de, D>(deserializer: D) -> Result<Uuid, D::Error>
                where
                    D: serde_core::Deserializer<'de>,
                {
                    uuid_orig::serde::$name::deserialize(deserializer).map(Uuid::from)
                }

                /// The same format, for an `Option<Uuid>`.
                pub mod option {
                    use crate::Uuid;

                    /// Serialize from an `Option<Uuid>`.
                    pub fn serialize<S>(u: &Option<Uuid>, serializer: S) -> Result<S::Ok, S::Error>
                    where
                        S: serde_core::Serializer,
                    {
                        uuid_orig::serde::$name::option::serialize(&u.map(Into::into), serializer)
                    }

                    /// Deserialize into an `Option<Uuid>`.
                    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Uuid>, D::Error>
                    where
                        D: serde_core::Deserializer<'de>,
                    {
                        uuid_orig::serde::$name::option::deserialize(deserializer).map(|u| u.map(Uuid::from))
                    }
                }
            }
        )+};
    }

    adapters! {
        /// Serialize a [`Uuid`](crate::Uuid) as a `[u8; 16]`.
        compact;
        /// Serialize a [`Uuid`](crate::Uuid) as bytes.
        bytes;
        /// Serialize a [`Uuid`](crate::Uuid) as a simple string.
        simple;
        /// Serialize a [`Uuid`](crate::Uuid) as a braced string.
        braced;
        /// Serialize a [`Uuid`](crate::Uuid) as a hyphenated string.
        hyphenated;
        /// Serialize a [`Uuid`](crate::Uuid) as a URN string.
        urn;
    }
}

#[cfg(feature = "arbitrary")]
mod arbitrary_support {
    use crate::{NonNilUuid, Uuid};
    use arbitrary::{Arbitrary, Unstructured};

    impl<'a> Arbitrary<'a> for Uuid {
        fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
            uuid_orig::Uuid::arbitrary(u).map(Uuid::from)
        }

        fn size_hint(depth: usize) -> (usize, Option<usize>) {
            <uuid_orig::Uuid as Arbitrary<'a>>::size_hint(depth)
        }
    }

    impl<'a> Arbitrary<'a> for NonNilUuid {
        fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
            uuid_orig::NonNilUuid::arbitrary(u).map(NonNilUuid::from)
        }

        fn size_hint(depth: usize) -> (usize, Option<usize>) {
            <uuid_orig::NonNilUuid as Arbitrary<'a>>::size_hint(depth)
        }
    }
}

#[cfg(feature = "borsh")]
mod borsh_support {
    use crate::Uuid;
    use borsh::{BorshDeserialize, BorshSerialize, io};

    impl BorshSerialize for Uuid {
        fn serialize<W: io::Write>(&self, writer: &mut W) -> io::Result<()> {
            self.0.serialize(writer)
        }
    }

    impl BorshDeserialize for Uuid {
        fn deserialize_reader<R: io::Read>(reader: &mut R) -> io::Result<Self> {
            uuid_orig::Uuid::deserialize_reader(reader).map(Uuid)
        }
    }
}

#[cfg(feature = "bytemuck")]
mod bytemuck_support {
    use crate::{Bytes, Uuid};

    // SAFETY: `Uuid` is `repr(transparent)` over uuid's `Uuid`, which is `repr(transparent)` over
    // `Bytes` and `Pod` itself, so all zeros is a valid `Uuid`, as is every other bit pattern.
    unsafe impl bytemuck::Zeroable for Uuid {}

    // SAFETY: as above.
    unsafe impl bytemuck::Pod for Uuid {}

    // SAFETY: `Uuid` is a transparent wrapper of a transparent wrapper of `Bytes`, which bytemuck
    // allows.
    unsafe impl bytemuck::TransparentWrapper<Bytes> for Uuid {}
}

#[cfg(feature = "slog")]
mod slog_support {
    use crate::{NonNilUuid, Uuid};

    impl slog::Value for Uuid {
        fn serialize(
            &self,
            record: &slog::Record<'_>,
            key: slog::Key,
            serializer: &mut dyn slog::Serializer,
        ) -> Result<(), slog::Error> {
            slog::Value::serialize(&self.0, record, key, serializer)
        }
    }

    impl slog::Value for NonNilUuid {
        fn serialize(
            &self,
            record: &slog::Record<'_>,
            key: slog::Key,
            serializer: &mut dyn slog::Serializer,
        ) -> Result<(), slog::Error> {
            slog::Value::serialize(&uuid_orig::NonNilUuid::from(*self), record, key, serializer)
        }
    }
}
