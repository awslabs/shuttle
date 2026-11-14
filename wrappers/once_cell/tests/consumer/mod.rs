//! Code written against once_cell, as a crate that depends on `shuttle-once_cell` under the name
//! `once_cell` would have it. The same source is compiled against `shuttle-once_cell` without the
//! `shuttle` feature, where it is once_cell, and against `shuttle-once_cell-inner`, which is what
//! `shuttle-once_cell` is under `shuttle`. The test that includes it names the crate `once_cell`.
//!
//! Without Shuttle the statics keep their values for the life of the test process, and under
//! Shuttle they start empty in every execution, so the checks hold whichever has happened before.

use once_cell::race::{OnceBool, OnceBox, OnceNonZeroUsize, OnceRef};
use once_cell::sync::{Lazy, OnceCell};
use once_cell::unsync;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;

static CONFIG: Lazy<HashMap<&'static str, u32>> = Lazy::new(|| {
    let mut m = HashMap::new();
    m.insert("retries", 3);
    m.insert("shards", 8);
    m
});
static VERSION: OnceCell<&'static str> = OnceCell::with_value("1.0");
static INSTANCE: OnceCell<String> = OnceCell::new();
static WINNER: OnceCell<usize> = OnceCell::new();
static FAST_PATH: OnceBool = OnceBool::new();
static WORKER_ID: OnceNonZeroUsize = OnceNonZeroUsize::new();
static NAME: OnceBox<String> = OnceBox::new();
static DEFAULT_NAME: &str = "default";
static NAME_REF: OnceRef<'static, &'static str> = OnceRef::new();

/// A component that keeps a table it learns of later in a field.
pub struct Engine {
    table: OnceCell<Arc<Vec<u32>>>,
    lazy_len: unsync::OnceCell<usize>,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    pub fn new() -> Self {
        Engine {
            table: OnceCell::new(),
            lazy_len: unsync::OnceCell::new(),
        }
    }

    pub fn install(&self, table: Arc<Vec<u32>>) -> Result<(), Arc<Vec<u32>>> {
        self.table.set(table)
    }

    pub fn table(&self) -> Option<Arc<Vec<u32>>> {
        self.table.get().cloned()
    }

    pub fn table_len(&self) -> usize {
        *self
            .lazy_len
            .get_or_init(|| self.table.get().map_or(0, |table| table.len()))
    }
}

/// What a worker saw.
#[derive(Debug, Clone, PartialEq)]
pub struct Seen {
    pub instance: String,
    pub winner: usize,
    pub fast_path: bool,
    pub worker_id: NonZeroUsize,
    pub name: String,
    pub name_ref: &'static str,
    pub shards: u32,
}

/// One of several workers that race for the statics.
pub fn worker(i: usize) -> Seen {
    Seen {
        instance: INSTANCE.get_or_init(|| format!("instance of worker {i}")).clone(),
        winner: *WINNER.get_or_init(|| i),
        fast_path: FAST_PATH.get_or_init(|| i < 2),
        worker_id: WORKER_ID.get_or_init(|| NonZeroUsize::new(i + 1).unwrap()),
        name: NAME.get_or_init(|| Box::new(format!("worker-{i}"))).clone(),
        name_ref: NAME_REF.get_or_init(|| &DEFAULT_NAME),
        shards: CONFIG["shards"],
    }
}

/// Every worker saw the same values, which one of them produced.
pub fn check(seen: &[Seen]) {
    let first = &seen[0];
    for other in seen {
        assert_eq!(other, first);
    }
    assert_eq!(first.shards, 8);
    assert_eq!(first.name_ref, "default");
}

/// Exercises once_cell's API from one thread.
pub fn exercise() {
    statics();
    sync_once_cell();
    sync_lazy();
    race();
    unsync_cells();
    engine();
}

fn statics() {
    assert_eq!(CONFIG.get("retries"), Some(&3));
    assert_eq!(Lazy::force(&CONFIG).len(), 2);
    assert_eq!(VERSION.get(), Some(&"1.0"));
    assert_eq!(VERSION.set("2.0"), Err("2.0"));
}

fn sync_once_cell() {
    let cell = OnceCell::new();
    assert_eq!(cell.get(), None);
    assert_eq!(format!("{cell:?}"), "OnceCell(Uninit)");
    assert_eq!(cell.get_or_try_init(|| "x".parse::<u32>()).map_err(|_| ()), Err(()));
    assert_eq!(cell.get_or_init(|| 92), &92);
    assert_eq!(cell.set(62), Err(62));
    assert_eq!(cell.try_insert(62), Err((&92, 62)));
    assert_eq!(cell.wait(), &92);
    assert_eq!(format!("{cell:?}"), "OnceCell(92)");
    assert_eq!(cell.clone(), OnceCell::from(92));

    let mut cell = cell;
    *cell.get_mut().unwrap() = 1;
    assert_eq!(cell.take(), Some(1));
    assert_eq!(cell.into_inner(), None);
    assert_eq!(OnceCell::with_value(5).into_inner(), Some(5));
    assert_eq!(OnceCell::<u8>::default(), OnceCell::new());
}

fn sync_lazy() {
    let lazy: Lazy<Vec<u32>> = Lazy::new(|| vec![1, 2, 3]);
    assert_eq!(Lazy::get(&lazy), None);
    assert_eq!(lazy.len(), 3);
    assert_eq!(Lazy::get(&lazy), Some(&vec![1, 2, 3]));
    assert_eq!(format!("{lazy:?}"), r#"Lazy { cell: OnceCell([1, 2, 3]), init: ".." }"#);

    let mut lazy = Lazy::new(|| String::from("a"));
    lazy.push('b');
    assert_eq!(Lazy::get_mut(&mut lazy).map(|s| s.as_str()), Some("ab"));
    assert_eq!(Lazy::into_value(lazy).ok(), Some(String::from("ab")));

    let suffix = String::from("!");
    let lazy = Lazy::new(move || format!("hello{suffix}"));
    assert_eq!(&*lazy, "hello!");

    let lazy = Lazy::<u8>::default();
    assert_eq!(*lazy, 0);
}

fn race() {
    let cell = OnceNonZeroUsize::new();
    assert_eq!(format!("{cell:?}"), "OnceNonZeroUsize { inner: 0 }");
    assert_eq!(cell.get_or_init(|| NonZeroUsize::new(4).unwrap()).get(), 4);
    assert_eq!(cell.set(NonZeroUsize::new(5).unwrap()), Err(()));
    assert_eq!(format!("{cell:?}"), "OnceNonZeroUsize { inner: 4 }");

    let cell = OnceBool::new();
    assert_eq!(cell.get_or_try_init(|| Err::<bool, _>("no")), Err("no"));
    assert!(cell.get_or_init(|| true));
    assert_eq!(format!("{cell:?}"), "OnceBool { inner: OnceNonZeroUsize { inner: 1 } }");

    let referent = 7;
    let cell = OnceRef::new();
    assert_eq!(cell.set(&referent), Ok(()));
    assert_eq!(cell.get(), Some(&7));

    let cell = OnceBox::new();
    assert_eq!(cell.get_or_init(|| Box::new(String::from("boxed"))), "boxed");
    assert_eq!(cell.set(Box::new(String::new())), Err(Box::new(String::new())));
    assert_eq!(OnceBox::with_value(Box::new(1)).clone().get(), Some(&1));
}

fn unsync_cells() {
    let cell = unsync::OnceCell::new();
    assert_eq!(cell.get_or_init(|| 1), &1);
    let lazy = unsync::Lazy::new(|| 2);
    assert_eq!(*lazy, 2);
}

fn engine() {
    let engine = Engine::new();
    assert_eq!(engine.table(), None);
    engine.install(Arc::new(vec![1, 2])).unwrap();
    assert!(engine.install(Arc::new(vec![])).is_err());
    assert_eq!(engine.table().map(|table| table.len()), Some(2));
    assert_eq!(engine.table_len(), 2);
}
