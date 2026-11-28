//! `Builder` and the constructors of `Uuid` that take its bits from the caller. They are all
//! deterministic, so they delegate to uuid's.

use crate::{Bytes, Error, Uuid, Variant, Version};

impl Uuid {
    /// The 'nil UUID' (all zeros).
    pub const fn nil() -> Self {
        Uuid(uuid_orig::Uuid::nil())
    }

    /// The 'max UUID' (all ones).
    pub const fn max() -> Self {
        Uuid(uuid_orig::Uuid::max())
    }

    /// Creates a UUID from four field values.
    pub const fn from_fields(d1: u32, d2: u16, d3: u16, d4: &[u8; 8]) -> Uuid {
        Uuid(uuid_orig::Uuid::from_fields(d1, d2, d3, d4))
    }

    /// Creates a UUID from four field values in little-endian order.
    pub const fn from_fields_le(d1: u32, d2: u16, d3: u16, d4: &[u8; 8]) -> Uuid {
        Uuid(uuid_orig::Uuid::from_fields_le(d1, d2, d3, d4))
    }

    /// Creates a UUID from a 128bit value.
    pub const fn from_u128(v: u128) -> Self {
        Uuid(uuid_orig::Uuid::from_u128(v))
    }

    /// Creates a UUID from a 128bit value in little-endian order.
    pub const fn from_u128_le(v: u128) -> Self {
        Uuid(uuid_orig::Uuid::from_u128_le(v))
    }

    /// Creates a UUID from two 64bit values.
    pub const fn from_u64_pair(high_bits: u64, low_bits: u64) -> Self {
        Uuid(uuid_orig::Uuid::from_u64_pair(high_bits, low_bits))
    }

    /// Creates a UUID using the supplied bytes.
    pub fn from_slice(b: &[u8]) -> Result<Uuid, Error> {
        uuid_orig::Uuid::from_slice(b).map(Uuid)
    }

    /// Creates a UUID using the supplied bytes in little endian order.
    pub fn from_slice_le(b: &[u8]) -> Result<Uuid, Error> {
        uuid_orig::Uuid::from_slice_le(b).map(Uuid)
    }

    /// Creates a UUID using the supplied bytes.
    pub const fn from_bytes(bytes: Bytes) -> Uuid {
        Uuid(uuid_orig::Uuid::from_bytes(bytes))
    }

    /// Creates a UUID using the supplied bytes in little endian order.
    pub const fn from_bytes_le(b: Bytes) -> Uuid {
        Uuid(uuid_orig::Uuid::from_bytes_le(b))
    }

    /// Creates a reference to a UUID from a reference to the supplied bytes.
    pub fn from_bytes_ref(bytes: &Bytes) -> &Uuid {
        Uuid::from_uuid_ref(uuid_orig::Uuid::from_bytes_ref(bytes))
    }
}

/// A builder for creating a UUID: uuid's [`Builder`](uuid_orig::Builder), building this crate's
/// [`Uuid`].
#[derive(Debug)]
pub struct Builder(Uuid);

impl Builder {
    /// Creates a `Builder` using the supplied bytes.
    pub const fn from_bytes(b: Bytes) -> Self {
        Builder(Uuid::from_bytes(b))
    }

    /// Creates a `Builder` using the supplied bytes in little endian order.
    pub const fn from_bytes_le(b: Bytes) -> Self {
        Builder(Uuid::from_bytes_le(b))
    }

    /// Creates a `Builder` for a version 1 UUID using the supplied timestamp, counter, and node ID.
    pub const fn from_gregorian_timestamp(ticks: u64, counter: u16, node_id: &[u8; 6]) -> Self {
        Self::from_uuid_orig(uuid_orig::Builder::from_gregorian_timestamp(ticks, counter, node_id))
    }

    /// Creates a `Builder` for a version 3 UUID using the supplied MD5 hashed bytes.
    pub const fn from_md5_bytes(md5_bytes: Bytes) -> Self {
        Self::from_uuid_orig(uuid_orig::Builder::from_md5_bytes(md5_bytes))
    }

    /// Creates a `Builder` for a version 4 UUID using the supplied random bytes.
    pub const fn from_random_bytes(random_bytes: Bytes) -> Self {
        Self::from_uuid_orig(uuid_orig::Builder::from_random_bytes(random_bytes))
    }

    /// Creates a `Builder` for a version 5 UUID using the supplied SHA-1 hashed bytes.
    pub const fn from_sha1_bytes(sha1_bytes: Bytes) -> Self {
        Self::from_uuid_orig(uuid_orig::Builder::from_sha1_bytes(sha1_bytes))
    }

    /// Creates a `Builder` for a version 6 UUID using the supplied timestamp, counter, and node ID.
    pub const fn from_sorted_gregorian_timestamp(ticks: u64, counter: u16, node_id: &[u8; 6]) -> Self {
        Self::from_uuid_orig(uuid_orig::Builder::from_sorted_gregorian_timestamp(
            ticks, counter, node_id,
        ))
    }

    /// Creates a `Builder` for a version 7 UUID using the supplied Unix timestamp and counter bytes.
    pub const fn from_unix_timestamp_millis(millis: u64, counter_random_bytes: &[u8; 10]) -> Self {
        Self::from_uuid_orig(uuid_orig::Builder::from_unix_timestamp_millis(
            millis,
            counter_random_bytes,
        ))
    }

    /// Creates a `Builder` for a version 8 UUID using the supplied user-defined bytes.
    pub const fn from_custom_bytes(custom_bytes: Bytes) -> Self {
        Self::from_uuid_orig(uuid_orig::Builder::from_custom_bytes(custom_bytes))
    }

    /// Creates a `Builder` using the supplied bytes.
    pub fn from_slice(b: &[u8]) -> Result<Self, Error> {
        Uuid::from_slice(b).map(Builder)
    }

    /// Creates a `Builder` using the supplied bytes in little endian order.
    pub fn from_slice_le(b: &[u8]) -> Result<Self, Error> {
        Uuid::from_slice_le(b).map(Builder)
    }

    /// Creates a `Builder` from four field values.
    pub const fn from_fields(d1: u32, d2: u16, d3: u16, d4: &[u8; 8]) -> Self {
        Builder(Uuid::from_fields(d1, d2, d3, d4))
    }

    /// Creates a `Builder` from four field values in little-endian order.
    pub const fn from_fields_le(d1: u32, d2: u16, d3: u16, d4: &[u8; 8]) -> Self {
        Builder(Uuid::from_fields_le(d1, d2, d3, d4))
    }

    /// Creates a `Builder` from a 128bit value.
    pub const fn from_u128(v: u128) -> Self {
        Builder(Uuid::from_u128(v))
    }

    /// Creates a UUID from a 128bit value in little-endian order.
    pub const fn from_u128_le(v: u128) -> Self {
        Builder(Uuid::from_u128_le(v))
    }

    /// Creates a `Builder` with an initial [`Uuid::nil`].
    pub const fn nil() -> Self {
        Builder(Uuid::nil())
    }

    /// Specifies the variant of the UUID.
    pub fn set_variant(&mut self, v: Variant) -> &mut Self {
        *self = Builder(self.0).with_variant(v);
        self
    }

    /// Specifies the variant of the UUID.
    pub const fn with_variant(self, v: Variant) -> Self {
        Self::from_uuid_orig(uuid_orig::Builder::from_bytes(self.0.into_bytes()).with_variant(v))
    }

    /// Specifies the version number of the UUID.
    pub fn set_version(&mut self, v: Version) -> &mut Self {
        *self = Builder(self.0).with_version(v);
        self
    }

    /// Specifies the version number of the UUID.
    pub const fn with_version(self, v: Version) -> Self {
        Self::from_uuid_orig(uuid_orig::Builder::from_bytes(self.0.into_bytes()).with_version(v))
    }

    /// Get a reference to the underlying [`Uuid`].
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }

    /// Convert the builder into a [`Uuid`].
    pub const fn into_uuid(self) -> Uuid {
        self.0
    }

    const fn from_uuid_orig(builder: uuid_orig::Builder) -> Self {
        Builder(Uuid(builder.into_uuid()))
    }
}

#[doc(hidden)]
impl Builder {
    #[deprecated(
        since = "1.10.0",
        note = "use `Builder::from_gregorian_timestamp(ticks, counter, node_id)`"
    )]
    pub const fn from_rfc4122_timestamp(ticks: u64, counter: u16, node_id: &[u8; 6]) -> Self {
        Builder::from_gregorian_timestamp(ticks, counter, node_id)
    }

    #[deprecated(
        since = "1.10.0",
        note = "use `Builder::from_sorted_gregorian_timestamp(ticks, counter, node_id)`"
    )]
    pub const fn from_sorted_rfc4122_timestamp(ticks: u64, counter: u16, node_id: &[u8; 6]) -> Self {
        Builder::from_sorted_gregorian_timestamp(ticks, counter, node_id)
    }
}

impl From<uuid_orig::Builder> for Builder {
    fn from(builder: uuid_orig::Builder) -> Self {
        Builder::from_uuid_orig(builder)
    }
}

impl From<Builder> for uuid_orig::Builder {
    fn from(builder: Builder) -> Self {
        uuid_orig::Builder::from_bytes(builder.0.into_bytes())
    }
}
