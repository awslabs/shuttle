use crate::runtime::execution::ExecutionState;
use crate::{ContinuationFunctionBehavior, UNGRACEFUL_SHUTDOWN_CONFIG};
use corosensei::Yielder;
use corosensei::{stack::DefaultStack, Coroutine, CoroutineResult};
use scoped_tls::scoped_thread_local;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ops::Deref;
use std::ops::DerefMut;
use std::panic::Location;
use std::rc::Rc;
use tracing::trace;

scoped_thread_local! {
    pub(crate) static CONTINUATION_POOL: ContinuationPool
}

/// A continuation is a green thread that can be resumed and yielded at will. We use it to
/// execute a "thread" from within a Future.
///
/// For efficiency, we reuse continuations. The continuation can be provided a new function
/// to run via `initialize`. A continuation is only reusable if the previous function it was
/// executing completed.
pub(crate) struct Continuation {
    coroutine: Coroutine<ContinuationInput, ContinuationOutput, ContinuationOutput>,
    function: ContinuationFunction,
    state: ContinuationState,
    pub yielder: *const Yielder<ContinuationInput, ContinuationOutput>,
}

/// A cell to pass functions into continuations
#[allow(clippy::type_complexity)]
#[derive(Clone)]
struct ContinuationFunction(Rc<Cell<Option<Box<dyn FnOnce()>>>>);

// Safety: we arrange for the `function` field of `Continuation` to only be accessed by one thread
// at a time: Shuttle tests are single threaded, and continuations are never shared across threads
// by the ContinuationPool, which is thread-local.
unsafe impl Send for ContinuationFunction {}

/// Inputs that we can pass to a continuation.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum ContinuationInput {
    Resume,
    Exit,
}

/// Outputs that a continuation can pass back to us
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum ContinuationOutput {
    Yielded,
    Finished(*const Yielder<ContinuationInput, ContinuationOutput>),
    Exited,
}

/// The current state of a continuation. Lifecycle runs from top to bottom.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum ContinuationState {
    NotReady,          // has no function in its cell; waiting for input about what to do next
    Initialized,       // has a function in its cell, but hasn't started running yet
    Ready,             // has a suspended function in its cell; waiting for input about what to do next
    Running,           // currently inside a user-provided function
    FinishedIteration, // has finished the previous function, can be initialized with a new one
    Exited,            // the internal coroutine has exited its loop and cannot receive new functions to execute
}

impl Continuation {
    pub fn new(stack_size: usize) -> Self {
        let function = ContinuationFunction(Rc::new(Cell::new(None)));

        let mut coroutine = {
            let function = function.clone();

            Coroutine::with_stack(DefaultStack::new(stack_size).unwrap(), move |yielder, input| {
                if let ContinuationInput::Exit = input {
                    return ContinuationOutput::Exited;
                }

                // Move the whole `ContinuationFunction`, not just its field (Rust 2021 thing)
                let _ = &function;

                loop {
                    // Tell the caller we've finished the previous user function (or if this is our
                    // first time around the loop, the caller below expects us to pretend we've
                    // finished the previous function).
                    match yielder.suspend(ContinuationOutput::Finished(yielder as *const _)) {
                        ContinuationInput::Exit => break,
                        ContinuationInput::Resume => {}
                    };

                    let f = function.0.take().expect("must have a function to run");

                    f();
                }

                ContinuationOutput::Exited
            })
        };

        // Resume the coroutine once to get it into the loop. Because the continuations are kept in a pool and reused,
        // this requires us to exfiltrate the yielder from the closure passed in to Coroutine::with_stack and persist it
        // for the lifetime of the continuation's inner function. The yielder exfiltration happens below on the first suspend
        // of the coroutine
        let yielder = match coroutine.resume(ContinuationInput::Resume) {
            CoroutineResult::Yield(ContinuationOutput::Finished(yielder)) => yielder,
            _ => panic!("Coroutine should yield a pointer to its `corosensei::Yielder` from the first resume"),
        };

        Self {
            coroutine,
            yielder,
            function,
            state: ContinuationState::NotReady,
        }
    }

    /// Provide a new function for the continuation to execute. The continuation must
    /// be in reusable state.
    pub fn initialize(&mut self, fun: Box<dyn FnOnce()>) {
        debug_assert!(self.reusable(), "shouldn't replace a function before it completes");

        let old = self.function.0.replace(Some(fun));
        debug_assert!(old.is_none(), "shouldn't replace a function before it runs");

        self.state = ContinuationState::Initialized;
    }

    /// Resume the continuation, and returns true if the function it was executing has finished.
    pub fn resume(&mut self) -> bool {
        debug_assert!(self.state == ContinuationState::Ready || self.state == ContinuationState::Initialized);

        let ret = self.resume_with_input(ContinuationInput::Resume);
        debug_assert_ne!(
            ret,
            ContinuationOutput::Exited,
            "continuation should not exit if resumed from user code"
        );

        matches!(ret, ContinuationOutput::Finished(_))
    }

    fn resume_with_input(&mut self, input: ContinuationInput) -> ContinuationOutput {
        self.state = ContinuationState::Running;
        match self.coroutine.resume(input) {
            CoroutineResult::Yield(output) => {
                self.state = match output {
                    ContinuationOutput::Finished(_) => ContinuationState::FinishedIteration,
                    ContinuationOutput::Yielded => ContinuationState::Ready,
                    ContinuationOutput::Exited => ContinuationState::Exited,
                };
                output
            }
            CoroutineResult::Return(output) => {
                self.state = ContinuationState::Exited;
                output
            }
        }
    }

    /// A continuation is reusable if it has completed running a user function and is waiting
    /// to be initialized with a new one. A continuation isn't reusable if it's still inside the user
    /// function `f` (Ready or Running), as changing it's inner function would leak the currently
    /// executing context. We also do not consider Initialized coroutines to be reusable to avoid
    /// accidentally overwriting a continuation before it has had a chance to run. Continuations which
    /// have Exited, are not reusable as they have broken out of the loop where their inner functions
    /// can be replaced.
    fn reusable(&self) -> bool {
        self.state == ContinuationState::NotReady || self.state == ContinuationState::FinishedIteration
    }
}

impl Drop for Continuation {
    fn drop(&mut self) {
        // If the continuation is reusable, we tell it to exit and gracefully clean up its
        // resources. If not, we can't send it an exit message because it might be stopped in
        // arbitrary user code. Its resources will still be cleaned up when the underlying
        // generator is dropped, but doing so is slower (the generator impl invokes a panic
        // inside the continuation), so this drop handler exists to avoid it when possible.
        match self.state {
            ContinuationState::Initialized | ContinuationState::FinishedIteration | ContinuationState::NotReady => {
                let ret = self.resume_with_input(ContinuationInput::Exit);
                debug_assert_eq!(ret, ContinuationOutput::Exited);
            }
            ContinuationState::Running | ContinuationState::Ready => {
                // If already panicking or at the end of the execution, don't worry about cleaning up resources
                // on individual coroutines which are still in-flight
                if std::thread::panicking() {
                    // SAFETY: `force_reset` leaks the coroutine. However, given that the execution is *already* panicking
                    // at this point and will soon exit due to the original panic, this is unlikely to cause issues. Leaking
                    // the corouting here also avoids most tricky issues with scheduling points in drop handlers during a panic,
                    // which can often result in difficult-to-debug aborts from double-panics.
                    unsafe {
                        self.coroutine.force_reset();
                    }
                }
                self.coroutine.force_unwind();
            }
            ContinuationState::Exited => {
                // Already exited, nothing to do
            }
        }
    }
}

/// A `ContinuationPool` just holds on to old `Continuation`s that are reusable, and vends
/// them back out again. This amortizes the cost of allocating continuations, which involve
/// allocating new stacks (`mmap`), `mprotect`, etc.
pub(crate) struct ContinuationPool {
    // invariant: if c is in this queue, c.reusable() == true
    continuations: Rc<RefCell<VecDeque<Continuation>>>,
}

impl ContinuationPool {
    pub fn new() -> Self {
        Self {
            continuations: Rc::new(RefCell::new(VecDeque::new())),
        }
    }

    /// Acquire a new continuation from the global pool. Panics if that pool was not yet initialized.
    pub fn acquire(stack_size: usize) -> PooledContinuation {
        CONTINUATION_POOL.with(|p| p.acquire_inner(stack_size))
    }

    fn acquire_inner(&self, stack_size: usize) -> PooledContinuation {
        // TODO add a check to ensure that if we recycled a continuation, its
        // TODO allocated stack size is at least the requested `stack_size`
        let continuation = self
            .continuations
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(move || Continuation::new(stack_size));

        PooledContinuation {
            continuation: Some(continuation),
            queue: self.continuations.clone(),
        }
    }
}

/// A thin wrapper around a `Continuation` that returns it to a `ContinuationPool`
/// when dropped, but only if it's reusable.
pub(crate) struct PooledContinuation {
    continuation: Option<Continuation>,
    queue: Rc<RefCell<VecDeque<Continuation>>>,
}

impl Drop for PooledContinuation {
    fn drop(&mut self) {
        let mut c = self.continuation.take().unwrap();
        if c.reusable() {
            self.queue.borrow_mut().push_back(c);
        } else if matches!(c.state, ContinuationState::Initialized) {
            // A continuation which has been initialized but not run cannot be immediately reused.
            // This is because arguments and captures may already have been moved into the function,
            // and thus these moved objects won't be dropped until the function itself has been
            // dropped. Thus we must drop the inner function before reusing it.
            let old = c.function.0.replace(None);
            c.state = ContinuationState::NotReady;
            if std::thread::panicking() {
                match UNGRACEFUL_SHUTDOWN_CONFIG.get().continuation_function_behavior {
                    ContinuationFunctionBehavior::Drop => drop(old),
                    ContinuationFunctionBehavior::Leak => std::mem::forget(old),
                }
            } else {
                drop(old);
            }
            self.queue.borrow_mut().push_back(c);
        }
    }
}

impl Deref for PooledContinuation {
    type Target = Continuation;

    fn deref(&self) -> &Self::Target {
        self.continuation.as_ref().unwrap()
    }
}

impl DerefMut for PooledContinuation {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.continuation.as_mut().unwrap()
    }
}

impl std::fmt::Debug for PooledContinuation {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("PooledContinuation").finish()
    }
}

// Safety: these aren't sent across real threads
unsafe impl Send for PooledContinuation {}

/// Possibly yield back to the executor to perform a context switch.  This function should be
/// called *before* any visible operation. If each visible operation has a scheduling point
/// before it, then there will be a potential context switch *in between* any pair of visible
/// operations, which is a necessary condition for completeness.
///
/// Putting scheduling points before visible operations, rather than after, has the advantage
/// of giving the scheduling algorithm additional information to make scheduling decisions
/// based on what is about to happen on each task. The disadvantage of this approach is that it
/// is more difficult to avoid double-yields for blocking operations, explained below.
///
/// In addition to the scheduling point before the operation begins, blocking operations will
/// result in a *second* context switch if the current thread is blocked. As an optimization,
/// the switch *before* the blocking operation can be conditionally omitted to avoid switching
/// twice for the same operation iff (1) the operation *will* block and (2) if the act of
/// blocking *commutes* with all other operations on that resource.
///
/// Reasoning: We can consider a blocking operation (`Y`) such as acquiring a mutex as two
/// sub-operations (`Y1`) blocking and (`Y2`) proceeding after being unblocked. The double-yield
/// optimization omits the scheduling point before `Y1`. For arbitrary events `X` and `Z` and
/// intra-thread orderings `T1: X Y1 Y2` and `T2: Z`, we have four interleavings:
///
/// `X Z Y1 Y2`
/// `X Y1 Z Y2`
/// `Z X Y1 Y2`
/// `X Y1 Y2 Z`
///
/// Note that the first interleaving is *not observable* if we omit the scheduling point before `Y1`.
/// Thus to maintain behavioral completeness when omitting this scheduling point, all states
/// observable from the first schedule must also be observable in one of the other schedules.
///
/// Observe that if `Y1` and `Z` commute, then the first two schedules are behaviorally equivalent,
/// thus the optimization is safe. So, to ensure the safety of the double-yield optimization for an
/// operation `Y1`, it suffices to check that `Y1` commutes with all operations `Z` on the same resource,
/// as operations on other resources should commute trivially.
#[track_caller]
pub(crate) fn switch() {
    crate::annotations::record_tick();
    trace!("switch from {}", Location::caller());
    if ExecutionState::maybe_yield() {
        let yielder = ExecutionState::with(|state| state.current().yielder);

        // SAFETY: A yielder reference will be valid for the lifetime of the continuation (see `corosensei::Coroutine::with_stack`)
        // The yielder field is stored on the Task, whose lifetime is necessarily subsumed by the lifetime of the continuation which contains it.
        // As a result, the task struct cannot contain an invalidated pointer to it's yielder. There are no mutable references to the yielder.
        match unsafe { &(*yielder) }.suspend(ContinuationOutput::Yielded) {
            ContinuationInput::Exit => panic!("unexpected exit continuation"),
            ContinuationInput::Resume => {}
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    #[test]
    fn reusable_continuation_drop() {
        let pool = ContinuationPool::new();
        let config: Config = Default::default();

        let mut c = pool.acquire_inner(config.stack_size);
        c.initialize(Box::new(|| {
            let _ = 1 + 1;
        }));
        let yielder = c.yielder;

        let r = c.resume();
        assert!(r, "continuation only has one step");

        drop(c);
        assert_eq!(
            pool.continuations.borrow().len(),
            1,
            "continuation should be reusable because the function finished"
        );

        let mut c = pool.acquire_inner(config.stack_size);
        c.initialize(Box::new(move || {
            // SAFETY: We must use the yielder directly here because we are not in a Shuttle execution
            // for `continuation::switch`, which requires access to `ExecutionState`. This is safe because
            // the yielder's lifetime is valid as long as the continuation has not `Exited`, and the
            // continuation cannot have exited if it is executing its current function.
            unsafe { &(*yielder) }.suspend(ContinuationOutput::Yielded);
            let _ = 1 + 1;
        }));

        let r = c.resume();
        assert!(!r, "continuation yields once, shouldn't be finished yet");

        drop(c);
        assert_eq!(
            pool.continuations.borrow().len(),
            0,
            "continuation should not be reusable because the function wasn't finished"
        );

        let c = pool.acquire_inner(config.stack_size);

        // Check that it's safe for a continuation to outlive the pool
        drop(pool);
        drop(c);
    }
}
