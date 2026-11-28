//! Inside a Shuttle test the backdoor items stay the `std` items: Shuttle neither replaces nor
//! resets them, and a backdoor thread is a real OS thread.

use std::sync::Arc;

use shuttle_backdoor::sync::Mutex;
use shuttle_backdoor::sync::atomic::{AtomicUsize, Ordering};
use shuttle_backdoor::thread;

#[test]
fn backdoor_state_outlives_each_execution() {
    const EXECUTIONS: usize = 20;
    // Shuttle backdoor: counts executions across the whole test, so Shuttle must not reset it.
    static STARTED: AtomicUsize = AtomicUsize::new(0);

    shuttle::check_random(
        || {
            STARTED.fetch_add(1, Ordering::SeqCst);
        },
        EXECUTIONS,
    );
    assert_eq!(STARTED.load(Ordering::SeqCst), EXECUTIONS);
}

#[test]
fn backdoor_mutex_serves_shuttle_tasks_when_held_without_a_yield() {
    shuttle::check_dfs(
        || {
            // Shuttle backdoor: the tasks only append to the log, and no task holds the lock
            // across a scheduling point, so it can't hang the execution.
            let log = Arc::new(Mutex::new(Vec::new()));
            let tasks: Vec<_> = (0..2)
                .map(|i| {
                    let log = Arc::clone(&log);
                    shuttle::thread::spawn(move || log.lock().unwrap().push(i))
                })
                .collect();
            for task in tasks {
                task.join().unwrap();
            }
            let mut appended = log.lock().unwrap().clone();
            appended.sort_unstable();
            assert_eq!(appended, vec![0, 1]);
        },
        None,
    );
}

#[test]
fn backdoor_current_is_the_same_os_thread_in_every_task() {
    shuttle::check_dfs(
        || {
            // Shuttle backdoor: reads the OS thread, which every task of an execution shares, so
            // this is not a per-task identity.
            let main_task = thread::current().id();
            let task = shuttle::thread::spawn(move || thread::current().id());
            assert_eq!(task.join().unwrap(), main_task);
        },
        None,
    );
}

#[test]
fn backdoor_thread_is_a_real_os_thread() {
    shuttle::check_dfs(
        || {
            let task_thread = std::thread::current().id();
            // Shuttle backdoor: a real OS thread that touches no Shuttle primitive.
            let worker = thread::spawn(|| std::thread::current().id());
            assert_ne!(worker.join().unwrap(), task_thread);
        },
        None,
    );
}
