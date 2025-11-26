use shuttle_tokio_impl_inner::time::{clear_triggers, sleep, sleep_until, Duration, Instant};
use test_log::test;

fn sleep_test(duration: Duration) {
    shuttle::check_dfs(
        move || {
            shuttle::future::block_on(async move {
                let _jh = shuttle::future::spawn(async move {
                    sleep(duration).await;
                    panic!();
                });
            });
        },
        None,
    );
}

fn sleep_until_test(deadline: Instant) {
    shuttle::check_dfs(
        move || {
            shuttle::future::block_on(async move {
                let _jh = shuttle::future::spawn(async move {
                    sleep_until(deadline).await;
                    panic!();
                });
            });
        },
        None,
    );
}

#[test]
#[should_panic(expected = "explicit panic")]
fn sleep_panic() {
    sleep_test(Duration::from_secs(10_0000));
}

#[test]
fn sleep_forever() {
    sleep_test(Duration::MAX);
}

#[test]
#[should_panic(expected = "explicit panic")]
fn sleep_until_panic() {
    sleep_until_test(Instant::now() + Duration::from_micros(500));
}

// Test that `deadline`, `is_elapsed` and `reset` work as expected.
#[test]
fn sleep_reset() {
    let old_deadline = Instant::now() - Duration::from_secs(5);
    let mut sleep = sleep_until(old_deadline);
    assert_eq!(sleep.deadline(), old_deadline);
    assert!(sleep.is_elapsed());

    let new_deadline = Instant::now() + Duration::from_secs(100);
    let pinned = std::pin::Pin::new(&mut sleep);
    pinned.reset(new_deadline);
    assert_eq!(sleep.deadline(), new_deadline);
    assert!(!sleep.is_elapsed());
}

mod timeout_tests {
    use super::*;
    use futures::future::join_all;
    use shuttle::current::{me, set_label_for_task, Labels};
    use shuttle::future;
    use shuttle_tokio_impl_inner::sync::Mutex;
    use shuttle_tokio_impl_inner::time::{timeout, trigger_timeouts};
    use std::sync::Arc;
    use test_log::test;

    #[derive(Clone, Debug)]
    struct Label(usize);

    // Create an instance of the Dining Philosophers problem with `count` philosophers.
    // The i'th philosopher grabs forks i and i+1 (modulo count).
    async fn dining_philosophers(count: usize, trigger: bool) {
        clear_triggers();
        let forks = (0..count).map(|_| Arc::new(Mutex::new(0))).collect::<Vec<_>>();

        let mut handles = Vec::new();
        for i in 0..count {
            let left_fork = forks[i].clone();
            let right_fork = forks[(i + 1) % count].clone();
            let h = future::spawn(async move {
                _ = set_label_for_task(me(), Label(i));
                let l = timeout(Duration::from_secs(1), left_fork.lock()).await;
                let r = timeout(Duration::from_secs(1), right_fork.lock()).await;
                l.is_err() || r.is_err()
            });
            handles.push(h);
        }

        if trigger {
            // Trigger timeout on the middle philosopher
            trigger_timeouts(move |labels: &Labels| labels.get::<Label>().expect("task label not set").0 == count / 2);
        }

        let _ = join_all(handles).await;
    }

    #[test]
    fn dining_philosophers_no_deadlock() {
        shuttle::check_random(
            || {
                shuttle::future::block_on(async move {
                    dining_philosophers(5, true).await;
                });
            },
            10_000,
        );
    }

    // Check that if we don't enable timeouts, the philosophers can deadlock.
    #[test]
    #[should_panic(expected = "deadlock")]
    fn dining_philosophers_with_deadlock() {
        shuttle::check_random(
            || {
                shuttle::future::block_on(async move {
                    dining_philosophers(5, false).await;
                });
            },
            10_000,
        );
    }

    // Do an exhaustive check for 2 philosophers.  Since this test takes a while, we mark it ignored.
    #[ignore]
    #[test]
    fn dining_philosophers_no_deadlock_exhaustive() {
        shuttle::check_dfs(
            || {
                shuttle::future::block_on(async move {
                    dining_philosophers(2, true).await;
                });
            },
            None,
        );
    }
}
