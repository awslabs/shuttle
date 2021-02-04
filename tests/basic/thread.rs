use shuttle::sync::Mutex;
use shuttle::{check_random, thread};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use test_env_log::test;

#[test]
fn thread_yield_point() {
    let success = Arc::new(AtomicU8::new(0));
    let success_clone = Arc::clone(&success);

    // We want to see executions that include both threads running first, otherwise we have
    // messed up the yieldpoints around spawn.
    check_random(
        move || {
            let flag = Arc::new(AtomicBool::new(false));
            let flag_clone = Arc::clone(&flag);

            thread::spawn(move || {
                flag_clone.store(true, Ordering::SeqCst);
            });

            if flag.load(Ordering::SeqCst) {
                success.fetch_or(0x1, Ordering::SeqCst);
            } else {
                success.fetch_or(0x2, Ordering::SeqCst);
            }
        },
        100,
    );

    assert_eq!(success_clone.load(Ordering::SeqCst), 0x3);
}

#[test]
fn thread_join() {
    check_random(
        || {
            let lock = Arc::new(Mutex::new(false));
            let lock_clone = Arc::clone(&lock);
            let handle = thread::spawn(move || {
                *lock_clone.lock().unwrap() = true;
                1
            });
            let ret = handle.join();
            assert_eq!(ret.unwrap(), 1);
            assert!(*lock.lock().unwrap());
        },
        100,
    );
}

#[test]
fn thread_join_drop() {
    check_random(
        || {
            let lock = Arc::new(Mutex::new(false));
            let lock_clone = Arc::clone(&lock);
            let handle = thread::spawn(move || {
                *lock_clone.lock().unwrap() = true;
                1
            });
            drop(handle);
            *lock.lock().unwrap() = true;
        },
        100,
    );
}

#[test]
fn thread_builder_name() {
    check_random(
        || {
            let builder = thread::Builder::new().name("producer".into());
            let handle = builder
                .spawn(|| {
                    thread::yield_now();
                })
                .unwrap();
            assert_eq!(handle.thread().name().unwrap(), "producer");
        },
        100,
    );
}
