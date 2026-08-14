//! The resumable user code behind a [`Task`](super::Task).

use crate::runtime::execution::ExecutionState;
use crate::runtime::thread::continuation::PooledContinuation;
use crate::runtime::thread::switch::{clear_yielded_for_switch, take_yielded_for_switch};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

/// How a task's code is represented, and therefore how Shuttle resumes it.
///
/// See [`TaskBackend`](crate::config::TaskBackend) for the trade-offs between the two.
pub enum TaskBody {
    /// The task runs on its own stack. Resuming it switches to that stack, and it switches back
    /// when it reaches a scheduling point.
    Stackful(PooledContinuation),

    /// The task is a future. Resuming it polls the future, and it suspends by returning
    /// `Poll::Pending`.
    Stackless {
        future: Pin<Box<dyn Future<Output = ()>>>,
        /// The waker to poll with. Held here rather than fetched from the task on each resume,
        /// because resuming happens often and this saves borrowing the `ExecutionState` for it.
        waker: Waker,
    },
}

impl std::fmt::Debug for TaskBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskBody::Stackful(c) => f.debug_tuple("Stackful").field(c).finish(),
            TaskBody::Stackless { .. } => f.debug_struct("Stackless").finish_non_exhaustive(),
        }
    }
}

// Safety: task bodies are never sent across real threads; a Shuttle execution runs entirely on the
// thread that created it. This mirrors the existing `unsafe impl Send for PooledContinuation`.
unsafe impl Send for TaskBody {}

impl TaskBody {
    /// Run the task until it reaches its next scheduling point. Returns true if the task's code ran
    /// to completion.
    ///
    /// Must be called with the task set as `ExecutionState`'s current task, and *without* holding a
    /// borrow of the `ExecutionState`, because the user code being resumed will want to borrow it.
    pub(crate) fn resume(&mut self) -> bool {
        match self {
            TaskBody::Stackful(continuation) => continuation.resume(),
            TaskBody::Stackless { future, waker } => Self::poll_once(future.as_mut(), waker),
        }
    }

    /// Poll a stackless task's future once, translating the result into "did it finish".
    fn poll_once(future: Pin<&mut dyn Future<Output = ()>>, waker: &Waker) -> bool {
        let cx = &mut Context::from_waker(waker);

        // Clear any stale flag so that what we read below is only about this poll.
        clear_yielded_for_switch();

        match future.poll(cx) {
            Poll::Ready(()) => true,
            Poll::Pending => {
                // A `Pending` means one of two things, and they need opposite handling:
                //
                // * A Shuttle scheduling point unwound the poll on purpose (`poll_switch`). The
                //   task's state has already been set by whichever primitive asked for the
                //   scheduling point — runnable for a plain context switch, blocked if it is
                //   waiting on something — so we must leave it alone.
                // * The future is genuinely waiting to be woken. The task should sleep until its
                //   waker fires, unless it was already woken during this poll.
                if !take_yielded_for_switch() {
                    ExecutionState::with(|state| state.current_mut().sleep_unless_woken());
                }
                false
            }
        }
    }
}
