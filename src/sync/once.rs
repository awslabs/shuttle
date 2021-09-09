use crate::runtime::execution::ExecutionState;
use crate::runtime::storage::StorageKey;
use crate::runtime::task::clock::VectorClock;
use crate::sync::Mutex;
use std::cell::RefCell;
use std::rc::Rc;
use tracing::trace;

/// A synchronization primitive which can be used to run a one-time global initialization. Useful
/// for one-time initialization for FFI or related functionality. This type can only be constructed
/// with [`Once::new()`].
#[derive(Debug)]
pub struct Once {
    // We use the address of the `Once` as an identifier, so it can't be zero-sized even though all
    // its state is stored in ExecutionState storage
    _dummy: usize,
}

/// A `Once` cell can either be `Running`, in which case a `Mutex` mediates racing threads trying to
/// invoke `call_once`, or `Complete` once an initializer has completed, in which case the `Mutex`
/// is no longer necessary.
enum OnceInitState {
    Running(Rc<Mutex<bool>>),
    Complete(VectorClock),
}

impl std::fmt::Debug for OnceInitState {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Running(_) => write!(f, "Running"),
            Self::Complete(_) => write!(f, "Complete"),
        }
    }
}

impl Once {
    /// Creates a new `Once` value.
    pub const fn new() -> Self {
        Self { _dummy: 0 }
    }

    /// Performs an initialization routine once and only once. The given closure will be executed
    /// if this is the first time `call_once` has been called, and otherwise the routine will *not*
    /// be invoked.
    ///
    /// This method will block the calling thread if another initialization routine is currently
    /// running.
    ///
    /// When this function returns, it is guaranteed that some initialization has run and completed
    /// (it may not be the closure specified).
    pub fn call_once<F>(&self, f: F)
    where
        F: FnOnce(),
    {
        self.call_once_inner(|_state| f(), false);
    }

    /// Performs the same function as [`Once::call_once()`] except ignores poisoning.
    ///
    /// If the cell has previously been poisoned, this function will still attempt to call the given
    /// closure. If the closure does not panic, the cell will no longer be poisoned.
    pub fn call_once_force<F>(&self, f: F)
    where
        F: FnOnce(&OnceState),
    {
        self.call_once_inner(f, true);
    }

    /// Returns `true` if some [`Once::call_once()`] call has completed successfully.
    pub fn is_completed(&self) -> bool {
        ExecutionState::with(|state| {
            let init = match self.get_state(state) {
                Some(init) => init,
                None => return false,
            };
            let init_state = init.borrow();
            match &*init_state {
                OnceInitState::Complete(clock) => {
                    let clock = clock.clone();
                    drop(init_state);
                    state.update_clock(&clock);
                    true
                }
                _ => false,
            }
        })
    }

    fn call_once_inner<F>(&self, f: F, ignore_poisoning: bool)
    where
        F: FnOnce(&OnceState),
    {
        let lock = ExecutionState::with(|state| {
            // Initialize the state of the `Once` cell if we're the first thread to try
            if self.get_state(state).is_none() {
                self.init_state(state, OnceInitState::Running(Rc::new(Mutex::new(false))));
            }

            let init = self.get_state(state).expect("must be initialized by this point");
            let init_state = init.borrow();
            trace!(state=?init_state, "call_once on cell {:p}", self);
            match &*init_state {
                OnceInitState::Complete(clock) => {
                    // If already complete, just update the clock from the thread that inited
                    let clock = clock.clone();
                    drop(init_state);
                    state.update_clock(&clock);
                    None
                }
                OnceInitState::Running(lock) => Some(Rc::clone(lock)),
            }
        });

        // If there's a lock, then we need to try racing on it to decide who gets to run their
        // initialization closure.
        if let Some(lock) = lock {
            let (mut flag, is_poisoned) = match lock.lock() {
                Ok(flag) => (flag, false),
                Err(_) if !ignore_poisoning => panic!("Once instance has previously been poisoned"),
                Err(err) => (err.into_inner(), true),
            };
            if *flag {
                return;
            }

            trace!("won the call_once race for cell {:p}", self);
            f(&OnceState(is_poisoned));

            *flag = true;
            // We were the thread that won the race, so remember our current clock to establish
            // causality with future threads that try (and fail) to run `call_once`. The threads
            // that were racing with us will get causality through acquiring the `Mutex`.
            ExecutionState::with(|state| {
                let clock = state.increment_clock().clone();
                *self
                    .get_state(state)
                    .expect("must be initialized by this point")
                    .borrow_mut() = OnceInitState::Complete(clock);
            });
        }
    }

    fn get_state<'a>(&self, from: &'a ExecutionState) -> Option<&'a RefCell<OnceInitState>> {
        from.get_storage::<_, RefCell<OnceInitState>>(self)
    }

    fn init_state(&self, into: &mut ExecutionState, new_state: OnceInitState) {
        into.init_storage::<_, RefCell<OnceInitState>>(self, RefCell::new(new_state));
    }
}

/// State yielded to [`Once::call_once_force()`]'s closure parameter. The state can be used to query
/// the poison status of the [`Once`].
#[derive(Debug)]
#[non_exhaustive]
pub struct OnceState(bool);

impl OnceState {
    /// Returns `true` if the associated [`Once`] was poisoned prior to the invocation of the
    /// closure passed to [`Once::call_once_force()`].
    pub fn is_poisoned(&self) -> bool {
        self.0
    }
}

impl From<&Once> for StorageKey {
    fn from(once: &Once) -> Self {
        StorageKey(once as *const _ as usize, 0x2)
    }
}
