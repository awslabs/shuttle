use crate::runtime::execution::ExecutionState;
use crate::runtime::task::TaskId;
use std::task::{RawWaker, RawWakerVTable, Waker};

/// Create a `Waker` that will make the given `task_id` runnable when invoked.
pub fn make_waker(task_id: TaskId) -> Waker {
    // We stash the task ID into the bits of the `data` pointer that all the vtable method below
    // receive as an argument.
    let data = task_id.0 as *const ();

    // SAFETY:
    // In order for `Waker::from_raw` to be safe, we need to ensure that `data` and the functions of
    // `RAW_WAKER_VTABLE` meet the contract of `RawWaker` and `RawWakerVTable`:
    // * each function is thread safe, because a `Waker` is `Send` and `Sync`.
    // * `clone` returns a `RawWaker` that wakes the same task, and keeps whatever resources that
    //   `RawWaker` needs.
    // * `wake` wakes the task, and releases the resources of the `RawWaker` it is called on.
    // * `drop` releases the resources of the `RawWaker` it is called on.
    // * `wake_by_ref` wakes the task like `wake` does, and releases nothing.
    //
    // `data` is not a pointer to anything. Its bits are the task ID, and no function reads through
    // it. So our wakers own no resources, and `clone`, `wake`, `wake_by_ref` and `drop` have none to
    // keep or release. `clone` copies `data`, so the copy wakes the same task. `wake` and
    // `wake_by_ref` wake the task that `data` names (see `Task::wake`), unless the task or its
    // execution has finished, when there is nothing left to wake.
    //
    // `wake` and `wake_by_ref` use only the `ExecutionState` of the calling OS thread, through
    // `ExecutionState::with`, which panics outside an execution. So the functions share no data
    // between OS threads, and they are thread safe.
    unsafe { Waker::from_raw(RawWaker::new(data, &RAW_WAKER_VTABLE)) }
}

/// The `clone` function of our wakers (see `make_waker`).
///
/// # Safety
///
/// The caller needs to ensure nothing. The function does not read through `data`. It is an
/// `unsafe fn` only because `RawWakerVTable` takes `unsafe fn`s.
unsafe fn raw_waker_clone(data: *const ()) -> RawWaker {
    // No resources associated with our wakers, so just duplicate the pointer
    RawWaker::new(data, &RAW_WAKER_VTABLE)
}

/// The `wake` function of our wakers (see `make_waker`).
///
/// # Safety
///
/// The caller needs to ensure nothing. The function does not read through `data`. It is an
/// `unsafe fn` only because `RawWakerVTable` takes `unsafe fn`s.
unsafe fn raw_waker_wake(data: *const ()) {
    let task_id = TaskId::from(data as usize);
    ExecutionState::with(|state| {
        // While execution teardown tears a task down, it is the current task, so the execution
        // isn't finished, and wakers work (see `ExecutionState::tear_down`).
        if state.is_finished() {
            return;
        }

        // A waker can outlive its execution (in a `static`, say), and then names a task of another one.
        let Some(waiter) = state.try_get_mut(task_id) else {
            return;
        };

        if waiter.finished() {
            return;
        }

        waiter.wake();
    });
}

/// The `wake_by_ref` function of our wakers (see `make_waker`).
///
/// # Safety
///
/// The caller needs to ensure nothing. The function does not read through `data`. It is an
/// `unsafe fn` only because `RawWakerVTable` takes `unsafe fn`s.
unsafe fn raw_waker_wake_by_ref(data: *const ()) {
    // Our wakers have no resources associated with them, so `wake` and `wake_by_ref` are the same

    // SAFETY:
    // In order for the call to be safe, we need to ensure what `raw_waker_wake` asks for, which is
    // nothing.
    unsafe { raw_waker_wake(data) };
}

/// The `drop` function of our wakers (see `make_waker`).
///
/// # Safety
///
/// The caller needs to ensure nothing. The function does nothing. It is an `unsafe fn` only
/// because `RawWakerVTable` takes `unsafe fn`s.
unsafe fn raw_waker_drop(_data: *const ()) {
    // No resources associated with our wakers, so nothing to do on drop
}

const RAW_WAKER_VTABLE: RawWakerVTable =
    RawWakerVTable::new(raw_waker_clone, raw_waker_wake, raw_waker_wake_by_ref, raw_waker_drop);
