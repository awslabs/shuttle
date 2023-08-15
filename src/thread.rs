//! Shuttle's implementation of [`std::thread`].

use crate::runtime::execution::ExecutionState;
use crate::runtime::task::TaskId;
use crate::runtime::thread;
use std::marker::PhantomData;
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
        ExecutionState::with(|s| {
            s.get_mut(self.id.task_id).unpark();
        });

        // Making the token available is a yield point
        thread::switch();
    }
}

/// Spawn a new thread, returning a JoinHandle for it.
///
/// The join handle can be used (via the `join` method) to block until the child thread has
/// finished.
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T,
    F: Send + 'static,
    T: Send + 'static,
{
    spawn_named(f, None, None)
}

fn spawn_named<F, T>(f: F, name: Option<String>, stack_size: Option<usize>) -> JoinHandle<T>
where
    F: FnOnce() -> T,
    F: Send + 'static,
    T: Send + 'static,
{
    // TODO Check if it's worth avoiding the call to `ExecutionState::config()` if we're going
    // TODO to use an existing continuation from the pool.
    let stack_size = stack_size.unwrap_or_else(|| ExecutionState::with(|s| s.config.stack_size));
    let result = std::sync::Arc::new(std::sync::Mutex::new(None));
    let task_id = {
        let result = std::sync::Arc::clone(&result);
        let f = move || thread_fn(f, result);
        ExecutionState::spawn_thread(f, stack_size, name.clone(), None)
    };

    thread::switch();

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
/// stores the result of the thread in the given lock.
pub(crate) fn thread_fn<F, T>(f: F, result: std::sync::Arc<std::sync::Mutex<Option<Result<T>>>>)
where
    F: FnOnce() -> T,
    F: Send + 'static,
    T: Send + 'static,
{
    let ret = f();

    tracing::trace!("thread finished, dropping thread locals");

    // Run thread-local destructors before publishing the result, because
    // [`JoinHandle::join`] says join "waits for the associated thread to finish", but
    // destructors must be run on the thread, so it can't be considered "finished" if the
    // destructors haven't run yet.
    // See `pop_local` for details on why this loop looks this slightly funky way.
    while let Some(local) = ExecutionState::with(|state| state.current_mut().pop_local()) {
        tracing::trace!("dropping thread local {:p}", local);
        drop(local);
    }

    tracing::trace!("done dropping thread locals");

    // Publish the result and unblock the waiter. We need to do this now, because once this
    // closure completes, the Execution will consider this task Finished and invoke the
    // scheduler.
    *result.lock().unwrap() = Some(Ok(ret));
    ExecutionState::with(|state| {
        if let Some(waiter) = state.current_mut().take_waiter() {
            state.get_mut(waiter).unblock();
        }
    });
}

/// An owned permission to join on a thread (block on its termination).
#[derive(Debug)]
pub struct JoinHandle<T> {
    task_id: TaskId,
    thread: Thread,
    result: std::sync::Arc<std::sync::Mutex<Option<Result<T>>>>,
}

impl<T> JoinHandle<T> {
    /// Waits for the associated thread to finish.
    pub fn join(self) -> Result<T> {
        ExecutionState::with(|state| {
            let me = state.current().id();
            let target = state.get_mut(self.task_id);
            if target.set_waiter(me) {
                state.current_mut().block(false);
            }
        });

        // TODO can we soundly skip the yield if the target thread has already finished?
        thread::switch();

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
    pub fn spawn<F, T>(self, f: F) -> std::io::Result<JoinHandle<T>>
    where
        F: FnOnce() -> T,
        F: Send + 'static,
        T: Send + 'static,
    {
        Ok(spawn_named(f, self.name, self.stack_size))
    }
}

/// A thread local storage key which owns its contents
// Sadly, the fields of this thing need to be public because function pointers in const fns are
// unstable, so an explicit instantiation is the only way to construct this struct. User code should
// not rely on these fields.
pub struct LocalKey<T: 'static> {
    #[doc(hidden)]
    pub init: fn() -> T,
    #[doc(hidden)]
    pub _p: PhantomData<T>,
}

// Safety: `LocalKey` implements thread-local storage; each thread sees its own value of the type T.
unsafe impl<T> Send for LocalKey<T> {}
unsafe impl<T> Sync for LocalKey<T> {}

impl<T: 'static> std::fmt::Debug for LocalKey<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalKey").finish_non_exhaustive()
    }
}

impl<T: 'static> LocalKey<T> {
    /// Acquires a reference to the value in this TLS key.
    ///
    /// This will lazily initialize the value if this thread has not referenced this key yet.
    pub fn with<F, R>(&'static self, f: F) -> R
    where
        F: FnOnce(&T) -> R,
    {
        self.try_with(f).expect(
            "cannot access a Thread Local Storage value \
            during or after destruction",
        )
    }

    /// Acquires a reference to the value in this TLS key.
    ///
    /// This will lazily initialize the value if this thread has not referenced this key yet. If the
    /// key has been destroyed (which may happen if this is called in a destructor), this function
    /// will return an AccessError.
    pub fn try_with<F, R>(&'static self, f: F) -> std::result::Result<R, AccessError>
    where
        F: FnOnce(&T) -> R,
    {
        let value = self.get().unwrap_or_else(|| {
            let value = (self.init)();

            ExecutionState::with(move |state| {
                state.current_mut().init_local(self, value);
            });

            self.get().unwrap()
        })?;

        Ok(f(value))
    }

    fn get(&'static self) -> Option<std::result::Result<&T, AccessError>> {
        // Safety: see the usage below
        unsafe fn extend_lt<'b, T>(t: &'_ T) -> &'b T {
            std::mem::transmute(t)
        }

        ExecutionState::with(|state| {
            if let Ok(value) = state.current().local(self)? {
                // Safety: unfortunately the lifetime of a value in our thread-local storage is
                // bound to the lifetime of `ExecutionState`, which has no visible relation to the
                // lifetime of the thread we're running on. However, *we* know that the
                // `ExecutionState` outlives any thread, including the caller, and so it's safe to
                // give the caller the lifetime it's asking for here.
                Some(Ok(unsafe { extend_lt(value) }))
            } else {
                // Slot has already been destructed
                Some(Err(AccessError))
            }
        })
    }
}

/// An error returned by [`LocalKey::try_with`]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub struct AccessError;

impl std::fmt::Display for AccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt("already destroyed", f)
    }
}

impl std::error::Error for AccessError {}
