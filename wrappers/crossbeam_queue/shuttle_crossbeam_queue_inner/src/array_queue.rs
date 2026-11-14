use crate::per_execution::{PerExecution, synchronize};
use shuttle_engine::runtime::task::clock::VectorClock;
use shuttle_engine::runtime::thread::switch;
use std::collections::VecDeque;
use std::fmt;
use std::panic::{RefUnwindSafe, UnwindSafe};

/// A bounded multi-producer multi-consumer queue, with the API of [`crossbeam_queue::ArrayQueue`].
///
/// Under Shuttle, each operation that the real queue performs with atomic operations other threads
/// can observe happens in one step at a scheduling point: [`push`](Self::push),
/// [`force_push`](Self::force_push), [`pop`](Self::pop), [`len`](Self::len),
/// [`is_empty`](Self::is_empty) and [`is_full`](Self::is_full). So the queue is linearizable, and
/// Shuttle explores every order in which concurrent operations take effect. The methods that take
/// `&mut self`, [`capacity`](Self::capacity), and the `Debug` and `Drop` impls are not scheduling
/// points.
///
/// The queue's contents belong to the running execution, so a queue that outlives an execution, such
/// as one the test closure captured, starts every execution empty. It must be used inside a Shuttle
/// execution; only [`new`](Self::new), [`capacity`](Self::capacity), `Debug` and dropping work
/// outside of one.
///
/// [`crossbeam_queue::ArrayQueue`]: https://docs.rs/crossbeam-queue/0.3/crossbeam_queue/struct.ArrayQueue.html
pub struct ArrayQueue<T> {
    cap: usize,
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
unsafe impl<T: Send> Sync for ArrayQueue<T> {}
unsafe impl<T: Send> Send for ArrayQueue<T> {}

impl<T> UnwindSafe for ArrayQueue<T> {}
impl<T> RefUnwindSafe for ArrayQueue<T> {}

impl<T> ArrayQueue<T> {
    /// Creates a new bounded queue with the given capacity.
    ///
    /// # Panics
    ///
    /// Panics if the capacity is zero or too large.
    pub fn new(cap: usize) -> ArrayQueue<T> {
        assert!(cap > 0, "capacity must be non-zero");
        // crossbeam's queue numbers its slots in laps of the next power of two above the capacity.
        assert!(
            cap.checked_add(1).and_then(usize::checked_next_power_of_two).is_some(),
            "capacity overflow"
        );
        ArrayQueue {
            cap,
            state: PerExecution::new(),
        }
    }

    fn with_state<R>(&self, f: impl FnOnce(&mut State<T>) -> R) -> R {
        let mut state = self.state.get(State::new).borrow_mut();
        synchronize(&mut state.clock);
        f(&mut state)
    }

    /// Attempts to push an element into the queue.
    ///
    /// If the queue is full, the element is returned back as an error.
    pub fn push(&self, value: T) -> Result<(), T> {
        switch();
        let cap = self.cap;
        self.with_state(|state| {
            if state.items.len() >= cap {
                Err(value)
            } else {
                state.items.push_back(value);
                Ok(())
            }
        })
    }

    /// Attempts to push an element using an exclusive reference of the queue.
    ///
    /// Not a scheduling point: no other thread can reach the queue.
    pub fn push_mut(&mut self, value: T) -> Result<(), T> {
        let cap = self.cap;
        let mut state = self.state.get(State::new).borrow_mut();
        if state.items.len() >= cap {
            Err(value)
        } else {
            state.items.push_back(value);
            Ok(())
        }
    }

    /// Pushes an element into the queue, replacing the oldest element if necessary.
    ///
    /// If the queue is full, the oldest element is replaced and returned, otherwise `None` is
    /// returned.
    pub fn force_push(&self, value: T) -> Option<T> {
        switch();
        let cap = self.cap;
        self.with_state(|state| {
            let oldest = if state.items.len() >= cap {
                state.items.pop_front()
            } else {
                None
            };
            state.items.push_back(value);
            oldest
        })
    }

    /// Attempts to pop an element from the queue.
    ///
    /// If the queue is empty, `None` is returned.
    pub fn pop(&self) -> Option<T> {
        switch();
        self.with_state(|state| state.items.pop_front())
    }

    /// Attempts to pop an element using an exclusive reference of the queue.
    ///
    /// Not a scheduling point: no other thread can reach the queue.
    pub fn pop_mut(&mut self) -> Option<T> {
        self.state.get(State::new).borrow_mut().items.pop_front()
    }

    /// Returns the capacity of the queue.
    ///
    /// Not a scheduling point, and works outside of a Shuttle execution too.
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Returns `true` if the queue is empty.
    pub fn is_empty(&self) -> bool {
        switch();
        self.with_state(|state| state.items.is_empty())
    }

    /// Returns `true` if the queue is full.
    pub fn is_full(&self) -> bool {
        switch();
        let cap = self.cap;
        self.with_state(|state| state.items.len() >= cap)
    }

    /// Returns the number of elements in the queue.
    pub fn len(&self) -> usize {
        switch();
        self.with_state(|state| state.items.len())
    }
}

impl<T> fmt::Debug for ArrayQueue<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("ArrayQueue { .. }")
    }
}

impl<T> IntoIterator for ArrayQueue<T> {
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

/// An iterator that consumes an [`ArrayQueue`], as crossbeam's does.
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
            .field("value", &format_args!("ArrayQueue {{ .. }}"))
            .finish()
    }
}
