//! State that a primitive keeps once for each execution.
//!
//! A Shuttle test runs its function many times, each run an execution of its own, which must not
//! depend on the ones before it: a failing execution is replayed on its own, and
//! `check_uncontrolled_nondeterminism` runs every schedule twice and compares. A primitive that
//! lives longer than an execution, most often because it is in a `static`, would carry its state
//! from one execution to the next: a semaphore's permits, a lock's waiters and poisoning, an
//! atomic's value, the clocks of their last releases. So each such primitive keeps its state in a
//! [`PerExecution`], which gives each execution a state of its own, created when the execution
//! first uses the primitive.
//!
//! A primitive can't tell whether it is in a `static`: its constructors are `const`, and the same
//! constructor makes a primitive on the heap. Most primitives are made inside an execution and
//! dropped inside it, and for those nothing changes: the first execution that uses a primitive
//! keeps its state in the primitive itself, as primitives always have, where it is as cheap to
//! reach as before and is dropped with the primitive. Only another execution, which can only be
//! using a primitive that outlived the first one, keeps its state elsewhere: in its own storage,
//! where it is dropped when that execution ends.
//!
//! Code that runs outside of any execution (a `static` read before a test starts, a value dropped
//! after its test ended) uses the state in the primitive, as it always has. If no execution has used
//! the primitive yet, it takes that state for itself, so that no execution starts from what it did.
//!
//! What a lock protects can't be copied for each execution: the lock has the one value it was
//! created with. A [`ValueLock`] keeps it to one execution at a time, and reports another
//! execution's guard that holds it: one of a test that shares the lock in a `static` and runs at the
//! same time on another thread, or one that an execution which has ended left behind.

use crate::runtime::execution::{current_execution_id, execution_thread, ExecutionState};
use crate::runtime::storage::StorageKey;
use std::cell::UnsafeCell;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The storage slots of `PerExecution` values have this type (see [`StorageKey`]).
const SLOT_TYPE: usize = 0x5;

/// Source of the identifiers of `PerExecution` values' storage slots.
static NEXT_SLOT: AtomicUsize = AtomicUsize::new(1);

// What `PerExecution::owner` holds, besides the identifier of the execution that owns the inline
// state. Execution identifiers never reach these (see `CurrentExecutionId::enter`).

/// Nothing has used the value yet, so the inline state is empty.
const UNCLAIMED: usize = 0;
/// Something is initializing the inline state, to own it (see `PerExecution::claim`).
const CLAIMING: usize = usize::MAX;
/// Code outside of any execution owns the inline state.
const OUTSIDE: usize = usize::MAX - 1;
/// The inline state is the state of every execution, and of code outside of them.
const SHARED: usize = usize::MAX - 2;

/// The state `S` that a primitive keeps once for each execution (see the module docs).
///
/// The first execution that uses the value keeps its state inline, in the value. Another execution
/// keeps its state in its own storage, under a slot that the value is assigned when it first needs
/// one. Code outside of any execution uses the inline state.
///
/// Each state is created when its execution first asks for it, by the function that the caller
/// passes, which must not use the same `PerExecution` itself.
pub struct PerExecution<S> {
    /// Who owns the inline state: the identifier of an execution, or one of the constants above.
    owner: AtomicUsize,
    /// The identifier of the storage slots of other executions' states, or zero until one is needed.
    slot: AtomicUsize,
    /// The owner's state. Empty exactly while `owner` is `UNCLAIMED` or `CLAIMING`.
    inline: UnsafeCell<Option<S>>,
}

impl<S> PerExecution<S> {
    /// A value with no state yet: each execution creates its own when it first uses the value.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            owner: AtomicUsize::new(UNCLAIMED),
            slot: AtomicUsize::new(0),
            inline: UnsafeCell::new(None),
        }
    }

    /// A value whose first state is the one `init` creates now, for the current execution, as for a
    /// primitive that the current task creates. Outside of an execution, this is
    /// [`PerExecution::new`], and `init` doesn't run.
    #[must_use]
    pub fn new_in_current_execution(init: impl FnOnce() -> S) -> Self {
        match current_execution_id() {
            Some(id) => Self {
                owner: AtomicUsize::new(id),
                slot: AtomicUsize::new(0),
                inline: UnsafeCell::new(Some(init())),
            },
            None => Self::new(),
        }
    }

    /// A value whose one state, `state`, is the state of every execution that uses it, and of code
    /// outside of them, as for a primitive before primitives kept their state per execution.
    ///
    /// This is for a primitive that is part of something that keeps its own state across
    /// executions, and that has to stay in step with it: a channel's semaphores, whose permits count
    /// the messages in the channel's buffer.
    #[must_use]
    pub const fn shared(state: S) -> Self {
        Self {
            owner: AtomicUsize::new(SHARED),
            slot: AtomicUsize::new(0),
            inline: UnsafeCell::new(Some(state)),
        }
    }

    /// The inline state, once something owns it.
    ///
    /// # Safety
    ///
    /// The caller must not race with the owner of the inline state, which holds it by
    /// `PerExecution::try_get`'s rules, nor with its initialization.
    unsafe fn inline(&self) -> &S {
        // Safety: the caller's. The inline state is only ever replaced while `owner` is `CLAIMING`.
        unsafe { (*self.inline.get()).as_ref() }.expect("a claimed state is initialized")
    }
}

impl<S: 'static> PerExecution<S> {
    /// The state of the current execution, created by `init` if this is the first time the
    /// execution asks for it. Outside of an execution, the inline state, created by `init` if
    /// nothing has used the value yet.
    ///
    /// # Panics
    ///
    /// If the execution keeps its state in its storage and Shuttle holds its own state borrowed, so
    /// that the storage can't be reached (see [`PerExecution::try_get`]).
    pub fn get(&self, init: impl FnOnce() -> S) -> &S {
        self.try_get(init)
            .expect("the per-execution state of a Shuttle primitive was used while Shuttle holds its own state")
    }

    /// Like [`PerExecution::get`], but returns `None` where `get` panics: when the state is in the
    /// execution's storage, and Shuttle holds its own state borrowed. Destructors of primitives can
    /// run there, and skip their bookkeeping (see `ExecutionState::should_stop`).
    ///
    /// The reference lives as long as `self`, which may be longer than the state: a state in an
    /// execution's storage is dropped when the execution ends. Callers must not keep it beyond the
    /// operation they use it for, nor hand it out.
    pub fn try_get(&self, init: impl FnOnce() -> S) -> Option<&S> {
        let me = current_execution_id().unwrap_or(OUTSIDE);
        let mut owner = self.owner.load(Ordering::Acquire);
        loop {
            match owner {
                // Safety: the inline state is ours, or everyone's.
                _ if owner == me || owner == SHARED => return Some(unsafe { self.inline() }),
                UNCLAIMED => match self
                    .owner
                    .compare_exchange(UNCLAIMED, CLAIMING, Ordering::AcqRel, Ordering::Acquire)
                {
                    Ok(_) => return Some(self.claim(me, init)),
                    Err(actual) => owner = actual,
                },
                // Another thread is initializing the inline state. Code outside of an execution waits
                // for it, as for any other owner. An execution doesn't need it.
                CLAIMING if me == OUTSIDE => {
                    std::thread::yield_now();
                    owner = self.owner.load(Ordering::Acquire);
                }
                // Code outside of an execution uses the inline state whoever owns it, as it did
                // before primitives kept their state per execution.
                //
                // Safety: the owner is code outside of an execution, or an execution that has
                // ended, or one that runs on another thread at the same time, which is the race
                // between threads that using a primitive outside of an execution always had.
                _ if me == OUTSIDE => return Some(unsafe { self.inline() }),
                // Another execution, or code outside of executions, owns the inline state.
                _ => return self.stored(init),
            }
        }
    }

    /// Initialize the inline state with `init`, for `me`, which has just set `owner` to `CLAIMING`.
    fn claim(&self, me: usize, init: impl FnOnce() -> S) -> &S {
        // If `init` panics, the inline state stays empty, and the next user claims it again.
        struct Unclaim<'a>(&'a AtomicUsize);
        impl Drop for Unclaim<'_> {
            fn drop(&mut self) {
                self.0.store(UNCLAIMED, Ordering::Release);
            }
        }
        let unclaim = Unclaim(&self.owner);
        let state = init();
        // Safety: while `owner` is `CLAIMING`, nothing else reaches the inline state: everything
        // else either waits for the claim to end, or uses a state of its own.
        unsafe { *self.inline.get() = Some(state) };
        std::mem::forget(unclaim);
        self.owner.store(me, Ordering::Release);
        // Safety: the inline state is ours.
        unsafe { self.inline() }
    }

    /// The current execution's state in its storage, created by `init` if there is none yet.
    fn stored(&self, init: impl FnOnce() -> S) -> Option<&S> {
        let key = StorageKey(self.slot(), SLOT_TYPE);
        let found =
            ExecutionState::try_with(|state| state.storage_slot::<S>(key).map(|value| value as *const S)).ok()?;
        let value = match found {
            Some(value) => value,
            None => {
                // Outside of `ExecutionState::with`, as `init` may need it (for the current task's
                // clock, say).
                let value = init();
                ExecutionState::try_with(|state| state.init_storage_destructed_last(key, value) as *const S).ok()?
            }
        };
        // Safety: a storage slot keeps its value boxed, at the same address, until the execution
        // destructs it. Execution teardown destructs it only once no task is left whose stack could
        // still be using the state (see `ExecutionState::tear_down`).
        Some(unsafe { &*value })
    }

    /// The identifier of this value's storage slots, assigned when first needed.
    fn slot(&self) -> usize {
        let slot = self.slot.load(Ordering::Relaxed);
        if slot != 0 {
            return slot;
        }
        let new = NEXT_SLOT.fetch_add(1, Ordering::Relaxed);
        assert_ne!(new, 0, "slot id overflow");
        match self.slot.compare_exchange(0, new, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => new,
            Err(slot) => slot,
        }
    }
}

// Safety: sending a `PerExecution` sends the state it owns.
unsafe impl<S: Send> Send for PerExecution<S> {}

// Safety: two threads that share a `PerExecution` each reach a state of their own, except for a
// state that every execution shares (see `PerExecution::shared`), which they may read at once, and
// one that code outside of executions reaches, which it may read while its owner runs, as it could
// before primitives kept their state per execution. Those two need `S: Sync`. The owner of the
// inline state can also change from one thread to another over time, which needs `S: Send`.
unsafe impl<S: Send + Sync> Sync for PerExecution<S> {}

impl<S> Default for PerExecution<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> fmt::Debug for PerExecution<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let owner = match self.owner.load(Ordering::Relaxed) {
            UNCLAIMED => "unclaimed".to_string(),
            CLAIMING => "claiming".to_string(),
            OUTSIDE => "outside of executions".to_string(),
            SHARED => "shared".to_string(),
            id => format!("execution {id}"),
        };
        f.debug_struct("PerExecution")
            .field("inline_owner", &owner)
            .finish_non_exhaustive()
    }
}

/// Keeps the value of a lock to one execution at a time.
///
/// A lock keeps its state once for each execution, but the value it protects belongs to the lock,
/// and so to every execution that uses it: the lock can't make a fresh copy of its value for each.
/// Then one execution's state can't stop another execution from taking the lock, and the value
/// with it. Two executions reach one lock's value when two tests that run at the same time on two
/// threads share the lock in a `static`, as `cargo test` runs the tests of a binary, and when a
/// guard outlives the execution that took it: one that a failed or stopped execution leaked, or
/// one that a test forgot or kept in a `static`. So a lock makes each guard
/// [`acquire`](ValueLock::acquire) its `ValueLock` once the execution's own state of the lock has
/// granted it, and [`release`](ValueLock::release) it when it is done with the value. The
/// `ValueLock` records which execution's guards hold the value, and `acquire` panics, rather than
/// hand out a second reference to the value, if guards of another execution hold it.
///
/// # Within one execution
///
/// Nothing changes. The lock's own state, which is the execution's, already decides which of the
/// execution's tasks hold the lock, as it does where no other execution can reach the lock, and the
/// guards of one execution share the value as the state allows. For them, `acquire` and `release`
/// take no scheduling point and never panic, so a lock takes exactly the scheduling points it takes
/// without a `ValueLock`.
///
/// # Whose guard
///
/// The panic says which of two causes it is, as they want different fixes. Each thread publishes
/// the execution it runs (see `ExecutionThread`), and execution identifiers are unique in the
/// process, so an execution that no thread runs has ended for good.
///
/// - **A test that runs at the same time.** A thread still runs the holder: tests that share a
///   `static` lock ran at the same time on different threads, and the message names the other
///   one's. They have to take turns, or run one at a time, or give each execution a lock of its
///   own, with `shuttle::lazy_static!` or a `LazyLock`.
/// - **A guard left behind.** The holder has ended. An execution ends once it has been torn down,
///   which drops what the tasks of a finished execution held, so a guard that still holds the value
///   was leaked: an execution that fails, or stops at its step bound, leaks the stacks of its
///   unfinished tasks with the guards on them, and `mem::forget` or a `static` can keep a guard
///   beyond its execution. It holds the value for good.
///
/// An execution that runs another one on its thread is suspended until the inner one ends, so the
/// inner one reports the outer one's guard as well.
///
/// Two executions can't hold the value at once even if both only read it, as readers of an
/// `RwLock` do: the `ValueLock` counts the guards of one execution, of whatever kind.
///
/// # Why not wait
///
/// The second execution could instead wait for the first to release the value, outside of
/// Shuttle's scheduler, so that tests that share a `static` take turns. But every task of an
/// execution runs on its one thread, so an execution that waits can't run its tasks either, and two
/// tests whose tasks hold guards of two `static` locks across scheduling points would deadlock,
/// where real threads wouldn't.
///
/// # Code outside of executions
///
/// Code outside of executions counts as one holder, which never ends: a value that it and an
/// execution both want panics. No Shuttle lock takes its value outside of an execution: each looks
/// the execution's state up, or takes a scheduling point, first, which it can only do in one.
#[derive(Debug)]
pub struct ValueLock {
    /// The execution whose guards hold the value, `OUTSIDE` for code outside of executions, or
    /// `NOBODY`.
    holder: AtomicUsize,
    /// How many guards of that execution hold the value.
    guards: AtomicUsize,
}

/// What `ValueLock::holder` holds when no guard holds the value. Execution identifiers are never
/// zero, and `OUTSIDE`, the holder for code outside of executions, isn't either.
const NOBODY: usize = 0;

impl ValueLock {
    /// A value that no guard holds.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            holder: AtomicUsize::new(NOBODY),
            guards: AtomicUsize::new(0),
        }
    }

    /// Record a new guard of the current execution, which the caller is about to make, after the
    /// execution's own state of the lock has granted it.
    ///
    /// # Panics
    ///
    /// If guards of another execution hold the value: one that runs at the same time on another
    /// thread, or one that has ended and left a guard behind (see the type's docs). `kind` names
    /// the lock in the message.
    #[track_caller]
    pub fn acquire(&self, kind: &'static str) {
        let me = current_execution_id().unwrap_or(OUTSIDE);
        loop {
            match self
                .holder
                .compare_exchange(NOBODY, me, Ordering::Acquire, Ordering::Relaxed)
            {
                Ok(_) => break,
                // Another guard of this execution, which the lock's state has granted it.
                Err(holder) if holder == me => break,
                // Returns only if the holder has let the value go since, to try again.
                Err(holder) => self.refuse(me, holder, kind),
            }
        }
        self.guards.fetch_add(1, Ordering::Relaxed);
    }

    /// Forget a guard, which the caller has finished using the value through.
    ///
    /// A lock calls this before the scheduling point of its release, so that the value is held only
    /// while a guard uses it: a test on another thread that takes the lock while this task is
    /// switched out in its release finds the value free. No other task of the execution can take the
    /// lock before the release, so none reaches the value meanwhile.
    pub fn release(&self) {
        let guards = self.guards.fetch_sub(1, Ordering::Relaxed);
        debug_assert_ne!(guards, 0, "released a guard of a lock's value that no guard holds");
        if guards == 1 {
            self.holder.store(NOBODY, Ordering::Release);
        }
    }

    /// Panic for the guards of `holder`, which hold the value that the current execution, `me`,
    /// wants, naming whose they are. Returns if they let the value go in the meantime, for `acquire`
    /// to try again.
    #[cold]
    #[inline(never)]
    #[track_caller]
    fn refuse(&self, me: usize, holder: usize, kind: &'static str) {
        if me == OUTSIDE {
            panic!("a Shuttle {kind} was locked outside of any Shuttle execution while an execution holds its value")
        }
        if holder == OUTSIDE {
            panic!("a Shuttle {kind} was locked while code outside of any Shuttle execution holds its value")
        }
        match execution_thread(holder) {
            Some(thread) if thread.is_current() => panic!(
                "a Shuttle {kind} was locked while a guard from outside of the current execution holds its value: \
                 a guard of the execution that this one runs inside of, on the same thread, which can't let it \
                 go before this one ends."
            ),
            Some(thread) => panic!(
                "a Shuttle {kind} was locked while a guard from outside of the current execution holds its value: \
                 a guard of the execution on {}, which runs at the same time and shares the {kind} in a \
                 `static`, as `cargo test` runs the tests of a binary on threads of their own. The {kind} keeps \
                 its locking state once for each execution, so this execution found it free, but its value is \
                 shared by every execution that uses it, and two can't hold it at once, even both to read it. \
                 Tests that share a `static` lock have to take turns, on a `std::sync::Mutex` of their own say, \
                 or run one at a time (`--test-threads=1`), or give each execution a lock of its own, with \
                 `shuttle::lazy_static!` or a `LazyLock`.",
                thread.describe()
            ),
            None => {
                // The holder has ended, and this thread sees what it did before it ended, through
                // `execution_thread`: if it still holds the value, its guard was left behind.
                if self.holder.load(Ordering::Acquire) == holder {
                    panic!(
                        "a Shuttle {kind} was locked while a guard from outside of the current execution holds its \
                         value: a guard left behind by an execution that has ended, which holds it for good. The \
                         {kind} keeps its locking state once for each execution, so this execution found it free, but \
                         its value is shared by every execution that uses it. An execution that fails, or stops at \
                         its step bound, leaks the stacks of its unfinished threads with the guards on them, and \
                         `mem::forget` or a `static` can keep a guard beyond its execution."
                    )
                }
            }
        }
    }
}

impl Default for ValueLock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_claims_inline() {
        let value = PerExecution::<u32>::new();
        assert_eq!(*value.get(|| 1), 1);
        // Code outside of executions keeps the state it created.
        assert_eq!(*value.get(|| 2), 1);
        assert_eq!(value.owner.load(Ordering::Relaxed), OUTSIDE);
    }

    #[test]
    fn shared_is_never_replaced() {
        let value = PerExecution::shared(7u32);
        assert_eq!(*value.get(|| unreachable!()), 7);
    }

    #[test]
    fn panicking_init_leaves_unclaimed() {
        let value = PerExecution::<u32>::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| *value.get(|| panic!("init failed"))));
        assert!(result.is_err());
        assert_eq!(value.owner.load(Ordering::Relaxed), UNCLAIMED);
        assert_eq!(*value.get(|| 3), 3);
    }

    #[test]
    fn new_in_current_execution_outside_is_new() {
        let value = PerExecution::<u32>::new_in_current_execution(|| unreachable!());
        assert_eq!(value.owner.load(Ordering::Relaxed), UNCLAIMED);
    }

    #[test]
    fn value_lock_counts_guards() {
        let lock = ValueLock::new();
        lock.acquire("lock");
        lock.acquire("lock");
        lock.release();
        assert_eq!(lock.holder.load(Ordering::Relaxed), OUTSIDE);
        lock.release();
        assert_eq!(lock.holder.load(Ordering::Relaxed), NOBODY);
    }

    #[test]
    fn value_lock_refuses_code_outside_of_executions_while_an_execution_holds_it() {
        let lock = ValueLock::new();
        // As if a guard of execution 1 held the value.
        lock.holder.store(1, Ordering::Relaxed);
        lock.guards.store(1, Ordering::Relaxed);
        let message = panic_message(std::panic::catch_unwind(|| lock.acquire("lock")));
        assert!(message.contains("outside of any Shuttle execution"), "{message}");
        assert_eq!(lock.guards.load(Ordering::Relaxed), 1);
    }

    /// The tests below run on threads that stand for executions of tests that run at the same time,
    /// without a scheduler: only `ValueLock` looks at which execution a thread runs.
    mod value_lock {
        use super::*;
        use crate::runtime::execution::CurrentExecutionId;
        use std::sync::mpsc;
        use std::thread::{Builder, JoinHandle};

        /// Runs `f` on a new thread named `name`, in an execution of its own.
        fn in_execution<T: Send + 'static>(name: &str, f: impl FnOnce() -> T + Send + 'static) -> JoinHandle<T> {
            Builder::new()
                .name(name.into())
                .spawn(move || {
                    let _execution = CurrentExecutionId::enter();
                    f()
                })
                .unwrap()
        }

        /// An execution that takes the value while an execution that runs at the same time holds it
        /// panics, naming the other's thread, and leaves the value with it.
        #[test]
        fn a_value_held_by_an_execution_running_at_the_same_time_is_reported() {
            static LOCK: ValueLock = ValueLock::new();
            let (held, holding) = mpsc::channel();
            let (reported, was_reported) = mpsc::channel::<()>();
            let holder = in_execution("holder", move || {
                LOCK.acquire("Mutex");
                held.send(()).unwrap();
                was_reported.recv().unwrap();
                LOCK.release();
            });
            holding.recv().unwrap();
            let message = panic_message(in_execution("taker", || LOCK.acquire("Mutex")).join());
            assert!(
                message.contains(
                    "a Shuttle Mutex was locked while a guard from outside of the current execution holds its \
                     value: a guard of the execution on thread 'holder', which runs at the same time"
                ),
                "{message}"
            );
            assert_eq!(LOCK.guards.load(Ordering::SeqCst), 1);
            reported.send(()).unwrap();
            holder.join().unwrap();
            assert_eq!(LOCK.holder.load(Ordering::SeqCst), NOBODY);
        }

        /// A value that an execution let go before it ended is free for the next one.
        #[test]
        fn a_value_released_before_its_execution_ended_is_free() {
            static LOCK: ValueLock = ValueLock::new();
            in_execution("first", || {
                LOCK.acquire("lock");
                LOCK.release();
            })
            .join()
            .unwrap();
            in_execution("second", || {
                LOCK.acquire("lock");
                LOCK.release();
            })
            .join()
            .unwrap();
        }

        /// A value that an ended execution holds is held for good, which the next execution that wants
        /// it reports, with a message that tells it from one held by a test running at the same time.
        #[test]
        fn a_value_left_behind_is_reported() {
            static LOCK: ValueLock = ValueLock::new();
            in_execution("leaks", || LOCK.acquire("Leaky")).join().unwrap();
            let message = panic_message(in_execution("finds", || LOCK.acquire("Leaky")).join());
            assert!(
                message.contains(
                    "a Shuttle Leaky was locked while a guard from outside of the current execution holds its \
                     value: a guard left behind by an execution that has ended"
                ),
                "{message}"
            );
            assert!(!message.contains("runs at the same time"), "{message}");
            assert_eq!(LOCK.guards.load(Ordering::SeqCst), 1);
        }

        /// A guard that an ended execution left behind and that another thread drops later frees the
        /// value as any release does.
        #[test]
        fn a_value_left_behind_and_released_later_is_free() {
            static LOCK: ValueLock = ValueLock::new();
            in_execution("leaks", || LOCK.acquire("lock")).join().unwrap();
            // Code that runs after the execution, outside of any, drops the guard.
            LOCK.release();
            in_execution("takes", || {
                LOCK.acquire("lock");
                LOCK.release();
            })
            .join()
            .unwrap();
        }

        /// An execution that runs inside another one on the same thread reports the outer one's
        /// guard, which can't be let go before the inner one ends.
        #[test]
        fn an_inner_execution_reports_the_outer_ones_guard() {
            static LOCK: ValueLock = ValueLock::new();
            let message = in_execution("outer", || {
                LOCK.acquire("lock");
                let inner = CurrentExecutionId::enter();
                let message = panic_message(std::panic::catch_unwind(|| LOCK.acquire("lock")));
                drop(inner);
                LOCK.release();
                message
            })
            .join()
            .unwrap();
            assert!(
                message.contains("a guard of the execution that this one runs inside of, on the same thread"),
                "{message}"
            );
        }

        /// The guards of one execution share the value.
        #[test]
        fn an_execution_shares_the_value_among_its_guards() {
            static LOCK: ValueLock = ValueLock::new();
            in_execution("one", || {
                LOCK.acquire("lock");
                LOCK.acquire("lock");
                LOCK.release();
                LOCK.release();
            })
            .join()
            .unwrap();
            assert_eq!(LOCK.holder.load(Ordering::SeqCst), NOBODY);
        }

        /// A value that code outside of executions holds is reported: no Shuttle lock takes its value
        /// there, so this is a bug.
        #[test]
        fn a_value_held_outside_of_executions_is_reported() {
            static LOCK: ValueLock = ValueLock::new();
            LOCK.acquire("lock");
            let message = panic_message(in_execution("inside", || LOCK.acquire("lock")).join());
            LOCK.release();
            assert!(
                message.contains("code outside of any Shuttle execution holds its value"),
                "{message}"
            );
        }
    }

    /// The message of a panic, from a `catch_unwind` or a `join`.
    fn panic_message<T>(result: std::thread::Result<T>) -> String {
        let Err(payload) = result else {
            panic!("expected a panic");
        };
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default()
    }
}
