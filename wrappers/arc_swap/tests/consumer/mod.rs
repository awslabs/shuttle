//! Code written against arc-swap, as a crate that depends on `shuttle-arc-swap` under the name
//! `arc_swap` would have it. The same source is compiled against `shuttle-arc-swap` without the
//! `shuttle` feature, where it is arc-swap, and against `shuttle-arc-swap-inner`, which is what
//! `shuttle-arc-swap` is under `shuttle`, so every check here holds for arc-swap and for the model
//! alike. It runs on one task; the inner crate's tests check the behaviour across tasks.
//!
//! The test that includes it names the crate `arc_swap`, with the `weak` and `serde` features on.
//! It asserts nothing about `Arc::strong_count` while a `Guard` is alive, which the model and
//! arc-swap differ on (see the model's crate documentation).

use arc_swap::access::{Access, Constant, DynAccess, Map};
// `DynAccess` and `Access` both have a `load`, so the calls below name the trait.
use arc_swap::cache::Access as CacheAccess;
use arc_swap::{ArcSwap, ArcSwapAny, ArcSwapOption, ArcSwapWeak, Cache, DefaultStrategy, Guard, RefCnt};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Weak};

static STATIC_EMPTY: ArcSwapOption<u32> = ArcSwapOption::const_empty();

/// Exercises arc-swap's API.
pub fn exercise() {
    load_and_store();
    options();
    compare_and_swap_and_rcu();
    guards();
    ref_cnt_impls();
    accesses();
    caches();
    formatting();
    serde_round_trip();
    weak();
    statics();
}

fn load_and_store() {
    let storage = ArcSwap::from(Arc::new(1u32));
    assert_eq!(**storage.load(), 1);
    assert_eq!(*storage.load_full(), 1);

    storage.store(Arc::new(2));
    assert_eq!(**storage.load(), 2);

    let old = storage.swap(Arc::new(3));
    assert_eq!(*old, 2);
    assert_eq!(**storage.load(), 3);

    assert_eq!(*ArcSwap::from_pointee(4u32).load_full(), 4);
    assert_eq!(*ArcSwap::new(Arc::new(5u32)).into_inner(), 5);
    assert_eq!(*ArcSwap::<u32>::default().load_full(), 0);
    assert_eq!(
        *ArcSwapAny::<Arc<u32>, DefaultStrategy>::with_strategy(Arc::new(6), DefaultStrategy::default()).load_full(),
        6
    );

    // The value a guard points at stays valid while another value is stored.
    let guard = storage.load();
    storage.store(Arc::new(7));
    assert_eq!(**guard, 3, "the guard still sees the value it loaded");
    assert_eq!(**storage.load(), 7);
    drop(guard);
}

fn options() {
    let storage = ArcSwapOption::<u32>::empty();
    assert!(storage.load().is_none());
    assert!(storage.load_full().is_none());

    storage.store(Some(Arc::new(1)));
    assert_eq!(**storage.load().as_ref().unwrap(), 1);
    let old = storage.swap(None);
    assert_eq!(*old.unwrap(), 1);
    assert!(storage.load().is_none());

    assert_eq!(**ArcSwapOption::from_pointee(2u32).load().as_ref().unwrap(), 2);
    assert!(ArcSwapOption::<u32>::from_pointee(None).load().is_none());
    assert!(ArcSwapOption::<u32>::const_empty().load().is_none());
    assert!(ArcSwapOption::<u32>::default().load().is_none());
    assert!(ArcSwapOption::<u32>::empty().into_inner().is_none());
}

fn compare_and_swap_and_rcu() {
    let storage = ArcSwap::from_pointee(1u32);

    // A compare and swap against the value that is there succeeds, and returns it.
    let current = storage.load();
    let previous = storage.compare_and_swap(&current, Arc::new(2));
    assert!(Arc::ptr_eq(&current, &previous), "the exchange happened");
    assert_eq!(**storage.load(), 2);

    // One against another value fails, and returns what is there.
    let previous = storage.compare_and_swap(&current, Arc::new(3));
    assert_eq!(**previous, 2, "the value that is there");
    assert_eq!(**storage.load(), 2, "unchanged");

    // A raw pointer, a `&Arc` and a `Guard` can all stand for the expected value.
    let current = storage.load_full();
    let previous = storage.compare_and_swap(&current, Arc::new(4));
    assert_eq!(**previous, 2);
    let current = storage.load();
    let previous = storage.compare_and_swap(Arc::as_ptr(&current), Arc::new(5));
    assert_eq!(**previous, 4);
    let current = storage.load();
    let previous = storage.compare_and_swap(current, Arc::new(6));
    assert_eq!(**previous, 5);
    assert_eq!(**storage.load(), 6);

    // `rcu` makes a new value out of the old one.
    let previous = storage.rcu(|old| Arc::new(**old + 1));
    assert_eq!(*previous, 6);
    assert_eq!(**storage.load(), 7);

    // The same on an option.
    let option = ArcSwapOption::<u32>::empty();
    let previous = option.compare_and_swap(&None::<Arc<u32>>, Some(Arc::new(1)));
    assert!(previous.is_none(), "it was empty");
    assert_eq!(**option.load().as_ref().unwrap(), 1);
    let previous = option.rcu(|old| old.as_ref().map(|value| Arc::new(**value + 1)));
    assert_eq!(*previous.unwrap(), 1);
    assert_eq!(**option.load().as_ref().unwrap(), 2);
}

fn guards() {
    let storage = ArcSwap::from_pointee(1u32);
    let guard: Guard<Arc<u32>> = storage.load();
    assert_eq!(**guard, 1);
    let value: Arc<u32> = Guard::into_inner(guard);
    assert_eq!(*value, 1);

    let guard = Guard::<_, DefaultStrategy>::from_inner(Arc::clone(&value));
    assert_eq!(**guard, 1);
    let guard: Guard<Arc<u32>> = Arc::clone(&value).into();
    assert_eq!(**guard, 1);
    let guard: Guard<Option<Arc<u32>>> = Guard::default();
    assert!(guard.is_none());
}

fn ref_cnt_impls() {
    // Every smart pointer arc-swap can hold.
    let arc = ArcSwapAny::<Arc<u32>>::from(Arc::new(1));
    assert_eq!(**arc.load(), 1);
    let rc = ArcSwapAny::<Rc<u32>>::from(Rc::new(2));
    assert_eq!(**rc.load(), 2);
    let option = ArcSwapAny::<Option<Arc<u32>>>::from(Some(Arc::new(3)));
    assert_eq!(**option.load().as_ref().unwrap(), 3);
    let pinned = ArcSwapAny::<Pin<Arc<u32>>>::from(Arc::pin(4));
    assert_eq!(**pinned.load(), 4);
    let pinned_rc = ArcSwapAny::<Pin<Rc<u32>>>::from(Rc::pin(5));
    assert_eq!(**pinned_rc.load(), 5);

    // The trait's own operations, which third-party smart pointers implement.
    let value = Arc::new(6u32);
    let ptr = Arc::as_ptr(&value);
    assert_eq!(<Arc<u32> as RefCnt>::as_ptr(&value), ptr as *mut u32);
    let raw = <Arc<u32> as RefCnt>::into_ptr(value);
    // Safety: `raw` owns the count `value` had.
    let value = unsafe { <Arc<u32> as RefCnt>::from_ptr(raw) };
    assert_eq!(*value, 6);
    let raw = <Arc<u32> as RefCnt>::inc(&value);
    // Safety: `inc` made a count that this takes back.
    unsafe { <Arc<u32> as RefCnt>::dec(raw) };
    assert_eq!(*value, 6);
}

/// A part of a larger structure, as the `access` module is for.
#[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct Config {
    name: String,
    port: u16,
}

fn accesses() {
    let storage = Arc::new(ArcSwap::from_pointee(Config {
        name: "first".to_owned(),
        port: 1,
    }));

    // `Access` on the storage itself, which yields the value behind the `Arc`.
    let whole: &dyn Fn() -> String = &|| Access::<Config>::load(&*storage).name.clone();
    assert_eq!(whole(), "first");

    // A projection into a part of it, both through `map` and through `Map::new`.
    let port = storage.map(|config: &Config| &config.port);
    assert_eq!(*Access::load(&port), 1);
    let name = Map::new(Arc::clone(&storage), |config: &Config| &config.name);
    assert_eq!(*Access::load(&name), "first");

    // Type erasure.
    let erased: Box<dyn DynAccess<u16>> = Box::new(storage.map(|config: &Config| &config.port));
    assert_eq!(*DynAccess::load(&*erased), 1);
    let erased_send: Box<dyn DynAccess<u16> + Send> = Box::new(storage.map(|config: &Config| &config.port));
    assert_eq!(*DynAccess::load(&*erased_send), 1);
    let erased_sync: Box<dyn DynAccess<u16> + Send + Sync> = Box::new(storage.map(|config: &Config| &config.port));
    assert_eq!(*DynAccess::load(&*erased_sync), 1);
    let converted = arc_swap::access::AccessConvert(erased);
    assert_eq!(*Access::<u16>::load(&converted), 1);

    // A constant, for code that takes an `Access` but has no storage.
    let constant = Constant(7u16);
    assert_eq!(*Access::load(&constant), 7);

    // Every access sees the new value.
    storage.store(Arc::new(Config {
        name: "second".to_owned(),
        port: 2,
    }));
    assert_eq!(whole(), "second");
    assert_eq!(*Access::load(&port), 2);
    assert_eq!(*Access::load(&name), "second");
    assert_eq!(*DynAccess::load(&*erased_sync), 2);
}

fn caches() {
    let storage = Arc::new(ArcSwap::from_pointee(Config {
        name: "first".to_owned(),
        port: 1,
    }));
    let mut cache = Cache::new(Arc::clone(&storage));
    assert_eq!(cache.load().name, "first");
    assert_eq!(cache.load().port, 1, "a second load uses the cached value");
    assert_eq!(cache.arc_swap().load().name, "first");

    storage.store(Arc::new(Config {
        name: "second".to_owned(),
        port: 2,
    }));
    assert_eq!(cache.load().name, "second", "the cache revalidates");

    // The cache's own `Access` trait, and a projection.
    let mut cache: Cache<_, Arc<Config>> = Cache::from(Arc::clone(&storage));
    assert_eq!(CacheAccess::load(&mut cache).port, 2);
    let mut mapped = cache.map(|config: &Arc<Config>| &config.port);
    assert_eq!(*CacheAccess::load(&mut mapped), 2);

    // A cache of an option.
    let option = Arc::new(ArcSwapOption::from_pointee(1u32));
    let mut cache = Cache::new(option);
    assert_eq!(**cache.load().as_ref().unwrap(), 1);
}

fn formatting() {
    let storage = ArcSwap::from_pointee(1u32);
    assert_eq!(format!("{storage:?}"), "ArcSwapAny(1)");
    assert_eq!(storage.to_string(), "1");
    let guard = storage.load();
    assert_eq!(format!("{guard:?}"), "1");
    assert_eq!(guard.to_string(), "1");
    let option = ArcSwapOption::<u32>::empty();
    assert_eq!(format!("{option:?}"), "ArcSwapAny(None)");
}

fn serde_round_trip() {
    let storage = ArcSwap::from_pointee(Config {
        name: "serde".to_owned(),
        port: 9,
    });
    let text = serde_json::to_string(&storage).unwrap();
    assert_eq!(text, r#"{"name":"serde","port":9}"#);
    let back: ArcSwap<Config> = serde_json::from_str(&text).unwrap();
    assert_eq!(*back.load_full(), *storage.load_full());

    let option: ArcSwapOption<u32> = serde_json::from_str("null").unwrap();
    assert!(option.load().is_none());
    assert_eq!(serde_json::to_string(&option).unwrap(), "null");
}

fn weak() {
    let value = Arc::new(1u32);
    let storage: ArcSwapWeak<u32> = ArcSwapWeak::new(Arc::downgrade(&value));
    assert_eq!(*storage.load().upgrade().unwrap(), 1);
    storage.store(Weak::new());
    assert!(storage.load().upgrade().is_none());
    // A weak pointer does not keep the value alive.
    let storage: ArcSwapWeak<u32> = ArcSwapWeak::new(Arc::downgrade(&value));
    drop(value);
    assert!(storage.load().upgrade().is_none());
}

fn statics() {
    assert!(
        STATIC_EMPTY.load().is_none(),
        "a const-constructed static starts out empty"
    );
    STATIC_EMPTY.store(Some(Arc::new(1)));
    assert_eq!(**STATIC_EMPTY.load().as_ref().unwrap(), 1);
}
