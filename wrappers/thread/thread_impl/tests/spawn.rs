//! A thread this crate spawns is a Shuttle task: the scheduler decides when it runs, it can use
//! Shuttle's primitives, and a test sees the interleavings of the threads.

use shuttle::sync::Mutex;
use shuttle::{check_dfs, check_random};
use shuttle_thread_impl::thread;
use std::collections::HashSet;
use std::sync::Arc;

/// The orders in which the two spawned threads ran, over every execution of the test.
type Orders = std::sync::Mutex<HashSet<Vec<u8>>>;

/// Two spawned threads, each appending its own id to a shared log. Both orders are reachable, which
/// they would not be if the threads ran outside Shuttle's scheduler, and each thread takes a
/// Shuttle `Mutex`, which panics on a thread Shuttle does not know about.
#[test]
fn spawn_runs_the_thread_as_a_shuttle_task() {
    let orders: Arc<Orders> = Arc::new(std::sync::Mutex::new(HashSet::new()));
    let seen = Arc::clone(&orders);

    check_dfs(
        move || {
            let log = Arc::new(Mutex::new(Vec::new()));
            let mut handles = Vec::new();
            for id in [1u8, 2] {
                let log = Arc::clone(&log);
                handles.push(thread::spawn(move || log.lock().unwrap().push(id)));
            }
            for handle in handles {
                handle.join().unwrap();
            }
            let order = log.lock().unwrap().clone();
            seen.lock().unwrap().insert(order);
        },
        None,
    );

    let seen = orders.lock().unwrap();
    assert_eq!(
        *seen,
        HashSet::from([vec![1, 2], vec![2, 1]]),
        "both orders are reachable"
    );
}

/// A thread that cannot run has not finished, in every schedule.
#[test]
fn is_finished_is_false_while_the_thread_is_blocked() {
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(0));
            let guard = lock.lock().unwrap();

            let handle = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || *lock.lock().unwrap() += 1)
            };
            // The thread cannot get past the lock this task holds, so it cannot have finished,
            // whatever the scheduler does.
            assert!(!handle.is_finished());

            drop(guard);
            handle.join().unwrap();
            assert_eq!(*lock.lock().unwrap(), 1);
        },
        None,
    );
}

/// `is_finished` becomes true once the thread has run, and a loop around it needs a `yield_now`,
/// because `is_finished` itself takes no scheduling point. A fair scheduler is needed for such a
/// loop: `check_dfs` would explore the schedule that runs the yielding task again forever.
#[test]
fn is_finished_becomes_true() {
    check_random(
        || {
            let handle = thread::spawn(|| 7);
            while !handle.is_finished() {
                thread::yield_now();
            }
            assert!(handle.is_finished());
            assert_eq!(handle.join().unwrap(), 7);
        },
        20,
    );
}

/// `Builder::spawn_scoped` lends the thread a borrow, like `Scope::spawn`, and names it.
#[test]
fn builder_spawn_scoped_borrows_and_names() {
    check_dfs(
        || {
            let mut counted = 0usize;
            let data = [1, 2, 3];

            thread::scope(|scope| {
                let handle = thread::Builder::new()
                    .name("counter".into())
                    .stack_size(0x8000)
                    .spawn_scoped(scope, || data.len())
                    .unwrap();
                assert_eq!(handle.thread().name(), Some("counter"));
                counted = handle.join().unwrap();
            });

            assert_eq!(counted, 3);
        },
        None,
    );
}

/// `Builder::spawn_unchecked` spawns a thread that borrows from the caller's stack, which the
/// caller then has to keep alive itself.
#[test]
fn builder_spawn_unchecked_borrows() {
    check_dfs(
        || {
            let data = String::from("shuttle");
            let borrowed: &str = &data;

            // SAFETY: the handle is joined below, before `data` goes out of scope.
            let handle = unsafe { thread::Builder::new().spawn_unchecked(move || borrowed.len()) }.unwrap();

            assert_eq!(handle.join().unwrap(), 7);
        },
        None,
    );
}

/// `thread::current` names the running task, and the id is stable within a task and different
/// between tasks.
#[test]
fn current_identifies_the_task() {
    check_dfs(
        || {
            let parent = thread::current().id();
            let handle = thread::Builder::new()
                .name("child".into())
                .spawn(move || {
                    let me = thread::current();
                    assert_eq!(me.name(), Some("child"));
                    let id = me.id();
                    thread::yield_now();
                    assert_eq!(thread::current().id(), id);
                    assert_ne!(id, parent);
                    id
                })
                .unwrap();
            let child = handle.join().unwrap();
            assert_ne!(child, thread::current().id());
            assert_eq!(thread::current().id(), parent);
        },
        None,
    );
}
