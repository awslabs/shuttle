//! Code written against crossbeam-epoch, as a crate that depends on `shuttle-crossbeam-epoch` under
//! the name `crossbeam-epoch` would have it. The same source is compiled against
//! `shuttle-crossbeam-epoch` without the `shuttle` feature, where it is crossbeam-epoch, and against
//! `shuttle-crossbeam-epoch-inner`, which is what it is under `shuttle`. The test that includes it
//! provides `crate::thread`, std's threads or Shuttle's, and runs `exercise` under Shuttle when the
//! pointers are Shuttle's.
//!
//! The checks hold whichever crate `crossbeam_epoch` is, so they say nothing about when an object is
//! reclaimed.

use crossbeam_epoch::{self as epoch, Atomic, Collector, Owned, Pointable, Shared};
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, SeqCst};

/// A pointer that lives in a `static`, which crossbeam-epoch allows.
static HEAD: Atomic<u64> = Atomic::null();

/// Exercises the pointers' API.
pub fn exercise() {
    owned();
    shared();
    atomics();
    compare_exchange();
    tags();
    statics();
    pinning();
    collectors();
    deferring();
    unsized_pointees();
    conversions();
    formatting();
    concurrent();
}

fn owned() {
    let o = Owned::new(7u64);
    assert_eq!(*o, 7);
    let mut o = o;
    *o = 8;
    assert_eq!(*o.as_ref(), 8);
    assert_eq!(o.tag(), 0);
    let o = o.with_tag(1);
    assert_eq!((o.tag(), *o), (1, 8));
    assert_eq!(*o.clone(), 8);
    assert_eq!(*o.into_box(), 8);

    let o: Owned<u64> = 9u64.into();
    assert_eq!(*o, 9);
    let o: Owned<u64> = Box::new(10u64).into();
    // SAFETY: the pointer came from an `Owned` just now, and is used once.
    let o = unsafe { Owned::from_raw(Box::into_raw(o.into_box())) };
    assert_eq!(*o, 10);
}

fn shared() {
    let guard = &epoch::pin();
    let s = Owned::new(11u64).into_shared(guard);
    assert!(!s.is_null());
    // SAFETY: nothing else reaches this object, and the model never frees it while the guard lives.
    unsafe {
        assert_eq!(*s.deref(), 11);
        assert_eq!(s.as_ref(), Some(&11));
    }
    assert_eq!(s, s.with_tag(0));
    assert_ne!(s, s.with_tag(1));
    assert_eq!(s.with_tag(1).tag(), 1);
    assert_eq!(s.as_raw(), unsafe { s.deref() } as *const u64);

    let null: Shared<'_, u64> = Shared::null();
    assert!(null.is_null() && Shared::<u64>::default().is_null());
    // SAFETY: a null `Shared` has no object.
    assert!(unsafe { null.as_ref() }.is_none());
    // SAFETY: a null `Shared` has nothing to own.
    assert!(unsafe { null.try_into_owned() }.is_none());
    assert!(null < s || s < null);

    let mut s = s;
    // SAFETY: nothing else reaches this object.
    unsafe {
        *s.deref_mut() = 12;
        assert_eq!(*s.into_owned(), 12);
    }
}

fn atomics() {
    let guard = &epoch::pin();
    let a = Atomic::new(1u64);
    // SAFETY: nothing else reaches this object.
    assert_eq!(unsafe { *a.load(SeqCst, guard).deref() }, 1);
    // SAFETY: as above.
    assert_eq!(unsafe { *a.load_consume(guard).deref() }, 1);

    let old = a.swap(Owned::new(2), SeqCst, guard);
    // SAFETY: the swapped-out object is ours now.
    assert_eq!(*unsafe { old.into_owned() }, 1);
    // SAFETY: nothing else reaches the object `a` points at.
    assert_eq!(unsafe { *a.load(SeqCst, guard).deref() }, 2);

    a.store(Owned::new(3), SeqCst);
    // SAFETY: as above.
    assert_eq!(unsafe { *a.load(Acquire, guard).deref() }, 3);
    a.store(Shared::null(), SeqCst);
    assert!(a.load(SeqCst, guard).is_null());

    let a: Atomic<u64> = Atomic::null();
    assert!(a.load(SeqCst, guard).is_null());
    assert!(Atomic::<u64>::default().load(SeqCst, guard).is_null());
    // SAFETY: a null pointer has nothing to own.
    assert!(unsafe { a.try_into_owned() }.is_none());

    let a = Atomic::new(4u64);
    let copy = a.clone();
    assert_eq!(a.load(SeqCst, guard), copy.load(SeqCst, guard));
    // SAFETY: the two pointers point at the same object, and only one of them takes it.
    assert_eq!(*unsafe { a.into_owned() }, 4);

    // `fetch_update` on the tag.
    let a = Atomic::new(5u64);
    // `fetch_update` returns the pointer it replaced.
    let updated = a.fetch_update(SeqCst, SeqCst, guard, |current| Some(current.with_tag(1)));
    assert_eq!(updated.map(|s| s.tag()), Ok(0));
    assert_eq!(a.load(SeqCst, guard).tag(), 1);
    let gave_up = a.fetch_update(SeqCst, SeqCst, guard, |_| None);
    assert_eq!(gave_up.map(|s| s.tag()).unwrap_err().tag(), 1);
}

fn compare_exchange() {
    let guard = &epoch::pin();
    let a = Atomic::new(1u64);
    let current = a.load(SeqCst, guard);

    // A mismatch hands the new pointer back.
    let err = a
        .compare_exchange(Shared::null(), Owned::new(2), SeqCst, SeqCst, guard)
        .expect_err("the pointer is not null");
    assert_eq!(err.current, current);
    assert_eq!(*err.new, 2);
    assert!(format!("{err:?}").contains("CompareExchangeError"));

    let new = a
        .compare_exchange(current, Owned::new(3), SeqCst, SeqCst, guard)
        .expect("the pointer is what we loaded");
    // SAFETY: nothing else reaches these objects.
    unsafe {
        assert_eq!(*new.deref(), 3);
        assert_eq!(*current.into_owned(), 1);
    }

    // The weak version may fail spuriously, so it is used in a loop.
    let a = Atomic::new(4u64);
    let mut old = a.load(SeqCst, guard);
    let swapped = loop {
        match a.compare_exchange_weak(old, Owned::new(5), SeqCst, SeqCst, guard) {
            Ok(new) => break new,
            Err(err) => old = err.current,
        }
    };
    // SAFETY: nothing else reaches the object.
    assert_eq!(unsafe { *swapped.deref() }, 5);

    // The tag is part of the comparison.
    let a = Atomic::new(6u64);
    let current = a.load(SeqCst, guard);
    assert!(
        a.compare_exchange(current.with_tag(1), Shared::null(), SeqCst, SeqCst, guard)
            .is_err()
    );
}

fn tags() {
    let guard = &epoch::pin();
    // A `u64` is 8-byte aligned, so three bits are free for a tag.
    let a = Atomic::new(1u64);
    assert_eq!(a.fetch_or(0b101, AcqRel, guard).tag(), 0);
    assert_eq!(a.load(SeqCst, guard).tag(), 0b101);
    assert_eq!(a.fetch_and(0b100, AcqRel, guard).tag(), 0b101);
    assert_eq!(a.load(SeqCst, guard).tag(), 0b100);
    assert_eq!(a.fetch_xor(0b111, AcqRel, guard).tag(), 0b100);
    assert_eq!(a.load(SeqCst, guard).tag(), 0b011);
    // A tag that does not fit is truncated, and the address is left alone.
    let tagged = a.load(SeqCst, guard).with_tag(usize::MAX);
    assert_eq!(tagged.tag(), 0b111);
    assert_eq!(tagged.as_raw(), a.load(SeqCst, guard).as_raw());
    // SAFETY: nothing else reaches the object, and the tag does not change the address.
    assert_eq!(*unsafe { a.into_owned() }, 1);
}

/// Leaves the `static` as it found it, so that the checks hold however many times this runs in a
/// process, and whether or not the pointer starts each run null.
fn statics() {
    let guard = &epoch::pin();
    let before = HEAD.load(SeqCst, guard);
    let mine = Owned::new(13u64).into_shared(guard);
    HEAD.store(mine, SeqCst);
    // SAFETY: nothing else reaches the object.
    assert_eq!(unsafe { *HEAD.load(SeqCst, guard).deref() }, 13);
    HEAD.store(before, SeqCst);
    // SAFETY: nothing else reaches the object, which is not in the pointer any more.
    drop(unsafe { mine.into_owned() });
}

fn pinning() {
    assert!(!epoch::is_pinned());
    let guard = epoch::pin();
    assert!(epoch::is_pinned());
    {
        // Pinning is reentrant.
        let inner = epoch::pin();
        assert!(epoch::is_pinned());
        drop(inner);
        assert!(epoch::is_pinned());
    }
    let mut guard = guard;
    guard.repin();
    assert!(epoch::is_pinned());
    let unpinned_inside = guard.repin_after(|| !epoch::is_pinned());
    assert!(unpinned_inside);
    assert!(epoch::is_pinned());
    guard.flush();
    drop(guard);
    assert!(!epoch::is_pinned());

    // SAFETY: the unprotected guard protects nothing; nothing else reaches the object below.
    unsafe {
        let guard = epoch::unprotected();
        assert!(guard.collector().is_none());
        let a = Atomic::new(14u64);
        assert_eq!(*a.load(SeqCst, guard).deref(), 14);
        drop(a.into_owned());
        guard.flush();
    }
}

fn collectors() {
    let collector = Collector::new();
    assert_eq!(collector, collector.clone());
    assert_ne!(collector, Collector::default());

    let handle = collector.register();
    assert_eq!(*handle.collector(), collector);
    assert!(!handle.is_pinned());
    let guard = handle.pin();
    assert!(handle.is_pinned());
    assert_eq!(guard.collector(), Some(&collector));
    // A handle outlives the collector it came from.
    drop(collector);
    guard.flush();
    drop(guard);
    assert!(!handle.is_pinned());
    handle.pin().flush();

    // The default collector is one collector, and the task's handle belongs to it.
    assert_eq!(epoch::default_collector(), epoch::default_collector());
    assert_eq!(epoch::pin().collector(), Some(epoch::default_collector()));
    assert_ne!(*epoch::default_collector(), Collector::new());
}

fn deferring() {
    let guard = &epoch::pin();
    // Whether these run is up to the collector; a consumer can only say that deferring is allowed.
    guard.defer(|| ());
    // SAFETY: the closure owns what it touches, and nothing else reaches the object below.
    unsafe {
        guard.defer_unchecked(move || Box::new(15u64));
        let a = Atomic::new(16u64);
        let shared = a.load(SeqCst, guard);
        guard.defer_destroy(shared);
    }
    guard.flush();
}

/// crossbeam-epoch's `Pointable` for a slice, which allocates its length with its elements.
fn unsized_pointees() {
    let guard = &epoch::pin();
    let mut o = Owned::<[MaybeUninit<u8>]>::init(4);
    assert_eq!(o.len(), 4);
    for (i, slot) in o.iter_mut().enumerate() {
        slot.write(i as u8);
    }
    let a = Atomic::<[MaybeUninit<u8>]>::init(2);
    let shared = a.swap(o, SeqCst, guard);
    // SAFETY: nothing else reaches either object.
    unsafe {
        assert_eq!(shared.deref().len(), 2);
        drop(shared.into_owned());
        let stored = a.load(SeqCst, guard);
        assert_eq!(stored.deref().len(), 4);
        assert_eq!(stored.deref()[3].assume_init(), 3);
        drop(a.into_owned());
    }
    assert!(<[MaybeUninit<u8>] as Pointable>::ALIGN >= std::mem::align_of::<usize>());
}

fn conversions() {
    let guard = &epoch::pin();
    let a: Atomic<u64> = 17u64.into();
    // SAFETY: nothing else reaches the object.
    assert_eq!(unsafe { *a.load(SeqCst, guard).deref() }, 17);
    let a: Atomic<u64> = Box::new(18u64).into();
    // SAFETY: as above.
    assert_eq!(unsafe { *a.load(SeqCst, guard).deref() }, 18);
    let owned = Owned::new(19u64);
    let a: Atomic<u64> = owned.into();
    let shared = a.load(SeqCst, guard);
    let b: Atomic<u64> = shared.into();
    assert_eq!(b.load(SeqCst, guard), shared);
    let raw = shared.as_raw();
    let c: Atomic<u64> = raw.into();
    assert_eq!(c.load(SeqCst, guard), shared);
    let d: Shared<'_, u64> = raw.into();
    assert_eq!(d, shared);
    // SAFETY: the four pointers point at one object, and only one of them takes it.
    drop(unsafe { a.into_owned() });
}

fn formatting() {
    let guard = &epoch::pin();
    let a = Atomic::new(20u64);
    assert!(format!("{a:?}").starts_with("Atomic {"));
    assert!(format!("{a:p}").starts_with("0x"));
    let shared = a.load(SeqCst, guard);
    assert!(format!("{shared:?}").starts_with("Shared {"));
    assert!(format!("{shared:p}").starts_with("0x"));
    // SAFETY: nothing else reaches the object.
    let owned = unsafe { a.into_owned() };
    assert!(format!("{owned:?}").starts_with("Owned {"));
    assert_eq!(format!("{:?}", epoch::pin()), "Guard { .. }");
    assert_eq!(format!("{:?}", Collector::new()), "Collector { .. }");
    assert_eq!(format!("{:?}", Collector::new().register()), "LocalHandle { .. }");
}

/// A Treiber-stack push from two threads: every value is pushed exactly once, and the list is
/// intact however the pushes interleave.
fn concurrent() {
    struct Node {
        value: u64,
        next: Atomic<Node>,
    }

    let head: Arc<Atomic<Node>> = Arc::new(Atomic::null());
    let pushers: Vec<_> = (1..=2)
        .map(|value| {
            let head = Arc::clone(&head);
            crate::thread::spawn(move || {
                let guard = &epoch::pin();
                let mut node = Owned::new(Node {
                    value,
                    next: Atomic::null(),
                });
                loop {
                    let current = head.load(Relaxed, guard);
                    node.next.store(current, Relaxed);
                    match head.compare_exchange(current, node, SeqCst, Relaxed, guard) {
                        Ok(_) => break,
                        Err(err) => node = err.new,
                    }
                }
            })
        })
        .collect();
    for pusher in pushers {
        pusher.join().unwrap();
    }

    let guard = &epoch::pin();
    let mut values = Vec::new();
    let mut next = head.load(SeqCst, guard);
    // SAFETY: the pushers are done, and no one else reaches the nodes.
    while let Some(node) = unsafe { next.as_ref() } {
        values.push(node.value);
        next = node.next.load(SeqCst, guard);
    }
    values.sort_unstable();
    assert_eq!(values, vec![1, 2]);

    // Hand the nodes to the collector, which frees them when it sees fit.
    let mut next = head.swap(Shared::null(), SeqCst, guard);
    // SAFETY: nothing reaches the nodes any more, which is what `defer_destroy` asks for.
    unsafe {
        while !next.is_null() {
            let after = next.deref().next.load(SeqCst, guard);
            guard.defer_destroy(next);
            next = after;
        }
    }
}
