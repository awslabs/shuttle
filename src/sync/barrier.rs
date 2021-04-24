use crate::runtime::execution::ExecutionState;
use crate::runtime::task::clock::VectorClock;
use crate::runtime::task::TaskId;
use crate::runtime::thread;
use std::cell::RefCell;
use std::collections::HashSet;
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

#[derive(Debug)]
struct BarrierState {
    bound: usize,
    leader: Option<TaskId>,
    waiters: HashSet<TaskId>,
    clock: VectorClock,
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
            leader: None,
            waiters: HashSet::new(),
            clock: VectorClock::new(),
        };

        Self {
            state: Rc::new(RefCell::new(state)),
        }
    }

    /// Blocks the current thread until all threads have rendezvoused here.
    pub fn wait(&self) -> BarrierWaitResult {
        let me = ExecutionState::me();

        let mut state = self.state.borrow_mut();
        trace!(leader=?state.leader, waiters=?state.waiters, "waiting on barrier {:p}", self);

        // TODO The documentation for `Barrier` states that
        // TODO    A single (arbitrary) thread will receive a `BarrierWaitResult` that returns true
        // TODO    from `BarrierWaitResult::is_leader()` when returning from this function
        // TODO Currently, the first waiter becomes the leader.  We should use Shuttle's nondeterministic
        // TODO choice to generalize this so that _any_ participant could become the leader.
        if state.leader.is_none() {
            state.leader = Some(me);
        }

        // Update the barrier's clock with the clock of this thread
        ExecutionState::with(|s| {
            let clock = s.increment_clock();
            state.clock.update(clock);
        });

        if state.waiters.len() + 1 < state.bound {
            // Block current thread
            assert!(state.waiters.insert(me)); // current thread shouldn't already be in the set
            ExecutionState::with(|s| s.current_mut().block());
            trace!(leader=?state.leader, waiters=?state.waiters, "blocked on barrier {:?}", self);
        } else {
            trace!(leader=?state.leader, waiters=?state.waiters, "releasing waiters on barrier {:?}", self);
            // Unblock each waiter, updating its clock with the barrier's clock
            let clock = state.clock.clone();
            ExecutionState::with(|s| {
                for tid in state.waiters.drain() {
                    debug_assert_ne!(tid, me);
                    let t = s.get_mut(tid);
                    debug_assert!(t.blocked());
                    t.clock.increment(tid);
                    t.clock.update(&clock);
                    t.unblock();
                }
                // Update the leader's clock
                let t = s.current_mut();
                t.clock.increment(me);
                t.clock.update(&clock);
            });
        }

        let result = BarrierWaitResult {
            is_leader: state.leader.unwrap() == me,
        };

        drop(state);

        thread::switch();

        result
    }
}

// Safety: Barrier is never actually passed across threads, only across continuations. The
// Rc<RefCell<_>> type therefore can't be preempted mid-bookkeeping-operation.
// TODO we shouldn't need to do this, but RefCell is not Send, and Barrier needs to be Send.
unsafe impl Send for Barrier {}
unsafe impl Sync for Barrier {}
