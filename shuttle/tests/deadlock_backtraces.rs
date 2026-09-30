//! Checks which stack each task in a deadlock report is shown with.
//!
//! These tests need `SHUTTLE_CAPTURE_BACKTRACE`, which Shuttle reads once per process, so they live
//! in their own test binary and every test turns it on before it touches Shuttle.
//!
//! Release builds carry no line info, so the tests identify frames by function name. Each wait
//! lives in its own `#[inline(never)]` function, and never as a tail call, so that it keeps a
//! frame of its own.

use futures::channel::oneshot;
use shuttle::sync::{Condvar, Mutex};
use shuttle::{check_dfs, check_random, future, thread};
use std::future::Future;
use std::panic::{self, UnwindSafe};
use std::pin::Pin;
// std atomics on purpose: Shuttle cannot see them, so they add no scheduling points.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once};
use std::task::{Context, Poll};

/// Run `f`, which must deadlock, with backtrace capture on, and return the deadlock report.
fn deadlock_report(f: impl FnOnce() + UnwindSafe) -> String {
    static ENABLE: Once = Once::new();
    ENABLE.call_once(|| std::env::set_var(shuttle::CAPTURE_BACKTRACE, "1"));

    let payload = panic::catch_unwind(f).expect_err("test should deadlock");
    let report = *payload.downcast::<String>().expect("a deadlock panics with a String");
    assert!(report.starts_with("deadlock!"), "expected a deadlock, got: {report}");
    report
}

/// The part of `report` about the task named `name`, from its header to the next task's.
fn entry<'a>(report: &'a str, name: &str) -> &'a str {
    let header = format!("{name} (task ");
    let start = report
        .find(&header)
        .unwrap_or_else(|| panic!("no task named {name} in:\n{report}"));
    let rest = &report[start..];
    // Entries are joined with ", ", and every backtrace ends with a newline.
    rest.find("\n, ").map_or(rest, |end| &rest[..end])
}

/// Whether `entry` has a frame for the function `name` in this test binary.
fn has_frame(entry: &str, name: &str) -> bool {
    // A frame's function name ends its line, so a closure or a longer name that merely starts with
    // `name` does not match.
    entry.contains(&format!("deadlock_backtraces::{name}\n"))
}

#[inline(never)]
fn worker_a(pair: Arc<(Mutex<bool>, Condvar)>) {
    let (lock, cvar) = &*pair;
    let mut ready = lock.lock().unwrap();
    while !*ready {
        ready = cvar.wait(ready).unwrap();
    }
    std::hint::black_box("a");
}

#[inline(never)]
fn worker_b(pair: Arc<(Mutex<bool>, Condvar)>) {
    let (lock, cvar) = &*pair;
    let mut ready = lock.lock().unwrap();
    while !*ready {
        ready = cvar.wait(ready).unwrap();
    }
    std::hint::black_box("b");
}

/// `notify_one` makes every waiter runnable, and the first one to run blocks the others again.
/// That re-block must not give a waiter the stack of the task that re-blocked it.
#[test]
fn reblocked_waiter_is_shown_with_its_own_stack() {
    let report = deadlock_report(|| {
        check_dfs(
            || {
                let pair = Arc::new((Mutex::new(false), Condvar::new()));
                let p = Arc::clone(&pair);
                thread::Builder::new()
                    .name("worker_a".into())
                    .spawn(move || worker_a(p))
                    .unwrap();
                let p = Arc::clone(&pair);
                thread::Builder::new()
                    .name("worker_b".into())
                    .spawn(move || worker_b(p))
                    .unwrap();
                let (lock, cvar) = &*pair;
                *lock.lock().unwrap() = true;
                cvar.notify_one();
            },
            None,
        )
    });

    // Exactly one worker is left waiting.
    let (blocked, other) = if report.contains("worker_a (task ") {
        ("worker_a", "worker_b")
    } else {
        ("worker_b", "worker_a")
    };
    let entry = entry(&report, blocked);
    assert!(
        has_frame(entry, blocked),
        "{blocked} is not in its own function:\n{entry}"
    );
    assert!(!has_frame(entry, other), "{blocked} has {other}'s stack:\n{entry}");
}

#[inline(never)]
fn wait_on_condvar_until_released(pair: &(Mutex<u8>, Condvar)) {
    let (lock, cvar) = pair;
    let mut state = lock.lock().unwrap();
    *state = 1;
    while *state != 2 {
        state = cvar.wait(state).unwrap();
    }
}

#[inline(never)]
fn wait_for_message_forever(rx: oneshot::Receiver<()>) {
    // `black_box` keeps the call out of tail position, or release builds drop this frame.
    let _ = std::hint::black_box(future::block_on(rx));
}

/// A thread that waited on a condvar and was released, and is now parked on a future forever, is
/// shown at the future.
#[test]
fn condvar_wait_then_future_park_is_shown_at_the_future() {
    let report = deadlock_report(|| {
        check_random(
            || {
                let pair = Arc::new((Mutex::new(0u8), Condvar::new()));
                let (tx, rx) = oneshot::channel::<()>();
                let pair2 = Arc::clone(&pair);
                let waiter = thread::Builder::new()
                    .name("waiter".into())
                    .spawn(move || {
                        wait_on_condvar_until_released(&pair2);
                        wait_for_message_forever(rx);
                    })
                    .unwrap();

                // Release the waiter once it is inside `cvar.wait`.
                loop {
                    let (lock, cvar) = &*pair;
                    let mut state = lock.lock().unwrap();
                    if *state == 1 {
                        *state = 2;
                        cvar.notify_one();
                        break;
                    }
                    drop(state);
                    thread::yield_now();
                }
                waiter.join().unwrap();
                drop(tx);
            },
            1,
        )
    });

    let entry = entry(&report, "waiter");
    assert!(
        has_frame(entry, "wait_for_message_forever"),
        "not shown at the future:\n{entry}"
    );
    assert!(
        !has_frame(entry, "wait_on_condvar_until_released"),
        "shown at the earlier condvar wait:\n{entry}"
    );
}

#[inline(never)]
fn lock_contended(lock: &Mutex<()>) {
    drop(lock.lock().unwrap());
}

#[inline(never)]
fn wait_for_task(task: future::JoinHandle<()>) {
    future::block_on(task).unwrap();
}

#[inline(never)]
fn wait_on_condvar_forever(pair: &(Mutex<bool>, Condvar)) {
    let (lock, cvar) = pair;
    let mut ready = lock.lock().unwrap();
    while !*ready {
        ready = cvar.wait(ready).unwrap();
    }
}

/// A thread that waited for a contended lock and got it, and is now blocked on a condvar forever,
/// is shown at the condvar.
#[test]
fn mutex_wait_then_condvar_is_shown_at_the_condvar() {
    let report = deadlock_report(|| {
        check_random(
            || {
                let lock = Arc::new(Mutex::new(()));
                let pair = Arc::new((Mutex::new(false), Condvar::new()));
                let queued = Arc::new(AtomicBool::new(false));
                let guard = lock.lock().unwrap();
                let (lock2, pair2, queued2) = (Arc::clone(&lock), Arc::clone(&pair), Arc::clone(&queued));
                let waiter = thread::Builder::new()
                    .name("waiter".into())
                    .spawn(move || {
                        // A std atomic is invisible to Shuttle, and a contended acquire of an
                        // unfair semaphore has no scheduling point before it queues, so once main
                        // sees this the waiter is queued on `lock`.
                        queued2.store(true, Ordering::SeqCst);
                        lock_contended(&lock2);
                        wait_on_condvar_forever(&pair2);
                    })
                    .unwrap();
                while !queued.load(Ordering::SeqCst) {
                    thread::yield_now();
                }
                drop(guard);
                waiter.join().unwrap();
            },
            1,
        )
    });

    let entry = entry(&report, "waiter");
    assert!(
        has_frame(entry, "wait_on_condvar_forever"),
        "not shown at the condvar:\n{entry}"
    );
    assert!(
        !has_frame(entry, "lock_contended"),
        "shown at the earlier lock:\n{entry}"
    );
}

/// A thread that waited for a task to finish, and is now blocked on a condvar forever, is shown at
/// the condvar.
#[test]
fn join_handle_wait_then_condvar_is_shown_at_the_condvar() {
    let report = deadlock_report(|| {
        check_random(
            || {
                let (tx, rx) = oneshot::channel::<()>();
                let task = future::spawn(async move { rx.await.unwrap() });
                let pair = Arc::new((Mutex::new(false), Condvar::new()));
                let parked = Arc::new(AtomicBool::new(false));
                let (pair2, parked2) = (Arc::clone(&pair), Arc::clone(&parked));
                let waiter = thread::Builder::new()
                    .name("waiter".into())
                    .spawn(move || {
                        // No scheduling point between this and parking on the unfinished task, so
                        // once main sees it the waiter is parked on `task`.
                        parked2.store(true, Ordering::SeqCst);
                        wait_for_task(task);
                        wait_on_condvar_forever(&pair2);
                    })
                    .unwrap();
                while !parked.load(Ordering::SeqCst) {
                    thread::yield_now();
                }
                tx.send(()).unwrap();
                waiter.join().unwrap();
            },
            1,
        )
    });

    let entry = entry(&report, "waiter");
    assert!(
        has_frame(entry, "wait_on_condvar_forever"),
        "not shown at the condvar:\n{entry}"
    );
    assert!(
        !has_frame(entry, "wait_for_task"),
        "shown at the earlier join:\n{entry}"
    );
}

#[inline(never)]
fn register_waker(cx: &Context<'_>) {
    drop(std::hint::black_box(cx.waker().clone()));
}

/// Registers for a wakeup that never comes.
struct WaitForever;

impl Future for WaitForever {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        register_waker(cx);
        Poll::Pending
    }
}

/// A future that parks on something Shuttle knows nothing about is shown at its await site.
#[test]
fn future_task_is_shown_at_its_await_site() {
    let report = deadlock_report(|| {
        check_random(
            || {
                future::block_on(future::spawn(WaitForever)).unwrap();
            },
            1,
        )
    });

    let entry = entry(&report, "<unknown>");
    assert!(
        has_frame(entry, "register_waker"),
        "not shown at its await site:\n{entry}"
    );
}

/// Clones the waker, as if registering it somewhere, then completes at once.
struct CloneWakerThenComplete;

impl Future for CloneWakerThenComplete {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        drop(cx.waker().clone());
        Poll::Ready(())
    }
}

#[inline(never)]
fn clone_waker_and_complete() {
    future::block_on(CloneWakerThenComplete);
}

#[inline(never)]
fn park_forever_without_cloning_the_waker() {
    // `std::future::pending` never touches the waker. `black_box` keeps the call out of tail
    // position, or release builds drop this frame.
    let _ = std::hint::black_box(future::block_on(std::future::pending::<u8>()));
}

/// A poll that clones the waker and then completes records an await site that nobody collects. It
/// must not be reported for the next task to park, which here is another thread.
#[test]
fn completed_poll_does_not_hand_its_await_site_to_another_task() {
    let report = deadlock_report(|| {
        check_random(
            || {
                clone_waker_and_complete();
                let other = thread::Builder::new()
                    .name("other".into())
                    .spawn(park_forever_without_cloning_the_waker)
                    .unwrap();
                other.join().unwrap();
            },
            1,
        )
    });

    let entry = entry(&report, "other");
    assert!(
        has_frame(entry, "park_forever_without_cloning_the_waker"),
        "not shown where it parked:\n{entry}"
    );
    assert!(
        !has_frame(entry, "clone_waker_and_complete"),
        "shown with the main thread's stack:\n{entry}"
    );
}

#[inline(never)]
fn lock_forever(lock: &Mutex<()>) {
    drop(lock.lock().unwrap());
}

/// A future that records an await site and then blocks on a lock in the same poll is shown at the
/// lock: the `block_on` inside `Mutex::lock` must not take the await site of the poll around it.
#[test]
fn lock_inside_poll_is_shown_at_the_lock() {
    let report = deadlock_report(|| {
        check_random(
            || {
                let lock = Arc::new(Mutex::new(()));
                let _held_forever = lock.lock().unwrap();
                let lock2 = Arc::clone(&lock);
                let task = future::spawn(async move {
                    CloneWakerThenComplete.await;
                    lock_forever(&lock2);
                });
                future::block_on(task).unwrap();
            },
            1,
        )
    });

    let entry = entry(&report, "<unknown>");
    assert!(has_frame(entry, "lock_forever"), "not shown at the lock:\n{entry}");
}

/// While one thread waits for a lock, a future that parks elsewhere is still shown at its await
/// site.
#[test]
fn lock_wait_elsewhere_does_not_hide_an_await_site() {
    let report = deadlock_report(|| {
        check_random(
            || {
                let lock = Arc::new(Mutex::new(()));
                let _held_forever = lock.lock().unwrap();
                let queued = Arc::new(AtomicBool::new(false));
                let (lock2, queued2) = (Arc::clone(&lock), Arc::clone(&queued));
                thread::Builder::new()
                    .name("locker".into())
                    .spawn(move || {
                        // As above: once main sees this, the locker is queued on `lock`.
                        queued2.store(true, Ordering::SeqCst);
                        lock_forever(&lock2);
                    })
                    .unwrap();
                while !queued.load(Ordering::SeqCst) {
                    thread::yield_now();
                }
                future::block_on(future::spawn(WaitForever)).unwrap();
            },
            1,
        )
    });

    let entry = entry(&report, "<unknown>");
    assert!(
        has_frame(entry, "register_waker"),
        "not shown at its await site:\n{entry}"
    );
}
