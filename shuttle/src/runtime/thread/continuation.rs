// To use the scoped version of the `generator` API, we'd need a way to store each continuation's
// `Scope` object locally. Normally that would be TLS but those use platform threads, so aren't
// aware of `generator` threads. Instead we just fall back to using the unscoped API.
// TODO: upgrade to the new scoped generator API
#![allow(deprecated)]

use crate::runtime::execution::ExecutionState;
use generator::{Generator, Gn};
use scoped_tls::scoped_thread_local;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ops::Deref;
use std::ops::DerefMut;
use std::panic::Location;
use std::rc::Rc;
use tracing::debug;

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
    generator: Generator<'static, ContinuationInput, ContinuationOutput>,
    function: ContinuationFunction,
    state: ContinuationState,
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
enum ContinuationInput {
    Resume,
    Exit,
}

/// Outputs that a continuation can pass back to us
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum ContinuationOutput {
    Yielded,
    Finished,
    Exited,
}

/// The current state of a continuation. Lifecycle runs from top to bottom.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum ContinuationState {
    NotReady, // has no function in its cell; waiting for input about what to do next
    Ready,    // has a function in its cell; waiting for input about what to do next
    Running,  // currently inside a user-provided function
}

impl Continuation {
    pub fn new(stack_size: usize) -> Self {
        let function = ContinuationFunction(Rc::new(Cell::new(None)));

        let mut gen = {
            let function = function.clone();

            Gn::new_opt(stack_size, move || {
                // Move the whole `ContinuationFunction`, not just its field (Rust 2021 thing)
                let _ = &function;

                loop {
                    // Tell the caller we've finished the previous user function (or if this is our
                    // first time around the loop, the caller below expects us to pretend we've
                    // finished the previous function).
                    match generator::yield_(ContinuationOutput::Finished) {
                        None | Some(ContinuationInput::Exit) => break,
                        _ => (),
                    }

                    let f = function.0.take().expect("must have a function to run");

                    f();
                }

                ContinuationOutput::Exited
            })
        };

        // Resume the generator once to get it into the loop
        let ret = gen.resume().unwrap();
        debug_assert_eq!(ret, ContinuationOutput::Finished);

        Self {
            generator: gen,
            function,
            state: ContinuationState::NotReady,
        }
    }

    /// Provide a new function for the continuation to execute. The continuation must
    /// be in reusable state.
    pub fn initialize(&mut self, fun: Box<dyn FnOnce()>) {
        debug_assert_eq!(
            self.state,
            ContinuationState::NotReady,
            "shouldn't replace a function before it runs"
        );

        let old = self.function.0.replace(Some(fun));
        debug_assert!(old.is_none(), "shouldn't replace a function before it runs");

        self.state = ContinuationState::Ready;
    }

    /// Resume the continuation, and returns true if the function it was executing has finished.
    pub fn resume(&mut self) -> bool {
        debug_assert!(self.state == ContinuationState::Ready || self.state == ContinuationState::Running);

        let ret = self.resume_with_input(ContinuationInput::Resume);
        debug_assert_ne!(
            ret,
            ContinuationOutput::Exited,
            "continuation should not exit if resumed from user code"
        );

        ret == ContinuationOutput::Finished
    }

    fn resume_with_input(&mut self, input: ContinuationInput) -> ContinuationOutput {
        self.generator.set_para(input);
        let ret = self.generator.resume().unwrap();
        if ret == ContinuationOutput::Finished {
            self.state = ContinuationState::NotReady;
        }
        ret
    }

    /// A continuation is reusable if it has completed running a user function and is waiting
    /// to resume. A continuation isn't reusable if it's still inside the user function `f`
    /// (for example, if the DFS scheduler terminated a path early, a function might not have
    /// completed, and resuming it will take us to somewhere arbitrary in user code).
    fn reusable(&self) -> bool {
        self.state == ContinuationState::NotReady
    }
}

impl Drop for Continuation {
    fn drop(&mut self) {
        // If the continuation is reusable, we tell it to exit and gracefully clean up its
        // resources. If not, we can't send it an exit message because it might be stopped in
        // arbitrary user code. Its resources will still be cleaned up when the underlying
        // generator is dropped, but doing so is slower (the generator impl invokes a panic
        // inside the continuation), so this drop handler exists to avoid it when possible.
        if self.reusable() {
            let ret = self.resume_with_input(ContinuationInput::Exit);
            debug_assert_eq!(ret, ContinuationOutput::Exited);
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

impl Drop for ContinuationPool {
    fn drop(&mut self) {
        // It's not safe to run Continuation's drop handler while dropping ContinuationPool,
        // because ContinuationPool is dropped by a thread local's destructor, and Continuation's
        // drop handler involves resuming a continuation, which reads a different thread local
        // from inside the generator implementation. Reading thread locals during thread local
        // destruction is forbidden on Linux.
        //
        // So we cheat here by prematurely marking the Continuation as unreusable. The underlying
        // resources will still get cleaned up, but we won't try to resume the continuation.
        for c in self.continuations.borrow_mut().iter_mut() {
            c.state = ContinuationState::Running;
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
        let c = self.continuation.take().unwrap();
        if c.reusable() {
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

/// Possibly yield back to the executor to perform a context switch.
/// This function should be called *before* any visible operation.
/// If each visible operation has a scheduling point before it, then there will
/// be a potential context switch *in between* each visible operation, which
/// is a necessary condition for Shuttle's completeness.
///
/// Putting scheduling points before visible operations, rather than after, has the
/// advantage of giving the scheduling algorithm *maximum information* to make scheduling
/// decisions based on what is about-to-happen on each task. The disadvantage of this
/// approach is that it can lead to double-yields for blocking operations, explained below.
///
/// Blocking operations will result in an additional switch when the current thread blocks.
/// As an optimization, the switch *before* the blocking operation can be conditionally
/// omitted to avoid switching twice for the same operation iff (1) the operation *will*
/// block in the current context and (2) if the act of blocking does not affect other tasks.
/// For example, a blocking Channel send does not satisfy (2) if there are not already
/// other blocking senders on the channel -- prior to the sender blocking, `try_recv` will
/// fail, but after the sender blocks, `try_recv` succeeds. Thus the act of blocking itself
/// is a visible operation, meaning that both scheduling points are necessary for complete
/// exploration of all possible behaviors.
#[track_caller]
pub(crate) fn switch_task() {
    crate::annotations::record_tick();
    debug!("switch from {}", Location::caller());
    if ExecutionState::maybe_yield() {
        let r = generator::yield_(ContinuationOutput::Yielded).unwrap();
        assert!(matches!(r, ContinuationInput::Resume));
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

        let r = c.resume();
        assert!(r, "continuation only has one step");

        drop(c);
        assert_eq!(
            pool.continuations.borrow().len(),
            1,
            "continuation should be reusable because the function finished"
        );

        let mut c = pool.acquire_inner(config.stack_size);
        c.initialize(Box::new(|| {
            generator::yield_with(ContinuationOutput::Yielded);
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
