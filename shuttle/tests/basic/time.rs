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
