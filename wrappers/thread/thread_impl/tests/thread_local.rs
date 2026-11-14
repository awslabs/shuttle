//! `thread_local!` keys belong to a task and to an execution, where `std::thread_local!` keys
//! belong to the one OS thread an execution runs on.

use shuttle::check_dfs;
use shuttle::sync::Mutex;
use shuttle_thread_impl::{thread, thread_local};
use std::cell::Cell;
use std::sync::Arc;

thread_local! {
    static COUNT: Cell<usize> = Cell::new(0);
    static NAME: Cell<&'static str> = const { Cell::new("unnamed") };
}

std::thread_local! {
    /// The same program on an OS thread-local, for contrast.
    static STD_COUNT: Cell<usize> = const { Cell::new(0) };
}

/// A `LocalKey` of this crate, not of `std::thread`: the expansion names Shuttle's type.
#[test]
fn the_key_is_shuttles() {
    let _: &'static thread::LocalKey<Cell<usize>> = &COUNT;
    let _: &'static thread::LocalKey<Cell<&'static str>> = &NAME;
}

/// Each task of an execution gets its own value, initialized the first time that task uses the key.
#[test]
fn each_task_has_its_own_value() {
    check_dfs(
        || {
            COUNT.with(|count| {
                assert_eq!(count.get(), 0);
                count.set(1);
            });
            NAME.with(|name| name.set("parent"));

            let handle = thread::spawn(|| {
                COUNT.with(|count| {
                    assert_eq!(count.get(), 0, "the parent's 1 is the parent's");
                    count.set(2);
                });
                NAME.with(|name| {
                    assert_eq!(name.get(), "unnamed");
                    name.set("child");
                });
            });
            handle.join().unwrap();

            COUNT.with(|count| assert_eq!(count.get(), 1));
            NAME.with(|name| assert_eq!(name.get(), "parent"));
        },
        None,
    );
}

/// The values of one execution do not reach the next: each execution starts at the initializer.
/// An OS thread-local, written by the same program, does carry over, and is shared by the tasks.
#[test]
fn values_are_fresh_in_each_execution() {
    // What each execution found in the two keys when it started, and how many executions ran.
    let starts: Arc<std::sync::Mutex<Vec<(usize, usize)>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = Arc::clone(&starts);

    check_dfs(
        move || {
            let start = (COUNT.with(Cell::get), STD_COUNT.with(Cell::get));
            seen.lock().unwrap().push(start);

            // Two tasks, so that the test runs more than one execution.
            let barrier = Arc::new(Mutex::new(()));
            let handle = {
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let _held = barrier.lock().unwrap();
                    COUNT.with(|count| count.set(count.get() + 1));
                    STD_COUNT.with(|count| count.set(count.get() + 1));
                })
            };
            {
                let _held = barrier.lock().unwrap();
                COUNT.with(|count| count.set(count.get() + 1));
                STD_COUNT.with(|count| count.set(count.get() + 1));
            }
            handle.join().unwrap();
        },
        None,
    );

    let starts = starts.lock().unwrap();
    assert!(starts.len() > 1, "more than one execution ran: {starts:?}");
    assert!(
        starts.iter().all(|(shuttle, _)| *shuttle == 0),
        "every execution starts at the initializer: {starts:?}"
    );
    assert!(
        starts.iter().any(|(_, std)| *std > 0),
        "the OS thread-local carries over, which is what Shuttle's key avoids: {starts:?}"
    );
}
