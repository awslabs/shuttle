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
use std::rc::Rc;

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
type ContinuationFunction = Rc<Cell<Option<Box<dyn FnOnce()>>>>;

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
    pub fn new() -> Self {
        let function: ContinuationFunction = Rc::new(Cell::new(None));

        let mut gen = {
            let function = function.clone();

            Gn::new_opt(0x8000, move || {
                loop {
                    // Tell the caller we've finished the previous user function (or if this is our
                    // first time around the loop, the caller below expects us to pretend we've
                    // finished the previous function).
                    match generator::yield_(ContinuationOutput::Finished) {
                        None | Some(ContinuationInput::Exit) => break,
                        _ => (),
                    }

                    let f = function.take().expect("must have a function to run");

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

        let old = self.function.replace(Some(fun));
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

    /// Acquire a new continuation from the pool, allocating one if the pool is empty.
    pub fn acquire() -> PooledContinuation {
        if CONTINUATION_POOL.is_set() {
            CONTINUATION_POOL.with(|p| p.acquire_inner())
        } else {
            let p = Self::new();
            p.acquire_inner()
        }
    }

    fn acquire_inner(&self) -> PooledContinuation {
        let continuation = self
            .continuations
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(Continuation::new);

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

// Safety: these aren't sent across real threads
unsafe impl Send for PooledContinuation {}

/// Possibly yield back to the executor to perform a context switch.
pub(crate) fn switch() {
    if ExecutionState::maybe_yield() {
        let r = generator::yield_(ContinuationOutput::Yielded).unwrap();
        assert!(matches!(r, ContinuationInput::Resume));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reusable_continuation_drop() {
        let pool = ContinuationPool::new();

        let mut c = pool.acquire_inner();
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

        let mut c = pool.acquire_inner();
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

        let c = pool.acquire_inner();

        // Check that it's safe for a continuation to outlive the pool
        drop(pool);
        drop(c);
    }
}
