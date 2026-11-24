use shuttle::check_dfs;
use shuttle_tokio_test_impl::task;
use std::mem::ManuallyDrop;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

/// Regression test: `wake_by_ref` must keep the reference that the `Waker` owns when
/// `ThreadWaker::wake` panics. It panics outside an execution, where Shuttle's `Mutex` can't be
/// locked.
#[test]
fn wake_by_ref_keeps_the_wakers_reference_when_wake_panics() {
    let out = Arc::new(Mutex::new(None));
    let slot = out.clone();
    check_dfs(
        move || {
            let mut spawned = task::spawn(std::future::ready(()));
            let waker = spawned.enter(|cx, _| cx.waker().clone());
            *slot.lock().unwrap() = Some((spawned, waker));
        },
        None,
    );
    let (spawned, waker) = out.lock().unwrap().take().unwrap();
    // If an assertion fails, both are leaked. Without the fix, dropping both would free the
    // `ThreadWaker` twice.
    let (spawned, waker) = (ManuallyDrop::new(spawned), ManuallyDrop::new(waker));
    // The `Spawn` and the `Waker` each own one reference.
    assert_eq!(spawned.waker_ref_count(), 2);
    assert!(catch_unwind(AssertUnwindSafe(|| waker.wake_by_ref())).is_err());
    assert_eq!(
        spawned.waker_ref_count(),
        2,
        "wake_by_ref released a reference that the Waker still owns"
    );
    drop(ManuallyDrop::into_inner(waker));
    drop(ManuallyDrop::into_inner(spawned));
}
