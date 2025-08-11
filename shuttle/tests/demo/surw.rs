use shuttle::{
    future,
    scheduler::{RandomScheduler, UniformRandomScheduler},
    sync::atomic::{AtomicUsize, Ordering},
    thread, Runner,
};
use std::sync::Arc;
use test_log::test;

// These examples are inspired by the motivating examples of "Selectively Uniform Concurrency Testing" by
// Huan Zhao, Dylan Wolff, Umang Mathur, and Abhik Roychoudhury - ASPLOS '25. Generally they create very
// uneven thread counts, which cause a naive random walk to strongly bias away from schedules in which the
// an event on one short thread is delayed until after many events on other threads.

const NUM_EVENTS: usize = 25;
const NUM_SHUTTLE_ITERATIONS_URW: usize = 1000;

fn test_uneven_execution<F, S>(executor: F, scheduler: S, expect_found: bool)
where
    F: Fn(Arc<AtomicUsize>, Arc<AtomicUsize>) + Send + Sync + 'static,
    S: shuttle::scheduler::Scheduler + 'static,
{
    let has_found_schedule = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let has_found_schedule_inner = Arc::clone(&has_found_schedule);

    let runner = Runner::new(scheduler, Default::default());
    runner.run(move || {
        if has_found_schedule_inner.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }

        let counter = Arc::new(AtomicUsize::new(1usize));
        let counter1 = Arc::clone(&counter);
        let counter2 = Arc::clone(&counter);

        executor(counter1, counter2);

        has_found_schedule_inner.store(counter.load(Ordering::SeqCst) == 0, std::sync::atomic::Ordering::SeqCst);
    });

    assert_eq!(
        expect_found,
        has_found_schedule.load(std::sync::atomic::Ordering::SeqCst)
    );
}

#[test]
fn non_uniform_random_degrades_for_uneven_task_lengths() {
    test_uneven_execution(
        |counter1, counter2| {
            let future1 = async move {
                counter1.store(0, Ordering::SeqCst);
            };
            let future2 = async move {
                for _ in 0..NUM_EVENTS {
                    counter2.store(1, Ordering::SeqCst);
                }
            };

            let t1 = future::spawn(future1);
            let t2 = future::spawn(future2);

            future::block_on(async {
                t1.await.ok();
                t2.await.ok();
            });
        },
        RandomScheduler::new_from_seed(0, NUM_SHUTTLE_ITERATIONS_URW * 10),
        false, // Random scheduler cannot find this bug even with an order of magnitude more executions
    );
}

#[test]
fn uneven_task_lengths_async() {
    test_uneven_execution(
        |counter1, counter2| {
            let future1 = async move {
                counter1.store(0, Ordering::SeqCst);
            };
            let future2 = async move {
                for _ in 0..NUM_EVENTS {
                    counter2.store(1, Ordering::SeqCst);
                }
            };

            let t1 = future::spawn(future1);
            let t2 = future::spawn(future2);

            future::block_on(async {
                t1.await.ok();
                t2.await.ok();
            });
        },
        UniformRandomScheduler::new_from_seed(0, NUM_SHUTTLE_ITERATIONS_URW),
        true,
    );
}

#[test]
fn uneven_thread_lengths() {
    test_uneven_execution(
        |counter1, counter2| {
            let t1 = thread::spawn(move || {
                counter1.store(0, Ordering::SeqCst);
            });
            let t2 = thread::spawn(move || {
                for _ in 0..NUM_EVENTS {
                    counter2.store(1, Ordering::SeqCst);
                }
            });

            t1.join().ok();
            t2.join().ok();
        },
        UniformRandomScheduler::new_from_seed(0, NUM_SHUTTLE_ITERATIONS_URW),
        true,
    );
}

#[test]
fn uneven_task_creation_uniformity() {
    test_uneven_execution(
        |counter1, counter2| {
            let t1 = thread::spawn(move || {
                counter1.store(0, Ordering::SeqCst);
            });

            let mut handles = Vec::new();
            for _i in 0..NUM_EVENTS {
                let counter2 = counter2.clone();
                handles.push(thread::spawn(move || {
                    counter2.store(1, Ordering::SeqCst);
                }));
            }
            for h in handles {
                h.join().ok();
            }
            t1.join().ok();
        },
        UniformRandomScheduler::new_from_seed(0, NUM_SHUTTLE_ITERATIONS_URW),
        true,
    );
}

#[test]
fn non_uniform_degrades_for_uneven_task_creation() {
    test_uneven_execution(
        |counter1, counter2| {
            let t1 = thread::spawn(move || {
                counter1.store(0, Ordering::SeqCst);
            });

            let mut handles = Vec::new();
            for _i in 0..NUM_EVENTS {
                let counter2 = counter2.clone();
                handles.push(thread::spawn(move || {
                    counter2.store(1, Ordering::SeqCst);
                }));
            }
            for h in handles {
                h.join().ok();
            }
            t1.join().ok();
        },
        RandomScheduler::new_from_seed(0, NUM_SHUTTLE_ITERATIONS_URW * 10),
        false,
    );
}
