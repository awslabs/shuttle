//! Real OS threads, real-time sleep, and the real `std::thread` handles, invisible to Shuttle.
//!
//! Each type is an alias of the `std` type, and each function calls the `std` function.
//! [`Builder`] and [`Scope`] are newtypes, because `.spawn(..)` through an alias would resolve to
//! the `std` method, which the lint gate denies once it lists `std::thread`.

use std::io;
use std::num::NonZeroUsize;
use std::time::Duration;

/// [`std::thread::JoinHandle`], invisible to Shuttle.
pub type JoinHandle<T> = std::thread::JoinHandle<T>;

/// [`std::thread::ScopedJoinHandle`], invisible to Shuttle.
pub type ScopedJoinHandle<'scope, T> = std::thread::ScopedJoinHandle<'scope, T>;

/// [`std::thread::Thread`], invisible to Shuttle: a handle to a real OS thread.
pub type Thread = std::thread::Thread;

/// [`std::thread::ThreadId`], invisible to Shuttle: the id of a real OS thread.
pub type ThreadId = std::thread::ThreadId;

/// [`std::thread::spawn`]: starts a real OS thread, which Shuttle doesn't schedule.
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::spawn(f)
}

/// [`std::thread::sleep`]: blocks the calling OS thread for `dur` of real time.
///
/// Inside a Shuttle test this blocks every task of the execution, since they share one OS
/// thread.
#[inline]
pub fn sleep(dur: Duration) {
    std::thread::sleep(dur)
}

/// [`std::thread::yield_now`]: yields the calling OS thread to the operating system.
///
/// It is not a scheduling point, so inside a Shuttle test it does not let another task run. A
/// loop that waits for a Shuttle task to make progress must yield through Shuttle instead, or it
/// spins forever and the test hangs.
#[inline]
pub fn yield_now() {
    std::thread::yield_now()
}

/// [`std::thread::park`]: blocks the calling OS thread until another thread unparks it.
///
/// Inside a Shuttle test this blocks every task of the execution, so only a real OS thread
/// outside Shuttle's control can unpark it.
#[inline]
pub fn park() {
    std::thread::park()
}

/// [`std::thread::park_timeout`]: blocks the calling OS thread for at most `dur` of real time.
///
/// Inside a Shuttle test this blocks every task of the execution, as [`park`] does.
#[inline]
pub fn park_timeout(dur: Duration) {
    std::thread::park_timeout(dur)
}

/// [`std::thread::current`]: the real OS thread this code runs on.
///
/// Shuttle runs every task of an execution on one OS thread, so inside a Shuttle test all tasks
/// get the same [`ThreadId`] and name from this. Use `shuttle::thread::current` for per-task
/// identity.
#[inline]
pub fn current() -> Thread {
    std::thread::current()
}

/// [`std::thread::available_parallelism`]: the host's real parallelism.
///
/// The value depends on the host, so a Shuttle test that sizes anything by it does not replay on
/// a machine with a different core count.
#[inline]
pub fn available_parallelism() -> io::Result<NonZeroUsize> {
    std::thread::available_parallelism()
}

/// [`std::thread::scope`]: a scope for real OS threads that may borrow from the caller.
///
/// The closure gets a backdoor [`Scope`] with the same `spawn` method. The scope joins every
/// thread it spawned before returning, which blocks the calling OS thread, and inside a Shuttle
/// test that blocks every task of the execution.
pub fn scope<'env, F, T>(f: F) -> T
where
    F: for<'scope> FnOnce(Scope<'scope, 'env>) -> T,
{
    std::thread::scope(|inner| f(Scope { inner }))
}

/// [`std::thread::Scope`], invisible to Shuttle.
///
/// This is a newtype, not an alias: through an alias, `.spawn(..)` would resolve to
/// `std::thread::Scope::spawn`, which the lint gate denies once it lists `std::thread`. It is
/// `Copy` and is passed by value, so the source of a scope body reads as it does with `std`.
#[derive(Debug, Clone, Copy)]
pub struct Scope<'scope, 'env: 'scope> {
    inner: &'scope std::thread::Scope<'scope, 'env>,
}

impl<'scope, 'env> Scope<'scope, 'env> {
    /// [`std::thread::Scope::spawn`]: starts a real OS thread, which Shuttle doesn't schedule.
    pub fn spawn<F, T>(&self, f: F) -> ScopedJoinHandle<'scope, T>
    where
        F: FnOnce() -> T + Send + 'scope,
        T: Send + 'scope,
    {
        self.inner.spawn(f)
    }
}

/// [`std::thread::Builder`], invisible to Shuttle.
///
/// This is a newtype, not an alias: through an alias, `.spawn(..)` would resolve to
/// `std::thread::Builder::spawn`, which the lint gate denies once it lists `std::thread`.
#[derive(Debug)]
pub struct Builder(std::thread::Builder);

impl Builder {
    /// [`std::thread::Builder::new`].
    pub fn new() -> Builder {
        Builder(std::thread::Builder::new())
    }

    /// [`std::thread::Builder::name`].
    pub fn name(self, name: String) -> Builder {
        Builder(self.0.name(name))
    }

    /// [`std::thread::Builder::stack_size`].
    pub fn stack_size(self, size: usize) -> Builder {
        Builder(self.0.stack_size(size))
    }

    /// [`std::thread::Builder::spawn`]: starts a real OS thread, which Shuttle doesn't schedule.
    pub fn spawn<F, T>(self, f: F) -> io::Result<JoinHandle<T>>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        self.0.spawn(f)
    }

    /// [`std::thread::Builder::spawn_scoped`]: starts a real OS thread in `scope`, which Shuttle
    /// doesn't schedule.
    pub fn spawn_scoped<'scope, 'env, F, T>(
        self,
        scope: Scope<'scope, 'env>,
        f: F,
    ) -> io::Result<ScopedJoinHandle<'scope, T>>
    where
        F: FnOnce() -> T + Send + 'scope,
        T: Send + 'scope,
    {
        self.0.spawn_scoped(scope.inner, f)
    }
}

impl Default for Builder {
    fn default() -> Builder {
        Builder::new()
    }
}
