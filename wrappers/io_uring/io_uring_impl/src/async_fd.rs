//! Readiness on a simulated ring, mirroring [`tokio::io::unix::AsyncFd`].
//!
//! Under Shuttle, tokio's `AsyncFd` is tokio's own: it needs a runtime with
//! an I/O driver, which a Shuttle execution does not have, and it could not
//! see a simulated kernel's events anyway. This `AsyncFd` waits on the two
//! fds the simulated kernel signals: a ring's own fd, readable whenever a
//! completion posts, and an eventfd registered with a ring, signalled per
//! `IORING_CQ_EVENTFD_DISABLED` as the kernel does. Any other fd is an error.
//!
//! Readiness follows tokio's rules: it stays set until the guard that
//! observed it is cleared, and clearing a guard does not clear an event that
//! arrived after the guard was taken.
//!
//! [`tokio::io::unix::AsyncFd`]: https://docs.rs/tokio/1/tokio/io/unix/struct.AsyncFd.html

use std::collections::HashMap;
use std::future::poll_fn;
use std::io;
use std::mem;
use std::ops::BitOr;
use std::os::fd::{AsRawFd, RawFd};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};

/// Simulated ring fds start here, above any fd the process opens, so they
/// never collide with the eventfds sharing the registry.
const RING_FD_BASE: RawFd = 1 << 28;

/// The fds an `AsyncFd` can wait on in this execution.
struct Registry {
    next_ring_fd: RawFd,
    fds: HashMap<RawFd, Arc<Readiness>>,
}

shuttle::lazy_static! {
    /// Per execution: Shuttle resets it between executions, so a reused
    /// eventfd number never sees an earlier execution's ring.
    static ref REGISTRY: Mutex<Registry> = Mutex::new(Registry {
        next_ring_fd: RING_FD_BASE,
        fds: HashMap::new(),
    });
}

fn registry() -> MutexGuard<'static, Registry> {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner())
}

/// An fd number for a new simulated ring.
pub(crate) fn alloc_ring_fd() -> RawFd {
    let mut registry = registry();
    let fd = registry.next_ring_fd;
    registry.next_ring_fd += 1;
    fd
}

/// Make `fd` waitable. Re-registering a number (an eventfd closed and
/// reopened) replaces the old entry; `AsyncFd`s built on it keep the old one.
pub(crate) fn register(fd: RawFd, readiness: Arc<Readiness>) {
    registry().fds.insert(fd, readiness);
}

/// Forget `fd`, if it still maps to `readiness`.
pub(crate) fn unregister(fd: RawFd, readiness: &Arc<Readiness>) {
    let mut registry = registry();
    if registry
        .fds
        .get(&fd)
        .is_some_and(|current| Arc::ptr_eq(current, readiness))
    {
        registry.fds.remove(&fd);
    }
}

/// Whether an fd is readable, and who waits for it to be.
pub(crate) struct Readiness {
    state: Mutex<ReadinessState>,
}

struct ReadinessState {
    /// Bumped by every event, so a clear can tell whether a newer one came.
    tick: u64,
    ready: bool,
    waiters: Vec<Waker>,
}

impl Readiness {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(ReadinessState {
                tick: 0,
                ready: false,
                waiters: Vec::new(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, ReadinessState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// An event: the fd is readable. Wakes every waiter.
    pub(crate) fn signal(&self) {
        let waiters = {
            let mut state = self.lock();
            state.tick += 1;
            state.ready = true;
            mem::take(&mut state.waiters)
        };
        for waiter in waiters {
            waiter.wake();
        }
    }

    /// Ready with the current event's tick, or register to be woken.
    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<u64> {
        let mut state = self.lock();
        if state.ready {
            return Poll::Ready(state.tick);
        }
        if !state.waiters.iter().any(|w| w.will_wake(cx.waker())) {
            state.waiters.push(cx.waker().clone());
        }
        Poll::Pending
    }

    /// Clear readiness observed at `tick`, unless a newer event arrived.
    fn clear(&self, tick: u64) {
        let mut state = self.lock();
        if state.tick == tick {
            state.ready = false;
        }
    }
}

/// Readiness interest, mirroring [`tokio::io::Interest`]. The simulation
/// signals readability only; the other interests are accepted.
///
/// [`tokio::io::Interest`]: https://docs.rs/tokio/1/tokio/io/struct.Interest.html
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interest(u8);

impl Interest {
    pub const READABLE: Interest = Interest(1);
    pub const WRITABLE: Interest = Interest(1 << 1);
    pub const PRIORITY: Interest = Interest(1 << 2);
    pub const ERROR: Interest = Interest(1 << 3);

    pub const fn is_readable(self) -> bool {
        self.0 & Self::READABLE.0 != 0
    }

    pub const fn is_writable(self) -> bool {
        self.0 & Self::WRITABLE.0 != 0
    }

    pub const fn is_priority(self) -> bool {
        self.0 & Self::PRIORITY.0 != 0
    }

    pub const fn is_error(self) -> bool {
        self.0 & Self::ERROR.0 != 0
    }

    pub const fn add(self, other: Interest) -> Interest {
        Interest(self.0 | other.0)
    }
}

impl BitOr for Interest {
    type Output = Interest;

    fn bitor(self, other: Interest) -> Interest {
        self.add(other)
    }
}

/// An fd whose readiness a simulated ring drives, mirroring tokio's
/// `AsyncFd`.
pub struct AsyncFd<T: AsRawFd> {
    inner: Option<T>,
    readiness: Arc<Readiness>,
}

impl<T: AsRawFd> AsyncFd<T> {
    /// Wait on `inner` for readability.
    pub fn new(inner: T) -> io::Result<Self> {
        Self::with_interest(inner, Interest::READABLE)
    }

    /// Wait on `inner`. It must be a simulated ring's fd or an eventfd
    /// registered with one.
    pub fn with_interest(inner: T, _interest: Interest) -> io::Result<Self> {
        let fd = inner.as_raw_fd();
        let readiness = registry().fds.get(&fd).cloned();
        let Some(readiness) = readiness else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "shuttle-io-uring: AsyncFd on fd {fd}, which is neither a simulated \
                     ring nor an eventfd registered with one"
                ),
            ));
        };
        Ok(Self {
            inner: Some(inner),
            readiness,
        })
    }

    pub fn get_ref(&self) -> &T {
        self.inner.as_ref().expect("AsyncFd holds its fd until into_inner")
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.inner.as_mut().expect("AsyncFd holds its fd until into_inner")
    }

    pub fn into_inner(mut self) -> T {
        self.inner.take().expect("AsyncFd holds its fd until into_inner")
    }

    /// Resolve once the fd is readable.
    pub async fn readable(&self) -> io::Result<AsyncFdReadyGuard<'_, T>> {
        poll_fn(|cx| self.poll_read_ready(cx)).await
    }

    /// Poll for readability, registering `cx`'s waker if not readable.
    pub fn poll_read_ready<'a>(&'a self, cx: &mut Context<'_>) -> Poll<io::Result<AsyncFdReadyGuard<'a, T>>> {
        self.readiness
            .poll_ready(cx)
            .map(|tick| Ok(AsyncFdReadyGuard { async_fd: self, tick }))
    }
}

impl<T: AsRawFd> AsRawFd for AsyncFd<T> {
    fn as_raw_fd(&self) -> RawFd {
        self.get_ref().as_raw_fd()
    }
}

/// Readiness an `AsyncFd` observed, mirroring tokio's `AsyncFdReadyGuard`.
pub struct AsyncFdReadyGuard<'a, T: AsRawFd> {
    async_fd: &'a AsyncFd<T>,
    tick: u64,
}

impl<'a, T: AsRawFd> AsyncFdReadyGuard<'a, T> {
    /// Mark the fd not readable until its next event. An event that arrived
    /// after this guard was taken stays set.
    pub fn clear_ready(&mut self) {
        self.async_fd.readiness.clear(self.tick);
    }

    /// Leave the readiness set.
    pub fn retain_ready(&mut self) {}

    /// Run `f`, the I/O this readiness was waited for. If it reports
    /// `WouldBlock`, the fd was not ready after all: clear the readiness, as
    /// tokio does, so the next wait waits for a new event, and return
    /// [`TryIoError`].
    pub fn try_io<R>(&mut self, f: impl FnOnce(&'a AsyncFd<T>) -> io::Result<R>) -> Result<io::Result<R>, TryIoError> {
        match f(self.async_fd) {
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                self.clear_ready();
                Err(TryIoError(()))
            }
            result => Ok(result),
        }
    }

    pub fn get_ref(&self) -> &'a AsyncFd<T> {
        self.async_fd
    }

    pub fn get_inner(&self) -> &'a T {
        self.async_fd.get_ref()
    }
}

/// [`AsyncFdReadyGuard::try_io`] found the fd not ready, mirroring tokio's
/// `TryIoError`.
#[derive(Debug)]
pub struct TryIoError(());
