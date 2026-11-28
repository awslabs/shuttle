//! Tests of the models of `once_cell::race`.

use shuttle::scheduler::DfsScheduler;
use shuttle::{Runner, check_dfs, check_pct, check_uncontrolled_nondeterminism, current, thread};
use shuttle_once_cell_inner::race::{OnceBool, OnceBox, OnceNonZeroUsize, OnceRef};
use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

fn nz(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n).unwrap()
}

#[test]
fn once_non_zero_usize() {
    check_dfs(
        || {
            let cell = OnceNonZeroUsize::new();
            assert_eq!(format!("{cell:?}"), "OnceNonZeroUsize { inner: 0 }");
            assert_eq!(cell.get(), None);
            assert_eq!(cell.get_or_try_init(|| Err(())), Err(()));
            assert_eq!(cell.get(), None);
            assert_eq!(cell.set(nz(92)), Ok(()));
            assert_eq!(cell.set(nz(62)), Err(()));
            assert_eq!(cell.get_or_init(|| unreachable!()), nz(92));
            assert_eq!(
                cell.get_or_try_init(|| -> Result<NonZeroUsize, ()> { unreachable!() }),
                Ok(nz(92))
            );
            // Safety: the cell is initialized, by this task.
            assert_eq!(unsafe { cell.get_unchecked() }, nz(92));
            assert_eq!(format!("{cell:?}"), "OnceNonZeroUsize { inner: 92 }");
            assert_eq!(OnceNonZeroUsize::default().get(), None);
        },
        None,
    );
}

#[test]
fn once_bool() {
    check_dfs(
        || {
            let cell = OnceBool::new();
            assert_eq!(format!("{cell:?}"), "OnceBool { inner: OnceNonZeroUsize { inner: 0 } }");
            assert_eq!(cell.get(), None);
            assert_eq!(cell.get_or_try_init(|| Err(())), Err(()));
            assert!(!cell.get_or_init(|| false));
            assert_eq!(cell.set(true), Err(()));
            assert_eq!(cell.get(), Some(false));
            assert_eq!(format!("{cell:?}"), "OnceBool { inner: OnceNonZeroUsize { inner: 2 } }");

            let cell = OnceBool::default();
            assert_eq!(cell.set(true), Ok(()));
            assert_eq!(
                cell.get_or_try_init(|| -> Result<bool, ()> { unreachable!() }),
                Ok(true)
            );
            assert_eq!(format!("{cell:?}"), "OnceBool { inner: OnceNonZeroUsize { inner: 1 } }");
        },
        None,
    );
}

#[test]
fn once_ref() {
    static VALUE: usize = 92;
    check_dfs(
        || {
            let cell: OnceRef<'_, usize> = OnceRef::new();
            assert_eq!(format!("{cell:?}"), "OnceRef(0x0)");
            assert_eq!(cell.get(), None);
            assert_eq!(cell.get_or_try_init(|| Err(())), Err(()));
            assert_eq!(cell.set(&VALUE), Ok(()));
            assert!(std::ptr::eq(cell.get().unwrap(), &VALUE));
            assert_eq!(format!("{cell:?}"), format!("OnceRef({:?})", &VALUE as *const usize));
            let other = 62;
            assert_eq!(cell.set(&other), Err(()));
            assert_eq!(cell.get_or_init(|| unreachable!()), &92);

            // A cell of references that do not live for `'static`.
            let local = vec![1, 2, 3];
            let cell = OnceRef::default();
            assert_eq!(cell.get_or_init(|| &local), &vec![1, 2, 3]);
            assert_eq!(
                cell.get_or_try_init(|| -> Result<&Vec<i32>, ()> { unreachable!() }),
                Ok(&local)
            );
        },
        None,
    );
}

#[test]
fn once_box() {
    check_dfs(
        || {
            let cell = OnceBox::new();
            assert_eq!(format!("{cell:?}"), "OnceBox(0x0)");
            assert_eq!(cell.get(), None);
            assert_eq!(cell.get_or_try_init(|| Err(())), Err(()));
            assert_eq!(cell.set(Box::new(92)), Ok(()));
            assert_eq!(cell.set(Box::new(62)), Err(Box::new(62)));
            assert_eq!(cell.get_or_init(|| unreachable!()), &92);
            assert_eq!(
                format!("{cell:?}"),
                format!("OnceBox({:?})", cell.get().unwrap() as *const i32)
            );
            assert_eq!(cell.clone().get(), Some(&92));
            assert_eq!(OnceBox::<i32>::default().clone().get(), None);

            let cell = OnceBox::with_value(Box::new("hello"));
            assert_eq!(cell.get(), Some(&"hello"));
            assert_eq!(cell.set(Box::new("goodbye")), Err(Box::new("goodbye")));
            assert_eq!(
                cell.get_or_try_init(|| -> Result<Box<&str>, ()> { unreachable!() }),
                Ok(&"hello")
            );
        },
        None,
    );
}

// The scheduling points: two to store a value in an empty cell, after the initializer ran, and
// none for anything else.
#[test]
fn context_switches() {
    check_dfs(
        || {
            let start = current::context_switches();
            let usize_cell = OnceNonZeroUsize::new();
            let bool_cell = OnceBool::new();
            let ref_cell = OnceRef::new();
            let box_cell = OnceBox::new();
            assert_eq!(usize_cell.get(), None);
            assert_eq!(bool_cell.get(), None);
            assert_eq!(ref_cell.get(), None);
            assert_eq!(box_cell.get(), None);
            assert_eq!(current::context_switches(), start);

            assert_eq!(usize_cell.get_or_init(|| nz(1)), nz(1));
            assert_eq!(current::context_switches(), start + 2);
            assert_eq!(bool_cell.set(true), Ok(()));
            assert_eq!(current::context_switches(), start + 4);
            assert_eq!(ref_cell.get_or_try_init(|| Ok::<_, ()>(&1)), Ok(&1));
            assert_eq!(current::context_switches(), start + 6);
            assert_eq!(box_cell.get_or_init(|| Box::new(1)), &1);
            assert_eq!(current::context_switches(), start + 8);
            // An initializer that fails stores nothing, so it takes none.
            assert_eq!(OnceNonZeroUsize::new().get_or_try_init(|| Err(())), Err(()));
            assert_eq!(current::context_switches(), start + 8);

            assert_eq!(usize_cell.get_or_init(|| unreachable!()), nz(1));
            assert_eq!(usize_cell.set(nz(2)), Err(()));
            assert!(bool_cell.get_or_init(|| unreachable!()));
            assert_eq!(ref_cell.get_or_init(|| unreachable!()), &1);
            assert_eq!(box_cell.set(Box::new(2)), Err(Box::new(2)));
            assert_eq!(box_cell.get_or_init(|| unreachable!()), &1);
            assert_eq!(current::context_switches(), start + 8);

            // Nor does a cell created with a value.
            let cell = OnceBox::with_value(Box::new(1));
            assert_eq!(cell.get_or_init(|| unreachable!()), &1);
            assert_eq!(cell.set(Box::new(2)), Err(Box::new(2)));
            assert_eq!(current::context_switches(), start + 8);
        },
        None,
    );
}

// Unlike `sync`, several tasks may run their initializers, and DFS finds both the schedules in
// which they do and those in which one finds the cell set. Every task returns the value that won,
// every task can win, and the values that lost are dropped.
#[test]
fn racing_initializers() {
    let initializer_counts = Arc::new(Mutex::new(HashSet::new()));
    let winners = Arc::new(Mutex::new(HashSet::new()));
    let (counts, winners_clone) = (Arc::clone(&initializer_counts), Arc::clone(&winners));

    struct CountDrops(usize, Arc<AtomicUsize>);
    impl Drop for CountDrops {
        fn drop(&mut self) {
            self.1.fetch_add(1, Ordering::SeqCst);
        }
    }

    check_dfs(
        move || {
            let cell = Arc::new(OnceBox::new());
            let initializers = Arc::new(AtomicUsize::new(0));
            let drops = Arc::new(AtomicUsize::new(0));
            let threads = (0..2)
                .map(|i| {
                    let (cell, initializers, drops) =
                        (Arc::clone(&cell), Arc::clone(&initializers), Arc::clone(&drops));
                    thread::spawn(move || {
                        cell.get_or_init(|| {
                            initializers.fetch_add(1, Ordering::SeqCst);
                            Box::new(CountDrops(i, drops))
                        })
                        .0
                    })
                })
                .collect::<Vec<_>>();
            let values = threads.into_iter().map(|t| t.join().unwrap()).collect::<HashSet<_>>();
            assert_eq!(values.len(), 1, "every task returns the value that won");
            let initializers = initializers.load(Ordering::SeqCst);
            assert_eq!(
                drops.load(Ordering::SeqCst),
                initializers - 1,
                "the losing values are dropped"
            );
            counts.lock().unwrap().insert(initializers);
            winners_clone.lock().unwrap().insert(cell.get().unwrap().0);
        },
        None,
    );

    assert_eq!(*initializer_counts.lock().unwrap(), HashSet::from([1, 2]));
    assert_eq!(*winners.lock().unwrap(), HashSet::from([0, 1]));
}

#[test]
fn racing_initializers_pct() {
    check_pct(
        || {
            let cell = Arc::new(OnceNonZeroUsize::new());
            let threads = (1..=4)
                .map(|i| {
                    let cell = Arc::clone(&cell);
                    thread::spawn(move || cell.get_or_init(|| nz(i)))
                })
                .collect::<Vec<_>>();
            let values = threads.into_iter().map(|t| t.join().unwrap()).collect::<HashSet<_>>();
            assert_eq!(values.len(), 1);
        },
        1_000,
        3,
    );
}

// A value read from a cell happens after the store, as with once_cell's `Acquire`/`Release`.
#[test]
fn reads_take_causality_from_the_store() {
    let saw_value = Arc::new(AtomicUsize::new(0));
    let saw_value_clone = Arc::clone(&saw_value);
    check_dfs(
        move || {
            let cell = Arc::new(OnceNonZeroUsize::new());
            let setter = {
                let cell = Arc::clone(&cell);
                thread::spawn(move || cell.set(nz(usize::from(thread::current().id()))).unwrap())
            };
            thread::yield_now();
            if let Some(id) = cell.get() {
                assert!(current::clock().get(id.get()) > 0);
                saw_value_clone.fetch_add(1, Ordering::SeqCst);
            }
            setter.join().unwrap();
        },
        None,
    );
    assert!(saw_value.load(Ordering::SeqCst) > 0);
}

// `static` cells are empty again in every execution.
#[test]
fn statics_reset_across_executions() {
    static USIZE: OnceNonZeroUsize = OnceNonZeroUsize::new();
    static BOOL: OnceBool = OnceBool::new();
    static REF: OnceRef<'static, u32> = OnceRef::new();
    static REFERENT: u32 = 7;
    static BOX: OnceBox<String> = OnceBox::new();
    static INITIALIZERS: AtomicUsize = AtomicUsize::new(0);

    let runner = Runner::new(DfsScheduler::new(None, false), Default::default());
    let executions = runner.run(|| {
        assert_eq!(USIZE.get(), None);
        assert_eq!(BOOL.get(), None);
        assert_eq!(REF.get(), None);
        assert_eq!(BOX.get(), None);
        let thread = thread::spawn(|| {
            USIZE.get_or_init(|| {
                INITIALIZERS.fetch_add(1, Ordering::SeqCst);
                nz(1)
            });
        });
        BOOL.set(true).unwrap();
        REF.set(&REFERENT).unwrap();
        BOX.set(Box::new("hello".to_string())).unwrap();
        thread.join().unwrap();
    });
    assert!(executions > 1);
    assert_eq!(INITIALIZERS.load(Ordering::SeqCst), executions);
}

#[test]
fn uncontrolled_nondeterminism() {
    static CELL: OnceBox<usize> = OnceBox::new();
    check_uncontrolled_nondeterminism(
        || {
            let threads = (0..3)
                .map(|_| thread::spawn(|| *CELL.get_or_init(|| Box::new(usize::from(thread::current().id())))))
                .collect::<Vec<_>>();
            let values = threads.into_iter().map(|t| t.join().unwrap()).collect::<HashSet<_>>();
            assert_eq!(values.len(), 1);
        },
        100,
    );
}

// The same check fails for once_cell's own, whose value outlives the execution that stored it.
#[test]
#[should_panic(expected = "possible nondeterminism")]
fn uncontrolled_nondeterminism_with_once_cell_itself() {
    static CELL: once_cell_orig::race::OnceBool = once_cell_orig::race::OnceBool::new();
    check_uncontrolled_nondeterminism(
        || {
            if CELL.set(true).is_ok() {
                thread::yield_now();
            }
        },
        10,
    );
}

#[test]
#[should_panic(expected = "`OnceNonZeroUsize::get_unchecked` was called on an empty cell")]
fn get_unchecked_on_an_empty_cell_panics() {
    check_dfs(
        || {
            // Safety: none; the model catches the mistake.
            unsafe { OnceNonZeroUsize::new().get_unchecked() };
        },
        None,
    );
}

// A `race` initializer runs outside of any lock, so initializing the same cell from it is not a
// deadlock, where the `sync` cells deadlock. once_cell's `race` behaves the same way: the inner
// `get_or_init` wins the compare-exchange and the outer one's value loses.
#[test]
fn reentrant_init_does_not_deadlock() {
    check_dfs(
        || {
            let cell = OnceNonZeroUsize::new();
            let value = cell.get_or_init(|| cell.get_or_init(|| nz(1)).saturating_add(1));
            assert_eq!(value, nz(1), "the inner initializer's value is stored");
            assert_eq!(cell.get(), Some(nz(1)));
        },
        None,
    );
}
