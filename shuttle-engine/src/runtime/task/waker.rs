use crate::runtime::execution::ExecutionState;
use crate::runtime::task::TaskId;
use std::task::{RawWaker, RawWakerVTable, Waker};

// Safety: the `RawWaker` interface is unsafe because it requires manually enforcing resource
// management contracts on each method in the vtable:
// * `clone` should create an additional RawWaker, including creating all the resources required
// * `wake` should consume the waker it was invoked on and release its resources
// * `wake_by_ref` is like `wake` but does not consume or release the resources
// * `drop` releases all the resources associated with a waker
// Our wakers don't have any resources associated with them -- the `data` pointer's bits are just
// the task ID, and on 64-bit targets a poll number (see `make_poll_waker`) -- so all these safety
// requirements are trivial.

/// The bits of a waker's `data` word that hold the task ID. On 64-bit targets the other half holds
/// a poll number (see `make_poll_waker`).
#[cfg(target_pointer_width = "64")]
const TASK_ID_MASK: usize = u32::MAX as usize;
#[cfg(not(target_pointer_width = "64"))]
const TASK_ID_MASK: usize = usize::MAX;

/// The task that a waker's `data` word wakes.
fn task_of(data: *const ()) -> TaskId {
    TaskId::from(data as usize & TASK_ID_MASK)
}

fn waker_from_data(data: usize) -> Waker {
    // Safety: see above
    unsafe { Waker::from_raw(RawWaker::new(data as *const (), &RAW_WAKER_VTABLE)) }
}

/// Create a `Waker` that will make the given `task_id` runnable when invoked.
pub fn make_waker(task_id: TaskId) -> Waker {
    // We stash the task ID into the bits of the `data` pointer that all the vtable method below
    // receive as an argument.
    waker_from_data(task_id.0)
}

/// Create a waker for `task_id` that [`Waker::will_wake`] tells apart from every other waker made
/// here, for a driver loop to poll a future with.
///
/// A future that keeps a waker commonly skips cloning a new one when the old one "will wake" the
/// same task: `AtomicWaker` does, and with it every futures-channel mpsc receiver. That clone is
/// where [`crate::await_backtrace`] records a parked future's await site, so a future polled again
/// after its task was woken for something else would otherwise record nothing. `will_wake` is
/// best-effort by contract, so futures already have to cope with it returning false.
///
/// This is done whether or not backtraces are captured, so that turning capture on cannot change
/// which paths futures take, and with them the schedule.
pub fn make_poll_waker(task_id: TaskId) -> Waker {
    #[cfg(target_pointer_width = "64")]
    {
        use std::cell::Cell;
        thread_local! {
            static POLLS: Cell<usize> = const { Cell::new(0) };
        }
        debug_assert_eq!(task_id.0 & !TASK_ID_MASK, 0, "task ID does not fit in a waker");
        let poll = POLLS.get().wrapping_add(1);
        POLLS.set(poll);
        waker_from_data((poll << 32) | task_id.0)
    }
    // No spare bits for a poll number, so polls keep the plain waker, and a future that skips
    // cloning it again is shown with the await site of an earlier poll.
    #[cfg(not(target_pointer_width = "64"))]
    make_waker(task_id)
}

unsafe fn raw_waker_clone(data: *const ()) -> RawWaker {
    // A future that is about to return `Pending` clones the waker it was handed so that whoever
    // owns the resource can wake it later. That clone happens inside the future's own `poll`, which
    // makes this the one point where Shuttle runs code while an arbitrary user future's await chain
    // is still on the stack. Grab it while we can — see `crate::await_backtrace`.
    crate::await_backtrace::note_waker_clone(task_of(data));

    // No resources associated with our wakers, so just duplicate the pointer
    RawWaker::new(data, &RAW_WAKER_VTABLE)
}

unsafe fn raw_waker_wake(data: *const ()) {
    let task_id = task_of(data);
    ExecutionState::with(|state| {
        if state.is_finished() {
            return;
        }

        let waiter = state.get_mut(task_id);

        if waiter.finished() {
            return;
        }

        waiter.wake();
    });
}

unsafe fn raw_waker_wake_by_ref(data: *const ()) {
    // Our wakers have no resources associated with then, so `wake` and `wake_by_ref` are the same
    raw_waker_wake(data);
}

unsafe fn raw_waker_drop(_data: *const ()) {
    // No resources associated with our wakers, so nothing to do on drop
}

// A `static` rather than a `const`: each use of a `const` may get a copy of its own, and
// `Waker::will_wake` compares vtable addresses, so in release builds it returned false even for a
// clone of the same waker.
static RAW_WAKER_VTABLE: RawWakerVTable =
    RawWakerVTable::new(raw_waker_clone, raw_waker_wake, raw_waker_wake_by_ref, raw_waker_drop);
