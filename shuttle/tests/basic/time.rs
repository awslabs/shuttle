use shuttle::scheduler::{DfsScheduler, RandomScheduler};
use shuttle::time::constant_stepped::ConstantSteppedTimeModel;
use shuttle::time::{async_interval, async_sleep, async_timeout, Duration, Instant};
use shuttle::{future, thread, Config, Runner};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
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
        let sleep_completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sleep_completed_clone = sleep_completed.clone();

        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            sleep_completed_clone.store(true, Ordering::SeqCst);
        });

        // Take many steps to advance time and wake the sleeping thread
        assert!(spin_switch_and_get_any(|| sleep_completed.load(Ordering::SeqCst)));
    });
}

mod regressions {
    use super::*;
    use futures::future::{pending, poll_fn, ready};
    use shuttle::time::{advance, get_time_model};
    use std::cell::Cell;
    use std::future::Future;
    use std::task::{Context, Poll, Wake, Waker};

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
                    assert!(async_sleep(Duration::ZERO).is_elapsed());
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
                assert!(!get_time_model().borrow_mut().wake_next());
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
                assert!(get_time_model().borrow_mut().wake_next());
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
                assert!(!get_time_model().borrow_mut().wake_next());
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
                assert!(!get_time_model().borrow_mut().wake_next());
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
                assert!(!get_time_model().borrow_mut().wake_next());
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
                assert!(!get_time_model().borrow_mut().wake_next());
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
