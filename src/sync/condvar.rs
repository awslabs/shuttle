use crate::my_clock;
use crate::runtime::execution::ExecutionState;
use crate::runtime::task::clock::VectorClock;
use crate::runtime::task::TaskId;
use crate::runtime::thread;
use crate::sync::MutexGuard;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::{LockResult, PoisonError};
use std::time::Duration;
use tracing::trace;

/// A `Condvar` represents the ability to block a thread such that it consumes no CPU time while
/// waiting for an event to occur.
#[derive(Debug)]
pub struct Condvar {
    state: Rc<RefCell<CondvarState>>,
}

#[derive(Debug)]
struct CondvarState {
    waiters: HashMap<TaskId, CondvarWaitStatus>,
    next_epoch: usize,
}

// For tracking causal dependencies, we record the clock C of the thread that does the notify.
// When a thread is unblocked, its clock is updated by C.
#[derive(PartialEq, Eq, Debug)]
enum CondvarWaitStatus {
    Waiting,
    // invariant: VecDeque is non-empty (if it's empty, we should be Waiting instead)
    Signal(VecDeque<(usize, VectorClock)>),
    Broadcast(VectorClock),
}

// TODO Check if we can avoid using epochs now that we have vector clocks.
// TODO See [Issue 39](https://github.com/awslabs/shuttle/issues/39)

// We implement `Condvar` by tracking the `CondvarWaitStatus` of each thread currently waiting on
// the `Condvar`.
//
// ## Terminology
//
// There's competing notions of "unblocked" here -- unblocked *from the condition variable*, which
// is the API-level notion of blocked, and unblocked *within Shuttle's scheduler*, which is an
// implementation detail. To disambiguate, we'll call the latter "runnable" or "unrunnable", even
// though that's a little awkward.
//
// ## `notify_one`
//
// A `notify_one` unblocks *one* currently blocked thread. We want the scheduler to be able to
// choose which thread that is, and so we implement `notify_one` by marking all waiters as runnable.
// Whichever waiter wins the race by running first will mark all other waiters as unrunnable again,
//
// This gets a little hairy if there are racing `wait`ers and `notify_one`s. The scenario we're
// concerned about is this:
//
//          Thread 1       | Thread 2       | Thread 3       | Thread 4       | Thread 5
//          ---------------|----------------|----------------|----------------| ----------------
//     (1)   wait()        |                |                |                |
//     (2)                 |  wait()        |                |                |
//     (3)                 |                |  notify_one()  |                |
//     (4)                 |                |                |  wait()        |
//     (5)                 |                |                |                |  wait()
//     (6)                 |                |  notify_one()  |                |
//     (7)                 |                |                |  wake          |
//
// Here, after (6), all 4 waiter threads are runnable. Thread 4 wins the race to run first at (7),
// and so is chosen as the unblocked thread. After (7), Thread 5 needs to be made unrunnable,
// because the only signal it can see was (6), which has already unblocked Thread 4. However,
// Threads 1 and 2 need to remain runnable, because they are eligible to be unblocked by (3), which
// has not yet been consumed.
//
// We solve this problem with "epochs". Each `notify_one` is associated with a unique epoch, and
// each waiter in the `CondvarState` remembers a list of the epochs that occurred while it was
// blocked. When a waiter runs after being made runnable by a `notify_one`, it checks to see which
// epoch that notify was associated with, and removes that epoch from the lists of every other
// waiter. Any waiter that still has a non-empty list of epochs should remain runnable, because
// there are still signals it's eligible to receive. Any waiter with an empty list of epochs is made
// unrunnable, because all the signals it was present for have been consumed.
//
// In the scenario above, there are two epochs (3) and (6). At (7), Threads 1 and 2 have the same
// epoch list [0, 1], and Threads 4 and 5 have the same epoch list [1]. When Thread 4 wins the race,
// it sees that it was woken by epoch 1, and so removes that epoch from all other waiter lists.
// Thread 1 and 2 still have epoch 0 in their list, so they remain runnable. Thread 5 has no more
// epochs in its list, so it is made unrunnable. Whichever of Threads 1 and 2 wins the subsequent
// race (not shown) will observe that it was woken by epoch 0, remove epoch 0 from whichever of the
// two threads lost the race, and then make that thread unrunnable again because there are no
// signals remaining for it to observe.
//
// ## `notify_all`
//
// `notify_all` is a broadcast that unblocks *all* currently blocked threads. Once a `notify_all`
// occurs, all the state discussed above is irrelevant -- every thread should be unblocked, so we
// don't need to remember whether there is also a `notify_one` that could have unblocked them.
// Waiters that arrive after the `notify_all` will be blocked as usual.
//
// `notify_all` atomically unblocks all currently blocked threads. For example, consider:
//
//          Thread 1       | Thread 2       | Thread 3       | Thread 4
//          ---------------|----------------|----------------|----------------
//     (1)   wait()        |                |                |
//     (2)                 |  wait()        |                |
//     (3)                 |                |  notify_all()  |
//     (4)                 |                |                |  wait()
//     (5)                 |                |  notify_one()  |
//
// After (3), Threads 1 and 2 are considered unblocked, even though the scheduler has not yet run
// them again, Thread 4 becomes blocked at (4). At (5), the only blocked thread is Thread 4, because
// the others were unblocked by (3), even though they have not yet woken up to discover this fact.
// In other words, this execution cannot deadlock -- if (4) happens-before (5), then Thread 4 is
// guaranteed to be the thread unblocked by (5). After (5), Threads 1, 2, and 4 are all runnable,
// and can run in any order (because they are all contending on the same mutex).
impl Condvar {
    /// Creates a new condition variable which is ready to be waited on and notified.
    pub fn new() -> Self {
        let state = CondvarState {
            waiters: HashMap::new(),
            next_epoch: 0,
        };

        Self {
            state: Rc::new(RefCell::new(state)),
        }
    }

    /// Blocks the current thread until this condition variable receives a notification.
    pub fn wait<'a, T>(&self, guard: MutexGuard<'a, T>) -> LockResult<MutexGuard<'a, T>> {
        let me = ExecutionState::me();

        let mut state = self.state.borrow_mut();

        trace!(waiters=?state.waiters, next_epoch=state.next_epoch, "waiting on condvar {:p}", self);

        assert!(state.waiters.insert(me, CondvarWaitStatus::Waiting).is_none());
        ExecutionState::with(|s| s.current_mut().block());
        drop(state);

        // Release the lock, which triggers a context switch now that we are blocked
        let mutex = guard.unlock();

        // After the context switch, consume whichever signal that woke this thread
        let mut state = self.state.borrow_mut();
        trace!(waiters=?state.waiters, next_epoch=state.next_epoch, "woken from condvar {:p}", self);
        let my_status = state.waiters.remove(&me).expect("should be waiting");
        match my_status {
            CondvarWaitStatus::Broadcast(clock) => {
                // Woken by a broadcast, so nothing to do except update the clock
                ExecutionState::with(|s| s.update_clock(&clock));
            }
            CondvarWaitStatus::Signal(mut epochs) => {
                let (epoch, clock) = epochs.pop_front().expect("should be a pending signal");
                // No other waiter is allowed to be unblocked by the epoch that woke us
                for (tid, status) in state.waiters.iter_mut() {
                    if let CondvarWaitStatus::Signal(epochs) = status {
                        if let Some(i) = epochs.iter().position(|e| epoch == e.0) {
                            epochs.remove(i);
                            if epochs.is_empty() {
                                *status = CondvarWaitStatus::Waiting;
                                // Make the task unrunnable if there are no pending signals that
                                // could unblock it
                                ExecutionState::with(|s| s.get_mut(*tid).block());
                            }
                        }
                    }
                }
                // Update the thread's clock with the clock from the notifier
                ExecutionState::with(|s| s.update_clock(&clock));
            }
            CondvarWaitStatus::Waiting => panic!("should not have been woken while in Waiting status"),
        }
        drop(state);

        // Reacquire the lock
        // TODO The context switch involved here might be redundant? The scheduler implicitly chose
        // TODO this thread to win the lock when it ran us after the context switch above.
        mutex.lock()
    }

    /// Waits on this condition variable for a notification, timing out after a specified duration.
    pub fn wait_timeout<'a, T>(
        &self,
        guard: MutexGuard<'a, T>,
        _dur: Duration,
    ) -> LockResult<(MutexGuard<'a, T>, WaitTimeoutResult)> {
        // TODO support the timeout case -- this method never times out
        self.wait(guard)
            .map(|guard| (guard, WaitTimeoutResult(false)))
            .map_err(|e| PoisonError::new((e.into_inner(), WaitTimeoutResult(false))))
    }

    /// Wakes up one blocked thread on this condvar.
    ///
    /// If there is a blocked thread on this condition variable, then it will be woken up from its
    /// call to wait or wait_timeout. Calls to notify_one are not buffered in any way.
    pub fn notify_one(&self) {
        let me = ExecutionState::me();

        let mut state = self.state.borrow_mut();

        trace!(waiters=?state.waiters, next_epoch=state.next_epoch, "notifying one on condvar {:p}", self);

        let epoch = state.next_epoch;
        for (tid, status) in state.waiters.iter_mut() {
            assert_ne!(*tid, me);

            let clock = my_clock();
            match status {
                CondvarWaitStatus::Waiting => {
                    let mut epochs = VecDeque::new();
                    epochs.push_back((epoch, clock));
                    *status = CondvarWaitStatus::Signal(epochs);
                }
                CondvarWaitStatus::Signal(epochs) => {
                    epochs.push_back((epoch, clock));
                }
                CondvarWaitStatus::Broadcast(_) => {
                    // no-op, broadcast will already unblock this task
                }
            }

            // Note: the task might have been unblocked by a previous signal
            ExecutionState::with(|s| s.get_mut(*tid).unblock());
        }
        state.next_epoch += 1;

        drop(state);

        thread::switch();
    }

    /// Wakes up all blocked threads on this condvar.
    pub fn notify_all(&self) {
        let me = ExecutionState::me();

        let mut state = self.state.borrow_mut();

        trace!(waiters=?state.waiters, next_epoch=state.next_epoch, "notifying all on condvar {:p}", self);

        for (tid, status) in state.waiters.iter_mut() {
            assert_ne!(*tid, me);
            *status = CondvarWaitStatus::Broadcast(my_clock());
            // Note: the task might have been unblocked by a previous signal
            ExecutionState::with(|s| s.get_mut(*tid).unblock());
        }

        drop(state);

        thread::switch();
    }
}

// Safety: Condvar is never actually passed across true threads, only across continuations. The
// Rc<RefCell<_>> type therefore can't be preempted mid-bookkeeping-operation.
// TODO we shouldn't need to do this, but RefCell is not Send
unsafe impl Send for Condvar {}
unsafe impl Sync for Condvar {}

impl Default for Condvar {
    fn default() -> Self {
        Self::new()
    }
}

/// A type indicating whether a timed wait on a condition variable returned due to a time out or not.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct WaitTimeoutResult(bool);

impl WaitTimeoutResult {
    /// Returns `true` if the wait was known to have timed out.
    pub fn timed_out(&self) -> bool {
        self.0
    }
}
