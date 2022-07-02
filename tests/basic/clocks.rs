use shuttle::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use shuttle::sync::mpsc::{channel, sync_channel};
use shuttle::sync::{Barrier, Condvar, Mutex, Once, RwLock};
use shuttle::{check_dfs, check_pct, check_random, current, thread};
use std::collections::HashSet;
use std::sync::Arc;
use test_log::test;

pub fn me() -> usize {
    usize::from(thread::current().id())
}

#[track_caller]
pub fn check_clock(f: impl Fn(usize, u32) -> bool) {
    for (i, &c) in current::clock().iter().enumerate() {
        assert!(
            f(i, c),
            "clock {:?} doesn't satisfy predicate at {}",
            current::clock(),
            i
        );
    }
}

fn clock_mutex(num_threads: usize) {
    // This test checks that when a thread acquires a lock, it inherits the vector clocks of
    // threads that accessed the lock before it.
    //
    // Test: create a mutex-protected set, initialized with 0 (the id of the main thread)
    // and spawn N threads where each thread does the following:
    //    (1) check that its own initial vector clock only has nonzero for the creator (thread 0)
    //        this checks that when a thread is created, it only inherits the clock of the spawner
    //    (2) lock the set and add its own thread id to it; let the resulting set be S
    //    (3) read its own clock again, call this C
    //    (4) check that the only nonzero entries in C are for the threads in S
    // For sanity checking, we also spawn an initial dummy thread (with id 1) and ensure that its
    // clock is always 0.
    let mut set = HashSet::new();
    set.insert(0);
    let set = Arc::new(Mutex::new(set));

    // Create dummy thread (should have id 1)
    thread::spawn(|| {
        assert_eq!(me(), 1usize);
    });

    let threads = (0..num_threads)
        .map(|_| {
            let set = Arc::clone(&set);
            thread::spawn(move || {
                check_clock(|i, c| (c > 0) == (i == 0));
                let mut set = set.lock().unwrap();
                set.insert(me());
                assert!(!set.contains(&1)); // dummy thread is never in the set
                check_clock(|i, c| (c > 0) == set.contains(&i));
            })
        })
        .collect::<Vec<_>>();

    for thd in threads {
        thd.join().unwrap();
    }

    assert_eq!(set.lock().unwrap().len(), 1 + num_threads); // +1 because we initialized the set to {0}
}

#[test]
fn clock_mutex_dfs() {
    check_dfs(|| clock_mutex(2), None);
}

#[test]
fn clock_mutex_pct() {
    check_pct(|| clock_mutex(20), 1000, 3);
}

// RWLocks
fn clock_rwlock(num_writers: usize, num_readers: usize) {
    // This test checks that when a thread acquires a RwLock, it inherits the clocks of writers that
    // accessed the lock before it. It's the same as `clock_mutex`, except that readers don't update
    // the set S, and aren't required to appear in the clock for future lock holders.
    //
    // TODO this test is pretty weak. Testing readers is hard because they race with each other; for
    // example, a reader might see the clock update from another reader before that reader has a
    // chance to update the set S. Causality is also pretty fuzzy for readers (see the TODOs in the
    // RwLock implementation). So we don't test very much about them here.
    let set = Arc::new(std::sync::Mutex::new(HashSet::from([0])));
    let lock = Arc::new(RwLock::new(()));

    // Create dummy thread (should have id 1)
    thread::spawn(|| {
        assert_eq!(me(), 1usize);
    });

    // Spawn the writers
    let _thds = (0..num_writers)
        .map(|_| {
            let set = Arc::clone(&set);
            let lock = Arc::clone(&lock);
            thread::spawn(move || {
                check_clock(|i, c| (c > 0) == (i == 0));
                let _guard = lock.write().unwrap();
                let mut set = set.lock().unwrap();
                set.insert(me());
                assert!(!set.contains(&1)); // dummy thread is never in the set
                check_clock(|i, c| !set.contains(&i) || (c > 0));
            })
        })
        .collect::<Vec<_>>();

    // Spawn the readers
    let _thds = (0..num_readers)
        .map(|_| {
            let set = Arc::clone(&set);
            let lock = Arc::clone(&lock);
            thread::spawn(move || {
                check_clock(|i, c| (c > 0) == (i == 0));
                let _guard = lock.read().unwrap();
                let set = set.lock().unwrap();
                assert!(!set.contains(&1)); // dummy thread is never in the set
                check_clock(|i, c| !set.contains(&i) || (c > 0));
            })
        })
        .collect::<Vec<_>>();
}

#[test]
fn clock_rwlock_dfs() {
    // Unfortunately anything larger than this takes > 500k iterations, too slow to be useful :(
    // But the PCT and random tests below buy us a much bigger search.
    check_dfs(|| clock_rwlock(1, 1), None);
}

#[test]
fn clock_rwlock_pct() {
    check_pct(|| clock_rwlock(4, 4), 10_000, 3);
}

#[test]
fn clock_rwlock_random() {
    check_random(|| clock_rwlock(4, 4), 10_000);
}

// Barrier
fn clock_barrier(n: usize) {
    // This test checks that threads waiting on a barrier inherit the clocks from all the other participants in the barrier.
    //
    // The test creates a barrier with bound n and creates n threads (including the main thread).
    // Each thread initially checks that its clock is nonzero only for the main thread, and then waits on the barrier.
    // When it exits the barrier, each thread checks that its current clock is nonzero for all threads.
    // For sanity checking, we also spawn a dummy thread and check that its clock entry is always 0.
    let barrier = Arc::new(Barrier::new(n));

    // Create dummy thread (should have id 1)
    thread::spawn(|| {
        assert_eq!(me(), 1usize);
    });

    let _thds = (0..n - 1)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                check_clock(|i, c| (c > 0) == (i == 0));
                barrier.wait();
                // Since all threads reached the barrier, everyone's clock must be nonzero
                // except the dummy, whose clock must be 0
                check_clock(|i, c| (c > 0) == (i != 1));
            });
        })
        .collect::<Vec<_>>();

    barrier.wait();
    // Since all threads reached the barrier, everyone's clock must be nonzero, except for the dummy
    check_clock(|i, c| (c > 0) == (i != 1));
}

#[test]
fn clock_barrier_dfs() {
    check_dfs(|| clock_barrier(4), None);
}

#[test]
fn clock_barrier_pct() {
    check_pct(|| clock_barrier(50), 1000, 3);
}

// Condvars
#[test]
fn clock_condvar_single() {
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(false));
            let cond = Arc::new(Condvar::new());

            {
                let lock = Arc::clone(&lock);
                let cond = Arc::clone(&cond);
                thread::spawn(move || {
                    assert_eq!(me(), 1);
                    *lock.lock().unwrap() = true;
                    cond.notify_one();
                });
            }

            let mut guard = lock.lock().unwrap();
            while !*guard {
                check_clock(|i, c| (c > 0) == (i == 0)); // spawned thread has not executed notify_one
                guard = cond.wait(guard).unwrap();
            }
            check_clock(|i, c| (c > 0) == (i == 0 || i == 1));
        },
        None,
    )
}

fn clock_condvar_notify_one(num_notifiers: usize, num_waiters: usize) {
    let lock = Arc::new(Mutex::new(0usize));
    let cond = Arc::new(Condvar::new());

    for _ in 0..num_notifiers {
        let lock = Arc::clone(&lock);
        let cond = Arc::clone(&cond);
        thread::spawn(move || {
            assert!(me() <= num_notifiers);
            *lock.lock().unwrap() = me();
            cond.notify_one();
        });
    }

    for _ in 0..num_waiters {
        let lock = Arc::clone(&lock);
        let cond = Arc::clone(&cond);
        thread::spawn(move || {
            let mut guard = lock.lock().unwrap();
            while *guard == 0 {
                check_clock(|i, c| !(i >= 1 && i <= num_notifiers) || (c == 0)); // no notifier has gone yet
                guard = cond.wait(guard).unwrap();
            }
            // Note that since all the threads touch the lock, any of them may have preceded this thread.
            // But we know for sure that the thread that unblocked us should causally precede us.
            check_clock(|i, c| (i != *guard) || (c > 0));
        });
    }
}

#[test]
fn clock_condvar_notify_one_dfs() {
    check_dfs(|| clock_condvar_notify_one(1, 1), None);
}

#[test]
fn clock_condvar_notify_one_pct() {
    check_pct(|| clock_condvar_notify_one(10, 10), 10_000, 3);
}

fn clock_condvar_notify_all(num_waiters: usize) {
    let lock = Arc::new(Mutex::new(0usize));
    let cond = Arc::new(Condvar::new());

    {
        let lock = Arc::clone(&lock);
        let cond = Arc::clone(&cond);
        thread::spawn(move || {
            assert_eq!(me(), 1);
            *lock.lock().unwrap() = me();
            cond.notify_all();
        });
    }

    for _ in 0..num_waiters {
        let lock = Arc::clone(&lock);
        let cond = Arc::clone(&cond);
        thread::spawn(move || {
            let mut guard = lock.lock().unwrap();
            while *guard == 0 {
                check_clock(|i, c| (i != 1) || (c == 0)); // notifier hasn't been scheduled
                guard = cond.wait(guard).unwrap();
            }
            // Note that since all the threads touch the lock, any of them may have preceded this thread.
            // But we know for sure that the thread that unblocked us should causally precede us.
            check_clock(|i, c| (i != *guard) || (c > 0));
        });
    }
}

#[test]
fn clock_condvar_notify_all_dfs() {
    check_dfs(|| clock_condvar_notify_all(2), None);
}

#[test]
fn clock_condvar_notify_all_pct() {
    check_pct(|| clock_condvar_notify_all(20), 10_000, 3);
}

// MPSC Channels
#[test]
fn clock_mpsc_unbounded() {
    const NUM_MSG: usize = 3;
    check_dfs(
        || {
            let (tx, rx) = channel::<usize>();
            thread::spawn(move || {
                assert_eq!(me(), 1);
                for i in 0..NUM_MSG {
                    tx.send(i).unwrap();
                }
            });
            for _ in 0..NUM_MSG {
                let c1 = current::clock().get(1); // save clock of thread 1
                let _ = rx.recv().unwrap();
                check_clock(|i, c| (i != 1) || (c > c1)); // thread 1's clock increased
            }
        },
        None,
    );
}

#[test]
fn clock_mpsc_bounded() {
    const BOUND: usize = 3;
    check_dfs(
        || {
            let (tx, rx) = sync_channel::<()>(BOUND);
            thread::spawn(move || {
                assert_eq!(me(), 1);
                for _ in 0..BOUND {
                    tx.send(()).unwrap();
                }
                // At this point the sender doesn't know about the receiver
                check_clock(|i, c| (c > 0) == (i == 0 || i == 1));
                tx.send(()).unwrap();
                // Here, we know the receiver picked up the 1st message, so its clock is nonzero
                let c1 = current::clock().get(2);
                assert!(c1 > 0);
                tx.send(()).unwrap();
                // Here, we know that the receiver picked up the 2nd message, so its clock has increased
                assert!(current::clock().get(2) > c1);
            });
            thread::spawn(move || {
                assert_eq!(me(), 2);
                // Receiver doesn't know about the sender yet
                check_clock(|i, c| (c > 0) == (i == 0));
                rx.recv().unwrap();
                // The sender has sent a message, so its clock is nonzero
                let c1 = current::clock().get(1);
                assert!(c1 > 0);
                rx.recv().unwrap();
                // The sender has sent another message, so its clock has increased
                assert!(current::clock().get(2) > c1);
                // Receive the remaining messages
                for _ in 0..BOUND {
                    rx.recv().unwrap();
                }
            });
        },
        None,
    );
}

#[test]
fn clock_mpsc_rendezvous() {
    check_dfs(
        || {
            let (tx, rx) = sync_channel::<()>(0);
            thread::spawn(move || {
                assert_eq!(me(), 1);
                // At this point the sender doesn't know about the receiver
                check_clock(|i, c| (c > 0) == (i == 0));
                tx.send(()).unwrap();
                // Since this is a rendezvous channel, and we successfully sent a message, we know about the receiver
                let c1 = current::clock().get(2);
                assert!(c1 > 0);
                tx.send(()).unwrap();
                // After the 2nd rendezvous, the receiver's clock has increased
                assert!(current::clock().get(2) > c1);
            });
            thread::spawn(move || {
                assert_eq!(me(), 2);
                // At this point the receiver doesn't know about the sender
                check_clock(|i, c| (c > 0) == (i == 0));
                rx.recv().unwrap();
                // Since we received a message, we know about the sender
                let c1 = current::clock().get(1);
                assert!(c1 > 0);
                rx.recv().unwrap();
                // After the 2nd rendezvous, the sender's clock has increased
                assert!(current::clock().get(1) > c1);
            });
        },
        None,
    );
}

// Threads
fn clock_threads(num_threads: usize) {
    // Use an AtomicBool to create a synchronization point so a thread's clock is incremented.
    let flag = Arc::new(AtomicBool::new(false));
    let handles = (1..num_threads + 1)
        .map(|k| {
            let flag = Arc::clone(&flag);
            thread::spawn(move || {
                assert_eq!(me(), k);
                check_clock(|i, c| (c > 0) == (i == 0));
                assert!(!flag.load(Ordering::SeqCst));
                check_clock(|i, c| (c > 0) == (i == 0) || (i == k));
                k
            })
        })
        .collect::<Vec<_>>();

    // As each thread joins, we get knowledge of its vector clock.
    for handle in handles {
        let k = handle.join().unwrap();
        check_clock(move |i, c| (c > 0) == (i <= k));
    }
}

#[test]
fn clock_threads_dfs() {
    check_dfs(|| clock_threads(2), None);
}

#[test]
fn clock_threads_pct() {
    check_pct(|| clock_threads(20), 10_000, 3);
}

#[test]
fn clock_fetch_update() {
    // Ensure that when a fetch_update fails, the caller does not inherit the clock from the register.
    check_dfs(
        || {
            let n = Arc::new(AtomicU32::new(0));

            {
                let n = Arc::clone(&n);
                thread::spawn(move || {
                    let _ = n.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |_| None);
                });
            }

            let _ = n.load(Ordering::SeqCst);
            // Note that we are using check_dfs, so there are executions where the fetch_update happens before this
            // load.  But the load above never causally depends on the spawned thread's clock, since it never managed to
            // store a value into the register.
            check_clock(|i, c| (c > 0) == (i == 0));
        },
        None,
    );
}

fn clock_once(num_threads: usize) {
    let once = Arc::new(Once::new());
    let init = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let threads = (0..num_threads)
        .map(|_| {
            let once = Arc::clone(&once);
            let init = Arc::clone(&init);
            thread::spawn(move || {
                check_clock(|i, c| (c > 0) == (i == 0));
                once.call_once(|| init.store(me(), std::sync::atomic::Ordering::SeqCst));
                let who_inited = init.load(std::sync::atomic::Ordering::SeqCst);
                // should have inhaled the clock of the thread that inited the Once, but might also
                // have inhaled the clocks of threads that we were racing with for initialization
                check_clock(|i, c| !(i == who_inited || i == 0 || i == me()) || c > 0);
            })
        })
        .collect::<Vec<_>>();

    for thd in threads {
        thd.join().unwrap();
    }
}

#[test]
fn clock_once_dfs() {
    check_dfs(|| clock_once(2), None);
}

#[test]
fn clock_once_pct() {
    check_pct(|| clock_once(20), 10_000, 3);
}
