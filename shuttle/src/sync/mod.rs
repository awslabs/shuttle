//! Shuttle's implementation of [`std::sync`].

pub mod atomic;
mod barrier;
mod condvar;
pub mod mpsc;
mod mutex;
mod once;
mod rwlock;

pub use barrier::{Barrier, BarrierWaitResult};
pub use condvar::{Condvar, WaitTimeoutResult};

use const_siphasher::sip::SipHasher;
pub use mutex::Mutex;
pub use mutex::MutexGuard;

pub use once::Once;
pub use once::OnceState;

pub use rwlock::RwLock;
pub use rwlock::RwLockReadGuard;
pub use rwlock::RwLockWriteGuard;

use std::hash::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::panic::Location;
pub use std::sync::{LockResult, PoisonError, TryLockError, TryLockResult};

// TODO implement true support for `Arc`
pub use std::sync::{Arc, Weak};

const CONST_CONTEXT_SIGNATURE: u64 = 0;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) enum TypedResourceSignature {
    Atomic(ResourceSignature),
    BatchSemaphore(ResourceSignature),
    Barrier(ResourceSignature),
    Condvar(ResourceSignature),
    Mutex(ResourceSignature),
    RwLock(ResourceSignature),
    Once(ResourceSignature),
    MpscChannel(ResourceSignature),
}

#[derive(Clone, Debug)]
/// A unique signature for identifying resources in Shuttle tests.
/// This is used internally to track and differentiate between different resource instances.
pub(crate) struct ResourceSignature {
    static_create_location: &'static Location<'static>,
    parent_task_signature: u64,
    create_location_counter: u32,
    static_create_location_hash: u64,
    signature_hash: u64,
}

const fn hash_bytes<'a>(hasher: &'a mut SipHasher, bytes: &'static [u8], index: usize) -> &'a mut SipHasher {
    if index < bytes.len() {
        hasher.write_u8(bytes[index]);
        hash_bytes(hasher, bytes, index + 1)
    } else {
        hasher
    }
}

impl ResourceSignature {
    pub(crate) const fn new_const(static_create_location: &'static Location<'static>) -> Self {
        let mut hasher = SipHasher::new();
        let file: &'static str = static_create_location.file();
        // Location::hash is not const, so we need to loop over the bytes
        // Tail recursive helper allows the loop to be unrolled to the length of the str
        hash_bytes(&mut hasher, file.as_bytes(), 0);
        let static_create_location_hash = hasher.finish();
        hasher.write_u64(static_create_location_hash);
        hasher.write_u32(static_create_location.line());
        hasher.write_u32(static_create_location.column());
        let signature_hash = hasher.finish();

        Self {
            static_create_location,
            parent_task_signature: CONST_CONTEXT_SIGNATURE,
            static_create_location_hash,
            create_location_counter: 1,
            signature_hash,
        }
    }

    pub(crate) fn new(
        static_create_location: &'static Location<'static>,
        parent_task_signature: u64,
        create_location_counter: u32,
    ) -> Self {
        let mut hasher = DefaultHasher::new();
        let mut rs = Self {
            static_create_location,
            parent_task_signature,
            static_create_location_hash: 0,
            create_location_counter,
            signature_hash: 0,
        };

        rs.static_create_location.hash(&mut hasher);
        rs.static_create_location_hash = hasher.finish();

        rs.hash(&mut hasher);
        rs.signature_hash = hasher.finish();

        rs
    }

    /// Hash of the static location within the source code where the task was spawned
    pub(crate) fn static_create_location_hash(&self) -> u64 {
        self.static_create_location_hash
    }

    /// Combined signature of the static location and dynamic context
    /// context where the task was spawned.
    pub(crate) fn signature_hash(&self) -> u64 {
        self.signature_hash
    }
}

impl Hash for ResourceSignature {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.static_create_location_hash().hash(state);
        self.parent_task_signature.hash(state);
        self.create_location_counter.hash(state);
    }
}

impl PartialEq for ResourceSignature {
    fn eq(&self, other: &Self) -> bool {
        self.signature_hash() == other.signature_hash()
    }
}

impl Eq for ResourceSignature {}
