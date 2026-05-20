use shuttle::check_random;
use shuttle::future::batch_semaphore::{BatchSemaphore, Fairness};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

#[test]
fn unique_resource_signature_batch_semaphore() {
    check_random(
        || {
            let sem1 = BatchSemaphore::new(1, Fairness::Unfair);
            let sem2 = BatchSemaphore::new(1, Fairness::Unfair);
            assert_ne!(sem1.signature(), sem2.signature());
        },
        1,
    );
}

#[test]
fn batch_semaphore_signatures_consistent_across_shuttle_iterations() {
    let all_signatures = Arc::new(Mutex::new(HashSet::new()));
    let all_signatures_clone = all_signatures.clone();

    check_random(
        move || {
            let sem1 = BatchSemaphore::new(1, Fairness::StrictlyFair);
            let sem2 = BatchSemaphore::new(2, Fairness::Unfair);

            all_signatures_clone
                .lock()
                .unwrap()
                .insert((sem1.available_permits(), sem1.signature().clone()));
            all_signatures_clone
                .lock()
                .unwrap()
                .insert((sem2.available_permits(), sem2.signature().clone()));
        },
        10,
    );

    assert_eq!(all_signatures.lock().unwrap().len(), 2);
}
