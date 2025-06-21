use crate::runtime::execution::ExecutionState;
use crate::runtime::task::clock::VectorClock;
use crate::runtime::task::TaskId;
use crate::runtime::thread;
use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt;
use std::rc::Rc;
use tracing::trace;

#[derive(Clone, Copy, Debug)]
/// A `BarrierWaitResult` is returned by `Barrier::wait()` when all threads in the `Barrier` have rendezvoused.
pub struct BarrierWaitResult {
    is_leader: bool,
}

impl BarrierWaitResult {
    /// Returns true if this thread is the "leader thread" for the call to `Barrier::wait()`.
    pub fn is_leader(&self) -> bool {
        self.is_leader
    }
}

/// We implement [Barrier] by keeping track of a list of [TaskId]s of threads that have called
/// `wait`. When the numbers of waiters gets to the barrier's `bound`, they all become unblocked.
/// Whichever task wakes up first after becoming unblocked will be designated as the "leader" via
/// the return value of `wait`.
///
/// Because barriers can be reused, it does not suffice to designate a single, permanent thread as
/// the leader in the [BarrierState], since if a different batch of threads waits on the same
/// barrier, one of those new threads should be the leader of that batch.
///
/// For example, if there's a barrier where the bound is 2 and there are 6 threads all concurrently
/// calling `wait`, then all threads will become unblocked in 3 batches of 2 threads each, with
/// each batch having its own leader, resulting in 3 (unique) leaders.
///
/// We implement this by tracking an `epoch` counter that gets incremented whenever a batch of
/// threads is released by `wait`. When that happens, we make a "leader token_" available for that
/// batch. The first thread of a batch to get scheduled after becoming unblocked takes the leader
/// token (without affecting any threads that are part of a different batch).
struct BarrierState {
    /// The number of tasks that must call `wait` before they all get unblocked (and one gets
    /// chosen as the leader).
    bound: usize,
    /// A counter of the number of "batches" of threads that have been released by the barrier,
    /// needed in order to keep track of the leaders of each batch separately.
    epoch: u64,
    /// The set of waiting tasks for the current epoch. Then the size of this set becomes equal to
    /// the bound, all waiters are unblocked and one of them becomes the leader.
    waiters: HashSet<TaskId>,
    /// The set of epochs of this [Barrier] that have reached the number of waiters to be
    /// unblocked, but haven't had a leader selected yet. The first thread to wake up after wait
    /// will take the token (by removing the epoch from the set) and become the leader.
    leader_tokens: HashSet<u64>,
    clock: VectorClock,
}

// Implement debug in order to not output the `VectorClock`
impl fmt::Debug for BarrierState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BarrierState")
            .field("bound", &self.bound)
            .field("epoch", &self.epoch)
            .field("waiters", &self.waiters)
            .field("leader_tokens", &self.leader_tokens)
            .finish()
    }
}

#[derive(Debug)]
/// A barrier enables multiple threads to synchronize the beginning of some computation.
pub struct Barrier {
    state: Rc<RefCell<BarrierState>>,
}

impl Barrier {
    /// Creates a new barrier that can block a given number of threads.
    /// A barrier will block n-1 threads which call `wait()` and then wake up all threads
    /// at once when the nth thread calls `wait()`.
    pub fn new(n: usize) -> Self {
        let state = BarrierState {
            bound: n,
            epoch: 0,
            waiters: HashSet::new(),
            leader_tokens: HashSet::new(),
            clock: VectorClock::new(),
        };

        Self {
            state: Rc::new(RefCell::new(state)),
        }
    }

    /// Blocks the current thread until all threads have rendezvoused here.
    pub fn wait(&self) -> BarrierWaitResult {
        let mut state = self.state.borrow_mut();
        let my_epoch = state.epoch;

        trace!(waiters=?state.waiters, epoch=my_epoch, "waiting on barrier {:p}", self);

        // Update the barrier's clock with the clock of this thread
        ExecutionState::with(|s| {
            let clock = s.increment_clock();
            state.clock.update(clock);
        });

        // Add the current thread to `waiters`. It shouldn't already be present.
        assert!(state.waiters.insert(ExecutionState::me()));

        if state.waiters.len() < state.bound {
            trace!(waiters=?state.waiters, epoch=my_epoch, "blocked on barrier {:?}", self);
            ExecutionState::with(|s| s.current_mut().block(false));
        } else {
            trace!(waiters=?state.waiters, epoch=my_epoch, "releasing waiters on barrier {:?}", self);

            debug_assert!(state.waiters.len() == state.bound || state.bound == 0);

            // Make the leader token available for this epoch. The first task to wake up will
            // take it and become the leader. The token shouldn't already be available.
            assert!(state.leader_tokens.insert(my_epoch));

            // Drain the set of waiters and increment the barrier's epoch, so any other task that
            // calls `wait` from now on becomes part of a separate group with its own leader.
            let waiters = state.waiters.drain().collect::<Vec<_>>();
            state.epoch += 1;

            trace!(
                waiters=?state.waiters,
                epoch=state.epoch,
                "releasing waiters on barrier {:?}",
                self,
            );

            let clock = state.clock.clone();
            ExecutionState::with(|s| {
                // `waiters` includes the current task.
                for tid in waiters {
                    let t = s.get_mut(tid);
                    t.clock.increment(tid);
                    t.clock.update(&clock);
                    t.unblock();
                }
            });
        };

        drop(state);

        thread::switch();

        // Try to remove the leader token for this epoch. If true, then the token was present and
        // we are the leader. Any future attempts to remove the token will return false.
        let is_leader = self.state.borrow_mut().leader_tokens.remove(&my_epoch);

        trace!(epoch=?my_epoch, is_leader, "returning from barrier {:?}", self);

        BarrierWaitResult { is_leader }
    }
}

// Safety: Barrier is never actually passed across threads, only across continuations. The
// Rc<RefCell<_>> type therefore can't be preempted mid-bookkeeping-operation.
// TODO we shouldn't need to do this, but RefCell is not Send, and Barrier needs to be Send.
unsafe impl Send for Barrier {}
unsafe impl Sync for Barrier {}
