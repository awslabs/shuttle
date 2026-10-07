//! Shuttle Explorer events. Explorer shows only semaphore events, so `RawRwLock` reports itself as
//! a semaphore of `PERMITS_ON_INITIALIZATION` permits (see the `raw_rwlock` module docs). These
//! tests check each event and its permit count.
//!
//! The tests need the `annotation` feature of this crate, which turns on `shuttle/annotation`.

#![cfg(feature = "annotation")]

use serde_json::Value;
use shuttle::scheduler::{AnnotationScheduler, RoundRobinScheduler};
use shuttle::{Runner, thread};
use shuttle_parking_lot_impl::{RwLock, RwLockUpgradableReadGuard, RwLockWriteGuard};
use std::sync::Arc;

/// The permit counts from the `raw_rwlock` module docs.
const PERMITS_ON_INITIALIZATION: u64 = 1 << 30;
const EXCLUSIVE: u64 = PERMITS_ON_INITIALIZATION;
const UPGRADABLE: u64 = PERMITS_ON_INITIALIZATION / 2 + 1;
const SHARED: u64 = 1;

/// A semaphore event: the task that recorded it, its kind without the `Semaphore` prefix, and its
/// arguments after the object ID.
#[derive(Debug, PartialEq)]
struct Event {
    task: u64,
    kind: String,
    args: Vec<Value>,
}

fn event(task: u64, kind: &str, args: &[Value]) -> Event {
    Event {
        task,
        kind: kind.to_string(),
        args: args.to_vec(),
    }
}

/// Run `f` once under an `AnnotationScheduler` and return the semaphore events. `f` must use one
/// lock and no other Shuttle primitive that records semaphore events.
///
/// `SHUTTLE_ANNOTATION_FILE` is process-global, so the tests run behind one mutex, as in
/// `shuttle/tests/basic/annotation.rs`.
fn lock_events<F>(f: F) -> Vec<Event>
where
    F: Fn() + Send + Sync + 'static,
{
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("annotated.json");
    // SAFETY: this binary has only the tests in this file, and they run one at a time behind the
    // mutex above, so no other thread reads or sets the environment at the same time.
    unsafe { std::env::set_var(shuttle::ANNOTATION_FILE, &path) };
    {
        // `AnnotationScheduler` writes the file when it is dropped.
        let runner = Runner::new(
            AnnotationScheduler::new(RoundRobinScheduler::new(1)),
            Default::default(),
        );
        runner.run(f);
    }
    let json = std::fs::read_to_string(&path).expect("the annotation file was not written");
    let schedule: Value = serde_json::from_str(&json).expect("the annotation file is not valid JSON");

    let mut object = None;
    let mut events = Vec::new();
    for info in schedule["events"].as_array().unwrap() {
        let Value::Object(map) = &info[2] else { continue };
        let (name, payload) = map.iter().next().unwrap();
        let Some(kind) = name.strip_prefix("Semaphore") else {
            continue;
        };
        let (id, args) = match payload {
            Value::Array(values) => (values[0].clone(), values[1..].to_vec()),
            id => (id.clone(), Vec::new()),
        };
        assert_eq!(*object.get_or_insert_with(|| id.clone()), id, "more than one semaphore");
        events.push(event(info[0].as_u64().unwrap(), kind, &args));
    }
    events
}

/// The permits that the events say each task holds never go below zero or above
/// `PERMITS_ON_INITIALIZATION`, and all of them are released at the end.
fn assert_balanced(events: &[Event]) {
    let mut held = 0i64;
    for e in events {
        let permits = |value: &Value| value.as_u64().unwrap() as i64;
        match (e.kind.as_str(), e.args.as_slice()) {
            ("AcquireFast", [count]) | ("AcquireUnblocked", [_, count]) => held += permits(count),
            ("TryAcquire", [count, granted]) if granted.as_bool().unwrap() => held += permits(count),
            ("Release", [count]) => held -= permits(count),
            _ => {}
        }
        assert!(
            (0..=PERMITS_ON_INITIALIZATION as i64).contains(&held),
            "{held} permits after {e:?} in {events:#?}"
        );
    }
    assert_eq!(held, 0, "permits still held at the end: {events:#?}");
}

fn n(permits: u64) -> Value {
    Value::from(permits)
}

/// Each operation, in one task, with the permits that the module docs give it.
#[test]
fn events_of_each_operation() {
    let events = lock_events(|| {
        let lock = RwLock::new(());
        drop(lock.read());
        drop(lock.try_write());
        let read = lock.read();
        assert!(lock.try_write().is_none());
        drop(lock.try_read());
        drop(read);
        let upgradable = lock.upgradable_read();
        let write = RwLockUpgradableReadGuard::upgrade(upgradable);
        let upgradable = RwLockWriteGuard::downgrade_to_upgradable(write);
        drop(RwLockUpgradableReadGuard::downgrade(upgradable));
        drop(RwLockWriteGuard::downgrade(lock.write()));
        let upgradable = lock.try_upgradable_read().unwrap();
        drop(RwLockUpgradableReadGuard::try_upgrade(upgradable).unwrap());
    });
    let (yes, no) = (Value::from(true), Value::from(false));
    let expected = vec![
        event(0, "Created", &[]),
        // read
        event(0, "AcquireFast", &[n(SHARED)]),
        event(0, "Release", &[n(SHARED)]),
        // try_write
        event(0, "TryAcquire", &[n(EXCLUSIVE), yes.clone()]),
        event(0, "Release", &[n(EXCLUSIVE)]),
        // read, a failed try_write, and a try_read
        event(0, "AcquireFast", &[n(SHARED)]),
        event(0, "TryAcquire", &[n(EXCLUSIVE), no]),
        event(0, "TryAcquire", &[n(SHARED), yes.clone()]),
        event(0, "Release", &[n(SHARED)]),
        event(0, "Release", &[n(SHARED)]),
        // upgradable_read, upgrade, downgrade_to_upgradable, downgrade_upgradable
        event(0, "AcquireFast", &[n(UPGRADABLE)]),
        event(0, "AcquireFast", &[n(EXCLUSIVE - UPGRADABLE)]),
        event(0, "Release", &[n(EXCLUSIVE - UPGRADABLE)]),
        event(0, "Release", &[n(UPGRADABLE - SHARED)]),
        event(0, "Release", &[n(SHARED)]),
        // write, downgrade
        event(0, "AcquireFast", &[n(EXCLUSIVE)]),
        event(0, "Release", &[n(EXCLUSIVE - SHARED)]),
        event(0, "Release", &[n(SHARED)]),
        // try_upgradable_read, try_upgrade
        event(0, "TryAcquire", &[n(UPGRADABLE), yes.clone()]),
        event(0, "TryAcquire", &[n(EXCLUSIVE - UPGRADABLE), yes]),
        event(0, "Release", &[n(EXCLUSIVE)]),
    ];
    assert_eq!(events, expected);
    assert_balanced(&events);
}

/// A read that waits behind a writer reports `AcquireBlocked`. The writer's unlock hands it the lock
/// (every unlock is fair, see the `raw_rwlock` module docs), so `AcquireUnblocked` comes from the
/// unlocking task, right after its `Release`.
#[test]
fn events_of_a_read_that_waits() {
    let events = lock_events(|| {
        let lock = Arc::new(RwLock::new(()));
        let write = lock.write();
        let reader = {
            let lock = Arc::clone(&lock);
            thread::spawn(move || drop(lock.read()))
        };
        // Under `RoundRobinScheduler`, these yields run the reader until it waits for the lock.
        for _ in 0..5 {
            thread::yield_now();
        }
        drop(write);
        reader.join().unwrap();
    });
    let expected = vec![
        event(0, "Created", &[]),
        event(0, "AcquireFast", &[n(EXCLUSIVE)]),
        event(1, "AcquireBlocked", &[n(SHARED)]),
        event(0, "Release", &[n(EXCLUSIVE)]),
        event(0, "AcquireUnblocked", &[n(1), n(SHARED)]),
        event(1, "Release", &[n(SHARED)]),
    ];
    assert_eq!(events, expected);
    assert_balanced(&events);
}

/// An upgrade that waits for a reader reports the missing permits as blocked, and then as unblocked
/// when the reader leaves.
#[test]
fn events_of_an_upgrade_that_waits() {
    let events = lock_events(|| {
        let lock = Arc::new(RwLock::new(()));
        let upgradable = lock.upgradable_read();
        let reader = {
            let lock = Arc::clone(&lock);
            thread::spawn(move || {
                let _read = lock.read();
                // Keep the read lock while the main task upgrades.
                for _ in 0..10 {
                    thread::yield_now();
                }
            })
        };
        // Under `RoundRobinScheduler`, these yields run the reader until it has the lock.
        for _ in 0..3 {
            thread::yield_now();
        }
        drop(RwLockUpgradableReadGuard::upgrade(upgradable));
        reader.join().unwrap();
    });
    let by = |task| events.iter().filter(|e| e.task == task).collect::<Vec<_>>();
    assert_eq!(
        by(0),
        [
            &event(0, "Created", &[]),
            &event(0, "AcquireFast", &[n(UPGRADABLE)]),
            &event(0, "AcquireBlocked", &[n(EXCLUSIVE - UPGRADABLE)]),
            &event(0, "AcquireUnblocked", &[n(0), n(EXCLUSIVE - UPGRADABLE)]),
            &event(0, "Release", &[n(EXCLUSIVE)]),
        ],
        "{events:#?}"
    );
    assert_eq!(
        by(1),
        [
            &event(1, "AcquireFast", &[n(SHARED)]),
            &event(1, "Release", &[n(SHARED)]),
        ],
        "{events:#?}"
    );
    assert_balanced(&events);
}
