//! The tasks a runtime owns, so that shutting the runtime down stops them, as in tokio (see the
//! Shutdown section of tokio's `Runtime` docs):
//!
//! - Shutting a runtime down cancels its tasks. A task isn't polled again, and is dropped when
//!   Shuttle next runs it, so a task that hasn't been polled yet never is. `spawn_blocking` work is
//!   the exception: it runs until it returns.
//! - A task spawned onto a runtime that has shut down is cancelled at once, `spawn_blocking` work
//!   too.
//! - Dropping a runtime waits until all of its tasks, blocking or not, have been dropped.
//!
//! A runtime's tasks are Shuttle tasks like any other. Each one carries a [`RuntimeContext`] label
//! that names its runtime, and that is how `Handle::current` finds the runtime a task runs on.

use pin_project::{pin_project, pinned_drop};
use shuttle::current::{
    get_label_for_task, me, remove_label_for_task, set_label_for_task, try_get_current_task, TaskId,
};
use shuttle_engine::runtime::execution::ExecutionState;
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

/// The state of one runtime, which its handles and its tasks share.
#[derive(Default)]
pub(crate) struct RuntimeState {
    // A std mutex rather than a Shuttle one: this is bookkeeping, which shouldn't add scheduling
    // points, and only one task runs at a time.
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    shut_down: bool,
    next_slot: usize,
    /// The runtime's tasks that shutting down cancels, until they're dropped: all but `spawn_blocking`
    /// work. Keyed by the slot each task got when it was spawned.
    cancellable: BTreeMap<usize, Cancel>,
    /// How many of the runtime's tasks, blocking or not, haven't been dropped yet.
    live: usize,
    /// Wakers of whoever waits for `live` to reach 0 (see [`Stopped`]).
    waiters: Vec<Waker>,
}

/// What cancelling one task takes: its flag, which stops it from being polled again, and the abort
/// that drops it.
#[derive(Clone)]
struct Cancel {
    cancelled: Arc<AtomicBool>,
    abort: shuttle::future::AbortHandle,
}

impl Cancel {
    /// Sets the flag at once. Aborting is a scheduling point, so other tasks may run before the task
    /// is woken to be dropped, but the flag keeps the task from being polled in between.
    fn cancel(self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.abort.abort();
    }
}

impl fmt::Debug for RuntimeState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeState").finish_non_exhaustive()
    }
}

impl RuntimeState {
    /// Spawns `future` as one of the runtime's tasks. `blocking` is for `spawn_blocking` work, which
    /// shutting down doesn't cancel.
    pub(crate) fn spawn<F>(self: &Arc<Self>, future: F, blocking: bool) -> shuttle::future::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let slot = {
            let mut inner = self.inner.lock().unwrap();
            inner.live += 1;
            inner.next_slot += 1;
            inner.next_slot
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let task = RuntimeTask {
            runtime: Arc::clone(self),
            slot,
            blocking,
            cancelled: Arc::clone(&cancelled),
            entered: false,
            future: Some(future),
        };
        let handle = shuttle::future::spawn(task);
        // Shuttle's `spawn` switches before it creates the task, not after, so nothing has run since:
        // the task hasn't been polled, and the runtime can't have shut down in between.
        let cancel = Cancel {
            cancelled,
            abort: handle.abort_handle(),
        };
        let shut_down = {
            let mut inner = self.inner.lock().unwrap();
            if !blocking && !inner.shut_down {
                inner.cancellable.insert(slot, cancel.clone());
            }
            inner.shut_down
        };
        if shut_down {
            cancel.cancel();
        }
        handle
    }

    /// Shuts the runtime down and cancels its tasks other than `spawn_blocking` work. Doesn't wait
    /// for them to be dropped: see [`Stopped`].
    pub(crate) fn shut_down(&self) {
        let tasks: Vec<Cancel> = {
            let mut inner = self.inner.lock().unwrap();
            if std::mem::replace(&mut inner.shut_down, true) {
                return;
            }
            inner.cancellable.values().cloned().collect()
        };
        // Stop every task from being polled before any abort switches to another task.
        for task in &tasks {
            task.cancelled.store(true, Ordering::Relaxed);
        }
        for task in tasks {
            task.abort.abort();
        }
    }

    fn task_dropped(&self, slot: usize) {
        let waiters = {
            let mut inner = self.inner.lock().unwrap();
            inner.cancellable.remove(&slot);
            inner.live -= 1;
            if inner.live == 0 {
                std::mem::take(&mut inner.waiters)
            } else {
                Vec::new()
            }
        };
        if !ExecutionState::should_stop() {
            for waker in waiters {
                waker.wake();
            }
        }
    }
}

/// A future that is ready once every task of the runtime has been dropped.
pub(crate) struct Stopped(pub(crate) Arc<RuntimeState>);

impl Future for Stopped {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut inner = self.0.inner.lock().unwrap();
        if inner.live == 0 {
            return Poll::Ready(());
        }
        if !inner.waiters.iter().any(|waker| waker.will_wake(cx.waker())) {
            inner.waiters.push(cx.waker().clone());
        }
        Poll::Pending
    }
}

/// One of the runtime's tasks: the spawned future, and the bookkeeping that lets the runtime stop it.
#[pin_project(PinnedDrop)]
struct RuntimeTask<F> {
    runtime: Arc<RuntimeState>,
    slot: usize,
    blocking: bool,
    cancelled: Arc<AtomicBool>,
    /// Whether the task has entered its runtime's context, which it does on its first poll.
    entered: bool,
    #[pin]
    future: Option<F>,
}

impl<F: Future> Future for RuntimeTask<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let this = self.project();
        if !*this.entered {
            *this.entered = true;
            let me = me();
            set_label_for_task(
                me,
                RuntimeContext {
                    runtime: Arc::clone(this.runtime),
                    owner: me,
                    blocking: *this.blocking,
                },
            );
        }
        if this.cancelled.load(Ordering::Relaxed) {
            // Cancelled: the abort that comes with the flag drops the task when it wakes it.
            return Poll::Pending;
        }
        this.future
            .as_pin_mut()
            .expect("a task isn't polled once its future has been dropped")
            .poll(cx)
    }
}

#[pinned_drop]
impl<F> PinnedDrop for RuntimeTask<F> {
    fn drop(self: Pin<&mut Self>) {
        let mut this = self.project();
        // Drop the future first, so that whoever waits for the runtime to stop sees the task's
        // destructors done.
        this.future.set(None);
        this.runtime.task_dropped(*this.slot);
    }
}

/// The label that puts a task in a runtime's context, like tokio's runtime context: each of a
/// runtime's tasks has it, and so does a thread while it is in the runtime's `block_on`.
#[derive(Clone)]
pub(crate) struct RuntimeContext {
    pub(crate) runtime: Arc<RuntimeState>,
    /// The task the label was set for. A task or thread inherits its parent's labels when it is
    /// spawned, but one that isn't spawned onto the runtime, such as with Shuttle's own `spawn` or as
    /// a thread, isn't in the runtime's context, as in tokio.
    owner: TaskId,
    /// Whether the task is `spawn_blocking` work, where blocking is allowed.
    blocking: bool,
}

impl fmt::Debug for RuntimeContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeContext")
            .field("blocking", &self.blocking)
            .finish_non_exhaustive()
    }
}

impl RuntimeContext {
    /// The runtime context the current task is in, if any.
    pub(crate) fn current() -> Option<Self> {
        let me = try_get_current_task()?;
        get_label_for_task::<RuntimeContext>(me).filter(|context| context.owner == me)
    }
}

/// Puts the current task in a runtime's context until it is dropped, as `block_on` does.
pub(crate) struct EnterGuard {
    task: TaskId,
    previous: Option<RuntimeContext>,
}

impl EnterGuard {
    pub(crate) fn enter(runtime: &Arc<RuntimeState>) -> Self {
        let task = me();
        let previous = set_label_for_task(
            task,
            RuntimeContext {
                runtime: Arc::clone(runtime),
                owner: task,
                blocking: false,
            },
        );
        Self { task, previous }
    }
}

impl Drop for EnterGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(previous) => {
                set_label_for_task(self.task, previous);
            }
            None => {
                remove_label_for_task::<RuntimeContext>(self.task);
            }
        }
    }
}

/// Panics as tokio does when a runtime is dropped, or shut down with a wait, where blocking isn't
/// allowed: in a runtime's context, unless in `spawn_blocking` work.
pub(crate) fn assert_blocking_allowed() {
    if RuntimeContext::current().is_some_and(|context| !context.blocking) {
        panic!(
            "Cannot drop a runtime in a context where blocking is not allowed. \
             This happens when a runtime is dropped from within an asynchronous context."
        );
    }
}

/// Whether no execution is running: outside of one, or once it has finished and Shuttle tears it
/// down. A runtime then has nothing to stop, as teardown drops every task.
pub(crate) fn execution_over() -> bool {
    ExecutionState::try_with(|state| state.is_finished()).unwrap_or(true)
}
