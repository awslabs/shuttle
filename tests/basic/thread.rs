use shuttle::sync::Mutex;
use shuttle::{check_dfs, check_random, thread};
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
}
