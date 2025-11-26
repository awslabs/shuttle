use shuttle::{check_dfs, check_random, future};
use shuttle_tokio_impl_inner::sync::{mpsc, oneshot};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[macro_export]
macro_rules! assert_pending {
    ($e:expr) => {{
        use core::task::Poll::*;
        match $e {
            Pending => {}
            Ready(v) => panic!("ready; value = {:?}", v),
        }
    }};
    ($e:expr, $($msg:tt)+) => {{
        use core::task::Poll::*;
        match $e {
            Pending => {}
            Ready(v) => {
                panic!("ready; value = {:?}; {}", v, format_args!($($msg)+))
            }
        }
    }};
}

#[test]
fn async_oneshot_send_recv() {
    check_dfs(
        || {
            future::block_on(async {
                let (tx, mut rx) = oneshot::channel();

                let h1 = future::spawn(async move {
                    let result = rx.try_recv();
                    if let Err(e) = result {
                        assert_eq!(e, oneshot::error::TryRecvError::Empty);
                        let result = rx.await;
                        assert_eq!(result, Ok(1));
                    } else {
                        assert_eq!(result, Ok(1));
                    }
                });

                let h2 = future::spawn(async move {
                    assert!(!tx.is_closed());
                    assert!(tx.send(1).is_ok());
                });

                futures::future::join_all(vec![h1, h2]).await;
            });
        },
        None,
    );
}

#[test]
fn async_oneshot_commands() {
    // Test a common use case for oneshot channels: as a way to acknowledge completion of an activity.
    // A client asks the server to do some work (increment a shared counter) and reply when done.
    check_dfs(
        || {
            future::block_on(async {
                let counter = Arc::new(AtomicUsize::new(0));
                let counter2 = counter.clone();
                let (tx, mut rx) = mpsc::channel(1);

                // Client
                let h1 = future::spawn(async move {
                    let (ctx, crx) = oneshot::channel();
                    tx.send(ctx).await.unwrap();
                    crx.await.unwrap();
                    assert_eq!(counter.load(Ordering::SeqCst), 3);
                });

                // Server
                let h2 = future::spawn(async move {
                    let ctx = rx.recv().await.unwrap();
                    for _ in 0..3 {
                        counter2.fetch_add(1, Ordering::SeqCst);
                    }
                    ctx.send(()).unwrap();
                });
                futures::future::join_all(vec![h1, h2]).await;
            });
        },
        None,
    );
}

#[test]
#[should_panic(expected = "assertion failed")]
fn async_oneshot_yield() {
    check_dfs(
        || {
            let (mut tx1, mut rx1) = oneshot::channel::<()>();
            let (tx2, mut rx2) = oneshot::channel::<()>();
            future::block_on(async {
                let h = future::spawn(async move {
                    rx1.close();
                    rx2.close();
                });
                tx1.closed().await; // wait for first channel to be closed
                assert!(tx2.is_closed()); // Some execution should fail this assertion
                h.await.unwrap();
            });
        },
        None,
    );
}

// If there is no synchronization point in `try_recv` (as was the case previously), then
// this test will hang forever.
#[test]
fn try_recv_loop() {
    check_random(
        || {
            let (tx1, mut rx1) = oneshot::channel::<()>();
            future::block_on(async {
                let try_recv_task = future::spawn(async move { while rx1.try_recv().is_err() {} });
                tx1.send(()).unwrap();
                try_recv_task.await.unwrap();
            });
        },
        100,
    );
}
