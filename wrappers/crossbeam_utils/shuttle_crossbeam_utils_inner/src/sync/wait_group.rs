//! `WaitGroup`, on Shuttle's `Mutex`, `Condvar` and atomics.

use shuttle::sync::atomic::{AtomicUsize, Ordering};
use shuttle::sync::{Condvar, Mutex};
use std::fmt;
use std::sync::Arc;

/// Enables threads to synchronize the beginning or end of some computation.
///
/// Every clone of a `WaitGroup` counts; [`WaitGroup::wait`] blocks until every one of them has
/// been dropped or waited on. Under Shuttle the waiting task blocks in Shuttle's `Condvar`, so an
/// execution in which some clone is never dropped is reported as a deadlock rather than hanging.
pub struct WaitGroup {
    inner: Arc<Inner>,
}

struct Inner {
    cvar: Condvar,
    lock: Mutex<()>,
    count: AtomicUsize,
}

impl Default for WaitGroup {
    fn default() -> Self {
        Self {
            inner: Arc::new(Inner {
                cvar: Condvar::new(),
                lock: Mutex::new(()),
                count: AtomicUsize::new(1),
            }),
        }
    }
}

impl WaitGroup {
    /// Creates a new wait group and returns the single reference to it.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drops this reference and waits until all other references are dropped.
    pub fn wait(self) {
        // Keep the state alive past the drop below, which releases this reference and wakes the
        // waiters if it was the last one.
        let inner = Arc::clone(&self.inner);
        drop(self);

        let mut guard = inner.lock.lock().unwrap();
        while inner.count.load(Ordering::Acquire) != 0 {
            guard = inner.cvar.wait(guard).unwrap();
        }
    }
}

impl Drop for WaitGroup {
    fn drop(&mut self) {
        if self.inner.count.fetch_sub(1, Ordering::Release) == 1 {
            drop(self.inner.lock.lock().unwrap());
            self.inner.cvar.notify_all();
        }
    }
}

impl Clone for WaitGroup {
    fn clone(&self) -> Self {
        self.inner.count.fetch_add(1, Ordering::Relaxed);
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl fmt::Debug for WaitGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self.inner.count.load(Ordering::Relaxed);
        f.debug_struct("WaitGroup").field("count", &count).finish()
    }
}
