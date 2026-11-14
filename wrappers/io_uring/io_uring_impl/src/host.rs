//! The simulated kernel's state that outlives a ring but not a Shuttle
//! execution: the fds an [`AsyncFd`](crate::AsyncFd) can wait on, the ops in
//! flight the [`buffers`](crate::buffers) checks watch, and the settings a test
//! chose ([`faults`](crate::faults), [`buffers::Checks`] and
//! [`version::Kernel`]).
//!
//! It lives in the storage of the running execution, which Shuttle drops when
//! the execution ends. So every execution starts with the defaults, no ops in
//! flight and the same ring fd numbers, whatever an earlier one did, and a
//! failing execution replays on its own as it ran in the test.
//!
//! A ring, and each op it watches, holds the host of the execution that created
//! it. Their destructors reach the host through it rather than through the
//! execution's storage, which teardown may have dropped by then: a ring in a
//! `lazy_static!` is dropped after the host's slot.

use std::sync::{Arc, Mutex, MutexGuard};

use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::storage::StorageKey;

use crate::async_fd;
use crate::buffers;
use crate::faults::{Errors, Faults, Posted};
use crate::version::Kernel;

/// One execution's simulated kernel.
pub(crate) struct Host {
    state: Mutex<HostState>,
}

#[derive(Default)]
pub(crate) struct HostState {
    /// The fds an `AsyncFd` can wait on, and the next ring fd.
    pub(crate) waitable: async_fd::Registry,
    /// The ops in flight on every ring, as the buffer checks see them.
    pub(crate) watched: buffers::Registry,
    pub(crate) checks: buffers::Checks,
    pub(crate) faults: Faults,
    pub(crate) errors: Errors,
    pub(crate) posted: Posted,
    /// The kernel new rings behave as.
    pub(crate) kernel: Kernel,
}

/// The host's slot in the execution's storage.
fn key() -> StorageKey {
    static ID: u8 = 0;
    // The second half tells this slot apart from Shuttle's own, whose first
    // half is also an address (`Once` uses 0x2, `lazy_static` 0x3).
    StorageKey(&ID as *const u8 as usize, 0x696f_7572)
}

impl Host {
    /// The running execution's host, created on first use. Panics outside a
    /// Shuttle test, as the rest of Shuttle does.
    pub(crate) fn current() -> Arc<Host> {
        ExecutionState::with(|state| {
            if let Some(host) = state.get_storage::<_, Arc<Host>>(key()) {
                return Arc::clone(host);
            }
            let host = Arc::new(Host {
                state: Mutex::new(HostState::default()),
            });
            state.init_storage(key(), Arc::clone(&host));
            host
        })
    }

    /// A plain lock, so it takes no scheduling point: one inside a submit
    /// would let the kernel task run between two SQEs of the same
    /// `io_uring_enter`. Nothing that can switch tasks runs under it, and it
    /// never nests.
    pub(crate) fn lock(&self) -> MutexGuard<'_, HostState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}
