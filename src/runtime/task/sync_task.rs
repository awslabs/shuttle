// To use the scoped version of the `generator` API, we'd need a way to store each continuation's
// `Scope` object locally. Normally that would be TLS but those use platform threads, so aren't
// aware of `generator` threads. Instead we just fall back to using the unscoped API.
// TODO: upgrade to the new scoped generator API
#![allow(deprecated)]

use crate::runtime::task::*;
use generator::{Generator, Gn};
use std::ops::{Deref, DerefMut};

pub(crate) struct SyncTask {
    continuation: Continuation,
}

pub(crate) fn new<F>(f: F) -> SyncTask
where
    F: FnOnce() + Send + 'static,
{
    SyncTask {
        continuation: Continuation::new(Box::new(f)),
    }
}

impl AtomicTask for SyncTask {
    fn step(&mut self) -> TaskResult {
        // Resume the continuation for the task we scheduled
        self.continuation.set_para(ContinuationInput::Resume);
        self.continuation.resume().unwrap()
    }

    fn cleanup(self: Box<Self>) {
        assert!(self.continuation.is_done());
    }
}

// Each continuation has a lifecycle:
// 1. a continuation is created with the function it will run. It stores that function
//    away, and yields a Ready message.
// 3. the next time it resumes, it expects to receive a Resume message, and then invokes
//    the function it received earlier. This invocation, and all future invocations, are
//    triggered by the scheduler. The function can yield at any time when it wants to invoke
//    a possible context switch.
// 4. when the function for this continuation finishes, the continuation yields a final
//    Finished message. Resuming the continuation after that point is an error.
pub(crate) struct Continuation(Generator<'static, ContinuationInput, TaskResult>);

impl Continuation {
    fn new(fun: Box<dyn FnOnce()>) -> Self {
        let mut gen = Gn::new_opt(0x8000, move || {
            let f = generator::yield_(TaskResult::WaitingForFunction).unwrap();

            match f {
                ContinuationInput::Function(f) => {
                    generator::yield_with(TaskResult::Ready);

                    f();

                    TaskResult::Finished
                }
                _ => panic!("unexpected continuation input"),
            }
        });

        // Resume once to prime the continuation for use. It will yield waiting to receive a
        // function to run.
        let r = gen.resume();
        assert!(matches!(r, Some(TaskResult::WaitingForFunction)));

        // Send the function into the continuation, which prepares the task
        // to be run, but doesn't actually run it yet.
        gen.set_para(ContinuationInput::Function(fun));
        let r = gen.resume();
        assert!(matches!(r, Some(TaskResult::Ready)));

        Self(gen)
    }
}

impl Deref for Continuation {
    type Target = Generator<'static, ContinuationInput, TaskResult>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Continuation {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Inputs that the executor can pass to a continuation.
pub(crate) enum ContinuationInput {
    Function(Box<dyn FnOnce()>),
    Resume,
}

/// Yield back to the scheduler to perform a (possible) context switch.
// TODO This method is here so that all knowledge of Generators lives in this abstraction only
// TODO It does mean that we have to expose this module crate-wide.  Is there a better layout?
pub(crate) fn switch() {
    let r = generator::yield_(TaskResult::Yielded).unwrap();
    assert!(matches!(r, ContinuationInput::Resume));
}
