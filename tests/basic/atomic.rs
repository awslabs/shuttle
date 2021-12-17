use crate::basic::clocks::{check_clock, me};
use shuttle::sync::atomic::*;
use shuttle::{asynch, check_dfs, thread};
use std::collections::HashSet;
use std::sync::Arc;
use test_log::test;

macro_rules! int_tests {
    ($name:ident, $ty:ident) => {
        mod $name {
            use super::*;
            use test_log::test;

            #[test]
            fn store_store_reordering() {
                let observed_states = Arc::new(std::sync::Mutex::new(HashSet::new()));
                let observed_states_clone = observed_states.clone();

                check_dfs(
                    move || {
                        let x = Arc::new($ty::new(0));
                        let y = Arc::new($ty::new(0));

                        let thd1 = {
                            let x = x.clone();
                            let y = y.clone();
                            thread::spawn(move || {
                                x.store(1, Ordering::SeqCst);
                                y.store(1, Ordering::SeqCst);
                                // A writer's clock is not affected by readers
                                assert_eq!(me(), 1usize);
                                check_clock(|i, c| (c > 0) == (i == 0 || i == 1));
                            })
                        };
                        let thd2 = {
                            let x = x.clone();
                            let y = y.clone();
                            thread::spawn(move || {
                                let y = y.load(Ordering::SeqCst);
                                let x = x.load(Ordering::SeqCst);
                                // A reader inherits the clock from writers
                                assert_eq!(me(), 2usize);
                                check_clock(|i, c| {
                                    (c > 0)
                                        == match (x, y) {
                                            (0, 0) => i == 0 || i == 2,
                                            _ => i == 0 || i == 2 || i == 1,
                                        }
                                });
                                (x, y)
                            })
                        };

                        let thd1 = thd1.join().unwrap();
                        let thd2 = thd2.join().unwrap();
                        observed_states.lock().unwrap().insert((thd1, thd2));
                    },
                    None,
                );

                let observed_states = Arc::try_unwrap(observed_states_clone)
                    .unwrap()
                    .into_inner()
                    .unwrap();
                assert!(observed_states.contains(&((), (0, 0))));
                assert!(observed_states.contains(&((), (1, 0))));
                assert!(observed_states.contains(&((), (1, 1))));
                assert!(!observed_states.contains(&((), (0, 1))));
            }

            #[test]
            fn store_load_reordering() {
                let observed_states = Arc::new(std::sync::Mutex::new(HashSet::new()));
                let observed_states_clone = observed_states.clone();

                check_dfs(
                    move || {
                        let x = Arc::new($ty::new(0));
                        let y = Arc::new($ty::new(0));

                        let thd1 = {
                            let x = x.clone();
                            let y = y.clone();
                            thread::spawn(move || {
                                let x = x.load(Ordering::SeqCst);
                                y.store(1, Ordering::SeqCst);
                                assert_eq!(me(), 1usize);
                                check_clock(|i, c| {
                                    (c > 0)
                                        == match x {
                                            0 => i == 0 || i == 1,
                                            _ => i == 0 || i == 1 || i == 2,
                                        }
                                });
                                (x,)
                            })
                        };
                        let thd2 = {
                            let x = x.clone();
                            let y = y.clone();
                            thread::spawn(move || {
                                let y = y.load(Ordering::SeqCst);
                                x.store(1, Ordering::SeqCst);
                                assert_eq!(me(), 2usize);
                                check_clock(|i, c| {
                                    (c > 0)
                                        == match y {
                                            0 => i == 0 || i == 2,
                                            _ => i == 0 || i == 2 || i == 1,
                                        }
                                });
                                (y,)
                            })
                        };

                        let thd1 = thd1.join().unwrap();
                        let thd2 = thd2.join().unwrap();
                        observed_states.lock().unwrap().insert((thd1, thd2));
                    },
                    None,
                );

                let observed_states = Arc::try_unwrap(observed_states_clone)
                    .unwrap()
                    .into_inner()
                    .unwrap();
                assert!(observed_states.contains(&((0,), (0,))));
                assert!(observed_states.contains(&((0,), (1,))));
                assert!(observed_states.contains(&((1,), (0,))));
                assert!(!observed_states.contains(&((1,), (1,))));
            }

            #[test]
            fn load_store_reordering() {
                let observed_states = Arc::new(std::sync::Mutex::new(HashSet::new()));
                let observed_states_clone = observed_states.clone();

                check_dfs(
                    move || {
                        let x = Arc::new($ty::new(0));
                        let y = Arc::new($ty::new(0));

                        let thd1 = {
                            let x = x.clone();
                            let y = y.clone();
                            thread::spawn(move || {
                                x.store(1, Ordering::SeqCst);
                                let y = y.load(Ordering::SeqCst);
                                assert_eq!(me(), 1usize);
                                check_clock(|i, c| {
                                    (c > 0)
                                        == match y {
                                            0 => i == 0 || i == 1,
                                            _ => i == 0 || i == 1 || i == 2,
                                        }
                                });
                                (y,)
                            })
                        };
                        let thd2 = {
                            let x = x.clone();
                            let y = y.clone();
                            thread::spawn(move || {
                                y.store(1, Ordering::SeqCst);
                                let x = x.load(Ordering::SeqCst);
                                assert_eq!(me(), 2usize);
                                check_clock(|i, c| {
                                    (c > 0)
                                        == match x {
                                            0 => i == 0 || i == 2,
                                            _ => i == 0 || i == 2 || i == 1,
                                        }
                                });
                                (x,)
                            })
                        };

                        let thd1 = thd1.join().unwrap();
                        let thd2 = thd2.join().unwrap();
                        observed_states.lock().unwrap().insert((thd1, thd2));
                    },
                    None,
                );

                let observed_states = Arc::try_unwrap(observed_states_clone)
                    .unwrap()
                    .into_inner()
                    .unwrap();
                assert!(!observed_states.contains(&((0,), (0,)))); // would be allowed on x86
                assert!(observed_states.contains(&((0,), (1,))));
                assert!(observed_states.contains(&((1,), (0,))));
                assert!(observed_states.contains(&((1,), (1,))));
            }

            #[test]
            fn fetch_update() {
                let observed_values = Arc::new(std::sync::Mutex::new(HashSet::new()));
                let observed_values_clone = observed_values.clone();

                check_dfs(
                    move || {
                        let x = Arc::new($ty::new(0));

                        let thd1 = {
                            let x = x.clone();
                            thread::spawn(move || {
                                x.store(1, Ordering::SeqCst);
                                assert_eq!(me(), 1);
                            })
                        };

                        let thd2 = {
                            let x = x.clone();
                            thread::spawn(move || {
                                x.store(2, Ordering::SeqCst);
                                assert_eq!(me(), 2);
                            })
                        };

                        let val = x
                            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |x| Some(x + 5))
                            .unwrap();

                        assert_eq!(me(), 0);
                        // Note: since either thd1 or thd2 can overwrite the other,
                        // we can only check implications here
                        check_clock(|i, c| match val {
                            0 => (i == 0) == (c > 0),  // neither thread has executed
                            1 => !(i == 1) || (c > 0), // must have inherited clock from thd1
                            2 => !(i == 2) || (c > 0), // must have inherited clock from thd2
                            _ => unreachable!(),
                        });

                        thd1.join().unwrap();
                        thd2.join().unwrap();

                        let x = Arc::try_unwrap(x).unwrap().into_inner();
                        observed_values_clone.lock().unwrap().insert(x);
                    },
                    None,
                );

                let observed_values = Arc::try_unwrap(observed_values).unwrap().into_inner().unwrap();
                assert_eq!(observed_values.len(), 4);
                assert!(observed_values.contains(&1));
                assert!(observed_values.contains(&2));
                assert!(observed_values.contains(&6));
                assert!(observed_values.contains(&7));
            }

            #[test]
            fn compare_exchange() {
                let observed_values = Arc::new(std::sync::Mutex::new(HashSet::new()));
                let observed_values_clone = observed_values.clone();

                check_dfs(
                    move || {
                        let x = Arc::new($ty::new(0));

                        let thd1 = {
                            let x = x.clone();
                            thread::spawn(move || {
                                x.store(1, Ordering::SeqCst);
                                assert_eq!(me(), 1);
                            })
                        };

                        let result = x.compare_exchange(1, 5, Ordering::SeqCst, Ordering::SeqCst);
                        check_clock(|i, c| {
                            (c > 0)
                                == match result {
                                    Ok(_) => i == 0 || i == 1, // thd1 must have executed
                                    Err(_) => i == 0,          // thd1 hasn't executed yet
                                }
                        });

                        thd1.join().unwrap();

                        observed_values_clone.lock().unwrap().insert(result);
                    },
                    None,
                );

                let observed_values = Arc::try_unwrap(observed_values).unwrap().into_inner().unwrap();
                assert_eq!(observed_values.len(), 2);
                assert!(observed_values.contains(&Ok(1)));
                assert!(observed_values.contains(&Err(0)));
            }

            #[test]
            fn compare_exchange_weak() {
                let observed_values = Arc::new(std::sync::Mutex::new(HashSet::new()));
                let observed_values_clone = observed_values.clone();

                check_dfs(
                    move || {
                        let x = Arc::new($ty::new(0));

                        let thd1 = {
                            let x = x.clone();
                            thread::spawn(move || {
                                x.store(1, Ordering::SeqCst);
                                assert_eq!(me(), 1);
                            })
                        };

                        let result = x.compare_exchange_weak(1, 5, Ordering::SeqCst, Ordering::SeqCst);
                        check_clock(|i, c| {
                            (c > 0)
                                == match result {
                                    Ok(_) => i == 0 || i == 1, // thd1 must have executed
                                    Err(_) => i == 0,          // thd1 hasn't executed yet
                                }
                        });

                        thd1.join().unwrap();

                        observed_values_clone.lock().unwrap().insert(result);
                    },
                    None,
                );

                let observed_values = Arc::try_unwrap(observed_values).unwrap().into_inner().unwrap();
                // TODO our compare_exchange_weak cannot spuriously fail
                assert_eq!(observed_values.len(), 2);
                assert!(observed_values.contains(&Ok(1)));
                assert!(observed_values.contains(&Err(0)));
            }
        }
    };
}

int_tests!(int_i8, AtomicI8);
int_tests!(int_i16, AtomicI16);
int_tests!(int_i32, AtomicI32);
int_tests!(int_i64, AtomicI64);
int_tests!(int_isize, AtomicIsize);
int_tests!(int_u8, AtomicU8);
int_tests!(int_u16, AtomicU16);
int_tests!(int_u32, AtomicU32);
int_tests!(int_u64, AtomicU64);
int_tests!(int_usize, AtomicUsize);

mod bool {
    use super::*;
    use test_log::test;

    #[test]
    fn fetch_update() {
        let observed_values = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let observed_values_clone = observed_values.clone();

        check_dfs(
            move || {
                let x = Arc::new(AtomicBool::new(false));

                let thd1 = {
                    let x = x.clone();
                    thread::spawn(move || {
                        x.store(true, Ordering::SeqCst);
                        assert_eq!(me(), 1);
                    })
                };

                let val = x
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |x| Some(!x))
                    .unwrap();
                assert_eq!(me(), 0);
                check_clock(|i, c| {
                    (c > 0)
                        == match val {
                            false => i == 0,          // thd1 hasn't executed
                            true => i == 0 || i == 1, // thd1 must have executed
                        }
                });

                thd1.join().unwrap();

                let x = Arc::try_unwrap(x).unwrap().into_inner();
                observed_values_clone.lock().unwrap().insert(x);
            },
            None,
        );

        let observed_values = Arc::try_unwrap(observed_values).unwrap().into_inner().unwrap();
        assert_eq!(observed_values.len(), 2);
    }

    #[test]
    fn compare_exchange() {
        let observed_values = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let observed_values_clone = observed_values.clone();

        check_dfs(
            move || {
                let x = Arc::new(AtomicBool::new(false));

                let thd1 = {
                    let x = x.clone();
                    thread::spawn(move || {
                        x.store(true, Ordering::SeqCst);
                        assert_eq!(me(), 1);
                    })
                };

                let result = x.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst);
                check_clock(|i, c| {
                    (c > 0)
                        == match result {
                            Ok(_) => i == 0,            // thd1 hasn't executed
                            Err(_) => i == 0 || i == 1, // thd1 must have executed
                        }
                });

                thd1.join().unwrap();

                observed_values_clone.lock().unwrap().insert(result);
            },
            None,
        );

        let observed_values = Arc::try_unwrap(observed_values).unwrap().into_inner().unwrap();
        assert_eq!(observed_values.len(), 2);
        assert!(observed_values.contains(&Ok(false)));
        assert!(observed_values.contains(&Err(true)));
    }
}

mod ptr {
    use super::*;
    use test_log::test;

    #[test]
    fn fetch_update() {
        let observed_values = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let observed_values_clone = observed_values.clone();

        check_dfs(
            move || {
                let mut x = 0usize;
                let x_ptr = &mut x as *mut _;
                let ptr = Arc::new(AtomicPtr::new(x_ptr));

                let thd1 = {
                    let ptr = ptr.clone();
                    thread::spawn(move || {
                        let mut y = 0usize;
                        ptr.store(&mut y as *mut _, Ordering::SeqCst);
                        assert_eq!(me(), 1);
                    })
                };

                let val = ptr
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |_| Some(x_ptr))
                    .unwrap();
                check_clock(|i, c| (c > 0) == if val == x_ptr { i == 0 } else { i == 0 || i == 1 });

                thd1.join().unwrap();

                let ptr = Arc::try_unwrap(ptr).unwrap().into_inner();
                observed_values_clone.lock().unwrap().insert(ptr == x_ptr);
            },
            None,
        );

        let observed_values = Arc::try_unwrap(observed_values).unwrap().into_inner().unwrap();
        assert_eq!(observed_values.len(), 2);
    }

    #[test]
    fn compare_exchange() {
        let observed_values = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let observed_values_clone = observed_values.clone();

        check_dfs(
            move || {
                let mut x = 0usize;
                let x_ptr = &mut x as *mut _;
                let mut x2 = 0usize;
                let x2_ptr = &mut x2 as *mut _;
                let ptr = Arc::new(AtomicPtr::new(x_ptr));

                let thd1 = {
                    let ptr = ptr.clone();
                    thread::spawn(move || {
                        let mut y = 0usize;
                        ptr.store(&mut y as *mut _, Ordering::SeqCst);
                        assert_eq!(me(), 1);
                    })
                };

                let result = ptr.compare_exchange(x_ptr, x2_ptr, Ordering::SeqCst, Ordering::SeqCst);
                check_clock(|i, c| {
                    (c > 0)
                        == match result {
                            Ok(_) => i == 0,            // thd1 hasn't executed
                            Err(_) => i == 0 || i == 1, // thd1 must have executed
                        }
                });

                thd1.join().unwrap();

                observed_values_clone.lock().unwrap().insert(result.is_ok());
            },
            None,
        );

        let observed_values = Arc::try_unwrap(observed_values).unwrap().into_inner().unwrap();
        assert_eq!(observed_values.len(), 2);
    }
}

// We don't support relaxed orderings, but they at least shouldn't crash. This test should fail if
// we modeled Ordering::Relaxed correctly, but we don't.
#[test]
fn relaxed_orderings_work() {
    check_dfs(
        move || {
            let flag = Arc::new(AtomicBool::new(false));
            let data = Arc::new(AtomicU64::new(0));

            {
                let flag = Arc::clone(&flag);
                let data = Arc::clone(&data);
                thread::spawn(move || {
                    data.store(42, Ordering::Relaxed);
                    flag.store(true, Ordering::Relaxed);
                });
            }

            if flag.load(Ordering::Relaxed) {
                assert_eq!(data.load(Ordering::Relaxed), 42);
            }
        },
        None,
    );
}

// Check that atomics work from within futures
#[test]
fn atomics_futures() {
    check_dfs(
        move || {
            let flag = Arc::new(AtomicUsize::new(0));

            let future = {
                let flag = Arc::clone(&flag);
                asynch::spawn(async move { flag.fetch_add(1, Ordering::SeqCst) })
            };

            let old = asynch::block_on(future).unwrap();

            assert_eq!(old, 0);
            assert_eq!(flag.load(Ordering::SeqCst), 1);
        },
        None,
    )
}
