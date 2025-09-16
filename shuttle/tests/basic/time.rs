use shuttle::scheduler::{DfsScheduler, RandomScheduler};
use shuttle::sync::time::constant_stepped::{ConstantSteppedTimeModel, ConstantTimeDistribution, Duration, Instant};
use shuttle::sync::time::TimeModel;
use shuttle::thread;
use shuttle::{Config, Runner};
use std::sync::{Arc, Mutex};
use tracing::trace;

#[test]
fn test_stepped_blocking_sleep() {
    let time_model = TimeModel::ConstantSteppedTimeModel(ConstantSteppedTimeModel::new(ConstantTimeDistribution::new(Duration::from_micros(10))));
    let scheduler = RandomScheduler::new(10);
    let runner = Runner::new_with_time_model(scheduler, time_model, Config::new());
    runner.run(|| {
        let start = Instant::now();
        thread::sleep(Duration::from_millis(50) + Duration::from_millis(50));
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(100));
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
    let less_than_count = Arc::new(Mutex::new(0));
    let greater_than_count = Arc::new(Mutex::new(0));

    let time_step = Duration::from_micros(10);

    let time_model = TimeModel::ConstantSteppedTimeModel(ConstantSteppedTimeModel::new(ConstantTimeDistribution::new(Duration::from_micros(10))));
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
            *greater_count_inner.lock().unwrap() += 1;
        } else {
            *less_count_inner.lock().unwrap() += 1;
        }
    });

    let less_count = *less_than_count.lock().unwrap();
    let greater_count = *greater_than_count.lock().unwrap();

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
