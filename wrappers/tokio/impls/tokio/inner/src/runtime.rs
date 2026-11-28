//! Shuttle stubs for the tokio runtime
//!
//! A runtime owns the tasks spawned onto it, and shutting it down stops them as in tokio (see
//! `owned`). Otherwise all runtimes are the same: their tasks are Shuttle tasks like any other, and
//! the builder's settings mean nothing.

use crate::runtime::dump::Dump;
use crate::task::JoinHandle;
use criterion::async_executor::AsyncExecutor;
use owned::{assert_blocking_allowed, execution_over, EnterGuard, RuntimeContext, RuntimeState, Stopped};
use std::fmt;
use std::future::Future;
use std::num::{NonZeroU32, NonZeroU64};
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

mod owned;

/// Runtime which is used to spawn and run async tasks
pub struct Runtime {
    handle: Handle,
    /// Whether the runtime has been shut down, so that dropping it after `shutdown_timeout` or
    /// `shutdown_background` doesn't again.
    shut_down: bool,
}

impl Runtime {
    /// Create a new Runtime with default configuration
    pub fn new() -> std::io::Result<Runtime> {
        Builder::new_multi_thread().enable_all().build()
    }

    /// Spawns a future onto the runtime
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.handle.spawn(future)
    }

    // TODO: See comment in `Handle::spawn_blocking`
    pub fn spawn_blocking<F, R>(&self, func: F) -> JoinHandle<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        self.handle.spawn_blocking(func)
    }

    /// Runs a future to completion on this runtime.
    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.handle.block_on(future)
    }

    /// Returns a handle to the runtime's spawner.
    pub fn handle(&self) -> &Handle {
        &self.handle
    }

    /// Shuts the runtime down, and waits at most `duration` for its tasks to stop.
    ///
    /// As in tokio, the tasks other than `spawn_blocking` work are cancelled, and a task that is
    /// still running when the wait ends keeps running. A zero `duration` doesn't wait, and only that
    /// one may be used in an asynchronous context.
    ///
    /// SHUTTLE_CHANGES: Shuttle has no clock, so the wait is a [`timeout`](crate::time::timeout) like
    /// any other: it ends early only if a test triggers it (see
    /// [`trigger_timeouts`](crate::time::trigger_timeouts)), and otherwise lasts until every task has
    /// been dropped.
    pub fn shutdown_timeout(mut self, duration: Duration) {
        self.shut_down(Some(duration));
    }

    /// Shuts the runtime down without waiting for its tasks to stop. As in tokio, the tasks other
    /// than `spawn_blocking` work are cancelled, and unlike dropping the runtime, this may be done in
    /// an asynchronous context.
    pub fn shutdown_background(mut self) {
        self.shut_down(Some(Duration::ZERO));
    }

    /// Shuts the runtime down, and waits for its tasks to stop: at most `wait`, or until they all
    /// have if it is `None`.
    fn shut_down(&mut self, wait: Option<Duration>) {
        if std::mem::replace(&mut self.shut_down, true) {
            return;
        }
        let Some(runtime) = self.handle.runtime.clone() else {
            return;
        };
        // Outside an execution, or while Shuttle tears one down, there is nothing to stop: teardown
        // drops every task. While a panic unwinds, the test is failing, and waiting may not be
        // possible, so leave the tasks to teardown as well.
        if execution_over() || std::thread::panicking() {
            return;
        }
        let waits = wait != Some(Duration::ZERO);
        if waits {
            assert_blocking_allowed();
        }
        runtime.shut_down();
        match wait {
            None => shuttle::future::block_on(Stopped(runtime)),
            Some(duration) if waits => {
                let _ = shuttle::future::block_on(crate::time::timeout(duration, Stopped(runtime)));
            }
            Some(_) => {}
        }
    }
}

impl Drop for Runtime {
    /// Shuts the runtime down, and waits until all of its tasks have stopped, as tokio's does. Panics
    /// in an asynchronous context, as tokio's does.
    fn drop(&mut self) {
        self.shut_down(None);
    }
}

impl AsyncExecutor for Runtime {
    fn block_on<T>(&self, future: impl Future<Output = T>) -> T {
        self.block_on(future)
    }
}

impl AsyncExecutor for &Runtime {
    fn block_on<T>(&self, future: impl Future<Output = T>) -> T {
        (*self).block_on(future)
    }
}

// NOTE: This only exists to get stuff to compile, and currently returns `0`s, `false` and `Duration::new(0, 0)`s
// Protected behind tokio_unstable and feature rt.
#[derive(Clone, Debug)]
pub struct RuntimeMetrics {}

impl RuntimeMetrics {
    pub fn num_workers(&self) -> usize {
        0
    }

    pub fn num_blocking_threads(&self) -> usize {
        0
    }

    pub fn active_tasks_count(&self) -> usize {
        0
    }

    pub fn num_alive_tasks(&self) -> usize {
        0
    }

    pub fn num_idle_blocking_threads(&self) -> usize {
        0
    }

    pub fn remote_schedule_count(&self) -> u64 {
        0
    }

    pub fn budget_forced_yield_count(&self) -> u64 {
        0
    }

    pub fn worker_park_count(&self, _worker: usize) -> u64 {
        0
    }

    pub fn worker_noop_count(&self, _worker: usize) -> u64 {
        0
    }

    pub fn worker_steal_count(&self, _worker: usize) -> u64 {
        0
    }

    pub fn worker_steal_operations(&self, _worker: usize) -> u64 {
        0
    }

    pub fn worker_poll_count(&self, _worker: usize) -> u64 {
        0
    }

    pub fn worker_total_busy_duration(&self, _worker: usize) -> Duration {
        Duration::new(0, 0)
    }

    pub fn worker_local_schedule_count(&self, _worker: usize) -> u64 {
        0
    }

    pub fn worker_overflow_count(&self, _worker: usize) -> u64 {
        0
    }

    pub fn injection_queue_depth(&self) -> usize {
        0
    }

    pub fn worker_local_queue_depth(&self, _worker: usize) -> usize {
        0
    }

    pub fn poll_count_histogram_enabled(&self) -> bool {
        false
    }

    pub fn poll_count_histogram_num_buckets(&self) -> usize {
        0
    }

    pub fn poll_count_histogram_bucket_range(&self, _bucket: usize) -> Range<Duration> {
        Duration::new(0, 0)..Duration::new(0, 0)
    }

    pub fn poll_count_histogram_bucket_count(&self, _worker: usize, _bucket: usize) -> u64 {
        0
    }

    pub fn worker_mean_poll_time(&self, _worker: usize) -> Duration {
        Duration::new(0, 0)
    }

    pub fn blocking_queue_depth(&self) -> usize {
        0
    }
}

// Protected behind cfg! net
impl RuntimeMetrics {
    pub fn io_driver_fd_registered_count(&self) -> u64 {
        0
    }

    pub fn io_driver_fd_deregistered_count(&self) -> u64 {
        0
    }

    pub fn io_driver_ready_count(&self) -> u64 {
        0
    }
}

/// Builds a runtime with custom configuration
pub struct Builder {}

impl Builder {
    /// Returns a new builder with the current thread scheduler
    pub fn new_current_thread() -> Self {
        Self {}
    }

    /// Returns a new builder with the multi thread scheduler
    pub fn new_multi_thread() -> Self {
        Self::new_current_thread()
    }

    /// Enables both I/O and time drivers
    pub fn enable_all(&mut self) -> &mut Self {
        self
    }

    /// Enables the time driver
    pub fn enable_time(&mut self) -> &mut Self {
        self
    }

    /// Start tasks paused
    pub fn start_paused(&mut self, _paused: bool) -> &mut Self {
        self
    }

    /// Sets thread name
    pub fn thread_name(&mut self, _name: impl Into<String>) -> &mut Self {
        self
    }

    /// Sets a function used to generate the name of threads spawned by the `Runtime`'s thread pool.
    pub fn thread_name_fn<F>(&mut self, _f: F) -> &mut Self
    where
        F: Fn() -> String + Send + Sync + 'static,
    {
        self
    }

    /// Sets the number of worker threads the `Runtime` will use.
    pub fn worker_threads(&mut self, val: usize) -> &mut Self {
        assert!(val > 0, "Worker threads cannot be set to 0");
        self
    }

    /// Creates the configured `Runtime`
    pub fn build(&mut self) -> std::io::Result<Runtime> {
        Ok(Runtime {
            handle: Handle {
                runtime: Some(Arc::new(RuntimeState::default())),
            },
            shut_down: false,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Handle {
    /// The runtime, or `None` for the handle that `Handle::current` returns outside any runtime's
    /// context. Unlike tokio, which panics there, Shuttle lets code spawn tasks there, which belong to
    /// no runtime.
    runtime: Option<Arc<RuntimeState>>,
}

#[derive(Debug)]
pub struct TryCurrentError {}

impl std::fmt::Display for TryCurrentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TryCurrentError")
    }
}

impl std::error::Error for TryCurrentError {}

#[derive(Debug, PartialEq, Eq)]
pub enum RuntimeFlavor {
    CurrentThread,
    MultiThread,
}

impl Handle {
    // TODO?: Make this panic when outside of Shuttle?
    /// Returns a `Handle` view over the currently running `Runtime`
    ///
    /// SHUTTLE_CHANGES: Outside any runtime's context, this returns a handle that spawns tasks that
    /// belong to no runtime, where tokio panics.
    pub fn current() -> Self {
        Self {
            runtime: RuntimeContext::current().map(|context| context.runtime),
        }
    }

    // TODO: Add a hook to Shuttle to check whether there is currently an ExecutionState, and return an error if there is not.
    pub fn try_current() -> Result<Self, TryCurrentError> {
        Ok(Self::current())
    }

    /// Spawns a future onto the runtime
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        match &self.runtime {
            Some(runtime) => JoinHandle::new(runtime.spawn(future, false)),
            None => JoinHandle::new(shuttle::future::spawn(future)),
        }
    }

    // TODO:
    // There is a case to be made for implementing this inside Shuttle, and having it map to `Task::from_closure`
    // (over `Task::from_future`, which is "wrong").
    // Deciding not to do that for now, as the API is inherently tied to tokio (as is `spawn` and `shuttle::future::JoinHandle`),
    // and I'd rather move those APIs out, than more in. We also generally don't want to encourage the use of `spawn_blocking`,
    // since we don't have any notion of an executor dedicated to blocking operations, and the kind of things one would run which
    // block the runtime are not the kind of things which should be run under Shuttle. It exists here to enable plug-and-play compilation.
    // TODO END.
    pub fn spawn_blocking<F, R>(&self, func: F) -> JoinHandle<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        match &self.runtime {
            Some(runtime) => JoinHandle::new(runtime.spawn(async { func() }, true)),
            None => JoinHandle::new(shuttle::future::spawn(async { func() })),
        }
    }

    /// Runs a future to completion on this `Handle`'s associated `Runtime`.
    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        let _context = self.runtime.as_ref().map(EnterGuard::enter);
        shuttle::future::block_on(future)
    }

    // NOTE: only available on `tokio_unstable`
    // NOTE: Shuttle does not have a notion of runtimes so this always returns 1.
    pub fn id(&self) -> Id {
        Id(NonZeroU64::new(1).unwrap())
    }

    // NOTE: only available on `tokio_unstable`
    pub fn metrics(&self) -> RuntimeMetrics {
        RuntimeMetrics {}
    }

    /// Captures a snapshot of the runtime's state.
    pub async fn dump(&self) -> Dump {
        unimplemented!();
    }

    /// Returns the flavor of the current `Runtime`.
    pub fn runtime_flavor(&self) -> RuntimeFlavor {
        unimplemented!()
    }
}

impl AsyncExecutor for Handle {
    fn block_on<T>(&self, future: impl Future<Output = T>) -> T {
        self.block_on(future)
    }
}

impl AsyncExecutor for &Handle {
    fn block_on<T>(&self, future: impl Future<Output = T>) -> T {
        (*self).block_on(future)
    }
}

// This whole module is unimplemented. It exists just to get code to compile.
mod dump {
    #[derive(Debug)]
    pub struct Dump {}

    #[derive(Debug)]
    pub struct Tasks {
        tasks: Vec<Task>,
    }

    #[derive(Debug)]
    pub struct Task {}

    #[derive(Debug)]
    pub struct Trace {}

    impl Dump {
        pub fn tasks(&self) -> &Tasks {
            unimplemented!();
        }
    }

    impl Tasks {
        pub fn iter(&self) -> impl Iterator<Item = &Task> {
            self.tasks.iter()
        }
    }

    impl Task {
        pub fn id(&self) -> crate::task::Id {
            unimplemented!()
        }

        pub fn trace(&self) -> &Trace {
            unimplemented!()
        }
    }

    impl std::fmt::Display for Trace {
        fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            unimplemented!()
        }
    }
}

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq)]
pub struct Id(NonZeroU64);

impl From<NonZeroU64> for Id {
    fn from(value: NonZeroU64) -> Self {
        Id(value)
    }
}

impl From<NonZeroU32> for Id {
    fn from(value: NonZeroU32) -> Self {
        Id(value.into())
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
