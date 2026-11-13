//! Tests for timed waits (`Task::block_with_timeout`), which time out only when no task can run.

use shuttle::{check_dfs, check_random, thread};
use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::task::TaskId;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use test_log::test;

/// Blocks the current task in a timed wait with `deadline`, and returns whether the wait timed out.
fn timed_wait(deadline: Duration) -> bool {
    ExecutionState::with(|state| state.current_mut().block_with_timeout(deadline));
    shuttle_engine::runtime::thread::switch();
    ExecutionState::with(|state| state.current_mut().take_timed_out())
}

fn me() -> TaskId {
    ExecutionState::me()
}

fn unblock(task: TaskId) {
    ExecutionState::with(|state| state.get_mut(task).unblock());
}

#[test]
fn a_lone_timed_wait_times_out() {
    check_dfs(
        || {
            assert!(timed_wait(Duration::from_secs(1)));
            // The answer is cleared once read.
            assert!(!ExecutionState::with(|state| state.current_mut().take_timed_out()));
        },
        None,
    );
}

/// What a waiter and the task that may unblock it share.
#[derive(Default)]
struct Shared {
    /// The waiter, from the step in which it blocks.
    waiting: Option<TaskId>,
    /// Whether the other task unblocked the waiter.
    unblocked: bool,
    /// Whether the other task is done.
    done: bool,
}

// A timed wait times out only once the other task is done: never while it can still run. Whether
// the other task finds the waiter blocked depends on the schedule, so both outcomes happen.
#[test]
fn a_timed_wait_does_not_time_out_while_another_task_can_run() {
    let outcomes = Arc::new(Mutex::new(std::collections::BTreeSet::new()));
    {
        let outcomes = Arc::clone(&outcomes);
        check_dfs(
            move || {
                let shared = Arc::new(Mutex::new(Shared::default()));
                let worker = {
                    let shared = Arc::clone(&shared);
                    thread::spawn(move || {
                        thread::yield_now();
                        thread::yield_now();
                        let mut shared = shared.lock().unwrap();
                        if let Some(waiter) = shared.waiting.take() {
                            unblock(waiter);
                            shared.unblocked = true;
                        }
                        shared.done = true;
                    })
                };
                // A chance for the worker to finish before the wait starts.
                thread::yield_now();
                // Publishing the waiter and blocking happen in the same step.
                shared.lock().unwrap().waiting = Some(me());
                let timed_out = timed_wait(Duration::from_secs(1));
                {
                    let shared = shared.lock().unwrap();
                    assert_eq!(timed_out, !shared.unblocked);
                    assert!(!timed_out || shared.done, "timed out while the worker could still run");
                }
                worker.join().unwrap();
                outcomes.lock().unwrap().insert(timed_out);
            },
            None,
        );
    }
    assert_eq!(*outcomes.lock().unwrap(), [false, true].into_iter().collect());
}

// The other task blocks too, untimed, so nothing can run until the timed wait times out, and the
// task that timed out then unblocks it.
#[test]
fn a_timed_wait_times_out_when_every_other_task_is_blocked() {
    check_dfs(
        || {
            let blocked = Arc::new(Mutex::new(None));
            let worker = {
                let blocked = Arc::clone(&blocked);
                thread::spawn(move || {
                    *blocked.lock().unwrap() = Some(me());
                    ExecutionState::with(|state| state.current_mut().block(false));
                    shuttle_engine::runtime::thread::switch();
                })
            };
            // Whether the worker has blocked yet or not, the wait times out only once it has.
            assert!(timed_wait(Duration::from_millis(10)));
            let worker_id = blocked
                .lock()
                .unwrap()
                .expect("the worker blocked before the wait timed out");
            unblock(worker_id);
            worker.join().unwrap();
        },
        None,
    );
}

#[test]
fn the_earliest_deadline_times_out_first() {
    check_dfs(
        || {
            let order = Arc::new(Mutex::new(Vec::new()));
            let handles = [30, 10, 20]
                .into_iter()
                .map(|millis| {
                    let order = Arc::clone(&order);
                    thread::spawn(move || {
                        assert!(timed_wait(Duration::from_millis(millis)));
                        order.lock().unwrap().push(millis);
                    })
                })
                .collect::<Vec<_>>();
            for handle in handles {
                handle.join().unwrap();
            }
            assert_eq!(*order.lock().unwrap(), [10, 20, 30]);
        },
        None,
    );
}

// Waits with the same deadline time out together, and the scheduler orders what they do next, so
// both orders happen.
#[test]
fn equal_deadlines_time_out_together() {
    let orders = Arc::new(Mutex::new(std::collections::BTreeSet::new()));
    {
        let orders = Arc::clone(&orders);
        check_dfs(
            move || {
                let order = Arc::new(Mutex::new(Vec::new()));
                let handles = ["a", "b"]
                    .into_iter()
                    .map(|name| {
                        let order = Arc::clone(&order);
                        thread::spawn(move || {
                            assert!(timed_wait(Duration::from_millis(10)));
                            order.lock().unwrap().push(name);
                        })
                    })
                    .collect::<Vec<_>>();
                for handle in handles {
                    handle.join().unwrap();
                }
                orders.lock().unwrap().insert(order.lock().unwrap().clone());
            },
            None,
        );
    }
    let orders = orders.lock().unwrap();
    assert!(orders.contains(&vec!["a", "b"]));
    assert!(orders.contains(&vec!["b", "a"]));
}

// Being unblocked ends the timed wait, so a later untimed block of the same task deadlocks rather
// than timing out. Whether the worker finds the waiter blocked depends on the schedule; if it does
// not, the wait times out instead, which ends it too.
#[test]
#[should_panic(expected = "deadlock")]
fn unblocking_ends_the_timed_wait() {
    check_random(
        || {
            let waiting = Arc::new(Mutex::new(None));
            let worker = {
                let waiting = Arc::clone(&waiting);
                thread::spawn(move || {
                    if let Some(waiter) = waiting.lock().unwrap().take() {
                        unblock(waiter);
                    }
                })
            };
            thread::yield_now();
            *waiting.lock().unwrap() = Some(me());
            timed_wait(Duration::from_secs(1));
            worker.join().unwrap();
            ExecutionState::with(|state| state.current_mut().block(false));
            shuttle_engine::runtime::thread::switch();
        },
        10,
    );
}

#[test]
#[should_panic(expected = "deadlock")]
fn an_untimed_block_still_deadlocks() {
    check_random(
        || {
            ExecutionState::with(|state| state.current_mut().block(false));
            shuttle_engine::runtime::thread::switch();
        },
        1,
    );
}

// Timing out is not a context switch of its own: the scheduling decision that finds no task to run
// times the waits out and then picks among the tasks they unblocked.
#[test]
fn timing_out_takes_one_scheduling_decision() {
    check_dfs(
        || {
            let before = shuttle::current::context_switches();
            assert!(timed_wait(Duration::from_secs(1)));
            assert_eq!(shuttle::current::context_switches(), before + 1);
        },
        None,
    );
}
