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
    //! Two guards keep it honest:
    //! - [`PollGuard`] marks the dynamic extent of a driver-loop `poll`, so clones made by the
    //!   executor itself (e.g. `Task::waker()`) are not mistaken for await sites.
    //! - [`InternalBlockOnGuard`] marks the `block_on` that the *synchronous* primitives use
    //!   internally. A task parked there keeps its whole call chain on its coroutine stack, so it is
    //!   captured lazily on deadlock instead. This is the hot path: capturing it eagerly is what
    //!   made `SHUTTLE_CAPTURE_BACKTRACE` cost ~79x.

    use std::backtrace::Backtrace;
    use std::cell::{Cell, RefCell};

    thread_local! {
        static IN_POLL_DEPTH: Cell<usize> = const { Cell::new(0) };
        static INTERNAL_BLOCK_ON_DEPTH: Cell<usize> = const { Cell::new(0) };
        /// Await-site backtrace for the poll currently in progress, if one was captured.
        static CAPTURED: RefCell<Option<Backtrace>> = const { RefCell::new(None) };
    }

    macro_rules! depth_guard {
        ($name:ident, $slot:ident, $doc:literal) => {
            #[doc = $doc]
            #[derive(Debug)]
            pub struct $name;

            impl $name {
                #[allow(clippy::new_without_default)]
                pub fn new() -> Self {
                    $slot.set($slot.get() + 1);
                    Self
                }
            }

            impl Drop for $name {
                fn drop(&mut self) {
                    $slot.set($slot.get() - 1);
                }
            }
        };
    }

    depth_guard!(
        PollGuard,
        IN_POLL_DEPTH,
        "Marks the dynamic extent of a `Future::poll` call made by one of Shuttle's driver loops."
    );
    depth_guard!(
        InternalBlockOnGuard,
        INTERNAL_BLOCK_ON_DEPTH,
        "Marks a `block_on` that Shuttle itself performs on the task's behalf, rather than one the user wrote."
    );

    /// Whether an await-site capture is worth taking right now.
    fn should_capture() -> bool {
        crate::backtrace_enabled() && IN_POLL_DEPTH.get() > 0 && INTERNAL_BLOCK_ON_DEPTH.get() == 0
    }

    /// Called from the waker vtable's `clone`. If we are inside a user future's `poll`, this stack
    /// contains the await chain, so record it.
    pub fn note_waker_clone() {
        if should_capture() {
            let backtrace = Backtrace::force_capture();
            CAPTURED.with(|slot| *slot.borrow_mut() = Some(backtrace));
        }
    }

    /// Take whatever the in-progress poll captured. Called by the driver loops once `poll` has
    /// returned `Pending`.
    ///
    /// `None` means no await site was recovered — either backtraces are off, or the future returned
    /// `Pending` without cloning the waker (some futures skip the clone when they already hold an
    /// equivalent one). Returning `None` rather than a stale value is deliberate: it lets the
    /// deadlock handler fall back to its lazy capture instead of printing a backtrace from an
    /// earlier, unrelated park.
    pub fn take_captured() -> Option<Backtrace> {
        CAPTURED.with(|slot| slot.borrow_mut().take())
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
