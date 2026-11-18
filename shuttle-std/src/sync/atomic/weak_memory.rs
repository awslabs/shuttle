//! Weak-memory emulation for the atomics, which [`Config::weak_memory`](shuttle_engine::Config)
//! turns on.
//!
//! This is a port of Miri's emulation: `src/concurrency/weak_memory.rs` in
//! <https://github.com/rust-lang/miri>, with the parts of `data_race.rs` that it relies on. Miri's
//! follows the operational semantics of "Dynamic Race Detection for C++11" by Lidbury and Donaldson
//! (POPL 2017), with Miri's changes for C++20. The comments that cite the standard are Miri's.
//!
//! Each atomic keeps the stores made to it in modification order, in its store buffer. A load reads
//! one of the stores that coherence and the SeqCst rules still allow, picked with the scheduler's
//! random numbers. A read-modify-write, and a compare-exchange that fails, reads the newest store.
//!
//! Happens-before comes from the vector clocks, which Shuttle's other primitives also keep. A store
//! records the clock it releases: the task's clock for a Release store, or the task's clock at its
//! last Release fence for a Relaxed one. An Acquire load joins that clock into the task's clock. A
//! Relaxed load keeps it for the task's next Acquire fence. A read-modify-write joins the clock of
//! the store it read into the clock it releases, which continues a release sequence.
//!
//! SeqCst stores, loads and fences follow Miri's rules. SeqCst fences synchronize with each other
//! through one global clock, which rules out some outcomes that C++20 allows. Miri's emulation never
//! produces an outcome that C++11 forbids. It can produce a few outcomes that C++20 forbids
//! (<https://github.com/rust-lang/miri/issues/2301>), and so can this one.
//!
//! Miri works on bytes, so it creates a store buffer at the first atomic write to a range and drops
//! it at the next non-atomic or differently sized access. Shuttle's atomics are typed objects, so
//! the only non-atomic access is through `get_mut` or `into_inner`. `get_mut` empties the store
//! buffer, because every earlier access happens before it.

use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::task::clock::VectorClock;
use shuttle_engine::runtime::task::TaskId;
use shuttle_engine::runtime::weak_memory::{TaskWeakMemoryClocks, WeakMemoryState};
use smallvec::SmallVec;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::Ordering;

/// How many stores a store buffer keeps, as in Miri. A load can't read a store older than these.
const STORE_BUFFER_LIMIT: usize = 128;

/// The stores made to one atomic.
#[derive(Debug)]
pub(super) struct StoreBuffer<T> {
    /// The execution that made these stores (see `WeakMemoryState::execution`). A static atomic
    /// outlives an execution, and the next execution must not read its stores.
    execution: u64,

    /// The stores in modification order, oldest first. It is empty until the atomic's first access
    /// in an execution, which adds the value the atomic has then. The newest store's value is also
    /// the atomic's value.
    stores: VecDeque<StoreElement<T>>,
}

#[derive(Debug)]
struct StoreElement<T> {
    /// The task that made the store.
    task: TaskId,

    /// The storing task's own entry in its clock, just after the store. The value the atomic has at
    /// its first access counts as a store by task 0 at time 0, which happens before every access.
    timestamp: u32,

    /// The clock that a load of this store acquires.
    sync_clock: VectorClock,

    /// Whether this store is SeqCst, or happens before a SeqCst store, or comes before one in
    /// modification order because a load of it happens before the SeqCst store. A SeqCst load
    /// ignores all but the newest store marked SeqCst.
    is_seqcst: bool,

    val: T,

    /// Each task's first load of this store, with the loading task's own clock entry at that load.
    /// A later load by the same task happens after the first, so the first is enough.
    loads: SmallVec<[(TaskId, u32); 4]>,

    /// Whether a SeqCst load has read this store.
    sc_loaded: bool,
}

/// Whether the current execution emulates weak memory.
pub(super) fn enabled() -> bool {
    ExecutionState::with(|state| state.config.weak_memory)
}

impl<T> StoreBuffer<T> {
    pub(super) const fn new() -> Self {
        Self {
            execution: 0,
            stores: VecDeque::new(),
        }
    }

    /// Forgets the stores. The next access starts over from the atomic's value.
    pub(super) fn clear(&mut self) {
        self.stores.clear();
    }
}

impl<T: Copy> StoreBuffer<T> {
    /// Gets the buffer ready for an access in the given execution: forgets the stores of an earlier
    /// execution, and adds the atomic's value if there are no stores.
    fn prepare(&mut self, execution: u64, value: T) {
        if self.execution != execution {
            self.execution = execution;
            self.stores.clear();
        }
        if self.stores.is_empty() {
            self.stores.push_back(StoreElement {
                task: TaskId::from(0),
                timestamp: 0,
                // The value isn't from an atomic store, so a load of it acquires nothing.
                sync_clock: VectorClock::new(),
                is_seqcst: false,
                val: value,
                loads: SmallVec::new(),
                sc_loaded: false,
            });
        }
    }

    /// The stores that a load by a task with these clocks may read, newest first, as indices into
    /// `stores`. This is Miri's `fetch_store`.
    fn readable(&self, is_seqcst: bool, clock: &VectorClock, clocks: &TaskWeakMemoryClocks) -> SmallVec<[usize; 8]> {
        let mut readable = SmallVec::new();
        let mut found_sc = false;
        for (i, store) in self.stores.iter().enumerate().rev() {
            if is_seqcst && store.is_seqcst {
                // An SC load needs to ignore all but the last store marked SC (stores not marked
                // SC are not affected).
                if !found_sc {
                    readable.push(i);
                }
                found_sc = true;
            } else {
                readable.push(i);
            }
            if store.is_oldest_readable(is_seqcst, clock, clocks) {
                break;
            }
        }
        readable
    }

    /// Adds a store, made by a task with the given clock.
    fn push(&mut self, store: StoreElement<T>, clock: &VectorClock) {
        let is_seqcst = store.is_seqcst;
        if self.stores.len() >= STORE_BUFFER_LIMIT {
            self.stores.pop_front();
        }
        self.stores.push_back(store);
        if is_seqcst {
            // Every store that happens-before or is coherence-ordered before the ongoing SC store
            // needs to be marked as SC, so that in a later SC load, only the latest SC-marked store
            // or unmarked stores can be picked.
            for store in self.stores.iter_mut() {
                if happens_before(store.task, store.timestamp, clock)
                    || store.loads.iter().any(|&(task, at)| happens_before(task, at, clock))
                {
                    store.is_seqcst = true;
                }
            }
        }
    }

    /// The newest store, which read-modify-writes and failed compare-exchanges read.
    fn newest(&mut self) -> &mut StoreElement<T> {
        self.stores.back_mut().expect("store buffer cannot be empty")
    }
}

impl<T> StoreElement<T> {
    /// Whether a load by a task with these clocks can't read anything older than this store.
    fn is_oldest_readable(&self, is_seqcst: bool, clock: &VectorClock, clocks: &TaskWeakMemoryClocks) -> bool {
        // CoWR: if a store happens-before the current load, then we can't read-from anything
        // earlier in modification order. C++20 §6.9.2.2 [intro.races] paragraph 18
        happens_before(self.task, self.timestamp, clock)
            // CoRR: if there was a load from this store which happened-before the current load,
            // then we cannot read-from anything earlier in modification order.
            // C++20 §6.9.2.2 [intro.races] paragraph 16
            || self.loads.iter().any(|&(task, at)| happens_before(task, at, clock))
            // The current non-SC load, which may be sequenced-after an SC fence, cannot read-before
            // the last SC store executed before the fence. C++17 §32.4 [atomics.order] paragraph 4
            || (self.is_seqcst && happens_before(self.task, self.timestamp, &clocks.write_seqcst))
            // The current SC load cannot read-from any but the last store sequenced-before the last
            // SC fence. C++17 §32.4 [atomics.order] paragraph 5
            || (is_seqcst && happens_before(self.task, self.timestamp, &clocks.read_seqcst))
            // The current SC load cannot read-before a store that an earlier SC load has observed.
            // See https://github.com/rust-lang/miri/issues/2301#issuecomment-1222720427.
            // Consequences of C++20 §31.4 [atomics.order] paragraph 3.1, 3.3 (coherence-ordered
            // before) and 4.1 (coherence-ordered before between SC makes global total order S).
            || (is_seqcst && self.sc_loaded)
    }

    /// Records a load of this store by `task` at its own clock entry `at`.
    fn record_load(&mut self, task: TaskId, at: u32, is_seqcst: bool) {
        self.sc_loaded |= is_seqcst;
        if !self.loads.iter().any(|&(t, _)| t == task) {
            self.loads.push((task, at));
        }
    }
}

/// A task's entry in a clock, which is 0 if the clock has none for it.
fn entry(clock: &VectorClock, task: TaskId) -> u32 {
    let time: &[u32] = clock;
    time.get(usize::from(task)).copied().unwrap_or(0)
}

/// Whether the event at time `at` in `task` happens before a task with this clock.
fn happens_before(task: TaskId, at: u32, clock: &VectorClock) -> bool {
    at <= entry(clock, task)
}

fn is_acquire(order: Ordering) -> bool {
    matches!(order, Ordering::Acquire | Ordering::AcqRel | Ordering::SeqCst)
}

fn is_release(order: Ordering) -> bool {
    matches!(order, Ordering::Release | Ordering::AcqRel | Ordering::SeqCst)
}

/// What a load does with the clock that the store it read released. An Acquire load joins it into
/// the task's clock. A Relaxed load keeps it for the task's next Acquire fence.
fn acquire(order: Ordering, sync_clock: &VectorClock, clock: &mut VectorClock, clocks: &mut TaskWeakMemoryClocks) {
    if is_acquire(order) {
        clock.update(sync_clock);
    } else {
        clocks.fence_acquire.update(sync_clock);
    }
}

/// Records `at` as the time of `task`'s last SeqCst store. This is the SC ATOMIC STORE rule of the
/// paper. A task's times only grow, so joining a clock that has only this entry sets it.
fn record_sc_write(state: &mut WeakMemoryState, task: TaskId, at: u32) {
    let mut time = vec![0; usize::from(task) + 1];
    time[usize::from(task)] = at;
    state.last_sc_write.update(&VectorClock::from(&time[..]));
}

/// Runs `f` with the current task, its clock and the execution's weak-memory state.
fn with<R>(f: impl FnOnce(TaskId, &mut VectorClock, &mut WeakMemoryState) -> R) -> R {
    ExecutionState::with(|state| {
        let (task, clock, weak_memory) = state.weak_memory();
        f(task, clock, weak_memory)
    })
}

/// An atomic load. `value` is the atomic's value, which is the newest store's.
pub(super) fn load<T: Copy>(buffer: &RefCell<StoreBuffer<T>>, value: &RefCell<T>, order: Ordering) -> T {
    let is_seqcst = order == Ordering::SeqCst;
    let readable = with(|me, clock, state| {
        let clocks = state.tasks.get(me);
        if is_seqcst {
            // The SC ATOMIC READ rule of the paper.
            clocks.read_seqcst.update(&state.last_sc_fence);
        }
        let mut buffer = buffer.borrow_mut();
        buffer.prepare(state.execution, *value.borrow());
        buffer.readable(is_seqcst, clock, clocks)
    });
    // `next_u64` needs the execution state itself, so the store is picked between the two calls of
    // `with`. Nothing else runs in between.
    let pick = match readable.len() {
        1 => readable[0],
        n => readable[(ExecutionState::next_u64() % n as u64) as usize],
    };
    with(|me, clock, state| {
        let clocks = state.tasks.get(me);
        let mut buffer = buffer.borrow_mut();
        let store = &mut buffer.stores[pick];
        clock.increment(me);
        acquire(order, &store.sync_clock, clock, clocks);
        store.record_load(me, entry(clock, me), is_seqcst);
        store.val
    })
}

// Each operation below starts by ticking the task's own entry in its clock, as Shuttle's other
// primitives do, and a store releases the clock after that tick. So a task that acquires a store
// sees the store itself in its clock, as `current::clock()` users expect. Miri releases the clock
// from before the tick. Either is sound: a load that reads the store also records itself on it,
// which stops the reading task from going back to older stores.

/// An atomic store.
pub(super) fn store<T: Copy>(buffer: &RefCell<StoreBuffer<T>>, value: &RefCell<T>, val: T, order: Ordering) {
    let is_seqcst = order == Ordering::SeqCst;
    with(|me, clock, state| {
        clock.increment(me);
        let timestamp = entry(clock, me);
        let clocks = state.tasks.get(me);
        let mut buffer = buffer.borrow_mut();
        buffer.prepare(state.execution, *value.borrow());
        // A Release store releases the task's clock. A Relaxed one releases what the task's last
        // Release fence did. Either way it ends the release sequence of the store before it: C++20
        // took out the exception for relaxed stores by the same thread.
        let sync_clock = if is_release(order) {
            clock.clone()
        } else {
            clocks.fence_release.clone()
        };
        if is_seqcst {
            record_sc_write(state, me, timestamp);
        }
        let store = StoreElement {
            task: me,
            timestamp,
            sync_clock,
            is_seqcst,
            val,
            loads: SmallVec::new(),
            sc_loaded: false,
        };
        buffer.push(store, clock);
        *value.borrow_mut() = val;
    })
}

/// An atomic read-modify-write. It reads the newest store and calls `f` on its value. If `f`
/// returns a new value, this stores it with the `success` ordering and returns `Ok` with the old
/// value. Otherwise it was only a load with the `failure` ordering, and this returns `Err`.
pub(super) fn rmw<T: Copy>(
    buffer: &RefCell<StoreBuffer<T>>,
    value: &RefCell<T>,
    success: Ordering,
    failure: Ordering,
    f: impl FnOnce(T) -> Option<T>,
) -> Result<T, T> {
    with(|me, clock, state| {
        clock.increment(me);
        let timestamp = entry(clock, me);
        let clocks = state.tasks.get(me);
        let mut buffer = buffer.borrow_mut();
        buffer.prepare(state.execution, *value.borrow());
        let read = buffer.newest();
        let old = read.val;
        match f(old) {
            Some(new) => {
                let is_seqcst = success == Ordering::SeqCst;
                acquire(success, &read.sync_clock, clock, clocks);
                // A read-modify-write continues the release sequence of the store it read, so a
                // load of it acquires what that store released, too.
                let mut sync_clock = read.sync_clock.clone();
                sync_clock.update(if is_release(success) {
                    &*clock
                } else {
                    &clocks.fence_release
                });
                read.record_load(me, timestamp, is_seqcst);
                if is_seqcst {
                    clocks.read_seqcst.update(&state.last_sc_fence);
                    record_sc_write(state, me, timestamp);
                }
                let store = StoreElement {
                    task: me,
                    timestamp,
                    sync_clock,
                    is_seqcst,
                    val: new,
                    loads: SmallVec::new(),
                    sc_loaded: false,
                };
                buffer.push(store, clock);
                *value.borrow_mut() = new;
                Ok(old)
            }
            None => {
                // A failed compare-exchange is a load of the newest store, with the failure
                // ordering.
                let is_seqcst = failure == Ordering::SeqCst;
                if is_seqcst {
                    clocks.read_seqcst.update(&state.last_sc_fence);
                }
                acquire(failure, &read.sync_clock, clock, clocks);
                read.record_load(me, timestamp, is_seqcst);
                Err(old)
            }
        }
    })
}

/// An atomic fence. `order` isn't `Relaxed`.
pub(super) fn fence(order: Ordering) {
    with(|me, clock, state| {
        // Only a fence that releases needs a tick of its own, which is what its task's later
        // Relaxed stores release.
        if is_release(order) {
            clock.increment(me);
        }
        let clocks = state.tasks.get(me);
        if is_acquire(order) {
            clock.update(&clocks.fence_acquire);
        }
        if order == Ordering::SeqCst {
            // Behave like an RMW on the global fence location. This takes full care of all the SC
            // fence requirements, including C++17 §32.4 [atomics.order] paragraph 6 (which would
            // limit what future reads can see). It also rules out many legal behaviors, but we
            // don't currently have a model that would be more precise.
            state.last_sc_fence.update(clock);
            clock.update(&state.last_sc_fence);
            // Also establish some sort of order with the last SC write that happened, globally
            // (but this is only respected by future reads).
            clocks.write_seqcst.update(&state.last_sc_write);
        }
        // The release fence is last, since both of the above could alter our clock, which should
        // be part of what is being released.
        if is_release(order) {
            clocks.fence_release = clock.clone();
        }
    })
}
