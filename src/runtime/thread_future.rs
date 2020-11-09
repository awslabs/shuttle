// To use the scoped version of the `generator` API, we'd need a way to store each continuation's
// `Scope` object locally. Normally that would be TLS but those use platform threads, so aren't
// aware of `generator` threads. Instead we just fall back to using the unscoped API.
// TODO: upgrade to the new scoped generator API
#![allow(deprecated)]

use crate::runtime::execution::ExecutionState;
use generator::{Generator, Gn};
use std::future::Future;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::task::{Context, Poll};

/// A `ThreadFuture` is a Future that wraps around a regular thread, implemented as a continuation.
///
/// Polling the `ThreadFuture` resumes executing the thread until it decides to yield. If it
/// yields because the thread has terminated, the poll returns Ready, otherwise it returns Pending.
///
/// The `ThreadFuture` is a compatibility layer that allows us to implement standard threading
/// constructs (`std::thread` and `std::sync`) on top of Shuttle's futures-based executor.
pub(crate) struct ThreadFuture {
    continuation: Continuation,
}

impl ThreadFuture {
    pub(crate) fn new<F>(f: F) -> ThreadFuture
    where
        F: FnOnce() + Send + 'static,
    {
        ThreadFuture {
            continuation: Continuation::new(Box::new(f)),
        }
    }
}

impl Future for ThreadFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.continuation.set_para(ContinuationInput::Resume);
        let r = self.continuation.resume().unwrap();
        match r {
            ContinuationOutput::Yielded => Poll::Pending,
            ContinuationOutput::Finished => Poll::Ready(()),
            _ => panic!("unexpected output from continuation"),
        }
    }
}

// A continuation is a green thread that can be resumed and yielded at will. We use it to
// execute a "thread" from within a Future. Each continuation has a lifecycle:
// 1. when it begins executing, it immediately yields a WaitingForFunction message. This
//    first invocation happens immediately at construction time.
// 2. the first time it resumes, it expects to receive a Function message containing the
//    function it will run. It stores that function away, and yields a Ready message. This
//    second invocation happens immediately at construction time.
// 3. the next time it resumes, it expects to receive a Resume message, and then invokes
//    the function it received earlier. This invocation, and all future invocations, are
//    triggered by the scheduler. The function can yield at any time when it wants to invoke
//    a possible context switch.
// 4. when the function for this continuation finishes, the continuation yields a final
//    Finished message. Resuming the continuation after that point is an error.
pub(crate) struct Continuation(Generator<'static, ContinuationInput, ContinuationOutput>);

impl Continuation {
    fn new(fun: Box<dyn FnOnce()>) -> Self {
        let mut gen = Gn::new_opt(0x8000, move || {
            let f = generator::yield_(ContinuationOutput::WaitingForFunction).unwrap();

            match f {
                ContinuationInput::Function(f) => {
                    generator::yield_with(ContinuationOutput::Ready);

                    f();

                    ContinuationOutput::Finished
                }
                _ => panic!("unexpected continuation input"),
            }
        });

        // Resume once to prime the continuation to accept input. It will yield waiting to receive a
        // function to run.
        let r = gen.resume();
        assert!(matches!(r, Some(ContinuationOutput::WaitingForFunction)));

        // Send the function into the continuation, which prepares the function to be run, but
        // doesn't actually run it yet.
        gen.set_para(ContinuationInput::Function(fun));
        let r = gen.resume();
        assert!(matches!(r, Some(ContinuationOutput::Ready)));

        Self(gen)
    }
}

impl Deref for Continuation {
    type Target = Generator<'static, ContinuationInput, ContinuationOutput>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Continuation {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Inputs that the ThreadFuture can pass to a continuation.
pub(crate) enum ContinuationInput {
    Function(Box<dyn FnOnce()>),
    Resume,
}

/// Outputs that a continuation can pass back to the ThreadFuture
#[derive(Debug)]
pub(crate) enum ContinuationOutput {
    WaitingForFunction,
    Ready,
    Yielded,
    Finished,
}

/// Possibly yield back to the executor to perform a context switch.
pub(crate) fn switch() {
    // Schedule the next task, and if it requires a context switch, yield back to the executor.
    //
    // We don't execute a context switch if we are currently panicking (e.g., perhaps we're unwinding
    // the stack for a panic triggered while someone held a Mutex, and so are executing the Drop
    // handler for MutexGuard, which calls switch()).
    //
    // Invoking `yield` from within a panic is dangerous, because we will continue executing the
    // scheduler even though some of its state is in the middle of being poisoned. It's also
    // counter-intuitive, because it might allow other threads to execute during the panic of this
    // thread and so create more complex schedules than necessary to reach this panic. Avoiding the
    // context switch allows the panic to unwind the stack back to the top of the continuation,
    // which will propagate it to the executor.
    if !std::thread::panicking() && ExecutionState::maybe_yield() {
        let r = generator::yield_(ContinuationOutput::Yielded).unwrap();
        assert!(matches!(r, ContinuationInput::Resume));
    }
}
