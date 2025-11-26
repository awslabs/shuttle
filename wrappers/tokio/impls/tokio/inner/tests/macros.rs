// We import as `some_other_name` to ensure that renaming to something arbitrary works.
use shuttle_tokio_impl_inner as some_other_name;
// We import as `tokio` to ensure that renaming to tokio works (there could in theory have been a collision).
use shuttle_tokio_impl_inner as tokio;
use shuttle_tokio_impl_inner::sync::Mutex;
use std::sync::Arc;

#[shuttle_tokio_impl_inner::test]
async fn litmus() {}

#[some_other_name::test]
async fn litmus_renamed() {}

// Test that it works with parameters
#[some_other_name::test(flavor = "multi_thread", worker_threads = 1)]
async fn litmus_params() {}

// Simple happy case test
#[some_other_name::test]
async fn should_succeed() {
    let lock = Arc::new(Mutex::new(0usize));
    let lock_clone = Arc::clone(&lock);

    let jh = tokio::spawn(async move {
        let mut counter = lock_clone.lock().await;
        *counter += 1;
    });

    {
        let mut counter = lock.lock().await;
        *counter += 1;
    }
    jh.await.unwrap();
    assert!(*lock.lock().await == 2);
}

// Simple failure case test
#[should_panic(expected = "Failed")]
#[tokio::test]
async fn should_fail() {
    panic!("Failed");
}

// Test that returning a result works.
#[tokio::test]
async fn return_value_ok() -> Result<(), &'static str> {
    Ok(())
}

#[should_panic(expected = "failure :(")]
#[tokio::test]
async fn return_value_err() -> Result<(), &'static str> {
    Err("failure :(")
}

// TODO: Make the `test` macro compatible with `proptest!` and make a test here which tests that.
