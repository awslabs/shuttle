use crate::per_execution::{PerExecution, synchronize};
use shuttle_engine::runtime::task::clock::VectorClock;
use shuttle_engine::runtime::thread::switch;
use std::collections::VecDeque;
use std::fmt;
use std::panic::{RefUnwindSafe, UnwindSafe};

/// An unbounded multi-producer multi-consumer queue, with the API of [`crossbeam_queue::SegQueue`].
///
/// Under Shuttle, each operation that the real queue performs with atomic operations other threads
/// can observe happens in one step at a scheduling point: [`push`](Self::push), [`pop`](Self::pop),
/// [`len`](Self::len) and [`is_empty`](Self::is_empty). So the queue is linearizable, and Shuttle
/// explores every order in which concurrent operations take effect. The methods that take
/// `&mut self`, and the `Debug` and `Drop` impls, are not scheduling points.
///
/// [`new`](Self::new) is `const`, so a `SegQueue` can be a `static`. The queue's contents belong to
/// the running execution, so a `static` queue, or one the test closure captured, starts every
/// execution empty, as Shuttle's `lazy_static!` does. It must be used inside a Shuttle execution;
/// only [`new`](Self::new), `Default`, `Debug` and dropping work outside of one.
///
/// [`crossbeam_queue::SegQueue`]: https://docs.rs/crossbeam-queue/0.3/crossbeam_queue/struct.SegQueue.html
pub struct SegQueue<T> {
    state: PerExecution<State<T>>,
}

struct State<T> {
    items: VecDeque<T>,
    clock: VectorClock,
}

impl<T> State<T> {
    fn new() -> Self {
        Self {
            items: VecDeque::new(),
            clock: VectorClock::new(),
        }
    }
}

// Safety: the same bounds crossbeam's queue has. The queue moves values between threads, and never
// hands out a reference to one, so sharing it only needs `T: Send`.
unsafe impl<T: Send> Send for SegQueue<T> {}
unsafe impl<T: Send> Sync for SegQueue<T> {}

impl<T> UnwindSafe for SegQueue<T> {}
impl<T> RefUnwindSafe for SegQueue<T> {}

impl<T> SegQueue<T> {
    /// Creates a new unbounded queue.
    pub const fn new() -> SegQueue<T> {
        SegQueue {
            state: PerExecution::new(),
        }
    }

    fn with_state<R>(&self, f: impl FnOnce(&mut State<T>) -> R) -> R {
        let mut state = self.state.get(State::new).borrow_mut();
        synchronize(&mut state.clock);
        f(&mut state)
    }

    /// Pushes back an element to the tail.
    pub fn push(&self, value: T) {
        switch();
        self.with_state(|state| state.items.push_back(value));
    }

    /// Pushes an element to the queue with exclusive mutable access.
    ///
    /// Not a scheduling point: no other thread can reach the queue.
    pub fn push_mut(&mut self, value: T) {
        self.state.get(State::new).borrow_mut().items.push_back(value);
    }

    /// Pops the head element from the queue.
    ///
    /// If the queue is empty, `None` is returned.
    pub fn pop(&self) -> Option<T> {
        switch();
        self.with_state(|state| state.items.pop_front())
    }

    /// Pops the head element from the queue using an exclusive reference.
    ///
    /// Not a scheduling point: no other thread can reach the queue.
    pub fn pop_mut(&mut self) -> Option<T> {
        self.state.get(State::new).borrow_mut().items.pop_front()
    }

    /// Returns `true` if the queue is empty.
    pub fn is_empty(&self) -> bool {
        switch();
        self.with_state(|state| state.items.is_empty())
    }

    /// Returns the number of elements in the queue.
    pub fn len(&self) -> usize {
        switch();
        self.with_state(|state| state.items.len())
    }
}

impl<T> fmt::Debug for SegQueue<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("SegQueue { .. }")
    }
}

impl<T> Default for SegQueue<T> {
    fn default() -> SegQueue<T> {
        SegQueue::new()
    }
}

impl<T> IntoIterator for SegQueue<T> {
    type Item = T;

    type IntoIter = IntoIter<T>;

    /// Consumes the queue, yielding this execution's elements, oldest first.
    ///
    /// Not a scheduling point. Outside of a Shuttle execution, and while one is being torn down, the
    /// iterator is empty.
    fn into_iter(mut self) -> Self::IntoIter {
        let items = self.state.take().map(|state| state.items).unwrap_or_default();
        IntoIter {
            items: items.into_iter(),
        }
    }
}

/// An iterator that consumes a [`SegQueue`], as crossbeam's does.
pub struct IntoIter<T> {
    items: std::collections::vec_deque::IntoIter<T>,
}

// Safety: as for the queue, which crossbeam's iterator holds: `&IntoIter` gives access to nothing.
unsafe impl<T: Send> Sync for IntoIter<T> {}

impl<T> Iterator for IntoIter<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        self.items.next()
    }
}

// crossbeam derives its iterator's `Debug`, which therefore needs `T: Debug` too.
impl<T: fmt::Debug> fmt::Debug for IntoIter<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IntoIter")
            .field("value", &format_args!("SegQueue {{ .. }}"))
            .finish()
    }
}
