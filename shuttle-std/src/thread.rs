//! Shuttle's implementation of [`std::thread`].

use shuttle_core::runtime::execution::ExecutionState;
use shuttle_core::runtime::task::TaskId;
use shuttle_core::runtime::thread;
use std::marker::PhantomData;
use std::panic::Location;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

pub use std::thread::{panicking, Result};

/// A unique identifier for a running thread
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ThreadId {
    // TODO Should we add an execution id here, like Loom does?
    task_id: TaskId,
}

impl From<ThreadId> for usize {
    fn from(id: ThreadId) -> usize {
        id.task_id.into()
    }
}

/// A handle to a thread.
#[derive(Debug, Clone)]
pub struct Thread {
    name: Option<String>,
    id: ThreadId,
}

impl Thread {
    /// Gets the thread's name.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Gets the thread's unique identifier
    pub fn id(&self) -> ThreadId {
        self.id
    }

    /// Atomically makes the handle's token available if it is not already.
    pub fn unpark(&self) {
        thread::switch();

        ExecutionState::with(|s| {
            s.get_mut(self.id.task_id).unpark();
        });
    }
}

/// A scope to spawn scoped threads in.
///
/// See [`scope`] for details.
pub struct Scope<'scope, 'env: 'scope> {
    num_running_threads: AtomicUsize,
    main_task: TaskId,
    scope: PhantomData<&'scope mut &'scope ()>,
    env: PhantomData<&'env mut &'env ()>,
}

impl std::fmt::Debug for Scope<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scope")
            .field("num_running_threads", &self.num_running_threads.load(Ordering::Relaxed))
            .field("main_thread", &self.main_task)
            .finish_non_exhaustive()
    }
}

impl<'scope> Scope<'scope, '_> {
    /// Spawns a new thread within a scope, returning a [`ScopedJoinHandle`] for it.
    ///
    /// Unlike non-scoped threads, threads spawned with this function may
    /// borrow non-`'static` data from the outside the scope. See [`scope`] for
    /// details.
    #[track_caller]
    pub fn spawn<F, T>(&'scope self, f: F) -> ScopedJoinHandle<'scope, T>
    where
        F: FnOnce() -> T + Send + 'scope,
        T: Send + 'scope,
    {
        self.num_running_threads.fetch_add(1, Ordering::Relaxed);

        let finished = std::sync::Arc::new(AtomicBool::new(false));
        let scope_closure = {
            let finished = finished.clone();
            move || {
                let ret = f();

                if ExecutionState::with(|s| s.exit_current_truncates_execution()) {
                    thread::switch();
                }

                finished.store(true, Ordering::Relaxed);

                if self.num_running_threads.fetch_sub(1, Ordering::Relaxed) == 1 {
                    ExecutionState::with(|s| s.get_mut(self.main_task).unblock());
                }

                ret
            }
        };

        // Note: Scoped threads wrap their inner function in some additional logic (above) to update the `finished` variable on termination.
        // This logic is expected to run atomically with scoped thread termination, so there *cannot* be a switch after the atomic store.
        // To avoid violating this invariant, we pass `switch_before_exit = false` (below). Instead, we provide our own context switch on exit
        // (above) in the `scope_closure` *before* setting `finished` to be `true`.
        // SAFETY: main task is blocked until all scoped closures complete so all captured references remain valid
        ScopedJoinHandle {
            handle: unsafe { spawn_named_unchecked(scope_closure, None, None, false, Location::caller()) },
            finished,
            _marker: PhantomData,
        }
    }
}

/// Creates a scope for spawning scoped threads.
///
/// The function passed to `scope` will be provided a [`Scope`] object,
/// through which scoped threads can be [spawned][`Scope::spawn`].
pub fn scope<'env, F, T>(f: F) -> T
where
    F: for<'scope> FnOnce(&'scope Scope<'scope, 'env>) -> T,
{
    let scope = Scope {
        num_running_threads: AtomicUsize::new(0),
        main_task: ExecutionState::with(|s| s.current().id()),
        env: PhantomData,
        scope: PhantomData,
    };

    let ret = f(&scope);

    if scope.num_running_threads.load(Ordering::Relaxed) != 0 {
        tracing::info!("thread blocked, waiting for completion of scoped threads");
        ExecutionState::with(|s| s.current_mut().block(false));
        thread::switch();
    }

    ret
}

/// Spawn a new thread, returning a JoinHandle for it.
///
/// The join handle can be used (via the `join` method) to block until the child thread has
/// finished.
#[track_caller]
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T,
    F: Send + 'static,
    T: Send + 'static,
{
    spawn_named(f, None, None, Location::caller())
}

fn spawn_named<F, T>(
    f: F,
    name: Option<String>,
    stack_size: Option<usize>,
    caller: &'static Location<'static>,
) -> JoinHandle<T>
where
    F: FnOnce() -> T,
    F: Send + 'static,
    T: Send + 'static,
{
    // SAFETY: F is static so all captured references must be `static and therefore
    // will outlive the spawned continuation
    unsafe { spawn_named_unchecked(f, name, stack_size, true, caller) }
}

/// Must ensure all captured references in f are valid for at least as long as the spawned continuation will run
unsafe fn spawn_named_unchecked<F, T>(
    f: F,
    name: Option<String>,
    stack_size: Option<usize>,
    switch_before_exit: bool,
    caller: &'static Location<'static>,
) -> JoinHandle<T>
where
    F: FnOnce() -> T,
    T: Send,
{
    // TODO Check if it's worth avoiding the call to `ExecutionState::config()` if we're going
    // TODO to use an existing continuation from the pool.
    let stack_size = stack_size.unwrap_or_else(|| ExecutionState::with(|s| s.config.stack_size));
    let result = std::sync::Arc::new(std::sync::Mutex::new(None));
    let task_id = {
        let result = std::sync::Arc::clone(&result);

        // Allocate `thread_fn` on the heap and assume a `'static` bound.
        let f: Box<dyn FnOnce()> = Box::new(move || thread_fn(f, switch_before_exit, result));
        let f: Box<dyn FnOnce() + 'static> = unsafe { std::mem::transmute(f) };

        ExecutionState::spawn_thread(f, stack_size, name.clone(), None, caller)
    };

    let thread = Thread {
        id: ThreadId { task_id },
        name,
    };

    JoinHandle {
        task_id,
        thread,
        result,
    }
}

/// Body of a Shuttle thread, that runs the given closure, handles thread-local destructors, and
pub(crate) use shuttle_core::thread_support::thread_fn;

/// An owned permission to join on a scoped thread (block on its termination).
///
/// See [`Scope::spawn`] for details.
#[derive(Debug)]
pub struct ScopedJoinHandle<'scope, T> {
    handle: JoinHandle<T>,
    finished: std::sync::Arc<AtomicBool>,
    _marker: PhantomData<&'scope T>,
}

impl<T> ScopedJoinHandle<'_, T> {
    /// Waits for the associated thread to finish.
    pub fn join(self) -> Result<T> {
        self.handle.join()
    }

    /// Extracts a handle to the underlying thread.
    pub fn thread(&self) -> &Thread {
        self.handle.thread()
    }

    /// Checks if the associated thread has finished running its main function.
    ///
    /// This might return `true` for a brief moment after the thread's main
    /// function has returned, but before the thread itself has stopped running.
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Relaxed)
    }
}

/// An owned permission to join on a thread (block on its termination).
#[derive(Debug)]
pub struct JoinHandle<T> {
    task_id: TaskId,
    thread: Thread,
    result: std::sync::Arc<std::sync::Mutex<Option<Result<T>>>>,
}

unsafe impl<T> Send for JoinHandle<T> {}
unsafe impl<T> Sync for JoinHandle<T> {}

impl<T> JoinHandle<T> {
    /// Waits for the associated thread to finish.
    pub fn join(self) -> Result<T> {
        let is_finished = ExecutionState::with(|state| state.get(self.task_id).finished());
        // If the joinee task is finished then the joiner will not block
        if is_finished {
            thread::switch();
        }

        let should_block = ExecutionState::with(|state| {
            let me = state.current().id();
            let target = state.get_mut(self.task_id);
            if target.set_waiter(me) {
                state.current_mut().block(false);
                true
            } else {
                false
            }
        });

        if should_block {
            thread::switch();
        }

        // Waiting thread inherits the clock of the finished thread
        ExecutionState::with(|state| {
            let target = state.get_mut(self.task_id);
            let clock = target.clock.clone();
            state.update_clock(&clock);
        });

        self.result.lock().unwrap().take().expect("target should have finished")
    }

    /// Extracts a handle to the underlying thread.
    pub fn thread(&self) -> &Thread {
        &self.thread
    }
}

/// Cooperatively gives up a timeslice to the Shuttle scheduler.
///
/// Some Shuttle schedulers use this as a hint to deprioritize the current thread in order for other
/// threads to make progress (e.g., in a spin loop).
pub fn yield_now() {
    let waker = ExecutionState::with(|state| state.current().waker());
    waker.wake_by_ref();
    ExecutionState::request_yield();
    thread::switch();
}

/// Puts the current thread to sleep for at least the specified amount of time.
// Note that Shuttle does not model time, so this behaves just like a context switch.
pub fn sleep(_dur: Duration) {
    thread::switch();
}

/// Get a handle to the thread that invokes it
pub fn current() -> Thread {
    let (task_id, name) = ExecutionState::with(|s| {
        let me = s.current();
        (me.id(), me.name())
    });

    Thread {
        id: ThreadId { task_id },
        name,
    }
}

/// Blocks unless or until the current thread's token is made available (may wake spuriously).
pub fn park() {
    let switch = ExecutionState::with(|s| s.current_mut().park());

    // We only need to context switch if the park token was unavailable. If it was available, then
    // any execution reachable by context switching here would also be reachable by having not
    // chosen this thread at the last context switch, because the park state of a thread is only
    // observable by the thread itself. We also mark it as an explicit yield request by the task,
    // since otherwise some schedulers might prefer to to reschedule the current task, which in this
    // context would result in spurious wakeups triggering nearly every time.
    if switch {
        ExecutionState::request_yield();
        thread::switch();
    }
}

/// Blocks unless or until the current thread's token is made available or the specified duration
/// has been reached (may wake spuriously).
///
/// Note that Shuttle does not model time, so this behaves identically to `park`. In particular,
/// Shuttle does not assume that the timeout will ever fire, so if all threads are blocked in a call
/// to `park_timeout` it will be treated as a deadlock.
pub fn park_timeout(_dur: Duration) {
    park();
}

/// Thread factory, which can be used in order to configure the properties of a new thread.
#[derive(Debug, Default)]
pub struct Builder {
    name: Option<String>,
    stack_size: Option<usize>,
}

impl Builder {
    /// Generates the base configuration for spawning a thread, from which configuration methods can be chained.
    pub fn new() -> Self {
        Self {
            name: None,
            stack_size: None,
        }
    }

    /// Names the thread-to-be. Currently the name is used for identification only in panic messages.
    pub fn name(mut self, name: String) -> Self {
        self.name = Some(name);
        self
    }

    /// Sets the size of the stack (in bytes) for the new thread.
    pub fn stack_size(mut self, stack_size: usize) -> Self {
        self.stack_size = Some(stack_size);
        self
    }

    /// Spawns a new thread by taking ownership of the Builder, and returns an `io::Result` to its `JoinHandle`.
    #[track_caller]
    pub fn spawn<F, T>(self, f: F) -> std::io::Result<JoinHandle<T>>
    where
        F: FnOnce() -> T,
        F: Send + 'static,
        T: Send + 'static,
    {
        Ok(spawn_named(f, self.name, self.stack_size, Location::caller()))
    }
}

pub use shuttle_core::thread_support::{AccessError, LocalKey};
