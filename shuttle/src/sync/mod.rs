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

use std::hash::{Hash, Hasher};
use std::panic::Location;
pub use std::sync::{LockResult, PoisonError, TryLockError, TryLockResult};

// TODO implement true support for `Arc`
pub use std::sync::{Arc, Weak};

const CONST_CONTEXT_SIGNATURE: u64 = 0;

/// A stable signature for identifying resources in Shuttle tests.
/// This is used internally to track and differentiate between different resource instances
/// *across* shuttle iterations. This signature hash is not *guaranteed* to be unique (two
/// different resources may have the same hash). In particular, resources which provide
/// `const` constructors cannot provide any *runtime* context to their signatures, so any
/// two resources created at the same source location will collide.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) enum ResourceSignature {
    Atomic(ResourceSignatureData),
    BatchSemaphore(ResourceSignatureData),
    Barrier(ResourceSignatureData),
    Condvar(ResourceSignatureData),
    Mutex(ResourceSignatureData),
    RwLock(ResourceSignatureData),
    Once(ResourceSignatureData),
    MpscChannel(ResourceSignatureData),
}

#[allow(unused)]
#[derive(Clone, Debug)]
pub(crate) struct ResourceSignatureData {
    static_create_location: &'static Location<'static>,
    parent_task_signature: u64,
    create_location_counter: u32,
    static_create_location_hash: u64,
    signature_hash: u64,
}

impl ResourceSignatureData {
    #[track_caller]
    pub(crate) const fn new_const() -> Self {
        let static_create_location = Location::caller();
        Self::new(static_create_location, CONST_CONTEXT_SIGNATURE, 1)
    }

    pub(crate) const fn new(
        static_create_location: &'static Location<'static>,
        parent_task_signature: u64,
        create_location_counter: u32,
    ) -> Self {
        let mut hasher = SipHasher::new();
        let file: &'static str = static_create_location.file();
        hasher.hash(file.as_bytes());
        hasher.write_u32(static_create_location.line());
        hasher.write_u32(static_create_location.column());
        let static_create_location_hash = hasher.finish();
        hasher.write_u64(static_create_location_hash);
        hasher.write_u64(parent_task_signature);
        hasher.write_u32(create_location_counter);
        let signature_hash = hasher.finish();

        Self {
            static_create_location,
            parent_task_signature,
            static_create_location_hash,
            create_location_counter,
            signature_hash,
        }
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

impl Hash for ResourceSignatureData {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.static_create_location_hash().hash(state);
        self.parent_task_signature.hash(state);
        self.create_location_counter.hash(state);
    }
}

impl PartialEq for ResourceSignatureData {
    fn eq(&self, other: &Self) -> bool {
        self.signature_hash() == other.signature_hash()
    }
}

impl Eq for ResourceSignatureData {}
