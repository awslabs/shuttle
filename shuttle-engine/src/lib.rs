#![deny(warnings, missing_debug_implementations)]
#![allow(dead_code, clippy::new_without_default)]

pub mod annotations;
pub mod config;
pub mod current;
pub mod future;
pub mod hint;
pub mod runtime;
pub mod scheduler;
pub mod sync_types;
pub mod thread_support;

pub use config::{
    Config, ContinuationFunctionBehavior, FailurePersistence, MaxSteps, UngracefulShutdownConfig,
    UNGRACEFUL_SHUTDOWN_CONFIG,
};
pub use runtime::runner::{PortfolioRunner, Runner};
pub use sync_types::{ResourceSignature, ResourceType};

/// If this environment variable is set, then Shuttle will capture the backtrace of each task and display
/// the backtraces in the panic message.
/// Capturing backtraces is quite expensive, so this should only be set when debugging a failing test.
pub const CAPTURE_BACKTRACE: &str = "SHUTTLE_CAPTURE_BACKTRACE";

/// The random seed used to initialize either the `RandomScheduler` or `PctScheduler`
/// (both in the `shuttle-schedulers` crate)
const RANDOM_SEED: &str = "SHUTTLE_RANDOM_SEED";

/// If this is set, then warnings about Shuttle's modelling of weak memory and differences between Shuttle's
/// version of LazyStatic and the regular version of LazyStatic will not be emitted.
pub const SILENCE_WARNINGS: &str = "SHUTTLE_SILENCE_WARNINGS";

/// Used in the annotation scheduler to specify where to write the annotations.
pub const ANNOTATION_FILE: &str = "SHUTTLE_ANNOTATION_FILE";

#[cfg(feature = "annotation")]
pub fn annotation_file() -> String {
    std::env::var(ANNOTATION_FILE).unwrap_or_else(|_| "annotated.json".to_string())
}

pub fn silence_warnings() -> bool {
    std::env::var(SILENCE_WARNINGS).is_ok()
}

pub mod await_backtrace {
    //! Recovering the *await site* of a task parked on a pending future.
    //!
    //! A stack backtrace cannot find it after the fact: when a future returns [`std::task::Poll::Pending`]
    //! its `poll` stack unwinds, and the await chain lives on in the compiler-generated state
    //! machine, which no unwinder can walk. So it has to be captured while that stack is still live.
    //!
    //! The hook with the right timing is the waker. A future that returns `Pending` is contractually
    //! obliged to arrange for a wakeup, and the ordinary way to do that is `cx.waker().clone()` —
    //! which runs *inside* the future's own `poll`, on the live stack, through a vtable Shuttle owns
    //! (see [`crate::runtime::task::waker`]). That works for arbitrary user futures, not just
    //! Shuttle's own leaves.
    //!
    //! Not every `Pending` comes with a clone, though. A future that already holds a waker that
    //! [`will_wake`](std::task::Waker::will_wake) the task may skip it, as `AtomicWaker::register`
    //! does, so polling it again records nothing. [`AwaitSite`] then keeps the site recorded for the
    //! task's previous park and marks it as coming from an earlier poll. Usually the task is still
    //! waiting on that same future, but it may have moved on to a later await whose future skipped
    //! the clone too, and the report says it may be stale.
    //!
    //! Three guards keep it honest:
    //! - [`PollGuard`] marks the dynamic extent of a driver-loop `poll`, so clones made by the
    //!   executor itself (e.g. `Task::waker()`) are not mistaken for await sites.
    //! - [`InternalBlockOnGuard`] marks the `block_on` that the *synchronous* primitives use
    //!   internally. A task parked there keeps its whole call chain on its coroutine stack, so it is
    //!   captured lazily on deadlock instead. This is the hot path: capturing it eagerly is what
    //!   made `SHUTTLE_CAPTURE_BACKTRACE` cost ~79x.
    //! - [`SwitchGuard`] keeps the state the other two track per task. It lives in thread-locals,
    //!   but a task can be switched out part-way through a poll or an internal `block_on`, and other
    //!   tasks then run on the same thread, so the guard sets the task's state aside until it is
    //!   switched back in.

    use crate::runtime::task::Task;
    use std::backtrace::Backtrace;
    use std::cell::{Cell, RefCell};

    thread_local! {
        // These describe the task running on this thread. See `SwitchGuard`.
        static IN_POLL_DEPTH: Cell<usize> = const { Cell::new(0) };
        static INTERNAL_BLOCK_ON_DEPTH: Cell<usize> = const { Cell::new(0) };
        /// Await-site backtrace for the poll currently in progress, if one was captured.
        static CAPTURED: RefCell<Option<Backtrace>> = const { RefCell::new(None) };
    }

    /// Marks the dynamic extent of a `Future::poll` call made by one of Shuttle's driver loops.
    #[derive(Debug)]
    pub struct PollGuard;

    impl PollGuard {
        #[allow(clippy::new_without_default)]
        pub fn new() -> Self {
            let depth = IN_POLL_DEPTH.get();
            if depth == 0 {
                // A capture belongs to the poll it was taken in. Drop any that an earlier poll left
                // behind by returning `Ready`, possibly in a task that has finished since, so that
                // this poll cannot report it if it returns `Pending` without cloning the waker.
                CAPTURED.with(|slot| slot.borrow_mut().take());
            }
            IN_POLL_DEPTH.set(depth + 1);
            Self
        }
    }

    impl Drop for PollGuard {
        fn drop(&mut self) {
            IN_POLL_DEPTH.set(IN_POLL_DEPTH.get() - 1);
        }
    }

    /// Marks a `block_on` that Shuttle itself performs on the task's behalf, rather than one the user wrote.
    #[derive(Debug)]
    pub struct InternalBlockOnGuard;

    impl InternalBlockOnGuard {
        #[allow(clippy::new_without_default)]
        pub fn new() -> Self {
            INTERNAL_BLOCK_ON_DEPTH.set(INTERNAL_BLOCK_ON_DEPTH.get() + 1);
            Self
        }
    }

    impl Drop for InternalBlockOnGuard {
        fn drop(&mut self) {
            INTERNAL_BLOCK_ON_DEPTH.set(INTERNAL_BLOCK_ON_DEPTH.get() - 1);
        }
    }

    /// Sets the running task's await-site state aside while it is switched out, and puts it back
    /// when it is switched back in.
    ///
    /// Held across the suspend in [`crate::runtime::thread::switch`]. Without it, the tasks that
    /// run in the meantime see the switched-out task's state. A task blocked on a `Mutex` holds an
    /// [`InternalBlockOnGuard`] for as long as it stays blocked, which would stop every other task's
    /// await site from being captured, and a capture taken part-way through one task's poll could
    /// be reported by another task's driver loop.
    #[derive(Debug)]
    pub struct SwitchGuard {
        in_poll_depth: usize,
        internal_block_on_depth: usize,
        captured: Option<Backtrace>,
    }

    impl SwitchGuard {
        #[allow(clippy::new_without_default)]
        pub fn new() -> Self {
            Self {
                in_poll_depth: IN_POLL_DEPTH.replace(0),
                internal_block_on_depth: INTERNAL_BLOCK_ON_DEPTH.replace(0),
                captured: CAPTURED.with(|slot| slot.borrow_mut().take()),
            }
        }
    }

    impl Drop for SwitchGuard {
        fn drop(&mut self) {
            // This also runs if the task is unwound instead of switched back in (see `Continuation`'s
            // `Drop`). Putting its state back is still right then: its own guards are dropped next,
            // and undo it.
            IN_POLL_DEPTH.set(self.in_poll_depth);
            INTERNAL_BLOCK_ON_DEPTH.set(self.internal_block_on_depth);
            let captured = self.captured.take();
            CAPTURED.with(|slot| *slot.borrow_mut() = captured);
        }
    }

    /// Whether an await-site capture is worth taking right now.
    fn should_capture() -> bool {
        crate::backtrace_enabled() && IN_POLL_DEPTH.get() > 0 && INTERNAL_BLOCK_ON_DEPTH.get() == 0
    }

    /// Called from the waker vtable's `clone`. If we are inside a user future's `poll`, this stack
    /// contains the await chain, so record it.
    ///
    /// Inlined because it sits on the waker-clone path, which every future that returns `Pending`
    /// exercises whether or not backtraces are enabled; inlining lets the `should_capture` check
    /// collapse to a load and a branch.
    #[inline]
    pub fn note_waker_clone() {
        if should_capture() {
            let backtrace = Backtrace::force_capture();
            CAPTURED.with(|slot| *slot.borrow_mut() = Some(backtrace));
        }
    }

    /// Take whatever the in-progress poll captured. `None` if it returned `Pending` without cloning
    /// the waker.
    fn take_captured() -> Option<Backtrace> {
        CAPTURED.with(|slot| slot.borrow_mut().take())
    }

    /// Where a future driver loop's task is waiting, kept from one park to the next.
    ///
    /// Each of Shuttle's driver loops keeps one, and calls [`park`](Self::park) once `poll` has
    /// returned `Pending` and [`unpark`](Self::unpark) once the task is switched back in. So the
    /// task carries an await site only while it is parked in that loop. While it runs it may block
    /// somewhere else, in a `Mutex::lock` inside its next poll or anywhere after `block_on`
    /// returns, and the deadlock handler only captures a backtrace for a task that has none.
    #[derive(Debug, Default)]
    pub struct AwaitSite(Option<Backtrace>);

    impl AwaitSite {
        /// Record where `task` is waiting, just before it parks after its future returned `Pending`:
        /// the await site this poll captured if it cloned the waker, and otherwise the one recorded
        /// for the task's previous park, marked as coming from an earlier poll.
        pub fn park(&mut self, task: &mut Task) {
            match take_captured() {
                Some(backtrace) => {
                    task.backtrace = Some(backtrace);
                    task.await_site_from_earlier_poll = false;
                }
                None => {
                    task.await_site_from_earlier_poll = self.0.is_some();
                    task.backtrace = self.0.take();
                }
            }
        }

        /// Take the await site back off `task` once it is switched back in, to keep for its next
        /// park.
        pub fn unpark(&mut self, task: &mut Task) {
            self.0 = task.backtrace.take();
            task.await_site_from_earlier_poll = false;
        }
    }
}

pub fn backtrace_enabled() -> bool {
    // Read once. This is called from `Task::block` and `Task::sleep`, so on every block and every
    // `Poll::Pending`, and `std::env::var` takes a lock on the environment and allocates a `String`.
    // Profiling showed it at 7-9% of self time on lock-heavy workloads.
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var(CAPTURE_BACKTRACE).is_ok())
}

pub fn seed_from_env(fallback_seed: u64) -> u64 {
    let seed_env = std::env::var(RANDOM_SEED);
    match seed_env {
        Ok(s) => match s.as_str().parse::<u64>() {
            Ok(seed) => {
                tracing::info!(
                    "Initializing scheduler with the seed provided by {}: {}",
                    RANDOM_SEED,
                    seed
                );
                seed
            }
            Err(err) => panic!("The seed provided by {RANDOM_SEED} is not a valid u64: {err}"),
        },
        Err(_) => fallback_seed,
    }
}
