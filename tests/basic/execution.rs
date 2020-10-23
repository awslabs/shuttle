use shuttle::{check, thread};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use test_env_log::test;

#[test]
fn basic_scheduler_test() {
    // Not actually trying to explore interleavings involving AtomicUsize, just using to smuggle a
    // mutable counter across threads
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = Arc::clone(&counter);

    check(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        let counter_clone = Arc::clone(&counter);
        thread::spawn(move || {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        });
        counter.fetch_add(1, Ordering::SeqCst);
    });

    assert_eq!(counter_clone.load(Ordering::SeqCst), 3);
}
