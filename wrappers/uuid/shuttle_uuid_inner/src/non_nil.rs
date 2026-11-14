//! `NonNilUuid`, whose constructors use this crate's `Uuid` constructors, so the ones that read
//! randomness or the clock are Shuttle's too.

use crate::{Error, Uuid};
use core::{cmp::Ordering, fmt};

/// A UUID that is guaranteed not to be the nil UUID: uuid's
/// [`NonNilUuid`](uuid_orig::NonNilUuid), for this crate's [`Uuid`].
///
/// It converts to and from uuid's `NonNilUuid` with `.into()`.
#[repr(transparent)]
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NonNilUuid(uuid_orig::NonNilUuid);

impl fmt::Debug for NonNilUuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for NonNilUuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl PartialEq<Uuid> for NonNilUuid {
    fn eq(&self, other: &Uuid) -> bool {
        self.get() == *other
    }
}

impl PartialEq<NonNilUuid> for Uuid {
    fn eq(&self, other: &NonNilUuid) -> bool {
        *self == other.get()
    }
}

impl PartialOrd<Uuid> for NonNilUuid {
    fn partial_cmp(&self, other: &Uuid) -> Option<Ordering> {
        self.get().partial_cmp(other)
    }
}

impl PartialOrd<NonNilUuid> for Uuid {
    fn partial_cmp(&self, other: &NonNilUuid) -> Option<Ordering> {
        self.partial_cmp(&other.get())
    }
}

impl NonNilUuid {
    /// Creates a non-nil UUID if the value is non-nil.
    pub const fn new(uuid: Uuid) -> Option<Self> {
        match uuid_orig::NonNilUuid::new(uuid.0) {
            Some(non_nil) => Some(NonNilUuid(non_nil)),
            None => None,
        }
    }

    /// Creates a non-nil UUID without checking whether the value is non-nil.
    ///
    /// # Safety
    ///
    /// The value must not be nil.
    pub const unsafe fn new_unchecked(uuid: Uuid) -> Self {
        // SAFETY: the caller guarantees that the value is not nil.
        NonNilUuid(unsafe { uuid_orig::NonNilUuid::new_unchecked(uuid.0) })
    }

    /// Get the underlying [`Uuid`] value.
    pub const fn get(self) -> Uuid {
        Uuid(self.0.get())
    }

    /// Creates a non-nil UUID of a version, which is never nil, as its version field is set.
    #[cfg(any(
        feature = "v1",
        feature = "v3",
        feature = "v4",
        feature = "v5",
        feature = "v6",
        feature = "v7",
        feature = "v8"
    ))]
    const fn versioned(uuid: Uuid) -> Self {
        match Self::new(uuid) {
            Some(non_nil) => non_nil,
            None => panic!("a UUID with its version set is never nil"),
        }
    }

    /// Creates a non-nil version 1 UUID: see [`Uuid::new_v1`].
    #[cfg(feature = "v1")]
    pub fn new_v1(ts: crate::Timestamp, node_id: &[u8; 6]) -> Self {
        Self::versioned(Uuid::new_v1(ts, node_id))
    }

    /// Creates a non-nil version 1 UUID: see [`Uuid::now_v1`].
    #[cfg(all(feature = "v1", feature = "std", feature = "rng"))]
    pub fn now_v1(node_id: &[u8; 6]) -> Self {
        Self::versioned(Uuid::now_v1(node_id))
    }

    /// Creates a non-nil version 3 UUID: see [`Uuid::new_v3`].
    #[cfg(feature = "v3")]
    pub fn new_v3(namespace: &Uuid, name: &[u8]) -> Self {
        Self::versioned(Uuid::new_v3(namespace, name))
    }

    /// Creates a non-nil version 4 UUID: see [`Uuid::new_v4`].
    #[cfg(feature = "v4")]
    pub fn new_v4() -> Self {
        Self::versioned(Uuid::new_v4())
    }

    /// Creates a non-nil version 5 UUID: see [`Uuid::new_v5`].
    #[cfg(feature = "v5")]
    pub fn new_v5(namespace: &Uuid, name: &[u8]) -> Self {
        Self::versioned(Uuid::new_v5(namespace, name))
    }

    /// Creates a non-nil version 6 UUID: see [`Uuid::new_v6`].
    #[cfg(feature = "v6")]
    pub fn new_v6(ts: crate::Timestamp, node_id: &[u8; 6]) -> Self {
        Self::versioned(Uuid::new_v6(ts, node_id))
    }

    /// Creates a non-nil version 6 UUID: see [`Uuid::now_v6`].
    #[cfg(all(feature = "v6", feature = "std", feature = "rng"))]
    pub fn now_v6(node_id: &[u8; 6]) -> Self {
        Self::versioned(Uuid::now_v6(node_id))
    }

    /// Creates a non-nil version 7 UUID: see [`Uuid::new_v7`].
    #[cfg(feature = "v7")]
    pub fn new_v7(ts: crate::Timestamp) -> Self {
        Self::versioned(Uuid::new_v7(ts))
    }

    /// Creates a non-nil version 7 UUID: see [`Uuid::now_v7`].
    #[cfg(all(feature = "v7", feature = "std"))]
    pub fn now_v7() -> Self {
        Self::versioned(Uuid::now_v7())
    }

    /// Creates a non-nil version 8 UUID: see [`Uuid::new_v8`].
    #[cfg(feature = "v8")]
    pub const fn new_v8(bytes: [u8; 16]) -> Self {
        Self::versioned(Uuid::new_v8(bytes))
    }
}

impl From<NonNilUuid> for Uuid {
    fn from(non_nil: NonNilUuid) -> Self {
        non_nil.get()
    }
}

impl TryFrom<Uuid> for NonNilUuid {
    type Error = Error;

    fn try_from(uuid: Uuid) -> Result<Self, Self::Error> {
        uuid_orig::NonNilUuid::try_from(uuid.0).map(NonNilUuid)
    }
}

impl From<uuid_orig::NonNilUuid> for NonNilUuid {
    fn from(non_nil: uuid_orig::NonNilUuid) -> Self {
        NonNilUuid(non_nil)
    }
}

impl From<NonNilUuid> for uuid_orig::NonNilUuid {
    fn from(non_nil: NonNilUuid) -> Self {
        non_nil.0
    }
}
