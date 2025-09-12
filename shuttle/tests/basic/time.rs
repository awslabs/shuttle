use shuttle::sync::time::{Duration, Instant};
use shuttle::thread;

#[test]
fn test_blocking_sleep() {
    shuttle::check_random(
        || {
            let start = Instant::now();
            thread::sleep(Duration::from_millis(100));
            let elapsed = start.elapsed();
            assert!(elapsed >= Duration::from_millis(100));
        },
        10,
    );
}

#[test]
fn test_elapsed_time() {
    shuttle::check_random(
        || {
            let start = Instant::now();
            // Do some work without blocking
            for _ in 0..1000 {
                std::hint::black_box(42);
            }
            let mid = Instant::now();
            let elapsed = mid - start;
            assert!(elapsed >= Duration::from_nanos(0));
        },
        10,
    );
}
