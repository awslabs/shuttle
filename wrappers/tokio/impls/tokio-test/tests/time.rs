//! A `MockTask`'s waker locks a Shuttle `Mutex` and notifies a Shuttle `Condvar`, so these tests check that Shuttle's
//! time model can wake it, both from the scheduler when no task is runnable and at a scheduling step.
use shuttle::time::constant_stepped::ConstantSteppedTimeModel;
use shuttle::time::{async_sleep, async_timeout, Duration};
use shuttle::{scheduler::RandomScheduler, thread, Config, Runner};
use shuttle_tokio_test_impl::task;

#[test]
fn mock_task_woken_while_no_task_is_runnable() {
    shuttle::check_random(
        || {
            let mut t = task::spawn(async_sleep(Duration::from_millis(10)));
            assert!(t.poll().is_pending());
            thread::sleep(Duration::from_millis(20));
            assert!(t.is_woken());
            assert!(t.poll().is_ready());
        },
        10,
    );
}

#[test]
fn mock_task_timeout_woken_while_no_task_is_runnable() {
    shuttle::check_random(
        || {
            let mut t = task::spawn(async_timeout(Duration::from_millis(10), std::future::pending::<()>()));
            assert!(t.poll().is_pending());
            thread::sleep(Duration::from_millis(20));
            assert!(t.is_woken());
            assert!(t.poll().is_ready());
        },
        10,
    );
}

#[test]
fn mock_task_woken_at_a_scheduling_step() {
    let time_model = ConstantSteppedTimeModel::new(std::time::Duration::from_millis(5));
    Runner::new_with_time_model(RandomScheduler::new(10), time_model, Config::new()).run(|| {
        let mut t = task::spawn(async_sleep(Duration::from_millis(50)));
        assert!(t.poll().is_pending());
        for _ in 0..100 {
            thread::yield_now();
        }
        assert!(t.is_woken());
    });
}
