use serde_json::Value;
use shuttle::scheduler::{AnnotationScheduler, RoundRobinScheduler};
use shuttle::sync::Mutex;
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
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("annotated.json");
    std::env::set_var(shuttle::ANNOTATION_FILE, &path);

    {
        // `AnnotationScheduler` writes the file when it is dropped
        let runner = Runner::new(
            AnnotationScheduler::new(RoundRobinScheduler::new(1)),
            Default::default(),
        );
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
