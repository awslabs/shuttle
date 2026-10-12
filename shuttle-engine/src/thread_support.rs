use crate::runtime::execution::ExecutionState;
use crate::runtime::thread;
use std::marker::PhantomData;

/// Cooperatively gives up a timeslice to the Shuttle scheduler.
pub fn yield_now() {
    let waker = ExecutionState::with(|state| state.current().waker());
    waker.wake_by_ref();
    ExecutionState::request_yield();
    thread::switch();
}

/// The body of a spawned thread. Runs `f`, drops thread locals, publishes result.
pub fn thread_fn<F, T>(
    f: F,
    switch_before_exit: bool,
    result: std::sync::Arc<std::sync::Mutex<Option<std::thread::Result<T>>>>,
) where
    F: FnOnce() -> T,
{
    let ret = f();

    if switch_before_exit && ExecutionState::with(|s| s.exit_current_truncates_execution()) {
        thread::switch();
    }

    // Nobody can join a thread whose `JoinHandle` is gone, so drop what it returned now, while its
    // thread-locals are still there for the value's destructors, as std does.
    let ret = if std::sync::Arc::strong_count(&result) > 1 {
        Some(ret)
    } else {
        drop(ret);
        None
    };

    tracing::trace!("thread finished, dropping thread locals");
    ExecutionState::drop_task_locals();
    tracing::trace!("done dropping thread locals");

    if let Some(ret) = ret {
        *result.lock().unwrap() = Some(Ok(ret));
    }
    ExecutionState::with(|state| {
        if let Some(waiter) = state.current_mut().take_waiter() {
            state.get_mut(waiter).unblock();
        }
    });
}

/// A key into Shuttle's thread-local storage.
pub struct LocalKey<T: 'static> {
    #[doc(hidden)]
    pub init: fn() -> T,
    // A `LocalKey` holds no `T` of its own, only the means to reach each thread's, so like
    // `std::thread::LocalKey` it is `Send`, `Sync` and unwind safe whatever `T` is. `fn() -> T`
    // gives it all four.
    #[doc(hidden)]
    pub _p: PhantomData<fn() -> T>,
}

/// A [`LocalKey`] whose slot is destroyed after all of a thread's other thread-locals when the
/// thread finishes, so that their destructors can still use it however early it was created.
/// `shuttle-tokio` uses it for the slots of its task-locals, which in tokio are thread-locals that
/// outlive every task.
#[doc(hidden)]
pub struct DestroyedLastKey<T: 'static> {
    key: LocalKey<T>,
}

impl<T: 'static> DestroyedLastKey<T> {
    /// A key whose slots start out as what `init` returns.
    pub const fn new(init: fn() -> T) -> Self {
        Self {
            key: LocalKey { init, _p: PhantomData },
        }
    }

    /// Like [`LocalKey::try_with`].
    pub fn try_with<F, R>(&'static self, f: F) -> std::result::Result<R, AccessError>
    where
        F: FnOnce(&T) -> R,
    {
        self.key.try_with_slot(f, true)
    }
}

impl<T: 'static> std::fmt::Debug for DestroyedLastKey<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DestroyedLastKey").finish_non_exhaustive()
    }
}

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
        self.try_with_slot(f, false)
    }

    /// `try_with`, creating the slot, if it has to, as one that is destroyed after the thread's other
    /// slots if `destroyed_last` is set.
    fn try_with_slot<F, R>(&'static self, f: F, destroyed_last: bool) -> std::result::Result<R, AccessError>
    where
        F: FnOnce(&T) -> R,
    {
        let value = self.get().unwrap_or_else(|| {
            let value = (self.init)();

            // `init`, or a `tracing` subscriber of the event that `ExecutionState::with` emits, may
            // have used this key, and so created the slot, since `get` found none. Keep that slot
            // then, and drop the new value once the state is no longer borrowed.
            let unused = ExecutionState::with(move |state| {
                if state.current().local(self).is_none() {
                    if destroyed_last {
                        state.current_mut().init_local_destroyed_last(self, value);
                    } else {
                        state.current_mut().init_local(self, value);
                    }
                    None
                } else {
                    Some(value)
                }
            });
            drop(unused);

            self.get().unwrap()
        })?;

        Ok(f(value))
    }

    fn get(&'static self) -> Option<std::result::Result<&'static T, AccessError>> {
        // Safety: see the usage below
        unsafe fn extend_lt<'b, T>(t: &'_ T) -> &'b T {
            std::mem::transmute(t)
        }

        ExecutionState::with(|state| {
            if let Ok(value) = state.current().local(self)? {
                // Safety: the `ExecutionState` outlives any thread, including the caller, and so
                // it's safe to give the caller the lifetime it's asking for here.
                Some(Ok(unsafe { extend_lt(value) }))
            } else {
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
