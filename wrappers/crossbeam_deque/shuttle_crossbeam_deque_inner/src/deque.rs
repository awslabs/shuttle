use crate::Steal;
use crate::per_execution::{PerExecution, synchronize};
use shuttle_engine::runtime::task::clock::VectorClock;
use shuttle_engine::runtime::thread::switch;
use std::collections::VecDeque;
use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

/// The capacity that crossbeam gives a worker's buffer, which it never shrinks below.
const MIN_CAP: usize = 64;

/// The most tasks that crossbeam's batch steals take by default.
const MAX_BATCH: usize = 32;

/// How many tasks a block of crossbeam's injector holds.
const BLOCK_CAP: u64 = 63;

/// Worker queue flavor: FIFO or LIFO.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flavor {
    /// The first-in first-out flavor.
    Fifo,
    /// The last-in first-out flavor.
    Lifo,
}

/// The deque that a [`Worker`] shares with its [`Stealer`]s.
struct Shared<T> {
    flavor: Flavor,
    state: PerExecution<DequeState<T>>,
}

/// A worker's deque in one execution.
struct DequeState<T> {
    /// The tasks, from the end stealers take from (and a FIFO worker pops from) to the end the
    /// worker pushes to (and a LIFO worker pops from), as in crossbeam's buffer.
    tasks: VecDeque<T>,
    /// How many tasks have left through the front: crossbeam's front index, which a steal moves with
    /// a compare-and-swap that fails if another operation moved it first.
    front: u64,
    /// Bumped whenever crossbeam's worker would swap its buffer for one of another size, which makes
    /// a steal in progress retry.
    generation: u64,
    /// The capacity of crossbeam's buffer.
    cap: usize,
    clock: VectorClock,
}

impl<T> DequeState<T> {
    fn new() -> Self {
        Self {
            tasks: VecDeque::new(),
            front: 0,
            generation: 0,
            cap: MIN_CAP,
            clock: VectorClock::new(),
        }
    }

    /// What a steal in progress compares against when it commits.
    fn position(&self) -> (u64, u64) {
        (self.front, self.generation)
    }

    fn resize(&mut self, cap: usize) {
        self.cap = cap;
        self.generation += 1;
    }

    /// Makes room for `n` more tasks, as crossbeam's `Worker::reserve` does before a batch steal
    /// copies into the worker.
    fn reserve(&mut self, n: usize) {
        if n > 0 {
            let len = self.tasks.len();
            if self.cap - len < n {
                self.resize((n + len).next_power_of_two());
            }
        }
    }

    /// Adds stolen tasks at the back, in the order crossbeam's batch steals write them there.
    fn append(&mut self, tasks: Vec<T>, reversed: bool) {
        debug_assert!(self.tasks.len() + tasks.len() <= self.cap, "reserved beforehand");
        if reversed {
            self.tasks.extend(tasks.into_iter().rev());
        } else {
            self.tasks.extend(tasks);
        }
    }
}

impl<T> Shared<T> {
    fn new(flavor: Flavor) -> Self {
        Self {
            flavor,
            state: PerExecution::new(),
        }
    }

    fn with_state<R>(&self, f: impl FnOnce(&mut DequeState<T>) -> R) -> R {
        let mut state = self.state.get(DequeState::new).borrow_mut();
        synchronize(&mut state.clock);
        f(&mut state)
    }
}

/// A worker queue, with the API of [`crossbeam_deque::Worker`].
///
/// Owned by one thread, which pushes and pops; other threads steal through [`Stealer`]s. See the
/// [crate documentation](crate) for where the scheduling points are.
///
/// [`crossbeam_deque::Worker`]: https://docs.rs/crossbeam-deque/0.8/crossbeam_deque/struct.Worker.html
pub struct Worker<T> {
    shared: Arc<Shared<T>>,
    /// Indicates that the worker cannot be shared among threads.
    _marker: PhantomData<*mut ()>,
}

// Safety: as crossbeam's. The worker moves tasks between threads only, and is not `Sync`.
unsafe impl<T: Send> Send for Worker<T> {}

impl<T> Worker<T> {
    /// Creates a FIFO worker queue: tasks are pushed and popped from opposite ends.
    pub fn new_fifo() -> Worker<T> {
        Self::new(Flavor::Fifo)
    }

    /// Creates a LIFO worker queue: tasks are pushed and popped from the same end.
    pub fn new_lifo() -> Worker<T> {
        Self::new(Flavor::Lifo)
    }

    fn new(flavor: Flavor) -> Worker<T> {
        Worker {
            shared: Arc::new(Shared::new(flavor)),
            _marker: PhantomData,
        }
    }

    /// Creates a stealer for this queue.
    ///
    /// The returned stealer can be shared among threads and cloned.
    pub fn stealer(&self) -> Stealer<T> {
        Stealer {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Returns `true` if the queue is empty.
    pub fn is_empty(&self) -> bool {
        switch();
        self.shared.with_state(|state| state.tasks.is_empty())
    }

    /// Returns the number of tasks in the deque.
    pub fn len(&self) -> usize {
        switch();
        self.shared.with_state(|state| state.tasks.len())
    }

    /// Pushes a task into the queue.
    pub fn push(&self, task: T) {
        switch();
        self.shared.with_state(|state| {
            if state.tasks.len() >= state.cap {
                let cap = 2 * state.cap;
                state.resize(cap);
            }
            state.tasks.push_back(task);
        });
    }

    /// Pops a task from the queue.
    pub fn pop(&self) -> Option<T> {
        switch();
        let flavor = self.shared.flavor;
        self.shared.with_state(|state| {
            let len = state.tasks.len();
            if len == 0 {
                return None;
            }
            match flavor {
                Flavor::Fifo => {
                    // crossbeam's FIFO worker takes the task by moving the front index.
                    let task = state.tasks.pop_front();
                    state.front += 1;
                    if state.cap > MIN_CAP && len <= state.cap / 4 {
                        let cap = state.cap / 2;
                        state.resize(cap);
                    }
                    task
                }
                Flavor::Lifo => {
                    let task = state.tasks.pop_back();
                    let left = len - 1;
                    if left == 0 {
                        // The last task: crossbeam's LIFO worker races the stealers for it by
                        // moving the front index too.
                        state.front += 1;
                    } else if state.cap > MIN_CAP && left < state.cap / 4 {
                        let cap = state.cap / 2;
                        state.resize(cap);
                    }
                    task
                }
            }
        })
    }
}

impl<T> fmt::Debug for Worker<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Worker { .. }")
    }
}

/// A stealer handle of a worker queue, with the API of [`crossbeam_deque::Stealer`].
///
/// Stealers can be shared among threads. See the [crate documentation](crate) for where the
/// scheduling points are and when a steal returns [`Steal::Retry`].
///
/// [`crossbeam_deque::Stealer`]: https://docs.rs/crossbeam-deque/0.8/crossbeam_deque/struct.Stealer.html
pub struct Stealer<T> {
    shared: Arc<Shared<T>>,
}

// Safety: as crossbeam's. A stealer moves tasks between threads, and never hands out a reference
// to one.
unsafe impl<T: Send> Send for Stealer<T> {}
unsafe impl<T: Send> Sync for Stealer<T> {}

impl<T> Stealer<T> {
    /// Returns `true` if the queue is empty.
    pub fn is_empty(&self) -> bool {
        switch();
        self.shared.with_state(|state| state.tasks.is_empty())
    }

    /// Returns the number of tasks in the deque.
    pub fn len(&self) -> usize {
        switch();
        self.shared.with_state(|state| state.tasks.len())
    }

    /// Steals a task from the queue.
    pub fn steal(&self) -> Steal<T> {
        switch();
        let Some(seen) = self
            .shared
            .with_state(|state| (!state.tasks.is_empty()).then(|| state.position()))
        else {
            return Steal::Empty;
        };
        switch();
        self.shared.with_state(|state| {
            if state.position() != seen {
                return Steal::Retry;
            }
            state.front += 1;
            Steal::Success(
                state
                    .tasks
                    .pop_front()
                    .expect("only a move of the front empties the deque"),
            )
        })
    }

    /// Steals a batch of tasks and pushes them into another worker.
    ///
    /// As crossbeam's, it steals half of the tasks, rounded up, and no more than 32.
    pub fn steal_batch(&self, dest: &Worker<T>) -> Steal<()> {
        self.steal_batch_with_limit(dest, MAX_BATCH)
    }

    /// Steals no more than `limit` of tasks and pushes them into another worker.
    ///
    /// As crossbeam's, it steals half of the tasks, rounded up, and no more than `limit`.
    pub fn steal_batch_with_limit(&self, dest: &Worker<T>, limit: usize) -> Steal<()> {
        assert!(limit > 0);
        if Arc::ptr_eq(&self.shared, &dest.shared) {
            return if dest.is_empty() {
                Steal::Empty
            } else {
                Steal::Success(())
            };
        }

        switch();
        let Some((batch, seen)) = self.shared.with_state(|state| {
            let len = state.tasks.len();
            (len > 0).then(|| (len.div_ceil(2).min(limit), state.position()))
        }) else {
            return Steal::Empty;
        };
        dest.shared.with_state(|dest| dest.reserve(batch));
        // Stolen tasks land in the destination in the order crossbeam writes them there, which is
        // reversed when the two queues' flavors differ.
        let reversed = self.shared.flavor != dest.shared.flavor;

        let stolen = match self.shared.flavor {
            // crossbeam takes a FIFO queue's batch with one compare-and-swap of the front index.
            Flavor::Fifo => {
                switch();
                let stolen = self.shared.with_state(|state| {
                    if state.position() != seen {
                        return None;
                    }
                    state.front += batch as u64;
                    Some(state.tasks.drain(..batch).collect::<Vec<_>>())
                });
                match stolen {
                    Some(stolen) => stolen,
                    None => return Steal::Retry,
                }
            }
            // From a LIFO queue, crossbeam steals the tasks one at a time, and stops early when the
            // queue runs out or another operation gets in the way.
            Flavor::Lifo => {
                let stolen = self.steal_one_by_one(seen, batch, Vec::new());
                if stolen.is_empty() {
                    return Steal::Retry;
                }
                stolen
            }
        };
        dest.shared.with_state(|dest| dest.append(stolen, reversed));
        Steal::Success(())
    }

    /// Steals a batch of tasks, pushes them into another worker, and pops a task from that worker.
    ///
    /// As crossbeam's, it steals half of the tasks, rounded up, and no more than 32.
    pub fn steal_batch_and_pop(&self, dest: &Worker<T>) -> Steal<T> {
        self.steal_batch_with_limit_and_pop(dest, MAX_BATCH)
    }

    /// Steals no more than `limit` of tasks, pushes them into another worker, and pops a task from
    /// that worker.
    ///
    /// As crossbeam's, it steals half of the tasks, rounded up, and no more than `limit`.
    pub fn steal_batch_with_limit_and_pop(&self, dest: &Worker<T>, limit: usize) -> Steal<T> {
        assert!(limit > 0);
        if Arc::ptr_eq(&self.shared, &dest.shared) {
            return match dest.pop() {
                None => Steal::Empty,
                Some(task) => Steal::Success(task),
            };
        }

        switch();
        let Some((batch, seen)) = self.shared.with_state(|state| {
            let len = state.tasks.len();
            (len > 0).then(|| (((len - 1) / 2).min(limit - 1), state.position()))
        }) else {
            return Steal::Empty;
        };
        dest.shared.with_state(|dest| dest.reserve(batch));

        let (task, rest, reversed) = match self.shared.flavor {
            Flavor::Fifo => {
                switch();
                let stolen = self.shared.with_state(|state| {
                    if state.position() != seen {
                        return None;
                    }
                    state.front += batch as u64 + 1;
                    let task = state
                        .tasks
                        .pop_front()
                        .expect("only a move of the front empties the deque");
                    Some((task, state.tasks.drain(..batch).collect::<Vec<_>>()))
                });
                match stolen {
                    Some((task, rest)) => (task, rest, dest.shared.flavor == Flavor::Lifo),
                    None => return Steal::Retry,
                }
            }
            Flavor::Lifo => {
                // crossbeam takes the first task with a compare-and-swap of the front index that does
                // not check for a new buffer, and then the rest one at a time, keeping the latest
                // for itself.
                switch();
                let first = self.shared.with_state(|state| {
                    if state.front != seen.0 {
                        return None;
                    }
                    state.front += 1;
                    Some(
                        state
                            .tasks
                            .pop_front()
                            .expect("only a move of the front empties the deque"),
                    )
                });
                let Some(first) = first else {
                    return Steal::Retry;
                };
                let mut stolen = self.steal_one_by_one((seen.0 + 1, seen.1), batch, vec![first]);
                let task = stolen.pop().expect("holds the first task at least");
                (task, stolen, dest.shared.flavor == Flavor::Fifo)
            }
        };
        dest.shared.with_state(|dest| dest.append(rest, reversed));
        Steal::Success(task)
    }

    /// Steals up to `count` tasks, one at a time, each at a scheduling point of its own, as crossbeam
    /// steals from a LIFO queue, and adds them to `stolen`. Stops at the first task that another
    /// operation took first, or when crossbeam's buffer has changed since `seen`.
    fn steal_one_by_one(&self, seen: (u64, u64), count: usize, mut stolen: Vec<T>) -> Vec<T> {
        let mut expected = seen;
        for _ in 0..count {
            switch();
            let task = self.shared.with_state(|state| {
                if state.tasks.is_empty() || state.position() != expected {
                    return None;
                }
                state.front += 1;
                expected.0 += 1;
                state.tasks.pop_front()
            });
            match task {
                Some(task) => stolen.push(task),
                None => break,
            }
        }
        stolen
    }
}

impl<T> Clone for Stealer<T> {
    fn clone(&self) -> Stealer<T> {
        Stealer {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T> fmt::Debug for Stealer<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Stealer { .. }")
    }
}

/// An injector queue, with the API of [`crossbeam_deque::Injector`].
///
/// A FIFO queue that can be shared among multiple threads. See the [crate documentation](crate) for
/// where the scheduling points are and when a steal returns [`Steal::Retry`].
///
/// [`crossbeam_deque::Injector`]: https://docs.rs/crossbeam-deque/0.8/crossbeam_deque/struct.Injector.html
pub struct Injector<T> {
    state: PerExecution<InjectorState<T>>,
}

/// An injector's queue in one execution.
struct InjectorState<T> {
    tasks: VecDeque<T>,
    /// How many tasks have been stolen: crossbeam's head index, which a steal moves with a
    /// compare-and-swap that fails if another steal moved it first.
    head: u64,
    clock: VectorClock,
}

impl<T> InjectorState<T> {
    fn new() -> Self {
        Self {
            tasks: VecDeque::new(),
            head: 0,
            clock: VectorClock::new(),
        }
    }

    /// How many tasks crossbeam's injector takes in a batch of at most `limit`: up to the end of the
    /// head's block of 63 if the tail is in a later block, and half of the tasks, rounded up,
    /// otherwise.
    fn advance(&self, limit: usize) -> usize {
        let len = self.tasks.len() as u64;
        let tail = self.head + len;
        if self.head / BLOCK_CAP != tail / BLOCK_CAP {
            ((BLOCK_CAP - self.head % BLOCK_CAP) as usize).min(limit)
        } else {
            (len as usize).div_ceil(2).min(limit)
        }
    }
}

// Safety: as crossbeam's. The injector moves tasks between threads, and never hands out a reference
// to one.
unsafe impl<T: Send> Send for Injector<T> {}
unsafe impl<T: Send> Sync for Injector<T> {}

impl<T> Default for Injector<T> {
    fn default() -> Self {
        Self {
            state: PerExecution::new(),
        }
    }
}

impl<T> Injector<T> {
    /// Creates a new injector queue.
    pub fn new() -> Injector<T> {
        Self::default()
    }

    fn with_state<R>(&self, f: impl FnOnce(&mut InjectorState<T>) -> R) -> R {
        let mut state = self.state.get(InjectorState::new).borrow_mut();
        synchronize(&mut state.clock);
        f(&mut state)
    }

    /// Pushes a task into the queue.
    pub fn push(&self, task: T) {
        switch();
        self.with_state(|state| state.tasks.push_back(task));
    }

    /// Steals a task from the queue.
    pub fn steal(&self) -> Steal<T> {
        switch();
        let Some(seen) = self.with_state(|state| (!state.tasks.is_empty()).then_some(state.head)) else {
            return Steal::Empty;
        };
        switch();
        self.with_state(|state| {
            if state.head != seen {
                return Steal::Retry;
            }
            state.head += 1;
            Steal::Success(state.tasks.pop_front().expect("only a steal empties the injector"))
        })
    }

    /// Steals a batch of tasks and pushes them into a worker.
    ///
    /// As crossbeam's, it steals half of the tasks, rounded up, no more than 32, and no more than
    /// are left in the block of crossbeam's queue that the head is in.
    pub fn steal_batch(&self, dest: &Worker<T>) -> Steal<()> {
        self.steal_batch_with_limit(dest, MAX_BATCH)
    }

    /// Steals no more than `limit` of tasks and pushes them into a worker.
    ///
    /// As crossbeam's, it steals half of the tasks, rounded up, no more than `limit`, and no more
    /// than are left in the block of crossbeam's queue that the head is in.
    pub fn steal_batch_with_limit(&self, dest: &Worker<T>, limit: usize) -> Steal<()> {
        assert!(limit > 0);
        match self.steal_some(limit) {
            Steal::Success(stolen) => {
                let reversed = dest.shared.flavor == Flavor::Lifo;
                dest.shared.with_state(|dest| {
                    dest.reserve(stolen.len());
                    dest.append(stolen, reversed);
                });
                Steal::Success(())
            }
            Steal::Empty => Steal::Empty,
            Steal::Retry => Steal::Retry,
        }
    }

    /// Steals a batch of tasks, pushes them into a worker, and pops a task from that worker.
    ///
    /// As crossbeam's, it steals half of the tasks, rounded up, no more than 33, and no more than
    /// are left in the block of crossbeam's queue that the head is in.
    pub fn steal_batch_and_pop(&self, dest: &Worker<T>) -> Steal<T> {
        self.steal_batch_with_limit_and_pop(dest, MAX_BATCH + 1)
    }

    /// Steals no more than `limit` of tasks, pushes them into a worker, and pops a task from that
    /// worker.
    ///
    /// As crossbeam's, it steals half of the tasks, rounded up, no more than `limit`, and no more
    /// than are left in the block of crossbeam's queue that the head is in.
    pub fn steal_batch_with_limit_and_pop(&self, dest: &Worker<T>, limit: usize) -> Steal<T> {
        assert!(limit > 0);
        match self.steal_some(limit) {
            Steal::Success(mut stolen) => {
                let task = stolen.remove(0);
                let reversed = dest.shared.flavor == Flavor::Lifo;
                dest.shared.with_state(|dest| {
                    dest.reserve(stolen.len());
                    dest.append(stolen, reversed);
                });
                Steal::Success(task)
            }
            Steal::Empty => Steal::Empty,
            Steal::Retry => Steal::Retry,
        }
    }

    /// Takes a batch of at most `limit` tasks from the head with one compare-and-swap, as crossbeam's
    /// batch steals do.
    fn steal_some(&self, limit: usize) -> Steal<Vec<T>> {
        switch();
        let Some((advance, seen)) =
            self.with_state(|state| (!state.tasks.is_empty()).then(|| (state.advance(limit), state.head)))
        else {
            return Steal::Empty;
        };
        switch();
        self.with_state(|state| {
            if state.head != seen {
                return Steal::Retry;
            }
            state.head += advance as u64;
            Steal::Success(state.tasks.drain(..advance).collect())
        })
    }

    /// Returns `true` if the queue is empty.
    pub fn is_empty(&self) -> bool {
        switch();
        self.with_state(|state| state.tasks.is_empty())
    }

    /// Returns the number of tasks in the queue.
    pub fn len(&self) -> usize {
        switch();
        self.with_state(|state| state.tasks.len())
    }
}

impl<T> fmt::Debug for Injector<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Injector { .. }")
    }
}
