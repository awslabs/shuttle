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
// the task ID -- so all these safety requirements are trivial.

/// Create a `Waker` that will make the given `task_id` runnable when invoked.
pub(crate) fn make_waker(task_id: TaskId) -> Waker {
    // We stash the task ID into the bits of the `data` pointer that all the vtable method below
    // receive as an argument.
    let data = task_id.0 as *const ();
    // Safety: see above
    unsafe { Waker::from_raw(RawWaker::new(data, &RAW_WAKER_VTABLE)) }
}

unsafe fn raw_waker_clone(data: *const ()) -> RawWaker {
    // No resources associated with our wakers, so just duplicate the pointer
    RawWaker::new(data, &RAW_WAKER_VTABLE)
}

unsafe fn raw_waker_wake(data: *const ()) {
    let task_id = TaskId::from(data as usize);
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

const RAW_WAKER_VTABLE: RawWakerVTable =
    RawWakerVTable::new(raw_waker_clone, raw_waker_wake, raw_waker_wake_by_ref, raw_waker_drop);
