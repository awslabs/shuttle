use shuttle::{future, scheduler::RandomScheduler, thread, Runner};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use test_log::test;
use tracing::field::{Field, Visit};
use tracing::{trace, Event, Id, Metadata, Subscriber};

type SigCounterMap = Arc<Mutex<HashMap<u64, usize>>>;
#[derive(Clone)]
struct SignatureSubscriber {
    signatures: SigCounterMap,
    static_create_locations: SigCounterMap,
}

impl SignatureSubscriber {
    pub fn new() -> Self {
        Self {
            signatures: Arc::new(Mutex::new(HashMap::new())),
            static_create_locations: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Subscriber for SignatureSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let metadata = event.metadata();
        if metadata.target() == "shuttle::runtime::task" && metadata.level() == &tracing::Level::DEBUG {
            struct SignatureVisitor {
                task_id: Option<String>,
                signature: Option<u64>,
                static_create_location: Option<u64>,
            }
            impl Visit for SignatureVisitor {
                fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                    if field.name() == "task_id" {
                        self.task_id = Some(format!("{:?}", value));
                    }
                }
                fn record_u64(&mut self, field: &Field, value: u64) {
                    if field.name() == "signature" {
                        self.signature = Some(value);
                    }
                    if field.name() == "static_create_location" {
                        self.static_create_location = Some(value);
                    }
                }
            }
            let mut visitor = SignatureVisitor {
                task_id: None,
                signature: None,
                static_create_location: None,
            };
            event.record(&mut visitor);

            if let Some(sig) = visitor.signature {
                self.signatures
                    .lock()
                    .unwrap()
                    .entry(sig)
                    .and_modify(|counter| *counter += 1)
                    .or_insert(1);
            }
            if let Some(loc) = visitor.static_create_location {
                self.static_create_locations
                    .lock()
                    .unwrap()
                    .entry(loc)
                    .and_modify(|counter| *counter += 1)
                    .or_insert(1);
            }
        }
    }

    fn enter(&self, _span: &Id) {}
    fn exit(&self, _span: &Id) {}
}

pub fn check_any_n_same_signatures(signatures: &SigCounterMap, expected_count: usize) {
    let signatures = signatures.lock().unwrap();
    trace!("Total signatures captured: {}", signatures.len());

    trace!("Signature counts: {:?}", signatures);

    let worker_signatures: Vec<u64> = signatures
        .iter()
        .filter(|(_, count)| **count == expected_count)
        .map(|(sig, _)| *sig)
        .collect();

    assert_eq!(
        worker_signatures.len(),
        1,
        "Should have exactly one signature appearing {} times",
        expected_count
    );
    trace!(
        "{} tasks have the same signature: {}",
        expected_count,
        worker_signatures[0]
    );
}

pub fn check_exactly_n_different_signatures(signatures: &SigCounterMap, expected_count: usize) {
    let signatures = signatures.lock().unwrap();
    trace!("Total signatures captured: {}", signatures.len());

    trace!("Signature counts: {:?}", signatures);

    let unique_signatures: Vec<u64> = signatures.keys().cloned().collect();
    assert_eq!(
        unique_signatures.len(),
        expected_count,
        "Should have {} different signatures",
        expected_count
    );
    trace!(
        "All {} tasks have different signatures: {:?}",
        expected_count,
        unique_signatures
    );
}

pub fn run_test_n_iterations_with_subscriber<F>(test_fn: F, iterations: usize) -> (SigCounterMap, SigCounterMap)
where
    F: Fn() + Send + Sync + 'static,
{
    let subscriber = SignatureSubscriber::new();
    let signatures = Arc::clone(&subscriber.signatures);
    let static_create_locations = Arc::clone(&subscriber.static_create_locations);
    let _guard = tracing::subscriber::set_default(subscriber);

    let scheduler = RandomScheduler::new(iterations);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(test_fn);

    (signatures, static_create_locations)
}

#[test]
fn sync_tasks_created_at_same_src_location_have_different_signatures() {
    fn worker_function() {}

    let (signatures, static_create_locations) = run_test_n_iterations_with_subscriber(
        || {
            let mut handles = Vec::new();
            for _ in 0..10 {
                handles.push(thread::spawn(worker_function));
            }
            for handle in handles {
                handle.join().unwrap();
            }
        },
        1,
    );

    check_any_n_same_signatures(&static_create_locations, 10);

    check_exactly_n_different_signatures(&signatures, 11);
}

#[test]
fn async_tasks_created_at_same_src_location_have_different_signatures() {
    async fn async_worker_function() {}

    let (signatures, static_create_locations) = run_test_n_iterations_with_subscriber(
        || {
            let mut handles = Vec::new();
            for _ in 0..10 {
                handles.push(future::spawn(async_worker_function()));
            }
            future::block_on(async {
                for handle in handles {
                    handle.await.unwrap();
                }
            });
        },
        1,
    );

    check_any_n_same_signatures(&static_create_locations, 10);
    check_exactly_n_different_signatures(&signatures, 11);
}

#[test]
fn sync_tasks_created_in_different_src_locations_have_different_signatures() {
    fn worker_function() {}

    let (signatures, _) = run_test_n_iterations_with_subscriber(
        || {
            let handle1 = thread::spawn(worker_function);
            let handle2 = thread::spawn(worker_function);
            handle1.join().unwrap();
            handle2.join().unwrap();
        },
        1,
    );

    check_exactly_n_different_signatures(&signatures, 3);
}

#[test]
fn async_tasks_created_in_different_src_locations_have_different_signatures() {
    async fn async_worker_function() {}

    let (signatures, _) = run_test_n_iterations_with_subscriber(
        || {
            let handle1 = future::spawn(async_worker_function());
            let handle2 = future::spawn(async_worker_function());
            future::block_on(async {
                handle1.await.unwrap();
                handle2.await.unwrap();
            });
        },
        1,
    );

    check_exactly_n_different_signatures(&signatures, 3);
}

#[test]
fn task_signatures_consistent_across_shuttle_iterations() {
    fn worker_with_nested_spawn() {
        let handle = thread::spawn(|| {});
        handle.join().unwrap();
    }

    let (signatures, _) = run_test_n_iterations_with_subscriber(
        || {
            let handle1 = thread::spawn(worker_with_nested_spawn);
            let handle2 = thread::spawn(worker_with_nested_spawn);
            handle1.join().unwrap();
            handle2.join().unwrap();
        },
        100,
    );

    check_exactly_n_different_signatures(&signatures, 5);
}
