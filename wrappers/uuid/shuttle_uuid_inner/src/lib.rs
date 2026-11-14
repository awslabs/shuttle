//! This is the "inner" crate implementing [`uuid`] support for [`Shuttle`].
//! This crate should not be depended on directly, the intended way to use this crate is via
//! the `shuttle-uuid` crate and feature flag `shuttle`.
//!
//! This crate has uuid 1.29's API, with its own [`Uuid`], [`NonNilUuid`], [`Builder`],
//! [`Timestamp`], contexts and [`fmt`] types. Each wraps or reimplements uuid's and behaves the
//! same, so code written against uuid compiles and runs unchanged, except where uuid reads the
//! operating system's random number generator or the system clock, which Shuttle doesn't control:
//!
//! - Random bits come from Shuttle's random number generator, so they are part of the schedule:
//!   `Uuid::new_v4()`, `new_v7()` and `now_v7()`, the counter of a `ContextV7`, and
//!   `ContextV1::new_random()`.
//! - The time comes from a fake clock: `Timestamp::now()`, and with it `Uuid::now_v1()`,
//!   `now_v6()` and `now_v7()`. In every execution the clock starts at 2026-01-01T00:00:00Z, so the
//!   timestamps say nothing about the actual time. Each reading asks Shuttle whether the clock
//!   advances, and moves it on by zero or one millisecond, so a test sees both two UUIDs in the same
//!   millisecond, which is what exercises a `ContextV7`'s counter, and two in consecutive ones.
//! - The fake clock's state, and the contexts that `now_v1()`, `now_v6()` and `now_v7()` share, live
//!   in the storage of the running execution, the same storage Shuttle's own `Once` and
//!   `lazy_static` use. Shuttle drops it when the execution ends, so each of these starts afresh in
//!   every execution.
//!
//! So the same seed or schedule gives the same UUIDs, and a failing schedule replays with the UUIDs
//! it failed with. Like the rest of Shuttle, these constructors panic outside of a Shuttle test.
//!
//! # Under a scheduler that provides no random data
//!
//! `shuttle::check_dfs` searches the schedules exhaustively, so its scheduler provides no random
//! data, and nor does any `DfsScheduler` created without `allow_random_data`. Under such a
//! scheduler the constructors above draw nothing from Shuttle's generator:
//!
//! - Random bits come from a generator of the execution's own: a counter of the values drawn from
//!   it so far, which starts at zero in every execution, mixed into each value by SplitMix64's
//!   output function. No two of its values in an execution are equal, and they pass for random, so
//!   two UUIDs of an execution are as unlikely to be equal as two random ones. What an execution
//!   gets depends only on the order in which it draws, which the schedule decides, so the same
//!   schedule gives the same UUIDs in every run of it, its replay included.
//! - Each reading of the fake clock moves it on by exactly one millisecond, so every timestamp of an
//!   execution is a millisecond after the one before. The time-based UUIDs of an execution then
//!   never share a millisecond, and a `ContextV7` reseeds its counter for each of them.
//!
//! Neither adds a step to the schedule, so making UUIDs adds no choice to the search: `check_dfs`
//! explores the same schedules as it would without them. A replay of a schedule makes the choices
//! its execution made, whichever scheduler that was.
//!
//! The types are not uuid's own, so code that hands a UUID to a crate built on uuid converts it
//! with `.into()`: each type converts to and from uuid's, except that a `Timestamp` only converts
//! to uuid's, as uuid doesn't expose a timestamp's counter. Write those calls as
//! `#[allow(clippy::useless_conversion)] let real = uuid.into();`, or put the attribute on the
//! enclosing function: without the `shuttle` feature the conversion is from a type to itself, which
//! Clippy's `useless_conversion` lint reports, and with it the conversion is a real one.
//!
//! [`Shuttle`]: <https://crates.io/crates/shuttle>
//!
//! [`uuid`]: <https://crates.io/crates/uuid>

mod builder;
#[cfg(feature = "std")]
mod clock;
mod external;
pub mod fmt;
mod macros;
mod non_nil;
mod parser;
#[cfg(feature = "rng")]
mod rng;
pub mod timestamp;
mod versions;

use core::hash::{Hash, Hasher};

pub use builder::Builder;
pub use non_nil::NonNilUuid;
pub use timestamp::{ClockSequence, Timestamp, context::NoContext};
pub use uuid_orig::{Bytes, Error, Variant, Version};

#[cfg(any(feature = "v1", feature = "v6"))]
#[allow(deprecated)]
pub use timestamp::context::Context;

#[cfg(any(feature = "v1", feature = "v6"))]
pub use timestamp::context::ContextV1;

#[cfg(feature = "v7")]
pub use timestamp::context::ContextV7;

#[cfg(feature = "v1")]
#[doc(hidden)]
// Soft-deprecated, as in uuid.
pub mod v1 {
    #[deprecated(note = "use types from the crate root instead")]
    #[allow(deprecated)]
    pub use crate::{Timestamp, timestamp::context::Context};
}

#[cfg(feature = "serde")]
pub use external::serde_adapters as serde;

// As in uuid, the `zerocopy` derives need `--cfg uuid_unstable`; without it the feature only adds
// the dependency.
#[cfg(all(feature = "zerocopy", not(uuid_unstable)))]
use zerocopy as _;

#[doc(hidden)]
pub mod __macro_support {
    pub use core::result::Result::{Err, Ok};
}

/// A Universally Unique Identifier (UUID): uuid's [`Uuid`](uuid_orig::Uuid), whose constructors
/// that read randomness or the clock use random bits from Shuttle and the fake clock (see the crate
/// documentation).
///
/// It converts to and from uuid's `Uuid` with `.into()`.
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
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
pub struct Uuid(uuid_orig::Uuid);

impl Uuid {
    /// UUID namespace for Domain Name System (DNS).
    pub const NAMESPACE_DNS: Self = Uuid(uuid_orig::Uuid::NAMESPACE_DNS);

    /// UUID namespace for ISO Object Identifiers (OIDs).
    pub const NAMESPACE_OID: Self = Uuid(uuid_orig::Uuid::NAMESPACE_OID);

    /// UUID namespace for Uniform Resource Locators (URLs).
    pub const NAMESPACE_URL: Self = Uuid(uuid_orig::Uuid::NAMESPACE_URL);

    /// UUID namespace for X.500 Distinguished Names (DNs).
    pub const NAMESPACE_X500: Self = Uuid(uuid_orig::Uuid::NAMESPACE_X500);

    /// Returns the variant of the UUID structure.
    pub const fn get_variant(&self) -> Variant {
        self.0.get_variant()
    }

    /// Returns the version number of the UUID.
    pub const fn get_version_num(&self) -> usize {
        self.0.get_version_num()
    }

    /// Returns the version of the UUID.
    pub const fn get_version(&self) -> Option<Version> {
        self.0.get_version()
    }

    /// Returns the four field values of the UUID.
    pub fn as_fields(&self) -> (u32, u16, u16, &[u8; 8]) {
        self.0.as_fields()
    }

    /// Returns the four field values of the UUID in little-endian order.
    pub fn to_fields_le(&self) -> (u32, u16, u16, &[u8; 8]) {
        self.0.to_fields_le()
    }

    /// Returns a 128bit value containing the value.
    pub const fn as_u128(&self) -> u128 {
        self.0.as_u128()
    }

    /// Returns a 128bit little-endian value containing the value.
    pub const fn to_u128_le(&self) -> u128 {
        self.0.to_u128_le()
    }

    /// Returns two 64bit values containing the value.
    pub const fn as_u64_pair(&self) -> (u64, u64) {
        self.0.as_u64_pair()
    }

    /// Returns a slice of 16 octets containing the value.
    pub const fn as_bytes(&self) -> &Bytes {
        self.0.as_bytes()
    }

    /// Consumes self and returns the underlying byte value of the UUID.
    pub const fn into_bytes(self) -> Bytes {
        self.0.into_bytes()
    }

    /// Returns the bytes of the UUID in little-endian order.
    pub const fn to_bytes_le(&self) -> Bytes {
        self.0.to_bytes_le()
    }

    /// Tests if the UUID is nil (all zeros).
    pub const fn is_nil(&self) -> bool {
        self.0.is_nil()
    }

    /// Tests if the UUID is max (all ones).
    pub const fn is_max(&self) -> bool {
        self.0.is_max()
    }

    /// A buffer that can be used for `encode_...` calls, that is guaranteed to be long enough for
    /// any of the format adapters.
    pub const fn encode_buffer() -> [u8; fmt::Urn::LENGTH] {
        uuid_orig::Uuid::encode_buffer()
    }

    /// If the UUID is the correct version (v1, v6, or v7) this will return the timestamp in a
    /// version-agnostic [`Timestamp`]. For other versions this will return `None`.
    pub const fn get_timestamp(&self) -> Option<Timestamp> {
        match self.0.get_timestamp() {
            // uuid makes a version 7 UUID's timestamp from its milliseconds, without a counter,
            Some(ts) if matches!(self.get_version(), Some(Version::SortRand)) => {
                let (seconds, subsec_nanos) = ts.to_unix();
                Some(Timestamp::from_unix_time(seconds, subsec_nanos, 0, 0))
            }
            // and a version 1 or 6 UUID's from its ticks and clock sequence, which `to_gregorian`
            // returns exactly.
            Some(ts) => {
                let (ticks, counter) = ts.to_gregorian();
                Some(Timestamp::from_gregorian_time(ticks, counter))
            }
            None => None,
        }
    }

    /// If the UUID is the correct version (v1, or v6) this will return the node value as a 6-byte
    /// array. For other versions this will return `None`.
    pub const fn get_node_id(&self) -> Option<[u8; 6]> {
        self.0.get_node_id()
    }

    /// Views uuid's `Uuid` as this crate's.
    pub(crate) const fn from_uuid_ref(uuid: &uuid_orig::Uuid) -> &Uuid {
        // SAFETY: `Uuid` is `repr(transparent)` over `uuid_orig::Uuid`.
        unsafe { &*(uuid as *const uuid_orig::Uuid as *const Uuid) }
    }
}

impl Hash for Uuid {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl Default for Uuid {
    fn default() -> Self {
        Uuid::nil()
    }
}

impl AsRef<Uuid> for Uuid {
    fn as_ref(&self) -> &Uuid {
        self
    }
}

impl AsRef<[u8]> for Uuid {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

#[cfg(feature = "std")]
impl From<Uuid> for Vec<u8> {
    fn from(value: Uuid) -> Self {
        value.0.into()
    }
}

#[cfg(feature = "std")]
impl TryFrom<Vec<u8>> for Uuid {
    type Error = Error;

    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        Uuid::from_slice(&value)
    }
}

impl From<uuid_orig::Uuid> for Uuid {
    fn from(uuid: uuid_orig::Uuid) -> Self {
        Uuid(uuid)
    }
}

impl From<Uuid> for uuid_orig::Uuid {
    fn from(uuid: Uuid) -> Self {
        uuid.0
    }
}
