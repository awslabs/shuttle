//! Shuttle's implementation of [`std::thread`].

use crate::runtime::execution::ExecutionState;
use crate::runtime::task::{TaskId, TaskType};
use crate::runtime::thread;
use crate::runtime::thread::future::ThreadFuture;
use std::time::Duration;

/// A unique identifier for a running thread
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadId {
    // TODO Should we add an execution id here, like Loom does?
    task_id: TaskId,
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
    let stack_size = stack_size.unwrap_or(ExecutionState::config().stack_size);
    let result = std::sync::Arc::new(std::sync::Mutex::new(None));
    let task_id = {
        let result = std::sync::Arc::clone(&result);
        // Build a new ThreadFuture that will simulate a thread inside a Future
        let task = ThreadFuture::new(stack_size, move || {
            let ret = f();
            // Publish the result and unblock the waiter. We need to do this now, because once this
            // closure completes, the Execution will consider this task Finished and invoke the
            // scheduler.
            *result.lock().unwrap() = Some(Ok(ret));
            ExecutionState::with(|state| {
                if let Some(waiter) = state.current_mut().take_waiter() {
                    state.get_mut(waiter).unblock();
                }
            });
        });
        ExecutionState::spawn(task, TaskType::Thread, name.clone())
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

/// An owned permission to join on a thread (block on its termination).
#[derive(Debug)]
pub struct JoinHandle<T> {
    task_id: TaskId,
    thread: Thread,
    result: std::sync::Arc<std::sync::Mutex<Option<std::thread::Result<T>>>>,
}

impl<T> JoinHandle<T> {
    /// Waits for the associated thread to finish.
    pub fn join(self) -> std::thread::Result<T> {
        ExecutionState::with(|state| {
            let me = state.current().id();
            let target = state.get_mut(self.task_id);
            if target.set_waiter(me) {
                state.current_mut().block();
            }
        });

        // TODO can we soundly skip the yield if the target thread has already finished?
        thread::switch();

        self.result.lock().unwrap().take().expect("target should have finished")
    }

    /// Extracts a handle to the underlying thread.
    pub fn thread(&self) -> &Thread {
        &self.thread
    }
}

// TODO: don't need this? Just call switch directly?
/// Cooperatively gives up a timeslice to the Shuttle scheduler.
pub fn yield_now() {
    thread::switch();
}

/// Puts the current thread to sleep for at least the specified amount of time.
// Note that Shuttle does not model time, so this behaves just like a context switch.
pub fn sleep(_dur: Duration) {
    thread::switch();
}

// TODO: Implement current()
// TODO: Implement park(), unpark()

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
