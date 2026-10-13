use serde_json::Value;
use shuttle::annotations::WithName;
use shuttle::future::{
    self,
    batch_semaphore::{BatchSemaphore, Fairness},
};
use shuttle::scheduler::{AnnotationScheduler, DfsScheduler, RoundRobinScheduler, Scheduler};
use shuttle::sync::{mpsc, Mutex};
use shuttle::{current, thread, Runner};
use std::sync::Arc;

/// Two tasks contending on a mutex, so the annotated schedule contains task creation
/// and termination, a semaphore, and both a fast and a blocking acquire.
fn contend() {
    let lock = Arc::new(Mutex::new(0usize));
    let handles = (0..2)
        .map(|i| {
            let lock = lock.clone();
            thread::spawn(move || {
                current::set_name_for_task(current::me(), format!("worker-{i}"));
                *lock.lock().unwrap() += 1;
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        handle.join().unwrap();
    }
}

/// Run `f` under an `AnnotationScheduler` and return the parsed annotated schedule.
///
/// `SHUTTLE_ANNOTATION_FILE` is process-global, so all annotation tests share this
/// helper and run behind a single mutex to keep them from clobbering each other.
fn annotated<F>(f: F) -> Value
where
    F: Fn() + Send + Sync + 'static,
{
    annotated_with(RoundRobinScheduler::new(1), f)
}

/// As `annotated`, with an `AnnotationScheduler` around `scheduler`, which runs one execution.
fn annotated_with<S, F>(scheduler: S, f: F) -> Value
where
    S: Scheduler + 'static,
    F: Fn() + Send + Sync + 'static,
{
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("annotated.json");
    std::env::set_var(shuttle::ANNOTATION_FILE, &path);

    {
        // `AnnotationScheduler` writes the file when it is dropped
        let runner = Runner::new(AnnotationScheduler::new(scheduler), Default::default());
        runner.run(f);
    }

    let json = std::fs::read_to_string(&path).expect("annotation file should have been written");
    serde_json::from_str(&json).expect("annotation file should be valid JSON")
}

/// The annotated schedule uses the schema Shuttle Explorer expects (see
/// `shuttle-explorer/src/common/schedule.mts`).
#[test]
fn annotation_schema() {
    let schedule = annotated(contend);

    assert_eq!(schedule["version"], 0);
    for key in ["files", "functions", "objects", "tasks", "events"] {
        assert!(schedule[key].is_array(), "`{key}` should be an array");
    }

    // the main task plus the two workers
    let tasks = schedule["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 3);
    for task in tasks {
        assert!(task["created_by"].is_u64());
        assert!(task["first_step"].as_u64().unwrap() <= task["last_step"].as_u64().unwrap());
    }
    assert_eq!(tasks[0]["name"], Value::Null, "the main task is unnamed");
    let worker_names = tasks[1..]
        .iter()
        .map(|task| task["name"].as_str().expect("workers are named"))
        .collect::<Vec<_>>();
    assert!(
        worker_names.iter().all(|name| name.starts_with("worker-")),
        "expected `set_name_for_task` to be recorded, got {worker_names:?}"
    );

    // the mutex's batch semaphore
    let objects = schedule["objects"].as_array().unwrap();
    assert_eq!(objects.len(), 1);
    assert!(objects[0]["created_by"].is_u64());

    let events = schedule["events"].as_array().unwrap();
    assert!(!events.is_empty());
    for event in events {
        let event = event.as_array().expect("an event is a tuple");
        assert!(event[0].is_u64(), "first element is the task ID");
        assert!(
            event[1].is_null() || event[1].is_array(),
            "second element is a backtrace"
        );
        assert!(
            event[4].is_null() || event[4].is_array(),
            "fifth element is runnable tasks"
        );
    }
}

/// Every event carries a non-empty vector clock.
///
/// Note that this test cannot catch the `annotation` feature failing to turn on
/// `vector-clocks`: the integration tests already get vector clocks via the
/// `shuttle = { path = ".", features = ["vector-clocks"] }` dev-dependency. The
/// `compile_error!` in `shuttle-engine/src/annotations/mod.rs` guards that instead.
#[test]
fn annotation_records_vector_clocks() {
    let schedule = annotated(contend);

    for event in schedule["events"].as_array().unwrap() {
        let clock = event[3].as_array().expect("every event should have a vector clock");
        assert!(!clock.is_empty(), "vector clocks should not be empty");
        assert!(clock.iter().all(|entry| entry.is_u64()));
    }
}

/// The event kinds a contended mutex is expected to produce all show up, and the
/// object and task IDs they carry are in range.
#[test]
fn annotation_records_events() {
    let schedule = annotated(contend);
    let num_objects = schedule["objects"].as_array().unwrap().len() as u64;
    let num_tasks = schedule["tasks"].as_array().unwrap().len() as u64;

    let mut kinds = std::collections::HashSet::new();
    for event in schedule["events"].as_array().unwrap() {
        match &event[2] {
            // unit variants serialize as a bare string
            Value::String(name) => {
                kinds.insert(name.clone());
            }
            // newtype/tuple variants serialize as a single-key object
            Value::Object(map) => {
                let (name, payload) = map.iter().next().expect("a variant has one key");
                match name.as_str() {
                    "SemaphoreCreated" | "SemaphoreClosed" => {
                        assert!(payload.as_u64().unwrap() < num_objects);
                    }
                    "TaskCreated" => {
                        assert!(payload[0].as_u64().unwrap() < num_tasks);
                        assert!(payload[1].is_boolean());
                    }
                    _ => {
                        assert!(payload[0].as_u64().unwrap() < num_objects, "{name} names an object");
                    }
                }
                kinds.insert(name.clone());
            }
            other => panic!("unexpected event kind: {other}"),
        }
    }

    for expected in [
        "TaskCreated",
        "TaskTerminated",
        "Tick",
        "SemaphoreCreated",
        "SemaphoreAcquireFast",
        "SemaphoreAcquireBlocked",
        "SemaphoreAcquireUnblocked",
        "SemaphoreRelease",
    ] {
        assert!(kinds.contains(expected), "expected a {expected} event, got {kinds:?}");
    }
}

/// Events from unfinished tasks' destructors and static destructors carry the task and clock
/// of the destructor, even though teardown runs without the scheduler.
#[test]
fn teardown_annotations_follow_task_and_clock() {
    type Snapshots = Arc<std::sync::Mutex<Vec<(usize, Vec<u32>, Vec<u32>)>>>;

    struct TraceOnDrop(Snapshots);

    impl Drop for TraceOnDrop {
        fn drop(&mut self) {
            let task = usize::from(current::me());
            let before = current::clock().to_vec();
            let mutex = Mutex::new(()).with_name(&format!("teardown-{task}"));
            let guard = mutex.lock().unwrap();
            let acquired = current::clock().to_vec();
            drop(guard);
            self.0.lock().unwrap().push((task, before, acquired));
        }
    }

    shuttle::lazy_static! {
        static ref STATIC: std::sync::Mutex<Option<TraceOnDrop>> = std::sync::Mutex::new(None);
    }

    let snapshots = Snapshots::default();
    let observed = snapshots.clone();
    let schedule = annotated(move || {
        *STATIC.lock().unwrap() = Some(TraceOnDrop(observed.clone()));
        for _ in 0..2 {
            let observed = observed.clone();
            let (started_tx, started_rx) = mpsc::channel();
            drop(future::spawn_local(async move {
                let _trace = TraceOnDrop(observed);
                started_tx.send(()).unwrap();
                std::future::pending::<()>().await;
            }));
            started_rx.recv().unwrap();
        }
    });

    let snapshots = snapshots.lock().unwrap();
    assert_eq!(
        snapshots.iter().map(|(task, _, _)| *task).collect::<Vec<_>>(),
        vec![1, 2, 0],
        "tasks are torn down in order, then the static runs as the main thread"
    );
    let objects = schedule["objects"].as_array().unwrap();
    let events = schedule["events"].as_array().unwrap();
    for (task, before, acquired) in snapshots.iter() {
        let object_id = objects
            .iter()
            .position(|object| object["name"] == format!("teardown-{task}"))
            .expect("the destructor created a named mutex");
        assert_eq!(objects[object_id]["created_by"], *task);
        for (kind, clock) in [
            ("SemaphoreCreated", before),
            ("SemaphoreAcquireFast", acquired),
            ("SemaphoreRelease", acquired),
        ] {
            let matching = events
                .iter()
                .filter(|event| {
                    let Some(payload) = event[2].get(kind) else {
                        return false;
                    };
                    let id = if kind == "SemaphoreCreated" {
                        payload.as_u64()
                    } else {
                        payload[0].as_u64()
                    };
                    id == Some(object_id as u64)
                })
                .collect::<Vec<_>>();
            assert_eq!(matching.len(), 1, "expected one {kind} for task {task}");
            assert_eq!(matching[0][0], *task, "{kind} has the wrong task");
            assert_eq!(matching[0][3], serde_json::json!(clock), "{kind} has the wrong clock");
            assert!(matching[0][4].is_null(), "teardown does not schedule runnable tasks");
        }
    }
}

/// The events of the first semaphore that `f` creates, as the task that recorded each, its kind
/// without the `Semaphore` prefix, and its arguments after the object ID.
fn semaphore_events<F>(f: F) -> Vec<(u64, String, Vec<Value>)>
where
    F: Fn() + Send + Sync + 'static,
{
    semaphore_events_with(RoundRobinScheduler::new(1), f)
}

/// As `semaphore_events`, under `scheduler`.
fn semaphore_events_with<S, F>(scheduler: S, f: F) -> Vec<(u64, String, Vec<Value>)>
where
    S: Scheduler + 'static,
    F: Fn() + Send + Sync + 'static,
{
    let schedule = annotated_with(scheduler, f);
    let mut object = None;
    let mut events = Vec::new();
    for event in schedule["events"].as_array().unwrap() {
        let Value::Object(map) = &event[2] else { continue };
        let (name, payload) = map.iter().next().unwrap();
        let Some(kind) = name.strip_prefix("Semaphore") else {
            continue;
        };
        let (id, args) = match payload {
            Value::Array(values) => (values[0].clone(), values[1..].to_vec()),
            id => (id.clone(), Vec::new()),
        };
        if *object.get_or_insert_with(|| id.clone()) == id {
            events.push((event[0].as_u64().unwrap(), kind.to_string(), args));
        }
    }
    events
}

fn event(task: u64, kind: &str, args: &[u64]) -> (u64, String, Vec<Value>) {
    (
        task,
        kind.to_string(),
        args.iter().map(|&arg| Value::from(arg)).collect(),
    )
}

fn try_acquire(task: u64, num_permits: u64) -> (u64, String, Vec<Value>) {
    (
        task,
        "TryAcquire".to_string(),
        vec![Value::from(num_permits), Value::from(true)],
    )
}

/// On a `Fairness::ParkingLot` semaphore, a release wakes a waiter to try again, and leaves the
/// permit free. The waiter takes it when it tries, so the waiter itself reports
/// `AcquireUnblocked`, after the release.
#[test]
fn parking_lot_events_of_a_woken_waiter() {
    let events = semaphore_events(|| {
        let semaphore = Arc::new(BatchSemaphore::new(1, Fairness::ParkingLot));
        semaphore.try_acquire(1).unwrap();
        let waiter = {
            let semaphore = semaphore.clone();
            thread::spawn(move || {
                semaphore.acquire_blocking(1).unwrap();
                semaphore.release(1);
            })
        };
        // Under `RoundRobinScheduler`, these yields run the waiter until it parks.
        for _ in 0..5 {
            thread::yield_now();
        }
        semaphore.release(1);
        waiter.join().unwrap();
    });
    let expected = vec![
        event(0, "Created", &[]),
        try_acquire(0, 1),
        event(1, "AcquireBlocked", &[1]),
        event(0, "Release", &[1]),
        event(1, "AcquireUnblocked", &[1, 1]),
        event(1, "Release", &[1]),
    ];
    assert_eq!(events, expected);
}

/// A woken waiter that loses the race for the permit parks again, which it reports as another
/// `AcquireBlocked`.
#[test]
fn parking_lot_events_of_a_lost_race() {
    let events = semaphore_events(|| {
        let semaphore = Arc::new(BatchSemaphore::new(1, Fairness::ParkingLot));
        semaphore.try_acquire(1).unwrap();
        let (go_tx, go_rx) = shuttle::sync::mpsc::channel::<()>();
        let waiter = {
            let semaphore = semaphore.clone();
            future::spawn(async move {
                let mut acquire = Box::pin(semaphore.acquire(1));
                assert!(futures::poll!(acquire.as_mut()).is_pending());
                go_rx.recv().unwrap();
                acquire.await.unwrap();
                semaphore.release(1);
            })
        };
        for _ in 0..5 {
            thread::yield_now();
        }
        semaphore.release(1); // wakes the waiter, which waits for `go`
        semaphore.try_acquire(1).unwrap();
        go_tx.send(()).unwrap();
        for _ in 0..5 {
            thread::yield_now();
        }
        semaphore.release(1);
        future::block_on(waiter).unwrap();
    });
    let expected = vec![
        event(0, "Created", &[]),
        try_acquire(0, 1),
        event(1, "AcquireBlocked", &[1]),
        event(0, "Release", &[1]),
        try_acquire(0, 1),
        event(1, "AcquireBlocked", &[1]),
        event(0, "Release", &[1]),
        event(1, "AcquireUnblocked", &[1, 1]),
        event(1, "Release", &[1]),
    ];
    assert_eq!(events, expected);
}

/// A fair release hands the permit to the waiter inside the release, so the releasing task
/// reports `AcquireUnblocked`, right after its `Release`, as on a strictly fair semaphore.
#[test]
fn parking_lot_events_of_a_fair_release() {
    let events = semaphore_events(|| {
        let semaphore = Arc::new(BatchSemaphore::new(1, Fairness::ParkingLot));
        semaphore.try_acquire(1).unwrap();
        let waiter = {
            let semaphore = semaphore.clone();
            thread::spawn(move || {
                semaphore.acquire_blocking(1).unwrap();
                semaphore.release(1);
            })
        };
        for _ in 0..5 {
            thread::yield_now();
        }
        semaphore.release_fair(1);
        waiter.join().unwrap();
    });
    let expected = vec![
        event(0, "Created", &[]),
        try_acquire(0, 1),
        event(1, "AcquireBlocked", &[1]),
        event(0, "Release", &[1]),
        event(0, "AcquireUnblocked", &[1, 1]),
        event(1, "Release", &[1]),
    ];
    assert_eq!(events, expected);
}

/// The eventual-fairness stand-in grants a woken waiter that has been overtaken twice inside a
/// release, so the releasing task reports `AcquireUnblocked`. Here the waiter waits for its
/// acquire, as a blocking call does, but the first execution of DFS runs the main task, which
/// takes and releases the permit, for as long as it can run.
#[test]
fn parking_lot_events_of_a_stand_in_grant() {
    let events = semaphore_events_with(DfsScheduler::new(Some(1), false), || {
        let semaphore = Arc::new(BatchSemaphore::new(1, Fairness::ParkingLot));
        semaphore.try_acquire(1).unwrap();
        let (parked_tx, parked_rx) = shuttle::sync::mpsc::channel::<()>();
        let waiter = {
            let semaphore = semaphore.clone();
            future::spawn(async move {
                let mut acquire = Box::pin(semaphore.acquire(1));
                assert!(futures::poll!(acquire.as_mut()).is_pending());
                parked_tx.send(()).unwrap();
                acquire.await.unwrap();
                semaphore.release(1);
            })
        };
        parked_rx.recv().unwrap();
        semaphore.release(1); // wakes the waiter
        for _ in 0..2 {
            semaphore.try_acquire(1).unwrap();
            semaphore.release(1); // overtakes the waiter, and the second time, grants it
        }
        future::block_on(waiter).unwrap();
    });
    let expected = vec![
        event(0, "Created", &[]),
        try_acquire(0, 1),
        event(1, "AcquireBlocked", &[1]),
        event(0, "Release", &[1]),
        try_acquire(0, 1),
        event(0, "Release", &[1]),
        try_acquire(0, 1),
        event(0, "Release", &[1]),
        event(0, "AcquireUnblocked", &[1, 1]),
        event(1, "Release", &[1]),
    ];
    assert_eq!(events, expected);
}

/// As `parking_lot_events_of_a_woken_waiter`, for a reserving acquire (a writer of an `RwLock`):
/// the woken waiter that wins its try reports `AcquireUnblocked` itself.
#[test]
fn parking_lot_events_of_a_woken_reserving_waiter() {
    let events = semaphore_events(|| {
        let semaphore = Arc::new(BatchSemaphore::new(2, Fairness::ParkingLot));
        semaphore.try_acquire(2).unwrap();
        let waiter = {
            let semaphore = semaphore.clone();
            thread::spawn(move || {
                future::block_on(semaphore.acquire_reserving(1, 2)).unwrap();
                semaphore.release(2);
            })
        };
        for _ in 0..5 {
            thread::yield_now();
        }
        semaphore.release(2);
        waiter.join().unwrap();
    });
    let expected = vec![
        event(0, "Created", &[]),
        try_acquire(0, 2),
        event(1, "AcquireBlocked", &[2]),
        event(0, "Release", &[2]),
        event(1, "AcquireUnblocked", &[1, 2]),
        event(1, "Release", &[2]),
    ];
    assert_eq!(events, expected);
}

/// As `parking_lot_events_of_a_lost_race`, for a reserving acquire: the woken waiter that loses
/// parks again, which it reports as another `AcquireBlocked`.
#[test]
fn parking_lot_events_of_a_lost_race_of_a_reserving_waiter() {
    let events = semaphore_events(|| {
        let semaphore = Arc::new(BatchSemaphore::new(2, Fairness::ParkingLot));
        semaphore.try_acquire(2).unwrap();
        let (go_tx, go_rx) = shuttle::sync::mpsc::channel::<()>();
        let waiter = {
            let semaphore = semaphore.clone();
            future::spawn(async move {
                let mut acquire = Box::pin(semaphore.acquire_reserving(1, 2));
                assert!(futures::poll!(acquire.as_mut()).is_pending());
                go_rx.recv().unwrap();
                acquire.await.unwrap();
                semaphore.release(2);
            })
        };
        for _ in 0..5 {
            thread::yield_now();
        }
        semaphore.release(2); // wakes the waiter, which waits for `go`
        semaphore.try_acquire(2).unwrap();
        go_tx.send(()).unwrap();
        for _ in 0..5 {
            thread::yield_now();
        }
        semaphore.release(2);
        future::block_on(waiter).unwrap();
    });
    let expected = vec![
        event(0, "Created", &[]),
        try_acquire(0, 2),
        event(1, "AcquireBlocked", &[2]),
        event(0, "Release", &[2]),
        try_acquire(0, 2),
        event(1, "AcquireBlocked", &[2]),
        event(0, "Release", &[2]),
        event(1, "AcquireUnblocked", &[1, 2]),
        event(1, "Release", &[2]),
    ];
    assert_eq!(events, expected);
}

/// A read of the state (`load_permits`) that grants a starving woken waiter its permit grants it
/// inside the read, so the reading task reports `AcquireUnblocked`, as for a stand-in grant at a
/// release.
#[test]
fn parking_lot_events_of_a_grant_at_a_read() {
    let events = semaphore_events(|| {
        let semaphore = Arc::new(BatchSemaphore::new(1, Fairness::ParkingLot));
        semaphore.try_acquire(1).unwrap();
        let (go_tx, go_rx) = shuttle::sync::mpsc::channel::<()>();
        let waiter = {
            let semaphore = semaphore.clone();
            future::spawn(async move {
                let mut acquire = Box::pin(semaphore.acquire(1));
                assert!(futures::poll!(acquire.as_mut()).is_pending());
                go_rx.recv().unwrap();
                acquire.await.unwrap();
                semaphore.release(1);
            })
        };
        for _ in 0..5 {
            thread::yield_now();
        }
        semaphore.release(1); // wakes the waiter, which waits for `go`
        assert_eq!(semaphore.load_permits(), Some(1)); // overtakes the waiter once
        assert_eq!(semaphore.load_permits(), Some(0)); // twice: it tries at the read, and wins
        go_tx.send(()).unwrap();
        future::block_on(waiter).unwrap();
    });
    let expected = vec![
        event(0, "Created", &[]),
        try_acquire(0, 1),
        event(1, "AcquireBlocked", &[1]),
        event(0, "Release", &[1]),
        event(0, "AcquireUnblocked", &[1, 1]),
        event(1, "Release", &[1]),
    ];
    assert_eq!(events, expected);
}

/// A starving woken waiter that loses at a read parks again, as one that loses its own try does,
/// and reports another `AcquireBlocked`. The read runs in another task, so the waiter's task
/// reports it when it next polls the `Acquire`, as it is still parked.
#[test]
fn parking_lot_events_of_a_lost_race_at_a_read() {
    let events = semaphore_events(|| {
        let semaphore = Arc::new(BatchSemaphore::new(1, Fairness::ParkingLot));
        semaphore.try_acquire(1).unwrap();
        let (go_tx, go_rx) = shuttle::sync::mpsc::channel::<()>();
        let waiter = {
            let semaphore = semaphore.clone();
            future::spawn(async move {
                let mut acquire = Box::pin(semaphore.acquire(1));
                assert!(futures::poll!(acquire.as_mut()).is_pending());
                go_rx.recv().unwrap();
                acquire.await.unwrap();
                semaphore.release(1);
            })
        };
        for _ in 0..5 {
            thread::yield_now();
        }
        semaphore.release(1); // wakes the waiter, which waits for `go`
        semaphore.try_acquire(1).unwrap();
        assert_eq!(semaphore.load_permits(), Some(0)); // overtakes the waiter once
        assert_eq!(semaphore.load_permits(), Some(0)); // twice: it tries at the read, and parks again
        assert!(semaphore.has_waiters(), "the waiter did not park again at the read");
        go_tx.send(()).unwrap();
        for _ in 0..5 {
            thread::yield_now(); // the waiter polls its `Acquire`, which is parked
        }
        semaphore.release_fair(1); // hands the waiter the permit
        future::block_on(waiter).unwrap();
    });
    let expected = vec![
        event(0, "Created", &[]),
        try_acquire(0, 1),
        event(1, "AcquireBlocked", &[1]),
        event(0, "Release", &[1]),
        try_acquire(0, 1),
        event(1, "AcquireBlocked", &[1]),
        event(0, "Release", &[1]),
        event(0, "AcquireUnblocked", &[1, 1]),
        event(1, "Release", &[1]),
    ];
    assert_eq!(events, expected);
}

/// As above, but a fair release hands the waiter the permit before its task polls the `Acquire`
/// again, as the `bump` of a lock that waits for `PARKED_BIT` does. The waiter reports nothing
/// of the park at the read then, only that it was blocked until the release unblocked it.
#[test]
fn parking_lot_events_of_a_lost_race_at_a_read_before_a_hand_off() {
    let events = semaphore_events(|| {
        let semaphore = Arc::new(BatchSemaphore::new(1, Fairness::ParkingLot));
        semaphore.try_acquire(1).unwrap();
        let (go_tx, go_rx) = shuttle::sync::mpsc::channel::<()>();
        let waiter = {
            let semaphore = semaphore.clone();
            future::spawn(async move {
                let mut acquire = Box::pin(semaphore.acquire(1));
                assert!(futures::poll!(acquire.as_mut()).is_pending());
                go_rx.recv().unwrap();
                acquire.await.unwrap();
                semaphore.release(1);
            })
        };
        for _ in 0..5 {
            thread::yield_now();
        }
        semaphore.release(1); // wakes the waiter, which waits for `go`
        semaphore.try_acquire(1).unwrap();
        assert_eq!(semaphore.load_permits(), Some(0)); // overtakes the waiter once
        assert_eq!(semaphore.load_permits(), Some(0)); // twice: it tries at the read, and parks again
        assert!(semaphore.has_waiters(), "the waiter did not park again at the read");
        semaphore.release_fair(1); // hands the waiter the permit
        go_tx.send(()).unwrap();
        future::block_on(waiter).unwrap();
    });
    let expected = vec![
        event(0, "Created", &[]),
        try_acquire(0, 1),
        event(1, "AcquireBlocked", &[1]),
        event(0, "Release", &[1]),
        try_acquire(0, 1),
        event(0, "Release", &[1]),
        event(0, "AcquireUnblocked", &[1, 1]),
        event(1, "Release", &[1]),
    ];
    assert_eq!(events, expected);
}

/// A starving woken waiter that loses at a release parks again, as at a read: here a fair release
/// hands the permit to another request, which is parked, so the waiter reports another
/// `AcquireBlocked` when its task next polls the `Acquire`, which is still parked.
#[test]
fn parking_lot_events_of_a_lost_race_at_a_release() {
    let events = semaphore_events(|| {
        let semaphore = Arc::new(BatchSemaphore::new(1, Fairness::ParkingLot));
        semaphore.try_acquire(1).unwrap();
        let (go_tx, go_rx) = shuttle::sync::mpsc::channel::<()>();
        let waiter = {
            let semaphore = semaphore.clone();
            future::spawn(async move {
                let mut acquire = Box::pin(semaphore.acquire(1));
                assert!(futures::poll!(acquire.as_mut()).is_pending());
                go_rx.recv().unwrap();
                acquire.await.unwrap();
                semaphore.release(1);
            })
        };
        for _ in 0..5 {
            thread::yield_now();
        }
        semaphore.release(1); // wakes the waiter, which waits for `go`
        semaphore.try_acquire(1).unwrap();
        semaphore.release(1); // overtakes the waiter once
        semaphore.try_acquire(1).unwrap();
        future::block_on(async {
            let mut other = Box::pin(semaphore.acquire(1));
            assert!(futures::poll!(other.as_mut()).is_pending());
            // Overtakes the waiter twice: it tries, and loses to `other`, which is handed the permit.
            semaphore.release_fair(1);
            assert!(semaphore.has_waiters(), "the waiter did not park again at the release");
            go_tx.send(()).unwrap();
            for _ in 0..5 {
                thread::yield_now(); // the waiter polls its `Acquire`, which is parked
            }
            other.await.unwrap();
        });
        semaphore.release(1); // wakes the waiter, which starves: granted
        future::block_on(waiter).unwrap();
    });
    let expected = vec![
        event(0, "Created", &[]),
        try_acquire(0, 1),
        event(1, "AcquireBlocked", &[1]),
        event(0, "Release", &[1]),
        try_acquire(0, 1),
        event(0, "Release", &[1]),
        try_acquire(0, 1),
        event(0, "AcquireBlocked", &[1]),
        event(0, "Release", &[1]),
        event(0, "AcquireUnblocked", &[0, 1]),
        event(1, "AcquireBlocked", &[1]),
        event(0, "Release", &[1]),
        event(0, "AcquireUnblocked", &[1, 1]),
        event(1, "Release", &[1]),
    ];
    assert_eq!(events, expected);
}

/// A request that a read parked again, and that then parks again by itself, reports the park of its
/// own only, as it no longer waits where the read parked it. Here its task finishes without
/// polling it, and the main task polls the abandoned `Acquire`, which leaves the queue, tries like
/// a new request, loses, and parks again (see `release_passes_over_a_parked_waiter_of_a_finished_task`).
#[test]
fn parking_lot_events_of_a_request_parked_by_a_read_and_then_by_itself() {
    let events = semaphore_events(|| {
        // Leaked, so that the abandoned `Acquire` can outlive the task that made it.
        let semaphore: &'static BatchSemaphore = Box::leak(Box::new(BatchSemaphore::new(1, Fairness::ParkingLot)));
        semaphore.try_acquire(1).unwrap();
        let stash = Arc::new(std::sync::Mutex::new(None));
        let (parked_tx, parked_rx) = shuttle::sync::mpsc::channel::<()>();
        let (go_tx, go_rx) = shuttle::sync::mpsc::channel::<()>();
        let registrant = {
            let stash = stash.clone();
            future::spawn(async move {
                let mut acquire = Box::pin(semaphore.acquire(1));
                assert!(futures::poll!(acquire.as_mut()).is_pending());
                *stash.lock().unwrap() = Some(acquire);
                parked_tx.send(()).unwrap();
                go_rx.recv().unwrap();
            })
        };
        parked_rx.recv().unwrap();
        semaphore.release(1); // wakes the registrant's request
        semaphore.try_acquire(1).unwrap();
        assert_eq!(semaphore.load_permits(), Some(0)); // overtakes the request once
        assert_eq!(semaphore.load_permits(), Some(0)); // twice: it tries at the read, and parks again
        go_tx.send(()).unwrap();
        future::block_on(registrant).unwrap(); // its task finishes without polling the request
        let mut acquire = stash.lock().unwrap().take().unwrap();
        future::block_on(async {
            // Leaves the queue, tries, loses, and parks again.
            assert!(futures::poll!(acquire.as_mut()).is_pending());
            // Parked, so it does not try, and has nothing more to report.
            assert!(futures::poll!(acquire.as_mut()).is_pending());
        });
        semaphore.release_fair(1); // hands it the permit
        future::block_on(acquire).unwrap();
        semaphore.release(1);
    });
    let expected = vec![
        event(0, "Created", &[]),
        try_acquire(0, 1),
        event(1, "AcquireBlocked", &[1]),
        event(0, "Release", &[1]),
        try_acquire(0, 1),
        event(0, "AcquireBlocked", &[1]),
        event(0, "Release", &[1]),
        event(0, "AcquireUnblocked", &[0, 1]),
        event(0, "Release", &[1]),
    ];
    assert_eq!(events, expected);
}

/// A request that never parks reports `AcquireFast`, as does a reserving one that takes all of
/// its permits at once. One that reserves the semaphore reports `AcquireBlocked`, as it waits, and
/// once it takes the permits that the reservation kept for it, it reports `AcquireUnblocked`
/// itself.
#[test]
fn parking_lot_events_of_a_reservation() {
    let events = semaphore_events(|| {
        let semaphore = Arc::new(BatchSemaphore::new(2, Fairness::ParkingLot));
        future::block_on(semaphore.acquire(1)).unwrap();
        let waiter = {
            let semaphore = semaphore.clone();
            thread::spawn(move || {
                // One permit is free, so this reserves the semaphore, and waits for the other.
                future::block_on(semaphore.acquire_reserving(1, 2)).unwrap();
                semaphore.release(2);
            })
        };
        for _ in 0..5 {
            thread::yield_now();
        }
        semaphore.release(1); // frees the last permit: wakes the holder of the reservation
        waiter.join().unwrap();
        future::block_on(semaphore.acquire_reserving(1, 2)).unwrap(); // both are free
        semaphore.release(2);
    });
    let expected = vec![
        event(0, "Created", &[]),
        event(0, "AcquireFast", &[1]),
        event(1, "AcquireBlocked", &[2]),
        event(0, "Release", &[1]),
        event(1, "AcquireUnblocked", &[1, 2]),
        event(1, "Release", &[2]),
        event(0, "AcquireFast", &[2]),
        event(0, "Release", &[2]),
    ];
    assert_eq!(events, expected);
}

/// A fair release that hands a parked request the reservation records nothing for it, as a
/// request that reserves the semaphore itself reports nothing more than that it waits: the request
/// reports `AcquireUnblocked` itself, once it has taken its permits.
#[test]
fn parking_lot_events_of_a_reservation_hand_over() {
    let events = semaphore_events(|| {
        let semaphore = Arc::new(BatchSemaphore::new(2, Fairness::ParkingLot));
        future::block_on(semaphore.acquire(2)).unwrap();
        let waiter = {
            let semaphore = semaphore.clone();
            thread::spawn(move || {
                future::block_on(semaphore.acquire_reserving(1, 2)).unwrap(); // parks
                semaphore.release(2);
            })
        };
        for _ in 0..5 {
            thread::yield_now();
        }
        semaphore.release_fair(1); // hands the waiter the reservation
        semaphore.release(1); // frees its last permit: wakes it
        waiter.join().unwrap();
    });
    let expected = vec![
        event(0, "Created", &[]),
        event(0, "AcquireFast", &[2]),
        event(1, "AcquireBlocked", &[2]),
        event(0, "Release", &[1]),
        event(0, "Release", &[1]),
        event(1, "AcquireUnblocked", &[1, 2]),
        event(1, "Release", &[2]),
    ];
    assert_eq!(events, expected);
}
