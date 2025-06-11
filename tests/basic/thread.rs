use shuttle::current::{get_name_for_task, me};
use shuttle::sync::{Barrier, Condvar, Mutex};
use shuttle::{check_dfs, check_random, future, thread};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use test_log::test;

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
fn thread_scope() {
    check_dfs(
        || {
            let mut trigger = false;
            thread::scope(|s| {
                s.spawn(|| {
                    trigger = true;
                });
            });
            assert!(trigger);
        },
        None,
    );
}

#[test]
fn thread_scope_multiple() {
    check_dfs(
        || {
            let mut data = vec![1, 2, 3, 4, 5, 6];

            thread::scope(|s| {
                let (left, right) = data.split_at_mut(3);

                s.spawn(|| {
                    for x in left {
                        *x *= 10;
                    }
                });

                s.spawn(|| {
                    for x in right {
                        *x += 100;
                    }
                });
            });

            assert_eq!(data, vec![10, 20, 30, 104, 105, 106]);
        },
        None,
    );
}

#[test]
fn thread_scope_join() {
    check_dfs(
        || {
            let num = Mutex::new(10);

            thread::scope(|s| {
                let handle = s.spawn(|| {
                    let mut num_guard = num.lock().unwrap();
                    *num_guard *= 10;
                    *num_guard
                });

                assert_eq!(handle.join().unwrap(), 100);

                let handle = s.spawn(|| {
                    let mut num_guard = num.lock().unwrap();
                    *num_guard += 10;
                    *num_guard
                });

                assert_eq!(handle.join().unwrap(), 110);
            });

            assert_eq!(*num.lock().unwrap(), 110);
        },
        None,
    );
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
                    let name = String::from(get_name_for_task(me()).unwrap());
                    assert_eq!(name, "producer");
                    thread::yield_now();
                })
                .unwrap();
            assert_eq!(handle.thread().name().unwrap(), "producer");
        },
        100,
    );
}

#[test]
fn thread_identity() {
    check_dfs(
        || {
            let lock = std::sync::Arc::new(Mutex::new(None));
            let lock2 = lock.clone();

            let builder = thread::Builder::new().name("producer".into());
            let _handle = builder
                .spawn(move || {
                    let me = thread::current();
                    assert_eq!(me.name(), Some("producer"));
                    let id = me.id();

                    thread::yield_now();

                    let me = thread::current();
                    assert_eq!(me.name(), Some("producer"));
                    assert_eq!(me.id(), id);

                    *lock2.lock().unwrap() = Some(id);
                })
                .unwrap();

            let id = *lock.lock().unwrap();
            if let Some(id) = id {
                assert_ne!(id, thread::current().id());
            }
        },
        None,
    );
}

/// Thread local tests
///
/// The first three are based on Loom's thread local tests:
/// https://github.com/tokio-rs/loom/blob/562211da5978f729e5728667566636c34c9cbc8b/tests/thread_local.rs
mod thread_local {
    use super::*;
    use shuttle::thread::LocalKey;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use test_log::test;

    #[test]
    fn basic() {
        shuttle::thread_local! {
            static THREAD_LOCAL: RefCell<usize> = RefCell::new(1);
        }

        fn do_test(n: usize) {
            THREAD_LOCAL.with(|local| {
                assert_eq!(*local.borrow(), 1);
            });
            THREAD_LOCAL.with(|local| {
                assert_eq!(*local.borrow(), 1);
                *local.borrow_mut() = n;
                assert_eq!(*local.borrow(), n);
            });
            THREAD_LOCAL.with(|local| {
                assert_eq!(*local.borrow(), n);
            });
        }

        check_dfs(
            || {
                let t1 = thread::spawn(|| do_test(2));

                let t2 = thread::spawn(|| do_test(3));

                do_test(4);

                t1.join().unwrap();
                t2.join().unwrap();
            },
            None,
        );
    }

    #[test]
    fn nested_with() {
        shuttle::thread_local! {
            static LOCAL1: RefCell<usize> = RefCell::new(1);
            static LOCAL2: RefCell<usize> = RefCell::new(2);
        }

        check_dfs(
            || {
                LOCAL1.with(|local1| *local1.borrow_mut() = LOCAL2.with(|local2| *local2.borrow()));
                LOCAL1.with(|local1| assert_eq!(*local1.borrow(), 2));
            },
            None,
        );
    }

    struct CountDrops {
        drops: &'static AtomicUsize,
        dummy: bool,
    }

    impl Drop for CountDrops {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Release);
        }
    }

    impl CountDrops {
        fn new(drops: &'static AtomicUsize) -> Self {
            Self { drops, dummy: true }
        }
    }

    #[test]
    fn drop() {
        static DROPS: AtomicUsize = AtomicUsize::new(0);

        shuttle::thread_local! {
            static DROPPED_LOCAL: CountDrops = CountDrops::new(&DROPS);
        }

        check_dfs(
            || {
                // Reset the drop counter. We don't count drops with a fresh AtomicUsize within the test
                // because we want to be able to check the drop counter *after* the last thread exits.
                // If this is the first iteration, drops will be 0, otherwise it should be 3 (set by the
                // previous iteration of the test).
                let drops = DROPS.swap(0, Ordering::SeqCst);
                assert!(drops == 0 || drops == 3);

                let thd = thread::spawn(|| {
                    // force access to the thread local so that it's initialized.
                    DROPPED_LOCAL.with(|local| assert!(local.dummy));
                    assert_eq!(DROPS.load(Ordering::SeqCst), 0);
                });
                thd.join().unwrap();

                // When the first spawned thread completed, its copy of the thread local should have
                // been dropped.
                assert_eq!(DROPS.load(Ordering::SeqCst), 1);

                let thd = thread::spawn(|| {
                    // force access to the thread local so that it's initialized.
                    DROPPED_LOCAL.with(|local| assert!(local.dummy));
                    assert_eq!(DROPS.load(Ordering::SeqCst), 1);
                });
                thd.join().unwrap();

                // When the second spawned thread completed, its copy of the thread local should have
                // been dropped as well.
                assert_eq!(DROPS.load(Ordering::SeqCst), 2);

                // force access to the thread local so that it's initialized.
                DROPPED_LOCAL.with(|local| assert!(local.dummy));
            },
            None,
        );

        // Finally, when the test's "main thread" completes, its copy of the local should also be
        // dropped. Because we reset the `DROPS` static to 0 at the start of each iteration, we know
        // the correct value here regardless of how many iterations run.
        assert_eq!(DROPS.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn drop_main_thread() {
        static DROPS: AtomicUsize = AtomicUsize::new(0);
        shuttle::thread_local! {
            static DROPPED_LOCAL: CountDrops = CountDrops::new(&DROPS);
        }
        let seen_drop_counters = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let seen_drop_counters_clone = seen_drop_counters.clone();

        check_dfs(
            move || {
                DROPS.store(0, Ordering::SeqCst);
                let seen_drop_counters = seen_drop_counters.clone();
                let _thd = thread::spawn(move || {
                    // force access to the thread local so that it's initialized
                    DROPPED_LOCAL.with(|local| assert!(local.dummy));
                    // record the current drop counter
                    seen_drop_counters.lock().unwrap().insert(DROPS.load(Ordering::SeqCst));
                });

                // force access to the thread local so that it's initialized
                DROPPED_LOCAL.with(|local| assert!(local.dummy));
            },
            None,
        );

        // We should have seen both orderings. In particular, it should be possible for the main
        // thread to drop its thread local before the spawned thread.
        assert_eq!(
            Arc::try_unwrap(seen_drop_counters_clone).unwrap().into_inner().unwrap(),
            HashSet::from([0, 1])
        );
    }

    // Like `drop`, but the destructor for one thread local initializes another thread local, and so
    // both destructors must be run (which Loom doesn't do)
    #[test]
    fn drop_init() {
        struct TouchOnDrop(CountDrops, &'static LocalKey<CountDrops>);

        impl Drop for TouchOnDrop {
            fn drop(&mut self) {
                self.1.with(|local| assert!(local.dummy));
            }
        }

        static DROPS: AtomicUsize = AtomicUsize::new(0);

        shuttle::thread_local! {
            static LOCAL1: CountDrops = CountDrops::new(&DROPS);
            static LOCAL2: TouchOnDrop = TouchOnDrop(CountDrops::new(&DROPS), &LOCAL1);
        }

        check_dfs(
            || {
                let drops = DROPS.swap(0, Ordering::SeqCst);
                // Same story as the `drop` test above, but the main thread only initializes LOCAL1
                // and not LOCAL2, so its destructors only trigger one drop.
                assert!(drops == 0 || drops == 3);

                let thd = thread::spawn(|| {
                    // force access to LOCAL2 so that it's initialized.
                    LOCAL2.with(|local| assert!(local.0.dummy));
                    assert_eq!(DROPS.load(Ordering::SeqCst), 0);
                });
                thd.join().unwrap();

                // When the first spawned thread completed, its copy of LOCAL2 should drop, and
                // LOCAL2's dtor initializes LOCAL1, which then also needs to be dropped. So we
                // should have run two CountDrops dtors by this point.
                assert_eq!(DROPS.load(Ordering::SeqCst), 2);

                // force access to LOCAL1 so that it's initialized.
                LOCAL1.with(|local| assert!(local.dummy));
            },
            None,
        );

        // The main thread only initializes LOCAL1 and not LOCAL2, so it only triggers one more drop
        assert_eq!(DROPS.load(Ordering::SeqCst), 3);
    }

    // Runs some synchronization code in the Drop handler
    #[test]
    fn drop_lock() {
        // Slightly funky type: the RefCell gives interior mutability, the Option allows us to
        // initialize the static before the lock is constructed.
        struct LockOnDrop(RefCell<Option<Arc<Mutex<usize>>>>);

        impl Drop for LockOnDrop {
            fn drop(&mut self) {
                *self.0.borrow_mut().as_ref().unwrap().lock().unwrap() += 1;
            }
        }

        shuttle::thread_local! {
            static LOCAL: LockOnDrop = LockOnDrop(RefCell::new(None));
        }

        check_dfs(
            || {
                let lock = Arc::new(Mutex::new(0));

                let thd = {
                    let lock = Arc::clone(&lock);
                    thread::spawn(move || {
                        // initialize LOCAL with the lock
                        LOCAL.with(|local| *local.0.borrow_mut() = Some(lock.clone()));
                        assert_eq!(*lock.lock().unwrap(), 0);
                    })
                };
                thd.join().unwrap();

                // When the first spawned thread completed, its copy of LOCAL should drop, and bump
                // the lock counter by 1.
                assert_eq!(*lock.lock().unwrap(), 1);
            },
            None,
        );
    }

    // Like `nested_with` but just testing the const variant of the `thread_local` macro
    #[test]
    fn const_nested_with() {
        shuttle::thread_local! {
            static LOCAL1: RefCell<usize> = const { RefCell::new(1) };
            static LOCAL2: RefCell<usize> = const { RefCell::new(2) };
        }

        check_dfs(
            || {
                LOCAL1.with(|local1| *local1.borrow_mut() = LOCAL2.with(|local2| *local2.borrow()));
            },
            None,
        );
    }

    #[test]
    fn multiple_accesses() {
        shuttle::thread_local! {
            static LOCAL: RefCell<usize> = RefCell::new(0);
        }

        fn increment() {
            LOCAL.with(|local| {
                *local.borrow_mut() += 1;
            });
        }

        fn check() {
            LOCAL.with(|local| {
                assert_eq!(*local.borrow(), 1);
            });
        }

        check_dfs(
            || {
                increment();
                check();
            },
            None,
        )
    }

    /// Thread-local destructors weren't being run for the main thread
    /// https://github.com/awslabs/shuttle/issues/86
    #[test]
    fn regression_86() {
        use core::cell::Cell;
        use shuttle::sync::*;
        let mu: &'static mut Mutex<usize> = Box::leak(Box::new(Mutex::new(0))); // a static_local here instead also works
        shuttle::thread_local! {
            static STASH: Cell<Option<MutexGuard<'_, usize>>> = Cell::new(None);
        }
        shuttle::check_random(
            || {
                shuttle::thread::spawn(|| {
                    STASH.with(|s| s.set(Some(mu.lock().unwrap())));
                });
                STASH.with(|s| s.set(Some(mu.lock().unwrap())));
            },
            100,
        );
    }
}

/// Test the common usage of park where a thread calls park in a loop waiting for a flag to be set.
#[test]
fn thread_park() {
    check_dfs(
        || {
            let shared_state = Arc::new(AtomicU8::new(0));
            let wakeup = Arc::new(AtomicBool::new(false));
            let thd = {
                let shared_state = Arc::clone(&shared_state);
                let wakeup = Arc::clone(&wakeup);
                thread::spawn(move || {
                    while !wakeup.load(Ordering::SeqCst) {
                        thread::park();
                    }
                    assert_eq!(shared_state.load(Ordering::SeqCst), 42);
                })
            };

            shared_state.store(42, Ordering::SeqCst);

            wakeup.store(true, Ordering::SeqCst);
            thd.thread().unpark();
            thd.join().unwrap();
        },
        Some(150), // We can't exhaust all orderings because of the infinite loop in the thread.
    )
}

// Test that shuttle explores schedulings where parked threads can be spuriously woken up.
// Threads should not rely on using park to accomplish synchronization.
#[test]
#[should_panic(expected = "spurious")]
fn thread_park_spuriously_wakeup() {
    check_dfs(
        || {
            let flag = Arc::new(AtomicBool::new(false));
            let thd = {
                let flag = Arc::clone(&flag);
                thread::spawn(move || {
                    thread::park();
                    assert!(flag.load(Ordering::SeqCst), "may not be true due to spurious wakeup");
                })
            };

            flag.store(true, Ordering::SeqCst);
            thd.thread().unpark();
            thd.join().unwrap();
        },
        None,
    )
}

// Test that Shuttle will detect a deadlock and panic if all threads are blocked in `park`, even
// though `park` can spuriously wake, since spurious wakeups aren't guaranteed and programs should
// not rely on them in order to make forward progress.
#[test]
#[should_panic(expected = "deadlock")]
fn thread_park_deadlock() {
    check_dfs(
        || {
            let thd = thread::spawn(thread::park);
            thread::park();
            thd.join().unwrap();
        },
        None,
    )
}

// From the docs: "Because the token is initially absent, `unpark` followed by `park` will result in
// the second call returning immediately"
#[test]
fn thread_unpark_park() {
    check_dfs(
        || {
            thread::current().unpark();
            thread::park();
        },
        None,
    )
}

// Test that `unpark` is not cumulative: multiple calls to `unpark` will still only unblock the very
// next call to `park`.
#[test]
#[should_panic(expected = "deadlock")]
fn thread_unpark_not_cumulative() {
    check_dfs(
        || {
            thread::current().unpark();
            thread::current().unpark();
            thread::park();
            thread::park();
        },
        None,
    )
}

// Unparking a thread should not unconditionally unblock it (e.g., if it's blocked waiting on a lock
// rather than parked)
#[test]
fn thread_unpark_unblock() {
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(false));
            let condvar = Arc::new(Condvar::new());

            let reader = {
                let lock = Arc::clone(&lock);
                let condvar = Arc::clone(&condvar);
                thread::spawn(move || {
                    let mut guard = lock.lock().unwrap();
                    while !*guard {
                        guard = condvar.wait(guard).unwrap();
                    }
                })
            };

            let _writer = {
                let lock = Arc::clone(&lock);
                let condvar = Arc::clone(&condvar);
                thread::spawn(move || {
                    let mut guard = lock.lock().unwrap();
                    *guard = true;
                    condvar.notify_one();
                })
            };

            reader.thread().unpark();
        },
        None,
    )
}

// Calling `unpark` on a thread that has already been unparked should be a no-op
#[test]
fn thread_double_unpark() {
    let seen_unparks = Arc::new(std::sync::Mutex::new(HashSet::new()));
    let seen_unparks_clone = Arc::clone(&seen_unparks);

    check_dfs(
        move || {
            let unpark_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let parkee = {
                let seen_unparks = Arc::clone(&seen_unparks);
                let unpark_count = Arc::clone(&unpark_count);
                thread::spawn(move || {
                    thread::park();
                    let unpark_count = unpark_count.load(Ordering::SeqCst);
                    seen_unparks.lock().unwrap().insert(unpark_count);
                    // If this is 1 we know `unpark` will be uncalled again, so this won't deadlock
                    if unpark_count == 1 {
                        thread::park();
                    }
                })
            };

            unpark_count.fetch_add(1, Ordering::SeqCst);
            parkee.thread().unpark();
            unpark_count.fetch_add(1, Ordering::SeqCst);
            parkee.thread().unpark();
        },
        None,
    );

    let seen_unparks = Arc::try_unwrap(seen_unparks_clone).unwrap().into_inner().unwrap();
    // It's possible for a thread to see 0 unparks, if it spuriously wakes up from a call to `park`.
    assert_eq!(seen_unparks, HashSet::from([0, 1, 2]));
}

// Test that `unpark` won't unblock a thread for reasons other than `park`, even if the blocked
// thread had called `park` before (and possible spuriously woke up).
#[test]
fn thread_unpark_after_spurious_wakeup() {
    check_dfs(
        || {
            let shared_state = Arc::new(AtomicU8::new(0));
            let barrier = Arc::new(Barrier::new(2));

            let thd = {
                let shared_state = Arc::clone(&shared_state);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    // Call `park`, which could possibly spuriously wake up.
                    thread::park();

                    // Wait on the barrier. If the main thread calls `unpark` after we've spuriously
                    // woken up from the `park` and starting waiting on the barrier, that shouldn't
                    // unblock this thread.
                    barrier.wait();
                    shared_state.store(1, Ordering::SeqCst);
                })
            };

            // It shouldn't be possible for this `unpark` to allow the thread to bypass waiting
            // on the barrier.
            thd.thread().unpark();
            assert_eq!(shared_state.load(Ordering::SeqCst), 0);

            barrier.wait();
            thd.join().unwrap();

            assert_eq!(shared_state.load(Ordering::SeqCst), 1);
        },
        None,
    )
}

#[test]
fn spawn_local_sanity() {
    check_dfs(
        || {
            let rc = Rc::new(0);
            shuttle::future::block_on(future::spawn_local(async { drop(rc) })).unwrap()
        },
        None,
    );
}
