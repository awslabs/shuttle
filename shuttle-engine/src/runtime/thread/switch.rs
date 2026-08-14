//! Scheduling points, and how a task suspends itself at one.
//!
//! A scheduling point is a place where Shuttle is allowed to stop running the current task and run
//! a different one. How a task actually suspends itself depends on the backend it is running on
//! (see [`TaskBackend`]):
//!
//! * On [`TaskBackend::Stackful`], the task runs on its own stack, so it can suspend from anywhere
//!   by switching stacks. [`switch`] does this and returns once the task is resumed.
//! * On [`TaskBackend::Futures`], the task *is* a future that Shuttle polls, so the only way for it
//!   to suspend is to return `Poll::Pending` all the way up to the executor. That can only be done
//!   from inside a `poll`, which is why scheduling points on this backend have to be reached
//!   through [`poll_switch`] or its `Future` wrapper [`switch_point`].
//!
//! Code that can be reached from an `async` context should prefer `switch_point().await`, which
//! does the right thing on both backends.

use crate::runtime::execution::{ExecutionState, SwitchMode};
use std::cell::Cell;
use std::future::Future;
use std::panic::Location;
use std::pin::Pin;
use std::task::{Context, Poll};
use tracing::trace;

std::thread_local! {
    /// Set when a task on the futures backend returns `Poll::Pending` in order to reach a
    /// scheduling point, rather than because it is waiting to be woken.
    ///
    /// The executor clears this before polling a task and reads it if the poll returns `Pending`,
    /// which is how it tells the two cases apart. See `TaskBody::resume`.
    static YIELDED_FOR_SWITCH: Cell<bool> = const { Cell::new(false) };
}

/// Record that the current `Poll::Pending` is a Shuttle scheduling point rather than the task
/// waiting on a waker.
fn set_yielded_for_switch() {
    YIELDED_FOR_SWITCH.set(true);
}

/// Clear the flag, returning whether it was set. Called by the executor after a poll returns
/// `Pending`.
pub(crate) fn take_yielded_for_switch() -> bool {
    YIELDED_FOR_SWITCH.replace(false)
}

/// Clear the flag without reading it. Called by the executor before each poll, so that a request
/// left behind by a future that was polled by hand can't be misattributed to this poll.
pub(crate) fn clear_yielded_for_switch() {
    YIELDED_FOR_SWITCH.set(false);
}

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
///
/// # Backends
///
/// This is the *synchronous* scheduling point, so on the futures backend it cannot actually
/// suspend the task. There, a scheduling point requested by a task that is still runnable is
/// dropped (and counted), and one requested by a task that has already blocked itself is a hard
/// error, because letting a blocked task keep running would be unsound. Reachable-from-async code
/// should use [`switch_point`] instead so that it works on both backends.
#[track_caller]
pub fn switch() {
    crate::annotations::record_tick();
    trace!("switch from {}", Location::caller());

    match ExecutionState::switch_mode() {
        SwitchMode::Stackful => {
            if ExecutionState::maybe_yield() {
                // SAFETY: see `suspend_current_continuation`.
                unsafe { super::continuation::suspend_current_continuation() }
            }
        }
        SwitchMode::DeferOnFutures => {}
    }
}

/// Reach a scheduling point from inside a `poll`.
///
/// On the stackful backend this suspends inline and always returns `Ready`. On the futures backend
/// it returns `Pending` if the executor should run a different task, in which case the caller
/// *must* propagate that `Pending` up to the executor without doing any further work. The caller
/// will be polled again once this task is scheduled, and should not request the scheduling point a
/// second time.
#[track_caller]
pub fn poll_switch() -> Poll<()> {
    crate::annotations::record_tick();
    trace!("poll_switch from {}", Location::caller());

    match ExecutionState::switch_mode() {
        SwitchMode::Stackful => {
            if ExecutionState::maybe_yield() {
                // SAFETY: see `suspend_current_continuation`.
                unsafe { super::continuation::suspend_current_continuation() }
            }
            Poll::Ready(())
        }
        SwitchMode::DeferOnFutures => {
            if ExecutionState::maybe_yield() {
                set_yielded_for_switch();
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        }
    }
}

/// A future that reaches a scheduling point once and then resolves.
///
/// This is the scheduling point to use from `async` code, because it works on both backends:
/// awaiting it suspends the task in whichever way the backend supports.
#[derive(Debug)]
#[must_use = "a scheduling point has no effect unless awaited"]
pub struct SwitchPoint {
    done: bool,
}

/// Reach a scheduling point, giving the scheduler the opportunity to run another task.
pub fn switch_point() -> SwitchPoint {
    SwitchPoint { done: false }
}

impl Future for SwitchPoint {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        if self.done {
            return Poll::Ready(());
        }
        self.done = true;
        poll_switch()
    }
}
