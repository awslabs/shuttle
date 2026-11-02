use shuttle::future;
use shuttle::rand::Rng;
use shuttle::scheduler::DfsScheduler;
use shuttle_tokio_impl_inner::sync::notify::{Notified, Notify};
use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::task::{Context, Waker};
use test_log::test;

fn check_dfs<F>(f: F)
where
    F: Fn() + Send + Sync + 'static,
{
    let mut config = shuttle::Config::default();
    config.max_steps = shuttle::MaxSteps::FailAfter(1_000);

    let scheduler = DfsScheduler::new(None, true);
    let runner = shuttle::Runner::new(scheduler, config);
    runner.run(f);
}

#[test]
fn notify_mpsc_channel() {
    // Example from the tokio docs for Notify

    // Unbound multi-producer single-consumer (mpsc) channel.
    //
    // No wakeups can be lost when using this channel because the call to
    // `notify_one()` will store a permit in the `Notify`, which the following call
    // to `notified()` will consume.
    struct Channel<T> {
        values: Mutex<VecDeque<T>>,
        notify: Notify,
    }

    impl<T> Channel<T> {
        fn new() -> Self {
            Self {
                values: Mutex::new(VecDeque::new()),
                notify: Notify::new(),
            }
        }
        pub fn send(&self, value: T) {
            let mut values = self.values.lock().unwrap();
            values.push_back(value);
            drop(values);

            // Notify the consumer a value is available
            self.notify.notify_one();
        }

        // This is a single-consumer channel, so several concurrent calls to
        // `recv` are not allowed.
        pub async fn recv(&self) -> T {
            loop {
                // Drain values
                {
                    let mut values = self.values.lock().unwrap();
                    if let Some(value) = values.pop_front() {
                        return value;
                    }
                    drop(values);
                }

                // Wait for values to be available
                self.notify.notified().await;
            }
        }
    }

    check_dfs(|| {
        future::block_on(async {
            let tx1 = Arc::new(Channel::new());
            let tx2 = tx1.clone();
            let rx = tx1.clone();
            future::spawn(async move {
                tx1.send(1);
            });
            future::spawn(async move {
                tx2.send(2);
            });
            let mut v = rx.recv().await;
            v += rx.recv().await;
            assert_eq!(v, 3);
        });
    });
}

fn notify_mpmc_channel_2_test(do_enable: bool) {
    // Example from the tokio docs

    // Unbound multi-producer multi-consumer (mpmc) channel.
    //
    // The call to `enable` is important because otherwise if you have two
    // calls to `recv` and two calls to `send` in parallel, the following could
    // happen:
    //
    //  1. Both calls to `try_recv` return `None`.
    //  2. Both new elements are added to the vector.
    //  3. The `notify_one` method is called twice, adding only a single
    //     permit to the `Notify`.
    //  4. Both calls to `recv` reach the `Notified` future. One of them
    //     consumes the permit, and the other sleeps forever.
    //
    // By adding the `Notified` futures to the list by calling `enable` before
    // `try_recv`, the `notify_one` calls in step three would remove the
    // futures from the list and mark them notified instead of adding a permit
    // to the `Notify`. This ensures that both futures are woken.
    struct Channel<T> {
        messages: Mutex<VecDeque<T>>,
        notify_on_sent: Notify,
        do_enable: bool,
    }

    impl<T> Channel<T> {
        fn new(do_enable: bool) -> Self {
            Self {
                messages: Mutex::new(VecDeque::new()),
                notify_on_sent: Notify::new(),
                do_enable,
            }
        }

        pub fn send(&self, msg: T) {
            let mut locked_queue = self.messages.lock().unwrap();
            locked_queue.push_back(msg);
            drop(locked_queue);

            // Send a notification to one of the calls currently
            // waiting in a call to `recv`.
            self.notify_on_sent.notify_one();
        }

        pub fn try_recv(&self) -> Option<T> {
            let mut locked_queue = self.messages.lock().unwrap();
            locked_queue.pop_front()
        }

        pub async fn recv(&self) -> T {
            let mut future = self.notify_on_sent.notified();
            let mut future = unsafe { std::pin::Pin::new_unchecked(&mut future) };

            loop {
                if self.do_enable {
                    // Make sure that no wakeup is lost if we get `None` from `try_recv`.
                    future.as_mut().enable();
                }

                if let Some(msg) = self.try_recv() {
                    return msg;
                }

                // No `yield_now` here: the poll below takes a scheduling point.

                // Wait for a call to `notify_one`.
                //
                // This uses `.as_mut()` to avoid consuming the future,
                // which lets us call `Pin::set` below.
                future.as_mut().await;

                // Reset the future in case another call to
                // `try_recv` got the message before us.
                future.set(self.notify_on_sent.notified());
            }
        }
    }

    check_dfs(move || {
        future::block_on(async {
            let counter = Arc::new(AtomicUsize::new(0));
            let counter1 = counter.clone();
            let counter2 = counter.clone();

            let tx1 = Arc::new(Channel::new(do_enable));
            let tx2 = tx1.clone();
            let rx1 = tx1.clone();
            let rx2 = tx1.clone();

            let mut h = vec![];

            h.push(future::spawn(async move {
                let x = rx1.recv().await;
                counter1.fetch_add(x, Ordering::SeqCst);
            }));
            h.push(future::spawn(async move {
                tx1.send(1);
            }));
            h.push(future::spawn(async move {
                tx2.send(2);
            }));

            // The second receive runs on this task, which would otherwise only join the others.
            let x = rx2.recv().await;
            counter2.fetch_add(x, Ordering::SeqCst);

            for handle in h {
                handle.await.unwrap();
            }
            assert_eq!(counter.load(Ordering::SeqCst), 3);
        });
    });
}

#[test]
fn notify_mpmc_no_deadlock() {
    notify_mpmc_channel_2_test(true);
}

#[test]
#[should_panic(expected = "deadlock")]
fn notify_mpmc_deadlock() {
    notify_mpmc_channel_2_test(false);
}

// Regression test: `notify_one` must not panic when the `Notified` future is
// dropped after the waiter's flag is set to `NotifiedOne` but before the internal
// oneshot send completes.
//
// The race:
//   1. Spawned task polls `notified()`, enabling its waiter (flag = `Enabled`).
//   2. `handle.abort()` wakes the task so it is runnable for cancellation.
//   3. `notify_one()` finds the `Enabled` waiter, removes it, sets flag = `NotifiedOne`,
//      then calls `tx.send()` which yields before the actual send.
//   4. At that yield the DFS scheduler runs the aborted task, dropping the
//      `Notified` future (and its oneshot receiver) while flag is already `NotifiedOne`.
//   5. The send resumes and returns `Err` — `let _ =` handles it; `.unwrap()` panics.
#[test]
fn notify_one_drop_notified_before_send() {
    check_dfs(|| {
        future::block_on(async {
            let notify = Arc::new(Notify::new());
            let notify2 = notify.clone();

            let handle = future::spawn(async move {
                notify2.notified().await;
            });

            // Let the spawned task poll its Notified future, transitioning
            // the waiter from `Init` to `Enabled`.
            shuttle::future::yield_now().await;

            // Abort the task — this wakes it so it is runnable for
            // cancellation at the next scheduling point.
            handle.abort();

            // notify_one sets flag = `NotifiedOne` then yields inside tx.send().
            // The DFS scheduler may run the aborted task at that yield,
            // dropping the receiver before the send completes.
            notify.notify_one();

            let _ = handle.await;
        });
    });
}

/// Regression test: Shuttle must be able to schedule a notifier between a
/// waiter's check and its `notified()`.
///
/// The classic `Notify` misuse: check a condition, then create the `Notified`.
/// If the notifier sets the condition and calls `notify_waiters()` between the
/// two, the notification is lost and the waiter waits forever.
#[test]
#[should_panic(expected = "deadlock")]
fn notify_lost_wakeup_deadlock() {
    check_dfs(|| {
        let notify = Arc::new(Notify::new());
        let ready = Arc::new(shuttle::sync::Mutex::new(false));

        let waiter = {
            let (notify, ready) = (notify.clone(), ready.clone());
            shuttle::thread::spawn(move || {
                let is_ready = *ready.lock().unwrap();
                if !is_ready {
                    // BUG (in this test program): registers after checking.
                    future::block_on(notify.notified());
                }
            })
        };

        *ready.lock().unwrap() = true;
        notify.notify_waiters();

        waiter.join().unwrap();
    });
}

/// `notify_lost_wakeup_deadlock` with a future task that checks an atomic, so
/// that the scheduling point in `notified()` is taken in the middle of a poll.
#[test]
#[should_panic(expected = "deadlock")]
fn notify_lost_wakeup_deadlock_async() {
    check_dfs(|| {
        let notify = Arc::new(Notify::new());
        let ready = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));

        let waiter = {
            let (notify, ready) = (notify.clone(), ready.clone());
            future::spawn(async move {
                if !ready.load(Ordering::SeqCst) {
                    // BUG (in this test program): registers after checking.
                    notify.notified().await;
                }
            })
        };

        ready.store(true, Ordering::SeqCst);
        notify.notify_waiters();

        future::block_on(waiter).unwrap();
    });
}

/// The fix for `notify_lost_wakeup_deadlock`: create the `Notified` before the
/// check. `notify_waiters` notifies it from then on, even before it is polled,
/// so no interleaving loses the wakeup.
#[test]
fn notify_register_before_check_no_deadlock() {
    check_dfs(|| {
        let notify = Arc::new(Notify::new());
        let ready = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));

        let waiter = {
            let (notify, ready) = (notify.clone(), ready.clone());
            future::spawn(async move {
                let notified = notify.notified();
                if !ready.load(Ordering::SeqCst) {
                    notified.await;
                }
            })
        };

        ready.store(true, Ordering::SeqCst);
        notify.notify_waiters();

        future::block_on(waiter).unwrap();
    });
}

/// Regression test: `notify_waiters` must not discard a stored permit.
///
/// `notify_one()` with no waiter stores a permit. `notify_waiters()` notifies
/// only the waiters registered when it is called and leaves the permit in place,
/// so the later `notified()` takes it and completes at once.
#[test]
fn notify_waiters_keeps_stored_permit() {
    check_dfs(|| {
        let notify = Notify::new();
        notify.notify_one();
        notify.notify_waiters();
        future::block_on(notify.notified());
    });
}

/// A waiter that `notify_waiters()` notifies before its first poll completes
/// without the stored permit, which the next `notified()` then takes. This is
/// tokio's behavior since 1.51; before, a waiter's first poll took the permit.
#[test]
fn notify_waiters_leaves_permit_for_next_waiter() {
    check_dfs(|| {
        let notify = Notify::new();
        notify.notify_one();
        let first = notify.notified();
        notify.notify_waiters();
        future::block_on(first);
        future::block_on(notify.notified());
    });
}

/// Regression test: Shuttle must be able to schedule another task between a notifier's previous
/// operation and its `notify_one()`.
///
/// The test program wrongly assumes that once it sees `ready`, the notifier has already called
/// `notify_one()`, so that a permit is stored. That fails when the waiter runs between the
/// notifier's store and its `notify_one()`, which Shuttle only explores if `notify_one()` takes a
/// scheduling point first.
#[test]
#[should_panic(expected = "no permit yet")]
fn notify_one_after_store_is_a_separate_step() {
    check_dfs(|| {
        let notify = Arc::new(Notify::new());
        let ready = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));

        let waiter = {
            let (notify, ready) = (notify.clone(), ready.clone());
            shuttle::thread::spawn(move || {
                let notified = std::pin::pin!(notify.notified());
                if ready.load(Ordering::SeqCst) {
                    // BUG (in this test program): assumes `notify_one()` has already run.
                    assert!(notified.enable(), "no permit yet");
                }
            })
        };

        ready.store(true, Ordering::SeqCst);
        notify.notify_one();

        waiter.join().unwrap();
    });
}

/// Regression test: `notify_one()` notifies a `Notified`, and the `Notified` is dropped before it
/// completes. As in tokio, the drop passes the notification on to another waiter. Previously, the
/// notification was lost, and the other waiter waited forever.
#[test]
fn notify_one_to_dropped_waiter_is_passed_on() {
    check_dfs(|| {
        future::block_on(async {
            let notify = Arc::new(Notify::new());

            // `notify_one()` can choose this waiter. It is then dropped before it completes.
            let mut dropped = Box::pin(notify.notified());
            assert!(!dropped.as_mut().enable());

            let other = {
                let notify = notify.clone();
                future::spawn(async move { notify.notified().await })
            };

            notify.notify_one();
            drop(dropped);

            other.await.unwrap();
        });
    });
}

/// A completed `Notified` does not pass its notification on when it is dropped. Passing it on would
/// store a permit that no `notify_one()` call stored.
#[test]
fn notify_one_to_completed_waiter_is_not_passed_on() {
    check_dfs(|| {
        future::block_on(async {
            let notify = Arc::new(Notify::new());

            let mut first = Box::pin(notify.notified());
            assert!(!first.as_mut().enable());
            notify.notify_one();
            first.as_mut().await;
            drop(first);

            let mut second = Box::pin(notify.notified());
            assert!(!second.as_mut().enable(), "permit stored by a dropped waiter");
        });
    });
}

/// A `Notified` that `notify_one()` notified, and that is dropped after its execution ended, does
/// not panic. Outside an execution, the drop does not pass the notification on.
#[test]
fn notify_one_to_waiter_dropped_after_execution() {
    use shuttle_tokio_impl_inner::sync::notify::Notified;
    use std::pin::Pin;

    static KEPT: Mutex<Option<Pin<Box<Notified<'static>>>>> = Mutex::new(None);
    check_dfs(|| {
        let notify: &'static Notify = Box::leak(Box::new(Notify::new()));
        let mut notified = Box::pin(notify.notified());
        assert!(!notified.as_mut().enable());
        notify.notify_one();
        // Keep the notified `Notified` past the end of the execution.
        *KEPT.lock().unwrap() = Some(notified);
    });
    drop(KEPT.lock().unwrap().take());
}

/// A `Notified` that `notify_one()` notified, and that is dropped while its task unwinds a caught
/// panic, passes the notification on. Here no other waiter is left, so the notification is stored as
/// a permit, and the next `notified()` completes.
#[test]
fn notify_one_to_waiter_dropped_during_panic_is_passed_on() {
    check_dfs(|| {
        let notify = Arc::new(Notify::new());
        {
            let notify = notify.clone();
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                let mut notified = Box::pin(notify.notified());
                assert!(!notified.as_mut().enable());
                notify.notify_one();
                panic!("expected");
            }))
            .unwrap_err();
        }
        future::block_on(notify.notified());
    });
}

/// Regression test: Shuttle must be able to schedule another task between a notifier's previous
/// operation and its `notify_waiters()`.
///
/// The test program wrongly assumes that once it sees `ready`, the notifier has already called
/// `notify_waiters()`, which notifies the registered waiter. That fails when the waiter runs between
/// the notifier's store and its `notify_waiters()`, which Shuttle only explores if
/// `notify_waiters()` takes a scheduling point first.
#[test]
#[should_panic(expected = "not notified yet")]
fn notify_waiters_after_store_is_a_separate_step() {
    check_dfs(|| {
        let notify = Arc::new(Notify::new());
        let ready = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));
        // Registered before the notifier runs, so `notify_waiters()` notifies it.
        let notified = Box::pin(notify.notified());

        let notifier = {
            let (notify, ready) = (notify.clone(), ready.clone());
            shuttle::thread::spawn(move || {
                ready.store(true, Ordering::SeqCst);
                notify.notify_waiters();
            })
        };

        let mut notified = notified;
        if ready.load(Ordering::SeqCst) {
            // BUG (in this test program): assumes `notify_waiters()` has already run.
            assert!(notified.as_mut().enable(), "not notified yet");
        }

        notifier.join().unwrap();
    });
}

/// Regression test: Shuttle must be able to schedule another task between a waiter's previous
/// operation and the first `enable()` (or poll) of its `Notified`.
///
/// The test program wrongly assumes that the notifier can't notify the waiter between the waiter's
/// store and its `enable()`. That fails when the notifier runs between the two, which Shuttle only
/// explores if the first `enable()` takes a scheduling point.
#[test]
#[should_panic(expected = "notified between the store and enable")]
fn first_enable_after_store_is_a_separate_step() {
    check_dfs(|| {
        let notify = Arc::new(Notify::new());
        let ready = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));

        let notifier = {
            let (notify, ready) = (notify.clone(), ready.clone());
            shuttle::thread::spawn(move || {
                if ready.load(Ordering::SeqCst) {
                    notify.notify_waiters();
                }
            })
        };

        let mut notified = Box::pin(notify.notified());
        ready.store(true, Ordering::SeqCst);
        // BUG (in this test program): assumes the notifier can't run before `enable()`.
        assert!(!notified.as_mut().enable(), "notified between the store and enable");

        notifier.join().unwrap();
    });
}

/// Regression test: Shuttle must be able to schedule another task between two `enable()` calls on
/// the same `Notified`. `enable()` registers no waker, so the second call doesn't follow a wake-up.
///
/// The test program wrongly assumes that no notification arrives between the two calls. That fails
/// when the notifier runs between them, which Shuttle only explores if the second `enable()` takes a
/// scheduling point.
#[test]
#[should_panic(expected = "notified between the two enables")]
fn notified_between_two_enables() {
    check_dfs(|| {
        let notify = Arc::new(Notify::new());
        let notifier = {
            let notify = notify.clone();
            shuttle::thread::spawn(move || notify.notify_one())
        };

        let mut notified = Box::pin(notify.notified());
        let first = notified.as_mut().enable();
        let second = notified.as_mut().enable();
        // BUG (in this test program): assumes no notification arrives between the two calls.
        assert!(first || !second, "notified between the two enables");

        notifier.join().unwrap();
    });
}

/// Regression test: Shuttle must be able to schedule another task between a task's previous
/// operation and the drop of a `Notified` that `notify_one()` notified, which passes the
/// notification on.
///
/// The test program wrongly assumes that once it sees `ready`, the drop has already passed the
/// notification on, so that a permit is stored. That fails when the waiter runs between the store
/// and the drop, which Shuttle only explores if the drop takes a scheduling point first.
#[test]
#[should_panic(expected = "no permit yet")]
fn drop_of_notified_one_after_store_is_a_separate_step() {
    check_dfs(|| {
        let notify = Arc::new(Notify::new());
        let ready = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));

        // Notified by `notify_one()`, but not completed.
        let mut dropped = Box::pin(notify.notified());
        assert!(!dropped.as_mut().enable());
        notify.notify_one();

        let waiter = {
            let (notify, ready) = (notify.clone(), ready.clone());
            shuttle::thread::spawn(move || {
                let notified = std::pin::pin!(notify.notified());
                if ready.load(Ordering::SeqCst) {
                    // BUG (in this test program): assumes the drop has already passed the
                    // notification on.
                    assert!(notified.enable(), "no permit yet");
                }
            })
        };

        ready.store(true, Ordering::SeqCst);
        drop(dropped);

        waiter.join().unwrap();
    });
}

/// Regression test: Shuttle must be able to schedule another task between a waiter's previous
/// operation and a poll of its `Notified` that follows `enable()`. `enable()` registers no waker, so
/// that poll doesn't follow a wake-up.
///
/// The test program wrongly assumes that the notifier can't notify the waiter between the waiter's
/// store and its poll. That fails when the notifier runs between the two, which Shuttle only
/// explores if the poll takes a scheduling point.
#[test]
#[should_panic(expected = "notified between the store and the poll")]
fn poll_after_enable_and_store_is_a_separate_step() {
    check_dfs(|| {
        let notify = Arc::new(Notify::new());
        let ready = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));

        let notifier = {
            let (notify, ready) = (notify.clone(), ready.clone());
            shuttle::thread::spawn(move || {
                if ready.load(Ordering::SeqCst) {
                    notify.notify_one();
                }
            })
        };

        let mut notified = Box::pin(notify.notified());
        assert!(!notified.as_mut().enable());
        ready.store(true, Ordering::SeqCst);
        let mut cx = Context::from_waker(Waker::noop());
        // BUG (in this test program): assumes the notifier can't run before the poll.
        assert!(
            notified.as_mut().poll(&mut cx).is_pending(),
            "notified between the store and the poll"
        );

        notifier.join().unwrap();
    });
}

/// Regression test: Shuttle must be able to schedule another task between a task's previous
/// operation and the drop of a `Notified` that no `notify_one` or `notify_waiters` notified.
///
/// The test program wrongly assumes that once it sees `ready`, the dropped waiter is gone, so that
/// `notify_one()` stores a permit for the next `enable()`. That fails when the notifier runs between
/// the store and the drop: `notify_one()` then notifies the dropped waiter, and the drop passes the
/// notification on only later. Shuttle only explores this if the drop takes a scheduling point
/// first.
#[test]
#[should_panic(expected = "no permit yet")]
fn drop_of_unnotified_waiter_after_store_is_a_separate_step() {
    check_dfs(|| {
        let notify = Arc::new(Notify::new());
        let ready = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));

        let notifier = {
            let (notify, ready) = (notify.clone(), ready.clone());
            shuttle::thread::spawn(move || {
                let mut next = Box::pin(notify.notified());
                if ready.load(Ordering::SeqCst) {
                    notify.notify_one();
                    // BUG (in this test program): assumes the dropped waiter is gone.
                    assert!(next.as_mut().enable(), "no permit yet");
                }
            })
        };

        let mut dropped = Box::pin(notify.notified());
        assert!(!dropped.as_mut().enable());
        ready.store(true, Ordering::SeqCst);
        drop(dropped);

        notifier.join().unwrap();
    });
}

/// The state of a waiter when the test starts.
#[derive(Clone, Copy, Debug)]
enum Start {
    /// Registered with `notified()`, but neither enabled nor polled.
    Registered,
    /// Enabled with `enable()`.
    Enabled,
    /// Polled once, with a waker. The poll returned `Pending`.
    Polled,
}

/// An operation of the operating thread in `yield_before_drop_finds_no_new_outcome`.
#[derive(Clone, Copy, Debug)]
enum Op {
    NotifyOne,
    NotifyWaiters,
    /// `enable()` on the first waiter that the operating thread holds, if it holds one.
    EnableFirst,
    /// A new waiter, registered with `notified()`.
    Register,
}

fn start_waiter(notify: &'static Notify, start: Start) -> Pin<Box<Notified<'static>>> {
    let mut waiter = Box::pin(notify.notified());
    match start {
        Start::Registered => {}
        Start::Enabled => assert!(!waiter.as_mut().enable()),
        Start::Polled => {
            let mut cx = Context::from_waker(Waker::noop());
            assert!(waiter.as_mut().poll(&mut cx).is_pending());
        }
    }
    waiter
}

/// What the operating thread read from `ready` before and after its operations, what each of its
/// `enable()` calls returned, how many of its waiters are ready at the end, and whether a permit is
/// stored at the end.
type Outcome = (bool, bool, Vec<bool>, usize, bool);

/// The outcomes that Shuttle finds for one scenario.
fn outcomes(
    dropped: Start,
    others: &'static [Start],
    ops: &'static [Op],
    shift: usize,
    yield_before_drop: bool,
) -> HashSet<Outcome> {
    let seen = Arc::new(Mutex::new(HashSet::new()));
    let seen_in_test = seen.clone();
    let runner = shuttle::Runner::new(DfsScheduler::new(None, true), shuttle::Config::default());
    runner.run(move || {
        // DFS gives every execution the same random data. Drawing `shift` values first changes
        // the random data that `notify_one` uses.
        for _ in 0..shift {
            let _: u64 = shuttle::rand::thread_rng().gen();
        }
        // Leaked, so that the waiters can move to other threads.
        let notify: &'static Notify = Box::leak(Box::new(Notify::new()));
        let ready = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));

        let waiter = start_waiter(notify, dropped);
        let held: Vec<_> = others.iter().map(|&start| start_waiter(notify, start)).collect();

        let dropping = {
            let ready = ready.clone();
            shuttle::thread::spawn(move || {
                ready.store(true, Ordering::SeqCst);
                if yield_before_drop {
                    shuttle::thread::yield_now();
                }
                drop(waiter);
            })
        };
        let operating = {
            let ready = ready.clone();
            shuttle::thread::spawn(move || {
                let mut held = held;
                let mut enabled = vec![];
                let before = ready.load(Ordering::SeqCst);
                for op in ops {
                    match op {
                        Op::NotifyOne => notify.notify_one(),
                        Op::NotifyWaiters => notify.notify_waiters(),
                        Op::EnableFirst => {
                            if let Some(waiter) = held.first_mut() {
                                enabled.push(waiter.as_mut().enable());
                            }
                        }
                        Op::Register => held.push(Box::pin(notify.notified())),
                    }
                }
                let after = ready.load(Ordering::SeqCst);
                (before, after, enabled, held)
            })
        };

        dropping.join().unwrap();
        let (before, after, enabled, mut held) = operating.join().unwrap();
        let ready_count = held
            .iter_mut()
            .map(|waiter| waiter.as_mut().enable())
            .filter(|&ready| ready)
            .count();
        let permit = Box::pin(notify.notified()).as_mut().enable();
        seen_in_test
            .lock()
            .unwrap()
            .insert((before, after, enabled, ready_count, permit));
    });
    Arc::try_unwrap(seen).unwrap().into_inner().unwrap()
}

/// A `yield_now()` right before the drop of a `Notified` lets Shuttle find no new outcome. This
/// holds if the drop's own scheduling point precedes every access that the drop makes.
///
/// Each scenario has two threads. The dropping thread stores `true` in `ready`, and then drops a
/// waiter that no `notify_one` or `notify_waiters` notified. The operating thread reads `ready`, runs
/// one or two operations on the same `Notify`, and reads `ready` again. The test runs each scenario
/// with DFS twice, once with a `yield_now()` between the store and the drop. Both runs must find the
/// same outcomes.
///
/// The scenarios cover each start state of the dropped waiter, zero to two other waiters in each
/// start state, and each sequence of one or two operations.
///
/// `notify_one` picks a waiter at random, and DFS doesn't explore that choice. So an outcome counts
/// the ready waiters, and doesn't name them. Each run also repeats with 8 different random data
/// streams.
#[test]
fn yield_before_drop_finds_no_new_outcome() {
    const STARTS: [Start; 3] = [Start::Registered, Start::Enabled, Start::Polled];
    const OPS: [Op; 4] = [Op::NotifyOne, Op::NotifyWaiters, Op::EnableFirst, Op::Register];
    const SHIFTS: usize = 8;

    let mut others: Vec<Vec<Start>> = vec![vec![]];
    others.extend(STARTS.iter().map(|&start| vec![start]));
    others.extend(STARTS.iter().flat_map(|&a| STARTS.iter().map(move |&b| vec![a, b])));
    let mut op_sequences: Vec<Vec<Op>> = OPS.iter().map(|&op| vec![op]).collect();
    op_sequences.extend(OPS.iter().flat_map(|&a| OPS.iter().map(move |&b| vec![a, b])));

    let mut differences = vec![];
    for dropped in STARTS {
        for others in &others {
            let others: &'static [Start] = Box::leak(others.clone().into_boxed_slice());
            for ops in &op_sequences {
                let ops: &'static [Op] = Box::leak(ops.clone().into_boxed_slice());
                let mut without = HashSet::new();
                let mut with = HashSet::new();
                for shift in 0..SHIFTS {
                    without.extend(outcomes(dropped, others, ops, shift, false));
                    with.extend(outcomes(dropped, others, ops, shift, true));
                }
                if without != with {
                    differences.push(format!(
                        "dropped {dropped:?}, others {others:?}, ops {ops:?}: only without the \
                        scheduling point {:?}, only with it {:?}",
                        without.difference(&with).collect::<Vec<_>>(),
                        with.difference(&without).collect::<Vec<_>>()
                    ));
                }
            }
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}
