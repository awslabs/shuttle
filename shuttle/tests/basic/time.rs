use shuttle::scheduler::{DfsScheduler, RandomScheduler};
use shuttle::time::constant_stepped::ConstantSteppedTimeModel;
use shuttle::time::{async_interval, async_sleep, async_timeout, Duration, Instant};
use shuttle::{future, thread, Config, Runner};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use test_log::test;
use tracing::trace;

#[test]
fn test_stepped_blocking_sleep() {
    let time_model = ConstantSteppedTimeModel::new(std::time::Duration::from_micros(10));
    let scheduler = RandomScheduler::new(10);
    let runner = Runner::new_with_time_model(scheduler, time_model, Config::new());
    runner.run(|| {
        let start = Instant::now();
        thread::sleep(Duration::from_millis(50) + Duration::from_millis(50));
        let elapsed = start.elapsed();
        assert!(elapsed.as_millis() == 100);
    });
}

/// This test includes 3 scheduling points which should advance the global time:
/// - 1 spawn and 1 yield on the main thread
/// - 1 yield on the child thread
///
/// Thus with a stepped time, it should be possible for either side of the condition to be met:
/// - True => both threads have executed (3 events * time_step)
/// - False => only the main thread has executed (2 events * time_step)
#[test]
fn test_stepped_elapsed_time() {
    let less_than_count = Arc::new(AtomicUsize::new(0));
    let greater_than_count = Arc::new(AtomicUsize::new(0));

    let time_step = Duration::from_micros(10);

    let time_model = ConstantSteppedTimeModel::new(std::time::Duration::from_micros(10));
    let scheduler = DfsScheduler::new(None, false);
    let runner = Runner::new_with_time_model(scheduler, time_model, Config::new());

    let less_count_inner = less_than_count.clone();
    let greater_count_inner = greater_than_count.clone();

    runner.run(move || {
        let start = Instant::now();
        thread::spawn(|| {
            thread::yield_now();
        });
        thread::yield_now();
        let elapsed = start.elapsed();
        trace!("elapsed {:?}", elapsed);

        if elapsed > 2 * time_step {
            greater_count_inner.fetch_add(1, Ordering::SeqCst);
        } else {
            less_count_inner.fetch_add(1, Ordering::SeqCst);
        }
    });

    let less_count = less_than_count.load(Ordering::SeqCst);
    let greater_count = greater_than_count.load(Ordering::SeqCst);

    assert!(
        less_count > 0,
        "Expected some executions with elapsed <= {:?}, got {}",
        time_step * 2,
        less_count
    );
    assert!(
        greater_count > 0,
        "Expected some executions with elapsed > {:?}, got {}",
        time_step * 2,
        greater_count
    );
}

#[test]
fn test_stepped_async_sleep() {
    let time_model = ConstantSteppedTimeModel::new(std::time::Duration::from_micros(10));
    let scheduler = RandomScheduler::new(10);
    let runner = Runner::new_with_time_model(scheduler, time_model, Config::new());
    runner.run(|| {
        future::block_on(async {
            let start = Instant::now();
            async_sleep(Duration::from_millis(50)).await;
            let elapsed = start.elapsed();
            assert_eq!(elapsed.as_millis(), 50);
        });
    });
}

#[test]
fn test_stepped_timeout_expired() {
    let time_model = ConstantSteppedTimeModel::new(std::time::Duration::from_micros(10));
    let scheduler = RandomScheduler::new(10);
    let runner = Runner::new_with_time_model(scheduler, time_model, Config::new());
    runner.run(|| {
        future::block_on(async {
            let start = Instant::now();
            let result = async_timeout(Duration::from_millis(50), async {
                async_sleep(Duration::from_millis(100)).await;
                42
            })
            .await;
            assert!(result.is_err());
            assert_eq!(start.elapsed().as_millis(), 50);
        });
    });
}

#[test]
fn test_stepped_timeout_not_expired() {
    let time_model = ConstantSteppedTimeModel::new(std::time::Duration::from_micros(10));
    let scheduler = RandomScheduler::new(10);
    let runner = Runner::new_with_time_model(scheduler, time_model, Config::new());
    runner.run(|| {
        future::block_on(async {
            let result = async_timeout(Duration::from_millis(50), async {
                async_sleep(Duration::from_millis(20)).await;
                42
            })
            .await;
            assert_eq!(result.unwrap(), 42);
        });
    });
}

#[test]
fn test_async_interval() {
    let time_model = ConstantSteppedTimeModel::new(std::time::Duration::from_micros(10));
    let scheduler = RandomScheduler::new(10);
    let runner = Runner::new_with_time_model(scheduler, time_model, Config::new());
    runner.run(|| {
        future::block_on(async {
            let mut interval = async_interval(Duration::from_millis(10));
            let start = Instant::now();
            interval.tick().await;
            let first_tick = start.elapsed();
            interval.tick().await;
            let second_tick = start.elapsed();
            interval.tick().await;
            let third_tick = start.elapsed();

            assert_eq!(first_tick.as_millis(), 0);
            assert_eq!(second_tick.as_millis(), 10);
            assert_eq!(third_tick.as_millis(), 20);
        });
    });
}

/// Probabilistically force a switch for random schedulers [P(no switch) = 1/T^bound for T threads]
/// Returns true if the condition ever holds while spinning.
fn spin_switch_and_get_any<F>(condition: F) -> bool
where
    F: Fn() -> bool,
{
    let bound = 10000;
    for _ in 0..bound {
        thread::yield_now();
        if condition() {
            return true;
        };
    }
    false
}

#[test]
fn test_stepped_sleep_woken_by_thread_steps() {
    let time_model = ConstantSteppedTimeModel::new(std::time::Duration::from_millis(10));
    let scheduler = RandomScheduler::new(10);
    let runner = Runner::new_with_time_model(scheduler, time_model, Config::new());

    runner.run(|| {
        let start = Instant::now();
        let woken_at = Arc::new(std::sync::Mutex::new(None));
        let sleep_completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sleep_completed_clone = sleep_completed.clone();
        let woken_at_clone = woken_at.clone();

        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            *woken_at_clone.lock().unwrap() = Some(Instant::now());
            sleep_completed_clone.store(true, Ordering::SeqCst);
        });

        // Take many steps to advance time and wake the sleeping thread. Only this thread takes steps while the other
        // one sleeps, so it takes at least 10 of them, at 10ms each.
        let yields = std::cell::Cell::new(0);
        assert!(spin_switch_and_get_any(|| {
            yields.set(yields.get() + 1);
            sleep_completed.load(Ordering::SeqCst)
        }));
        assert!(yields.get() >= 10, "woke after {} yields", yields.get());
        assert!(woken_at.lock().unwrap().unwrap() - start >= Duration::from_millis(100));
    });
}

mod regressions {
    use super::*;
    use futures::future::{pending, poll_fn, ready};
    use shuttle::time::{advance, get_time_model};
    use std::cell::Cell;
    use std::future::Future;
    use std::task::{Context, Poll, Wake, Waker};
    use test_log::test;

    /// Moves the clock to the next deadline and wakes the timers that expire there, as the scheduler does
    /// when no task is runnable. Returns false if no timer is registered.
    fn fire_next_timer() -> bool {
        let time_model = get_time_model();
        let expired = {
            let mut time_model = time_model.borrow_mut();
            if !time_model.advance_to_next_deadline() {
                return false;
            }
            time_model.take_expired()
        };
        expired.into_iter().for_each(Waker::wake);
        true
    }

    #[derive(Default)]
    struct CountWake(AtomicUsize);

    impl Wake for CountWake {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn poll_tick_advances_to_the_next_period() {
        shuttle::check_random(
            || {
                future::block_on(async {
                    let period = Duration::from_millis(10);
                    let mut interval = async_interval(period);
                    let first = poll_fn(|cx| interval.poll_tick(cx)).await;
                    let second = poll_fn(|cx| interval.poll_tick(cx)).await;
                    assert_eq!(second - first, period);
                });
            },
            1,
        );
    }

    #[test]
    fn tick_and_poll_tick_share_progress() {
        shuttle::check_random(
            || {
                future::block_on(async {
                    let period = Duration::from_millis(10);
                    let mut interval = async_interval(period);
                    let first = interval.tick().await;
                    assert_eq!(poll_fn(|cx| interval.poll_tick(cx)).await, first + period);
                    assert_eq!(interval.tick().await, first + period * 2);
                    assert_eq!(poll_fn(|cx| interval.poll_tick(cx)).await, first + period * 3);
                });
            },
            1,
        );
    }

    #[test]
    fn completed_sleep_reports_elapsed_at_its_deadline() {
        shuttle::check_random(
            || {
                future::block_on(async {
                    let mut sleep = Box::pin(async_sleep(Duration::from_millis(10)));
                    sleep.as_mut().await;
                    assert_eq!(Instant::now(), sleep.deadline());
                    assert!(sleep.is_elapsed());
                    // As in tokio, a sleep is elapsed only once it has been polled or reset.
                    let mut zero = Box::pin(async_sleep(Duration::ZERO));
                    assert!(!zero.is_elapsed());
                    zero.as_mut().await;
                    assert!(zero.is_elapsed());
                    let mut reset = Box::pin(async_sleep(Duration::from_secs(1)));
                    reset.as_mut().reset(Instant::now());
                    assert!(reset.is_elapsed());
                    reset.as_mut().reset(Instant::now() + Duration::from_secs(1));
                    assert!(!reset.is_elapsed());
                });
            },
            1,
        );
    }

    #[test]
    fn zero_timeout_accepts_an_immediately_ready_future() {
        shuttle::check_random(
            || {
                assert_eq!(future::block_on(async_timeout(Duration::ZERO, ready(42))), Ok(42));
                assert!(future::block_on(async_timeout(Duration::ZERO, pending::<()>())).is_err());
            },
            1,
        );
    }

    #[test]
    fn interval_reset_waits_one_period_from_now() {
        shuttle::check_random(
            || {
                future::block_on(async {
                    let period = Duration::from_millis(10);
                    let mut interval = async_interval(period);
                    interval.tick().await;
                    advance(Duration::from_millis(25));
                    let reset_at = Instant::now();
                    interval.reset();
                    assert_eq!(interval.tick().await, reset_at + period);
                    assert_eq!(interval.tick().await, reset_at + period * 2);
                });
            },
            1,
        );
    }

    #[test]
    fn interval_start_is_set_at_construction() {
        shuttle::check_random(
            || {
                future::block_on(async {
                    let constructed_at = Instant::now();
                    let period = Duration::from_millis(10);
                    let mut interval = async_interval(period);
                    advance(Duration::from_millis(25));
                    assert_eq!(interval.tick().await, constructed_at);
                    assert_eq!(interval.tick().await, constructed_at + period);
                    assert_eq!(interval.tick().await, constructed_at + period * 2);
                    assert_eq!(interval.tick().await, constructed_at + period * 3);
                });
            },
            1,
        );
    }

    #[test]
    fn resetting_a_pending_sleep_updates_its_wakeup() {
        shuttle::check_random(
            || {
                future::block_on(async {
                    let start = Instant::now();
                    let mut sleep = Box::pin(async_sleep(Duration::from_secs(10)));
                    let mut reset = false;
                    poll_fn(|cx| {
                        let result = sleep.as_mut().poll(cx);
                        if !reset {
                            sleep.as_mut().reset(start + Duration::from_secs(1));
                            reset = true;
                        }
                        result
                    })
                    .await;
                    assert_eq!(start.elapsed(), Duration::from_secs(1));
                });
            },
            1,
        );
    }

    #[test]
    fn resetting_a_completed_sleep_registers_the_next_wakeup() {
        shuttle::check_random(
            || {
                let start = Instant::now();
                let counts = Arc::new(CountWake::default());
                let waker = Waker::from(counts.clone());
                let mut cx = Context::from_waker(&waker);
                let mut sleep = Box::pin(async_sleep(Duration::from_secs(1)));
                assert!(sleep.as_mut().poll(&mut cx).is_pending());
                advance(Duration::from_secs(1));
                assert_eq!(counts.0.load(Ordering::SeqCst), 1);
                assert!(sleep.as_mut().poll(&mut cx).is_ready());
                sleep.as_mut().reset(start + Duration::from_secs(3));
                // Reset must arm the timer even without another poll.
                advance(Duration::from_secs(2));
                assert_eq!(counts.0.load(Ordering::SeqCst), 2);
                assert!(sleep.as_mut().poll(&mut cx).is_ready());
            },
            1,
        );
    }

    #[test]
    fn resetting_to_an_elapsed_deadline_wakes_the_waiter() {
        shuttle::check_random(
            || {
                let counts = Arc::new(CountWake::default());
                let waker = Waker::from(counts.clone());
                let mut cx = Context::from_waker(&waker);
                let mut sleep = Box::pin(async_sleep(Duration::from_secs(10)));
                assert!(sleep.as_mut().poll(&mut cx).is_pending());
                sleep.as_mut().reset(Instant::now());
                assert_eq!(counts.0.load(Ordering::SeqCst), 1);
                assert!(sleep.as_mut().poll(&mut cx).is_ready());
                assert!(!fire_next_timer());
            },
            1,
        );
    }

    #[test]
    fn extending_a_sleep_removes_the_old_deadline() {
        shuttle::check_random(
            || {
                let start = Instant::now();
                let counts = Arc::new(CountWake::default());
                let waker = Waker::from(counts.clone());
                let mut cx = Context::from_waker(&waker);
                let mut sleep = Box::pin(async_sleep(Duration::from_secs(10)));
                assert!(sleep.as_mut().poll(&mut cx).is_pending());
                sleep.as_mut().reset(start + Duration::from_secs(20));
                assert!(fire_next_timer());
                assert_eq!(start.elapsed(), Duration::from_secs(20));
                assert_eq!(counts.0.load(Ordering::SeqCst), 1);
                assert!(sleep.as_mut().poll(&mut cx).is_ready());
            },
            1,
        );
    }

    #[test]
    fn dropping_sleep_releases_its_waker_and_deadline() {
        shuttle::check_random(
            || {
                let start = Instant::now();
                let counts = Arc::new(CountWake::default());
                let waker = Waker::from(counts.clone());
                let mut cx = Context::from_waker(&waker);
                let mut sleep = Box::pin(async_sleep(Duration::from_secs(3600)));
                for _ in 0..100 {
                    assert!(sleep.as_mut().poll(&mut cx).is_pending());
                }
                assert!(Arc::strong_count(&counts) > 2);
                drop(sleep);
                assert_eq!(Arc::strong_count(&counts), 2);
                assert!(!fire_next_timer());
                assert_eq!(Instant::now(), start);
            },
            1,
        );
    }

    #[test]
    fn sleep_wakes_only_the_latest_registered_waker() {
        shuttle::check_random(
            || {
                let first = Arc::new(CountWake::default());
                let second = Arc::new(CountWake::default());
                let first_waker = Waker::from(first.clone());
                let second_waker = Waker::from(second.clone());
                let mut sleep = Box::pin(async_sleep(Duration::from_secs(1)));
                assert!(sleep.as_mut().poll(&mut Context::from_waker(&first_waker)).is_pending());
                assert!(sleep
                    .as_mut()
                    .poll(&mut Context::from_waker(&second_waker))
                    .is_pending());
                advance(Duration::from_secs(1));
                assert_eq!(first.0.load(Ordering::SeqCst), 0);
                assert_eq!(second.0.load(Ordering::SeqCst), 1);
            },
            1,
        );
    }

    #[test]
    fn timeout_releases_its_registration_when_the_inner_future_completes() {
        shuttle::check_random(
            || {
                let complete = Cell::new(false);
                let counts = Arc::new(CountWake::default());
                let waker = Waker::from(counts.clone());
                let mut cx = Context::from_waker(&waker);
                let inner = poll_fn(|_| if complete.get() { Poll::Ready(42) } else { Poll::Pending });
                let mut timeout = Box::pin(async_timeout(Duration::from_secs(3600), inner));
                assert!(timeout.as_mut().poll(&mut cx).is_pending());
                complete.set(true);
                assert_eq!(timeout.as_mut().poll(&mut cx), Poll::Ready(Ok(42)));
                assert_eq!(Arc::strong_count(&counts), 2);
                assert!(!fire_next_timer());
            },
            1,
        );
    }

    #[test]
    fn dropping_timeout_releases_its_registration() {
        shuttle::check_random(
            || {
                let counts = Arc::new(CountWake::default());
                let waker = Waker::from(counts.clone());
                let mut cx = Context::from_waker(&waker);
                let mut timeout = Box::pin(async_timeout(Duration::from_secs(3600), pending::<()>()));
                assert!(timeout.as_mut().poll(&mut cx).is_pending());
                drop(timeout);
                assert_eq!(Arc::strong_count(&counts), 2);
                assert!(!fire_next_timer());
            },
            1,
        );
    }

    #[test]
    fn dropping_interval_cancels_a_pending_tick() {
        shuttle::check_random(
            || {
                let counts = Arc::new(CountWake::default());
                let waker = Waker::from(counts.clone());
                let mut cx = Context::from_waker(&waker);
                let mut interval = async_interval(Duration::from_secs(1));
                assert!(interval.poll_tick(&mut cx).is_ready());
                assert!(interval.poll_tick(&mut cx).is_pending());
                drop(interval);
                assert_eq!(Arc::strong_count(&counts), 2);
                assert!(!fire_next_timer());
            },
            1,
        );
    }

    #[test]
    fn dropping_a_timer_from_an_old_execution_does_not_cancel_a_new_timer() {
        let saved = Arc::new(std::sync::Mutex::new(None));
        let old = saved.clone();
        shuttle::check_random(
            move || {
                let mut sleep = Box::pin(async_sleep(Duration::from_secs(10)));
                assert!(sleep
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending());
                *old.lock().unwrap() = Some(sleep);
            },
            1,
        );

        shuttle::check_random(
            move || {
                future::block_on(async {
                    let start = Instant::now();
                    let mut sleep = Box::pin(async_sleep(Duration::from_secs(1)));
                    poll_fn(|cx| {
                        let result = sleep.as_mut().poll(cx);
                        // Timer ids restart in each execution. Dropping the old timer must not
                        // deregister the current execution's timer with that same id.
                        drop(saved.lock().unwrap().take());
                        result
                    })
                    .await;
                    assert_eq!(start.elapsed(), Duration::from_secs(1));
                });
            },
            1,
        );
    }
}

/// The time model wakes timers both from the scheduler, when no task is runnable, and at the end of a scheduling step.
/// A waker may call back into Shuttle, so these tests use wakers that do.
mod wakers {
    use super::*;
    use shuttle::sync::atomic::AtomicBool;
    use shuttle::sync::mpsc::{sync_channel, SyncSender};
    use std::future::Future;
    use std::pin::{pin, Pin};
    use std::sync::Mutex as StdMutex;
    use std::task::{Context, Poll, Wake, Waker};
    use test_log::test;

    /// The `Wake` example from the std docs, with Shuttle's threads. `Thread::unpark` is a scheduling point.
    struct ThreadWaker(thread::Thread);

    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    fn park_block_on<T>(fut: impl Future<Output = T>) -> T {
        let mut fut = pin!(fut);
        let waker = Arc::new(ThreadWaker(thread::current())).into();
        let mut cx = Context::from_waker(&waker);
        loop {
            match fut.as_mut().poll(&mut cx) {
                Poll::Ready(res) => return res,
                Poll::Pending => thread::park(),
            }
        }
    }

    #[test]
    fn unpark_waker_woken_while_no_task_is_runnable() {
        shuttle::check_random(|| park_block_on(async_sleep(Duration::from_millis(1))), 10);
    }

    #[test]
    fn unpark_waker_of_a_timeout_woken_while_no_task_is_runnable() {
        shuttle::check_random(
            || {
                let result = park_block_on(async_timeout(Duration::from_millis(1), std::future::pending::<()>()));
                assert!(result.is_err());
            },
            10,
        );
    }

    #[test]
    fn unpark_waker_woken_at_a_scheduling_step() {
        let time_model = ConstantSteppedTimeModel::new(std::time::Duration::from_micros(10));
        Runner::new_with_time_model(RandomScheduler::new(10), time_model, Config::new()).run(|| {
            let spinner = thread::spawn(|| {
                for _ in 0..100 {
                    thread::yield_now();
                }
            });
            park_block_on(async_sleep(Duration::from_micros(50)));
            spinner.join().unwrap();
        });
    }

    /// Sets a Shuttle atomic, which is a scheduling point.
    struct FlagWaker(AtomicBool);

    impl Wake for FlagWaker {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn atomic_flag_waker() {
        shuttle::check_random(
            || {
                let flag = Arc::new(FlagWaker(AtomicBool::new(false)));
                let waker = Waker::from(flag.clone());
                let mut cx = Context::from_waker(&waker);
                let mut sleep = pin!(async_sleep(Duration::from_millis(1)));
                assert!(sleep.as_mut().poll(&mut cx).is_pending());
                thread::sleep(Duration::from_millis(2));
                assert!(flag.0.load(Ordering::SeqCst));
                assert!(sleep.as_mut().poll(&mut cx).is_ready());
            },
            10,
        );
    }

    type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

    /// A task of an executor that runs tasks it receives on a Shuttle channel. Its waker sends the task again.
    struct ChannelTask {
        future: StdMutex<Option<BoxFuture>>,
        queue: SyncSender<Arc<ChannelTask>>,
    }

    impl Wake for ChannelTask {
        fn wake(self: Arc<Self>) {
            self.queue.send(self.clone()).expect("queue full");
        }
    }

    #[test]
    fn channel_executor_waker() {
        shuttle::check_random(
            || {
                let (tx, rx) = sync_channel::<Arc<ChannelTask>>(16);
                let task = Arc::new(ChannelTask {
                    future: StdMutex::new(Some(Box::pin(async_sleep(Duration::from_millis(1))))),
                    queue: tx.clone(),
                });
                tx.send(task).unwrap();
                drop(tx);
                // The waker holds a sender, so stop once the only task is done.
                while let Ok(task) = rx.recv() {
                    let mut slot = task.future.lock().unwrap();
                    let mut future = slot.take().unwrap();
                    let waker = Waker::from(task.clone());
                    if future.as_mut().poll(&mut Context::from_waker(&waker)).is_ready() {
                        break;
                    }
                    *slot = Some(future);
                }
            },
            10,
        );
    }

    /// Records when it was woken, then wakes the task.
    struct StampWaker(StdMutex<Option<Instant>>, Waker);

    impl Wake for StampWaker {
        fn wake(self: Arc<Self>) {
            *self.0.lock().unwrap() = Some(Instant::now());
            self.1.wake_by_ref();
        }
    }

    #[test]
    fn waker_reads_the_clock() {
        for step in [std::time::Duration::ZERO, std::time::Duration::from_micros(10)] {
            let runner = Runner::new_with_time_model(
                RandomScheduler::new(10),
                ConstantSteppedTimeModel::new(step),
                Config::new(),
            );
            runner.run(|| {
                future::block_on(async {
                    let start = Instant::now();
                    let mut sleep = pin!(async_sleep(Duration::from_millis(1)));
                    let mut stamp = None;
                    futures::future::poll_fn(|cx| {
                        let waker = stamp
                            .get_or_insert_with(|| Arc::new(StampWaker(StdMutex::new(None), cx.waker().clone())))
                            .clone();
                        sleep.as_mut().poll(&mut Context::from_waker(&Waker::from(waker)))
                    })
                    .await;
                    let woken_at = stamp.unwrap().0.lock().unwrap().unwrap();
                    assert!(woken_at - start >= Duration::from_millis(1));
                });
            });
        }
    }
}

mod advance {
    use super::*;
    use shuttle::sync::atomic::AtomicBool;
    use shuttle::time::frozen::FrozenTimeModel;
    use shuttle::time::{advance, TimeModel, WakerRegistered};
    use std::collections::HashSet;
    use std::future::Future;
    use std::sync::Mutex as StdMutex;
    use std::task::Waker;
    use test_log::test;

    // Other tasks can run between the caller's previous operation and the clock jump, as `advance` has its
    // scheduling point first.
    #[test]
    fn scheduling_point_comes_before_the_clock_jump() {
        let seen = Arc::new(StdMutex::new(HashSet::new()));
        let seen_inner = seen.clone();
        Runner::new(DfsScheduler::new(None, false), Config::new()).run(move || {
            let start = Instant::now();
            let flag = Arc::new(AtomicBool::new(false));
            let child = {
                let flag = flag.clone();
                thread::spawn(move || (flag.load(Ordering::SeqCst), start.elapsed()))
            };
            flag.store(true, Ordering::SeqCst);
            advance(Duration::from_secs(10));
            seen_inner.lock().unwrap().insert(child.join().unwrap());
        });
        let seen = seen.lock().unwrap();
        assert!(seen.contains(&(true, Duration::ZERO)), "{seen:?}");
        assert!(seen.contains(&(true, Duration::from_secs(10))), "{seen:?}");
    }

    // A sleeper whose deadline `advance` passes is runnable right away, so another task can see its effects
    // before the caller's next operation.
    #[test]
    fn wakes_the_timers_it_passes() {
        let woke_first = Arc::new(AtomicUsize::new(0));
        let woke_first_inner = woke_first.clone();
        Runner::new(DfsScheduler::new(None, false), Config::new()).run(move || {
            let flag = Arc::new(AtomicBool::new(false));
            let child = {
                let flag = flag.clone();
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(10));
                    flag.store(true, Ordering::SeqCst);
                })
            };
            advance(Duration::from_millis(20));
            if flag.load(Ordering::SeqCst) {
                woke_first_inner.fetch_add(1, Ordering::SeqCst);
            }
            child.join().unwrap();
        });
        assert!(woke_first.load(Ordering::SeqCst) > 0);
    }

    /// A model with a manual clock: `step` does nothing.
    #[derive(Debug, Default)]
    struct ManualClock(FrozenTimeModel);

    impl TimeModel for ManualClock {
        fn advance_to_next_deadline(&mut self) -> bool {
            self.0.advance_to_next_deadline()
        }
        fn new_execution(&mut self) {
            self.0.new_execution()
        }
        fn step(&mut self) {}
        fn instant(&self) -> Instant {
            self.0.instant()
        }
        fn pause(&mut self) {}
        fn resume(&mut self) {}
        fn advance(&mut self, duration: Duration) {
            self.0.advance(duration)
        }
        fn register_sleep(&mut self, deadline: Instant, id: u64, waker: Waker) -> WakerRegistered {
            self.0.register_sleep(deadline, id, waker)
        }
        fn unregister_sleep(&mut self, id: u64) {
            self.0.unregister_sleep(id)
        }
        fn take_expired(&mut self) -> Vec<Waker> {
            self.0.take_expired()
        }
    }

    #[derive(Default)]
    struct CountWake(AtomicUsize);

    impl std::task::Wake for CountWake {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    // Shuttle fires the timers after every clock change, so a model doesn't have to.
    #[test]
    fn manual_clock_model_wakes_sleepers() {
        let runner = Runner::new_with_time_model(RandomScheduler::new(10), ManualClock::default(), Config::new());
        runner.run(|| {
            let counts = Arc::new(CountWake::default());
            let waker = Waker::from(counts.clone());
            let mut sleep = std::pin::pin!(async_sleep(Duration::from_millis(10)));
            assert!(sleep
                .as_mut()
                .poll(&mut std::task::Context::from_waker(&waker))
                .is_pending());
            advance(Duration::from_millis(20));
            assert_eq!(counts.0.load(Ordering::SeqCst), 1);
        });
    }
}

/// Destructors that sleep, or poll a `Sleep`, when the execution ends.
mod teardown {
    use super::*;
    use futures::FutureExt;
    use test_log::test;

    struct SleepOnDrop;

    impl Drop for SleepOnDrop {
        fn drop(&mut self) {
            thread::sleep(Duration::from_millis(1));
        }
    }

    struct PollSleepOnDrop;

    impl Drop for PollSleepOnDrop {
        fn drop(&mut self) {
            assert!(async_sleep(Duration::from_millis(1)).now_or_never().is_none());
        }
    }

    // The detached task is still waiting when main returns, so its future is dropped during cleanup.
    #[test]
    fn sleep_in_the_destructor_of_a_detached_task() {
        shuttle::check_random(
            || {
                drop(future::spawn(async {
                    let _guard = SleepOnDrop;
                    futures::future::pending::<()>().await;
                }));
                future::block_on(future::yield_now());
            },
            10,
        );
    }

    #[test]
    fn poll_a_sleep_in_the_destructor_of_a_detached_task() {
        shuttle::check_random(
            || {
                drop(future::spawn(async {
                    let _guard = PollSleepOnDrop;
                    futures::future::pending::<()>().await;
                }));
                future::block_on(future::yield_now());
            },
            10,
        );
    }

    shuttle::lazy_static! {
        static ref SLEEPS_ON_DROP: SleepOnDrop = SleepOnDrop;
    }

    // Shuttle drops statics at the end of each execution.
    #[test]
    fn sleep_in_the_destructor_of_a_static() {
        shuttle::check_random(
            || {
                let _ = &*SLEEPS_ON_DROP;
            },
            10,
        );
    }
}

/// Durations too long to represent, as in "sleep forever" or "no timeout".
mod overflow {
    use super::*;
    use shuttle::sync::Mutex;
    use shuttle::MaxSteps;
    use test_log::test;

    const THIRTY_YEARS: Duration = Duration::from_secs(86400 * 365 * 30);

    fn run_stepped(iterations: usize, f: impl Fn() + Send + Sync + 'static) {
        let time_model = ConstantSteppedTimeModel::new(std::time::Duration::from_micros(10));
        Runner::new_with_time_model(RandomScheduler::new(iterations), time_model, Config::new()).run(f);
    }

    #[test]
    fn sleep_for_duration_max() {
        run_stepped(10, || {
            let start = Instant::now();
            thread::sleep(Duration::MAX);
            assert!(start.elapsed() >= THIRTY_YEARS);
            let _ = Instant::now() + Duration::from_millis(1);
        });
    }

    #[test]
    fn timeout_of_duration_max_elapses_when_nothing_else_can_run() {
        run_stepped(10, || {
            let result = future::block_on(async_timeout(Duration::MAX, std::future::pending::<()>()));
            assert!(result.is_err());
        });
    }

    #[test]
    fn the_clock_keeps_room_after_a_sleeper_wakes_at_duration_max() {
        shuttle::check_random(
            || {
                thread::spawn(|| thread::sleep(Duration::MAX)).join().unwrap();
                let _ = Instant::now() + Duration::from_secs(3600);
            },
            10,
        );
    }

    // A finished timeout leaves nothing behind, so a real deadlock is reported as one.
    #[test]
    #[should_panic(expected = "deadlock")]
    fn deadlock_after_a_finished_timeout_of_duration_max() {
        run_stepped(1, || {
            future::block_on(async_timeout(Duration::MAX, async {})).unwrap();
            let lock = Mutex::new(());
            let _first = lock.lock().unwrap();
            let _second = lock.lock().unwrap();
        });
    }

    // Each sleep moves the clock 30 years, but still has a scheduling point.
    #[test]
    #[should_panic(expected = "exceeded max_steps bound")]
    fn sleeping_forever_in_a_loop_hits_max_steps() {
        let mut config = Config::new();
        config.max_steps = MaxSteps::FailAfter(1000);
        Runner::new(RandomScheduler::new(1), config).run(|| loop {
            thread::sleep(Duration::MAX);
        });
    }

    #[test]
    fn interval_with_a_period_of_duration_max() {
        shuttle::check_random(
            || {
                future::block_on(async {
                    let mut interval = async_interval(Duration::MAX);
                    let first = interval.tick().await;
                    let second = interval.tick().await;
                    let third = interval.tick().await;
                    assert!(first < second && second < third);
                });
            },
            1,
        );
    }
}

/// `Instant` behaves like `std::time::Instant` and tokio's.
mod instant {
    use super::*;
    use std::collections::HashSet;
    use test_log::test;

    #[test]
    fn is_hashable() {
        shuttle::check_random(
            || {
                let start = Instant::now();
                thread::sleep(Duration::from_millis(1));
                let instants: HashSet<Instant> = [start, Instant::now(), start].into_iter().collect();
                assert_eq!(instants.len(), 2);
            },
            1,
        );
    }

    #[test]
    fn subtracting_a_later_instant_saturates_to_zero() {
        shuttle::check_random(
            || {
                let deadline = Instant::now() + Duration::from_millis(1);
                thread::sleep(Duration::from_millis(2));
                let now = Instant::now();
                assert_eq!(deadline - now, Duration::ZERO);
                assert_eq!(deadline.duration_since(now), Duration::ZERO);
                assert_eq!(deadline.saturating_duration_since(now), Duration::ZERO);
                assert_eq!(deadline.checked_duration_since(now), None);
                assert_eq!(now - deadline, Duration::from_millis(1));
            },
            1,
        );
    }

    // The "start out expired" idiom: the clock doesn't start at zero, so there is room before the start.
    #[test]
    fn instants_before_the_start_of_the_execution() {
        shuttle::check_random(
            || {
                let ttl = Duration::from_secs(3600);
                let last_refresh = Instant::now() - ttl;
                assert_eq!(last_refresh.elapsed(), ttl);
                assert_eq!(Instant::now().checked_sub(ttl), Some(last_refresh));
            },
            1,
        );
    }

    #[test]
    fn elapsed_of_a_future_instant_is_zero() {
        shuttle::check_random(
            || assert_eq!((Instant::now() + Duration::from_secs(1)).elapsed(), Duration::ZERO),
            1,
        );
    }

    #[test]
    #[should_panic(expected = "overflow when subtracting duration from instant")]
    fn subtracting_too_long_a_duration_panics_as_in_std() {
        shuttle::check_random(
            || {
                let _ = Instant::now() - Duration::MAX;
            },
            1,
        );
    }

    #[test]
    #[should_panic(expected = "overflow when adding duration to instant")]
    fn adding_too_long_a_duration_panics_as_in_std() {
        shuttle::check_random(
            || {
                let _ = Instant::now() + Duration::MAX;
            },
            1,
        );
    }
}

/// `Interval` behaves like tokio's.
mod interval {
    use super::*;
    use shuttle::time::MissedTickBehavior;
    use test_log::test;

    /// The instants of the first `n` ticks after a tick due at 10ms is missed until 35ms, relative to the start.
    fn ticks_after_missing_until_35ms(behavior: MissedTickBehavior, n: usize) -> Vec<Duration> {
        future::block_on(async move {
            let start = Instant::now();
            let mut interval = async_interval(Duration::from_millis(10));
            interval.set_missed_tick_behavior(behavior);
            assert_eq!(interval.missed_tick_behavior(), behavior);
            assert_eq!(interval.tick().await, start);
            thread::sleep(Duration::from_millis(35));
            let mut ticks = Vec::new();
            for _ in 0..n {
                ticks.push(interval.tick().await - start);
            }
            ticks
        })
    }

    #[test]
    fn missed_ticks() {
        shuttle::check_random(
            || {
                let ms = Duration::from_millis;
                assert_eq!(
                    ticks_after_missing_until_35ms(MissedTickBehavior::Burst, 4),
                    [ms(10), ms(20), ms(30), ms(40)]
                );
                assert_eq!(
                    ticks_after_missing_until_35ms(MissedTickBehavior::Delay, 3),
                    [ms(10), ms(45), ms(55)]
                );
                assert_eq!(
                    ticks_after_missing_until_35ms(MissedTickBehavior::Skip, 3),
                    [ms(10), ms(40), ms(50)]
                );
            },
            1,
        );
    }

    #[test]
    fn bursts_by_default() {
        shuttle::check_random(
            || {
                let interval = async_interval(Duration::from_millis(10));
                assert_eq!(interval.missed_tick_behavior(), MissedTickBehavior::Burst);
            },
            1,
        );
    }

    #[test]
    #[should_panic(expected = "`period` must be non-zero.")]
    fn zero_period_panics() {
        shuttle::check_random(
            || {
                let _ = async_interval(Duration::ZERO);
            },
            1,
        );
    }
}

/// `Runner<S>` has one type parameter, whatever the time model.
mod runner {
    use super::*;
    use test_log::test;

    struct Harness {
        runner: Runner<RandomScheduler>,
    }

    fn stepped() -> Runner<RandomScheduler> {
        let time_model = ConstantSteppedTimeModel::new(std::time::Duration::from_millis(1));
        Runner::new_with_time_model(RandomScheduler::new(1), time_model, Config::new())
    }

    #[test]
    fn runners_with_different_time_models_have_the_same_type() {
        for harness in [
            Harness { runner: stepped() },
            Harness {
                runner: Runner::new(RandomScheduler::new(1), Config::new()),
            },
        ] {
            assert_eq!(harness.runner.run(thread::yield_now), 1);
        }
    }
}

/// Failures found under a custom time model replay under the same model.
mod replay {
    use super::*;
    use shuttle::{replay_from_file_with_time_model, replay_with_time_model, FailurePersistence};
    use std::any::Any;
    use std::panic;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;
    use test_log::test;

    fn time_model() -> ConstantSteppedTimeModel {
        ConstantSteppedTimeModel::new(std::time::Duration::from_millis(10))
    }

    // Fails when the sleeper wakes up before main is done yielding, which depends on the time model.
    fn sleeper_wakes_before_main_is_done() {
        let flag = Arc::new(AtomicBool::new(false));
        let child = {
            let flag = flag.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(100));
                flag.store(true, Ordering::SeqCst);
            })
        };
        for _ in 0..50 {
            thread::yield_now();
        }
        assert!(!flag.load(Ordering::SeqCst), "the sleeper woke up first");
        child.join().unwrap();
    }

    fn panic_message(payload: Box<dyn Any + Send>) -> String {
        match payload.downcast::<String>() {
            Ok(message) => *message,
            Err(payload) => payload.downcast::<&str>().map(|m| m.to_string()).unwrap_or_default(),
        }
    }

    /// Runs the test until it fails, and returns the file its failing schedule was saved to.
    fn failing_schedule(dir: &Path) -> PathBuf {
        let dir_inner = dir.to_path_buf();
        let result = panic::catch_unwind(move || {
            let mut config = Config::new();
            config.failure_persistence = FailurePersistence::File(Some(dir_inner));
            Runner::new_with_time_model(RandomScheduler::new(100), time_model(), config)
                .run(sleeper_wakes_before_main_is_done);
        });
        assert_eq!(
            panic_message(result.expect_err("test should fail")),
            "the sleeper woke up first"
        );
        dir.join("schedule000.txt")
    }

    #[test]
    fn replay_a_schedule() {
        let dir = tempfile::tempdir().expect("could not create tempdir");
        let schedule = std::fs::read_to_string(failing_schedule(dir.path())).unwrap();
        let replayed =
            panic::catch_unwind(|| replay_with_time_model(sleeper_wakes_before_main_is_done, &schedule, time_model()));
        assert_eq!(
            panic_message(replayed.expect_err("replay should fail")),
            "the sleeper woke up first"
        );
    }

    #[test]
    fn replay_a_schedule_from_a_file() {
        let dir = tempfile::tempdir().expect("could not create tempdir");
        let path = failing_schedule(dir.path());
        let replayed = panic::catch_unwind(|| {
            replay_from_file_with_time_model(sleeper_wakes_before_main_is_done, path, time_model())
        });
        assert_eq!(
            panic_message(replayed.expect_err("replay should fail")),
            "the sleeper woke up first"
        );
    }
}

/// The default time model, `FrozenTimeModel`: the clock only moves when no task can run, or on `advance`.
mod frozen {
    use super::*;
    use test_log::test;

    #[test]
    fn sleep_moves_the_clock_by_exactly_its_duration() {
        shuttle::check_random(
            || {
                let start = Instant::now();
                thread::sleep(Duration::from_secs(1));
                assert_eq!(start.elapsed(), Duration::from_secs(1));
            },
            10,
        );
    }

    #[test]
    fn the_clock_does_not_move_at_scheduling_points() {
        shuttle::check_random(
            || {
                let start = Instant::now();
                let child = thread::spawn(|| {
                    for _ in 0..10 {
                        thread::yield_now();
                    }
                });
                for _ in 0..10 {
                    thread::yield_now();
                }
                child.join().unwrap();
                assert_eq!(Instant::now(), start);
            },
            10,
        );
    }

    #[test]
    fn sleepers_wake_in_deadline_order() {
        shuttle::check_dfs(
            || {
                let order = Arc::new(std::sync::Mutex::new(Vec::new()));
                let sleepers: Vec<_> = [30, 10, 20]
                    .into_iter()
                    .map(|ms| {
                        let order = order.clone();
                        thread::spawn(move || {
                            thread::sleep(Duration::from_millis(ms));
                            order.lock().unwrap().push(ms);
                        })
                    })
                    .collect();
                for sleeper in sleepers {
                    sleeper.join().unwrap();
                }
                assert_eq!(*order.lock().unwrap(), [10, 20, 30]);
            },
            None,
        );
    }
}
