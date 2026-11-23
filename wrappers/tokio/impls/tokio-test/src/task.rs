//! Futures task based helpers to easily test futures and manually written futures.
//!
//! The [`Spawn`] type is used as a mock task harness that allows you to poll futures
//! without needing to setup pinning or context. Any future can be polled but if the
//! future requires the tokio async context you will need to ensure that you poll the
//! [`Spawn`] within a tokio context, this means that as long as you are inside the
//! runtime it will work and you can poll it via [`Spawn`].
//!
//! [`Spawn`] also supports [`Stream`] to call `poll_next` without pinning
//! or context.
//!
//! In addition to circumventing the need for pinning and context, [`Spawn`] also tracks
//! the amount of times the future/task was woken. This can be useful to track if some
//! leaf future notified the root task correctly.
//!
//! # Example
//!
//! ```ignore
//! use shuttle_tokio_test_impl::task;
//!
//! let fut = async {};
//!
//! let mut task = task::spawn(fut);
//!
//! assert!(task.poll().is_ready(), "Task was not ready!");
//! ```

use shuttle::sync::{Arc, Condvar, Mutex};
use std::future::Future;
use std::mem;
use std::ops;
use std::pin::Pin;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use tokio_stream::Stream;

/// Spawn a future into a [`Spawn`] which wraps the future in a mocked executor.
///
/// This can be used to spawn a [`Future`] or a [`Stream`].
///
/// For more information, check the module docs.
pub fn spawn<T>(task: T) -> Spawn<T> {
    Spawn {
        task: MockTask::new(),
        future: Box::pin(task),
    }
}

/// Future spawned on a mock task that can be used to poll the future or stream
/// without needing pinning or context types.
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct Spawn<T> {
    task: MockTask,
    future: Pin<Box<T>>,
}

#[derive(Debug, Clone)]
struct MockTask {
    waker: Arc<ThreadWaker>,
}

#[derive(Debug)]
struct ThreadWaker {
    state: Mutex<usize>,
    condvar: Condvar,
}

const IDLE: usize = 0;
const WAKE: usize = 1;
const SLEEP: usize = 2;

impl<T> Spawn<T> {
    /// Consumes `self` returning the inner value
    pub fn into_inner(self) -> T
    where
        T: Unpin,
    {
        *Pin::into_inner(self.future)
    }

    /// Returns `true` if the inner future has received a wake notification
    /// since the last call to `enter`.
    pub fn is_woken(&self) -> bool {
        self.task.is_woken()
    }

    /// Returns the number of references to the task waker
    ///
    /// The task itself holds a reference. The return value will never be zero.
    pub fn waker_ref_count(&self) -> usize {
        self.task.waker_ref_count()
    }

    /// Enter the task context
    pub fn enter<F, R>(&mut self, f: F) -> R
    where
        F: FnOnce(&mut Context<'_>, Pin<&mut T>) -> R,
    {
        let fut = self.future.as_mut();
        self.task.enter(|cx| f(cx, fut))
    }
}

impl<T: Unpin> ops::Deref for Spawn<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.future
    }
}

impl<T: Unpin> ops::DerefMut for Spawn<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.future
    }
}

impl<T: Future> Spawn<T> {
    /// If `T` is a [`Future`] then poll it. This will handle pinning and the context
    /// type for the future.
    pub fn poll(&mut self) -> Poll<T::Output> {
        let fut = self.future.as_mut();
        self.task.enter(|cx| fut.poll(cx))
    }
}

impl<T: Stream> Spawn<T> {
    /// If `T` is a [`Stream`] then `poll_next` it. This will handle pinning and the context
    /// type for the stream.
    pub fn poll_next(&mut self) -> Poll<Option<T::Item>> {
        let stream = self.future.as_mut();
        self.task.enter(|cx| stream.poll_next(cx))
    }
}

impl<T: Future> Future for Spawn<T> {
    type Output = T::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.future.as_mut().poll(cx)
    }
}

impl<T: Stream> Stream for Spawn<T> {
    type Item = T::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.future.as_mut().poll_next(cx)
    }
}

impl MockTask {
    /// Creates new mock task
    fn new() -> Self {
        MockTask {
            waker: Arc::new(ThreadWaker::new()),
        }
    }

    /// Runs a closure from the context of the task.
    ///
    /// Any wake notifications resulting from the execution of the closure are
    /// tracked.
    fn enter<F, R>(&mut self, f: F) -> R
    where
        F: FnOnce(&mut Context<'_>) -> R,
    {
        self.waker.clear();
        let waker = self.waker();
        let mut cx = Context::from_waker(&waker);

        f(&mut cx)
    }

    /// Returns `true` if the inner future has received a wake notification
    /// since the last call to `enter`.
    fn is_woken(&self) -> bool {
        self.waker.is_woken()
    }

    /// Returns the number of references to the task waker
    ///
    /// The task itself holds a reference. The return value will never be zero.
    fn waker_ref_count(&self) -> usize {
        Arc::strong_count(&self.waker)
    }

    fn waker(&self) -> Waker {
        let raw = to_raw(self.waker.clone());

        // SAFETY:
        // In order for the call to be safe, we need to ensure:
        // * `raw` and its vtable meet the contract in the docs of `RawWaker` and `RawWakerVTable`.
        //
        // `raw`'s data pointer is an `Arc<ThreadWaker>` that `to_raw` turned into a pointer, and
        // it owns one strong reference. Its vtable is `VTABLE`. Of `VTABLE`'s functions, `clone`
        // adds a reference for the new `RawWaker`, `wake` and `drop_waker` release the reference
        // they get, and `wake_by_ref` keeps it. `wake` and `wake_by_ref` wake the `ThreadWaker`
        // that `raw` points to, and a clone points to the same one.
        // The functions may run on any thread, as `Waker` requires. `Arc` counts atomically, and
        // `ThreadWaker` uses only Shuttle's `Mutex` and `Condvar`, which are `Sync`. Their `Sync`
        // impls do not hold when two OS threads use one of them at the same time (see their
        // SAFETY comments), so this does not hold then either.
        // This also does not hold if `ThreadWaker::wake` panics in `wake_by_ref`, see its docs.
        unsafe { Waker::from_raw(raw) }
    }
}

impl Default for MockTask {
    fn default() -> Self {
        Self::new()
    }
}

impl ThreadWaker {
    fn new() -> Self {
        ThreadWaker {
            state: Mutex::new(IDLE),
            condvar: Condvar::new(),
        }
    }

    /// Clears any previously received wakes, avoiding potential spurious
    /// wake notifications. This should only be called immediately before running the
    /// task.
    fn clear(&self) {
        *self.state.lock().unwrap() = IDLE;
    }

    fn is_woken(&self) -> bool {
        match *self.state.lock().unwrap() {
            IDLE => false,
            WAKE => true,
            _ => unreachable!(),
        }
    }

    fn wake(&self) {
        // First, try transitioning from IDLE -> NOTIFY, this does not require a lock.
        let mut state = self.state.lock().unwrap();
        let prev = *state;

        if prev == WAKE {
            return;
        }

        *state = WAKE;

        if prev == IDLE {
            return;
        }

        // The other half is sleeping, so we wake it up.
        assert_eq!(prev, SLEEP);
        self.condvar.notify_one();
    }
}

static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop_waker);

/// Turns `waker` into a `RawWaker` that owns `waker`'s strong reference and uses `VTABLE`.
/// `Waker::from_raw` relies on what it returns.
fn to_raw(waker: Arc<ThreadWaker>) -> RawWaker {
    RawWaker::new(Arc::into_raw(waker).cast::<()>(), &VTABLE)
}

/// # Safety
///
/// The caller must ensure:
/// * `raw` is the data pointer of a `RawWaker` that `to_raw` made, and that is still alive.
/// * the caller owns that `RawWaker`'s strong reference, which the returned `Arc` takes over. If
///   it does not, it must forget the returned `Arc` instead of dropping it.
unsafe fn from_raw(raw: *const ()) -> Arc<ThreadWaker> {
    // SAFETY:
    // In order for the call to be safe, we need to ensure:
    // * `raw` came from `Arc::into_raw` of an `Arc<ThreadWaker>`.
    // * the returned `Arc` drops only a reference that someone gave up for it.
    //
    // `to_raw` made `raw` with `Arc::into_raw`, as the caller ensures. The caller also ensures the
    // second.
    unsafe { Arc::from_raw(raw.cast::<ThreadWaker>()) }
}

/// The `clone` function of `VTABLE`.
///
/// # Safety
///
/// The caller must ensure:
/// * `raw` is the data pointer of a live `RawWaker` that `to_raw` made.
///
/// `Waker` calls this only with the data pointer of a `RawWaker` that it holds.
unsafe fn clone(raw: *const ()) -> RawWaker {
    // SAFETY:
    // In order for the call to be safe, we need to ensure:
    // * `raw` is the data pointer of a live `RawWaker` that `to_raw` made.
    // * the reference that `waker` takes over is given back.
    //
    // The caller ensures the first. `to_raw` below gives the reference back to the `RawWaker`
    // that is cloned, and nothing between the two calls can panic.
    let waker = unsafe { from_raw(raw) };

    // Increment the ref count, for the new `RawWaker`.
    mem::forget(waker.clone());

    to_raw(waker)
}

/// The `wake` function of `VTABLE`.
///
/// # Safety
///
/// The caller must ensure:
/// * `raw` is the data pointer of a live `RawWaker` that `to_raw` made.
/// * the caller gives up that `RawWaker`, whose reference this function releases.
///
/// `Waker::wake` calls this with the `RawWaker` of the `Waker` that it consumes.
unsafe fn wake(raw: *const ()) {
    // SAFETY:
    // In order for the call to be safe, we need to ensure:
    // * `raw` is the data pointer of a live `RawWaker` that `to_raw` made.
    // * the reference that `waker` takes over is one that its owner gave up.
    //
    // The caller ensures both. `waker` releases that reference when it is dropped, also if
    // `ThreadWaker::wake` panics.
    let waker = unsafe { from_raw(raw) };
    waker.wake();
}

/// The `wake_by_ref` function of `VTABLE`.
///
/// # Safety
///
/// The caller must ensure:
/// * `raw` is the data pointer of a live `RawWaker` that `to_raw` made.
///
/// `Waker::wake_by_ref` calls this with the `RawWaker` of a `Waker` that it borrows.
unsafe fn wake_by_ref(raw: *const ()) {
    // SAFETY:
    // In order for the call to be safe, we need to ensure:
    // * `raw` is the data pointer of a live `RawWaker` that `to_raw` made.
    // * the reference that `waker` takes over is given back.
    //
    // The caller ensures the first. `mem::forget` below gives the reference back.
    // That does not hold if `ThreadWaker::wake` panics, as it can in three cases. Outside an
    // execution, Shuttle's `Mutex` can't be locked. Execution teardown can unwind a task's stack
    // from a scheduling point (see `ExecutionState::tear_down`), and the `lock` in
    // `ThreadWaker::wake` is one. And if `state` is poisoned, `unwrap` panics. The unwinding then
    // drops `waker`, and so releases a reference that the `RawWaker` still owns. The
    // `ThreadWaker` can then be freed while a `Waker`, or the `MockTask`'s own `Arc`, still
    // points to it.
    let waker = unsafe { from_raw(raw) };
    waker.wake();

    // We don't actually own a reference to the unparker
    mem::forget(waker);
}

/// The `drop` function of `VTABLE`.
///
/// # Safety
///
/// The caller must ensure:
/// * `raw` is the data pointer of a live `RawWaker` that `to_raw` made.
/// * the caller gives up that `RawWaker`, whose reference this function releases.
///
/// `Waker`'s `drop` calls this with the `RawWaker` of the `Waker` that is dropped.
unsafe fn drop_waker(raw: *const ()) {
    // SAFETY:
    // In order for the call to be safe, we need to ensure:
    // * `raw` is the data pointer of a live `RawWaker` that `to_raw` made.
    // * the reference that the `Arc` takes over is one that its owner gave up.
    //
    // The caller ensures both. Dropping the `Arc` releases that reference.
    let _ = unsafe { from_raw(raw) };
}
