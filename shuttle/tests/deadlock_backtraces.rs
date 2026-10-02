//! Checks which stack each task in a deadlock report is shown with.
//!
//! These tests need `SHUTTLE_CAPTURE_BACKTRACE`, which Shuttle reads once per process, so they live
//! in their own test binary and every test turns it on before it touches Shuttle.
//!
//! Release builds carry no line info, so the tests identify frames by function name. Each wait
//! lives in its own `#[inline(never)]` function, and never as a tail call, so that it keeps a
//! frame of its own.

use futures::channel::oneshot;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use shuttle::future::batch_semaphore::{Acquire, BatchSemaphore, Fairness};
use shuttle::sync::{Condvar, Mutex};
use shuttle::{check_dfs, check_random, future, thread};
use std::future::Future;
use std::panic::{self, UnwindSafe};
use std::pin::Pin;
// std atomics on purpose: Shuttle cannot see them, so they add no scheduling points.
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Once};
use std::task::{Context, Poll, Waker};

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

/// Leaves its waker in the slot for another task to use, and never completes.
struct LeaveWakerAndWait(Arc<StdMutex<Option<Waker>>>);

impl Future for LeaveWakerAndWait {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        *self.0.lock().unwrap() = Some(cx.waker().clone());
        Poll::Pending
    }
}

#[inline(never)]
fn clone_foreign_waker(slot: &StdMutex<Option<Waker>>) {
    drop(std::hint::black_box(slot.lock().unwrap().clone()));
}

/// Clones the waker in the slot, as a task about to wake its owner would, then waits without
/// registering a waker of its own.
struct CloneForeignWakerAndWait(Arc<StdMutex<Option<Waker>>>);

impl Future for CloneForeignWakerAndWait {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        clone_foreign_waker(&self.0);
        Poll::Pending
    }
}

/// Cloning another task's waker, to wake it later, is not a task registering for a wakeup of its
/// own, so it is not that task's await site.
#[test]
fn cloning_another_tasks_waker_is_not_an_await_site() {
    let report = deadlock_report(|| {
        check_random(
            || {
                let slot = Arc::new(StdMutex::new(None));
                let slot2 = Arc::clone(&slot);
                thread::Builder::new()
                    .name("waiter".into())
                    .spawn(move || future::block_on(LeaveWakerAndWait(slot2)))
                    .unwrap();
                while slot.lock().unwrap().is_none() {
                    thread::yield_now();
                }
                future::block_on(future::spawn(CloneForeignWakerAndWait(slot))).unwrap();
            },
            1,
        )
    });

    let entry = entry(&report, "<unknown>");
    assert!(
        !has_frame(entry, "clone_foreign_waker"),
        "shown at a clone of another task's waker:\n{entry}"
    );
}

/// Polls a semaphore's `Acquire` from a function of its own, so that its await site has a frame to
/// look for.
struct NamedAcquire<'a>(Pin<Box<Acquire<'a>>>);

impl Future for NamedAcquire<'_> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        poll_acquire(self.0.as_mut(), cx)
    }
}

#[inline(never)]
fn poll_acquire(acquire: Pin<&mut Acquire<'_>>, cx: &mut Context<'_>) -> Poll<()> {
    // `black_box` keeps the call out of tail position, or release builds drop this frame.
    std::hint::black_box(acquire.poll(cx).map(|acquired| acquired.unwrap()))
}

/// When a task takes permits from an unfair semaphore, the semaphore blocks every waiter that can
/// no longer succeed. That must not cost a future waiting there its await site.
#[test]
fn reblocked_future_keeps_its_await_site() {
    let report = deadlock_report(|| {
        check_random(
            || {
                let semaphore = Arc::new(BatchSemaphore::new(1, Fairness::Unfair));
                let queued = Arc::new(AtomicBool::new(false));
                let (semaphore2, queued2) = (Arc::clone(&semaphore), Arc::clone(&queued));
                let task = future::spawn(async move {
                    // An acquire that cannot succeed has no scheduling point before it queues, so
                    // once main sees this the task is queued on `semaphore`.
                    queued2.store(true, Ordering::SeqCst);
                    NamedAcquire(Box::pin(semaphore2.acquire(2))).await;
                });
                while !queued.load(Ordering::SeqCst) {
                    thread::yield_now();
                }
                // Taking the last permit blocks the task, which wants two.
                semaphore.try_acquire(1).unwrap();
                future::block_on(task).unwrap();
            },
            1,
        )
    });

    let entry = entry(&report, "<unknown>");
    assert!(
        has_frame(entry, "poll_acquire"),
        "not shown at its await site:\n{entry}"
    );
}

#[inline(never)]
fn register_unless_same(slot: &mut Option<Waker>, cx: &Context<'_>) {
    if !slot.as_ref().is_some_and(|waker| waker.will_wake(cx.waker())) {
        *slot = Some(cx.waker().clone());
    }
}

/// Registers its waker the way `AtomicWaker` does: only if the one it already holds would not wake
/// the same task. So it clones the waker on its first poll, and on a later poll only if the waker
/// looks different.
struct RegisterUnlessSame(Option<Waker>);

impl Future for RegisterUnlessSame {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        register_unless_same(&mut self.0, cx);
        Poll::Pending
    }
}

/// A future that is polled again after something else woke its task, and that keeps the waker it
/// registered the first time, is still shown at its await site, as the poll it is parked after
/// recorded it.
#[test]
fn repolled_future_is_shown_at_its_await_site() {
    let report = deadlock_report(|| {
        check_random(
            || {
                let (tx, rx) = oneshot::channel::<()>();
                let polled = Arc::new(AtomicBool::new(false));
                let polled2 = Arc::clone(&polled);
                let task = future::spawn(async move {
                    // No scheduling point between this and the first poll of both branches, so
                    // once main sees it, both have registered and the task has parked.
                    polled2.store(true, Ordering::SeqCst);
                    // The oneshot then fires, so the join is polled again with only the first
                    // branch still waiting.
                    futures::join!(RegisterUnlessSame(None), async { rx.await.unwrap() });
                });
                while !polled.load(Ordering::SeqCst) {
                    thread::yield_now();
                }
                tx.send(()).unwrap();
                future::block_on(task).unwrap();
            },
            1,
        )
    });

    let entry = entry(&report, "<unknown>");
    assert!(
        has_frame(entry, "register_unless_same"),
        "not shown at its await site:\n{entry}"
    );
    assert!(
        !entry.contains("from an earlier poll"),
        "shown with an await site from before the task was woken:\n{entry}"
    );
}

#[inline(never)]
fn register_waker_too(cx: &Context<'_>) {
    // Not the same body as `register_waker`, so that a release build cannot fold the two into one.
    drop(std::hint::black_box((cx.waker().clone(), "too")));
}

/// Like `WaitForever`, but registers from a function of its own.
struct AlsoWaitForever;

impl Future for AlsoWaitForever {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        register_waker_too(cx);
        Poll::Pending
    }
}

/// A future waiting on both branches of a `join!` is shown at both.
#[test]
fn every_branch_of_a_join_is_shown() {
    let report = deadlock_report(|| {
        check_random(
            || {
                future::block_on(future::spawn(async {
                    futures::join!(WaitForever, AlsoWaitForever);
                }))
                .unwrap();
            },
            1,
        )
    });

    let entry = entry(&report, "<unknown>");
    assert!(has_frame(entry, "register_waker"), "first branch missing:\n{entry}");
    assert!(
        has_frame(entry, "register_waker_too"),
        "second branch missing:\n{entry}"
    );
}

/// A deadlocked task says where it was spawned, which identifies it even when its backtrace does
/// not. The main thread was not spawned by the test, so it does not.
#[test]
fn spawn_site_is_shown() {
    static SPAWN_LINE: AtomicU32 = AtomicU32::new(0);
    let report = deadlock_report(|| {
        check_random(
            || {
                let (task, line) = (future::spawn(WaitForever), line!());
                SPAWN_LINE.store(line, Ordering::SeqCst);
                future::block_on(task).unwrap();
            },
            1,
        )
    });

    let line = SPAWN_LINE.load(Ordering::SeqCst);
    let spawned = entry(&report, "<unknown>");
    let expected = format!("Spawned at: {}:{line}:", file!());
    assert!(spawned.contains(&expected), "no `{expected}` in:\n{spawned}");
    let main = entry(&report, "main-thread");
    assert!(!main.contains("Spawned at"), "main thread has a spawn site:\n{main}");
}

/// One of two children of a `FuturesUnordered` that wait on a semaphore, each from a function of its
/// own.
struct Child<'a>(Pin<Box<Acquire<'a>>>, bool);

impl Future for Child<'_> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.1 {
            child_a_waits(self.0.as_mut(), cx)
        } else {
            child_b_waits(self.0.as_mut(), cx)
        }
    }
}

#[inline(never)]
fn child_a_waits(acquire: Pin<&mut Acquire<'_>>, cx: &mut Context<'_>) -> Poll<()> {
    std::hint::black_box(acquire.poll(cx).map(|acquired| acquired.unwrap()))
}

#[inline(never)]
fn child_b_waits(acquire: Pin<&mut Acquire<'_>>, cx: &mut Context<'_>) -> Poll<()> {
    // Not the same body as `child_a_waits`, so that a release build cannot fold the two into one.
    std::hint::black_box(acquire.poll(cx).map(|acquired| {
        acquired.unwrap();
        std::hint::black_box(1u8);
    }))
}

/// `FuturesUnordered` polls its children with wakers of its own, so a child's wait is recorded by
/// the Shuttle future it waits on rather than by a waker clone.
#[test]
fn futures_unordered_children_are_shown_where_they_wait() {
    let report = deadlock_report(|| {
        check_random(
            || {
                future::block_on(future::spawn(async {
                    let semaphore = BatchSemaphore::new(0, Fairness::StrictlyFair);
                    let mut children = FuturesUnordered::new();
                    children.push(Child(Box::pin(semaphore.acquire(1)), true));
                    children.push(Child(Box::pin(semaphore.acquire(1)), false));
                    while children.next().await.is_some() {}
                }))
                .unwrap();
            },
            1,
        )
    });

    let entry = entry(&report, "<unknown>");
    assert!(has_frame(entry, "child_a_waits"), "first child missing:\n{entry}");
    assert!(has_frame(entry, "child_b_waits"), "second child missing:\n{entry}");
}

/// A task joining tasks through a `FuturesUnordered`, as tokio's `JoinSet` does, says which tasks.
#[test]
fn futures_unordered_join_handles_name_the_tasks_they_join() {
    let report = deadlock_report(|| {
        check_random(
            || {
                let (_tx, rx) = oneshot::channel::<()>();
                future::block_on(future::spawn(async move {
                    let mut set = FuturesUnordered::new();
                    set.push(future::spawn(async move {
                        let _ = rx.await;
                    }));
                    while set.next().await.is_some() {}
                }))
                .unwrap();
            },
            1,
        )
    });

    let joiner = report
        .split("\n, ")
        .find(|entry| entry.contains("Waiting inside a combinator"))
        .unwrap_or_else(|| panic!("no task waits inside a combinator:\n{report}"));
    assert!(joiner.contains(", joining task "), "joined task not named:\n{joiner}");
}

/// A future polled under another waker and then dropped while pending leaves nothing behind.
#[test]
fn a_dropped_pending_future_leaves_no_wait_record() {
    let report = deadlock_report(|| {
        check_random(
            || {
                future::block_on(future::spawn(async {
                    let semaphore = BatchSemaphore::new(0, Fairness::StrictlyFair);
                    let mut acquire = Box::pin(semaphore.acquire(1));
                    let waker = futures::task::noop_waker();
                    assert!(acquire.as_mut().poll(&mut Context::from_waker(&waker)).is_pending());
                    drop(acquire);
                    std::future::pending::<()>().await;
                }))
                .unwrap();
            },
            1,
        )
    });

    let entry = entry(&report, "<unknown>");
    assert!(
        !entry.contains("Waiting inside a combinator"),
        "record outlived its future:\n{entry}"
    );
}

/// A deadlocked execution never drops its futures, so their records must not reach the next one.
#[test]
fn wait_records_do_not_outlive_their_execution() {
    let first = deadlock_report(|| {
        check_random(
            || {
                future::block_on(future::spawn(async {
                    let semaphore = BatchSemaphore::new(0, Fairness::StrictlyFair);
                    let mut children = FuturesUnordered::new();
                    children.push(Child(Box::pin(semaphore.acquire(1)), true));
                    while children.next().await.is_some() {}
                }))
                .unwrap();
            },
            1,
        )
    });
    assert!(has_frame(entry(&first, "<unknown>"), "child_a_waits"));

    let second = deadlock_report(|| {
        check_random(
            || future::block_on(future::spawn(std::future::pending::<()>())).unwrap(),
            1,
        )
    });
    let entry = entry(&second, "<unknown>");
    assert!(
        !entry.contains("Waiting inside a combinator"),
        "record from an earlier execution:\n{entry}"
    );
}
