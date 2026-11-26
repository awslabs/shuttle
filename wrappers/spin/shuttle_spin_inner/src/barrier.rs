//! Synchronization primitive allowing multiple threads to synchronize the beginning of some
//! computation.

use crate::state::{Core, State};
use crate::{RelaxStrategy, Spin};
use shuttle_engine::runtime::thread;
use std::cell::{Cell, UnsafeCell};
use std::marker::PhantomData;

/// A primitive that synchronizes the execution of multiple threads.
///
/// # Under Shuttle
///
/// Each [`wait`](Self::wait) takes one scheduling point before it counts its arrival, so every
/// order of arrival is reachable, and every task can be the leader, which is the last to arrive.
/// The other tasks block until the leader arrives. The count and the generation live in the
/// running execution, so a `static` barrier starts every execution with nobody waiting.
pub struct Barrier<R = Spin> {
    core: Core,
    num_threads: usize,
    // spin's barrier is a `Mutex<_, R>`, which decides its auto traits.
    phantom: PhantomData<(R, UnsafeCell<()>)>,
}

unsafe impl<R> Send for Barrier<R> {}
unsafe impl<R> Sync for Barrier<R> {}

/// A `BarrierWaitResult` is returned by [`wait`](struct.Barrier.html#method.wait) when all threads
/// in the [`Barrier`] have rendezvoused.
pub struct BarrierWaitResult(bool);

impl<R: RelaxStrategy> Barrier<R> {
    /// Blocks the current task until all tasks have rendezvoused here.
    ///
    /// Barriers are re-usable after all tasks have rendezvoused once, and can be used continuously.
    ///
    /// A single (arbitrary) task will receive a [`BarrierWaitResult`] that returns `true` from
    /// [`is_leader`](BarrierWaitResult::is_leader) when returning from this function, and all
    /// other tasks will receive a result that will return `false` from `is_leader`.
    pub fn wait(&self) -> BarrierWaitResult {
        thread::switch();
        // The word is the number of tasks that wait; `extra` is the generation.
        let (state, _) = self.core.state(|| State::new(0, Cell::new(0usize)));
        state.observe();
        let count = state.word.get() + 1;
        if count < self.num_threads {
            // not the leader
            state.word.set(count);
            state.publish();
            let local_gen = state.extra.get();
            while local_gen == state.extra.get() {
                state.wait();
            }
            state.observe();
            BarrierWaitResult(false)
        } else {
            // this task is the leader, and is responsible for incrementing the generation
            state.release(|state| {
                state.word.set(0);
                state.extra.set(state.extra.get().wrapping_add(1));
            });
            BarrierWaitResult(true)
        }
    }
}

impl<R> Barrier<R> {
    /// Creates a new barrier that can block a given number of threads.
    ///
    /// A barrier will block `n`-1 threads which call [`wait`] and then wake up all threads at once
    /// when the `n`th thread calls [`wait`]. A Barrier created with n = 0 will behave identically
    /// to one created with n = 1.
    ///
    /// [`wait`]: #method.wait
    pub const fn new(n: usize) -> Self {
        Self {
            core: Core::new(),
            num_threads: n,
            phantom: PhantomData,
        }
    }
}

impl BarrierWaitResult {
    /// Returns whether this thread from [`wait`] is the "leader thread".
    ///
    /// Only one thread will have `true` returned from their result, all other threads will have
    /// `false` returned.
    ///
    /// [`wait`]: struct.Barrier.html#method.wait
    pub fn is_leader(&self) -> bool {
        self.0
    }
}
