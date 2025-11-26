#![warn(rust_2018_idioms)]
#![cfg(all(feature = "time", not(target_os = "wasi")))] // Wasi does not support panic recovery
#![cfg(panic = "unwind")]

use shuttle::sync::Arc;
use shuttle::sync::Mutex;
use shuttle_tokio_stream_impl::{self as stream, StreamExt};
use std::panic;
use tokio::time::Duration;

fn test_panic<Func: FnOnce() + panic::UnwindSafe>(func: Func) -> Option<String> {
    let panic_mutex = Mutex::new(());

    {
        let _guard = panic_mutex.lock();
        let panic_file: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let prev_hook = panic::take_hook();
        {
            let panic_file = panic_file.clone();
            panic::set_hook(Box::new(move |panic_info| {
                let panic_location = panic_info.location().unwrap();
                panic_file
                    .lock()
                    .unwrap()
                    .clone_from(&Some(panic_location.file().to_string()));
            }));
        }

        let result = panic::catch_unwind(func);
        // Return to the previously set panic hook (maybe default) so that we get nice error
        // messages in the tests.
        panic::set_hook(prev_hook);

        if result.is_err() {
            panic_file.lock().unwrap().clone()
        } else {
            None
        }
    }
}

#[tokio::test]
async fn stream_chunks_timeout_panic_caller() {
    let panic_location_file = test_panic(|| {
        let iter = vec![1, 2, 3].into_iter();
        let stream0 = stream::iter(iter);

        let _chunk_stream = stream0.chunks_timeout(0, Duration::from_secs(2));
    });

    // The panic location should be in this file
    assert_eq!(&panic_location_file.unwrap(), file!());
}
