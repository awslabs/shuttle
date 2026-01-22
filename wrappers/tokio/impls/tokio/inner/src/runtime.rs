//! Shuttle stubs for the tokio runtime

use crate::runtime::dump::Dump;
use crate::task::JoinHandle;
use criterion::async_executor::AsyncExecutor;
use std::fmt;
use std::future::Future;
use std::num::{NonZeroU32, NonZeroU64};
use std::ops::Range;
use std::time::Duration;

/// Runtime which is used to spawn and run async tasks
pub struct Runtime {
    handle: Handle,
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
        shuttle::future::block_on(future)
    }

    /// Returns a handle to the runtime's spawner.
    pub fn handle(&self) -> &Handle {
        &self.handle
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
        Ok(Runtime { handle: Handle {} })
    }
}

#[derive(Debug, Clone)]
pub struct Handle {}

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
    pub fn current() -> Self {
        Self {}
    }

    // TODO: Add a hook to Shuttle to check whether there is currently an ExecutionState, and return an error if there is not.
    pub fn try_current() -> Result<Self, TryCurrentError> {
        Ok(Self {})
    }

    /// Spawns a future onto the runtime
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        JoinHandle::new(shuttle::future::spawn(future))
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
        JoinHandle::new(shuttle::future::spawn(async { func() }))
    }

    /// Runs a future to completion on this `Handle`'s associated `Runtime`.
    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
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
