//! Some basic tests that show we can use the `futures` crate's channels off-the-shelf.
//!
//! TODO (1) not certain that this is sound -- are we guaranteed to see all the possible
//! TODO     interleavings if we use an off-the-shelf impl?
//! TODO (2) need mpsc channels too
//! TODO (3) I wonder if we can use an off-the-shelf tokio-compat shim here to get tokio support

use futures::channel::{mpsc, oneshot};
use futures::{Sink, SinkExt, Stream, StreamExt};
use shuttle::{asynch, check_dfs};
use std::collections::HashSet;
use test_log::test;

#[test]
fn oneshot_once_blocking() {
    check_dfs(
        || {
            let (tx, rx) = oneshot::channel();

            let _ = asynch::spawn(async move {
                tx.send(42u32).unwrap();
            });

            asynch::block_on(async {
                let x = rx.await.unwrap();
                assert_eq!(x, 42u32);
            });
        },
        None,
    )
}

#[test]
fn oneshot_once() {
    check_dfs(
        || {
            let (tx, rx) = oneshot::channel();

            asynch::spawn(async move {
                tx.send(42u32).unwrap();
            });

            asynch::spawn(async {
                let x = rx.await.unwrap();
                assert_eq!(x, 42u32);
            });
        },
        None,
    )
}

#[test]
fn oneshot_ping_pong() {
    check_dfs(
        || {
            let (tx1, rx1) = oneshot::channel();
            let (tx2, rx2) = oneshot::channel();

            let _ = asynch::spawn(async move {
                tx1.send(0u32).unwrap();
                let x = rx2.await.unwrap();
                assert_eq!(x, 1);
            });

            asynch::block_on(async {
                let x = rx1.await.unwrap();
                assert_eq!(x, 0);
                tx2.send(1u32).unwrap();
            })
        },
        None,
    )
}

#[test]
#[should_panic(expected = "deadlock")]
fn oneshot_deadlock() {
    check_dfs(
        || {
            let (tx, rx) = oneshot::channel();

            let task = asynch::spawn(async move {
                let _ = rx.await;
            });

            asynch::block_on(async move {
                task.await.unwrap();
                tx.send(0u32).unwrap();
            })
        },
        None,
    )
}

fn mpsc_stream_sum<Tx, Rx, F>(num_tasks: usize, creator: F)
where
    F: Fn() -> (Tx, Rx),
    Tx: Sink<usize> + Clone + Unpin + Send + 'static,
    Tx::Error: std::fmt::Debug,
    Rx: Stream<Item = usize> + Send + 'static,
{
    let (tx, rx) = creator();

    for i in 0..num_tasks {
        let mut tx = tx.clone();
        asynch::spawn(async move {
            tx.send(i).await.expect("send should succeed");
        });
    }

    // A channel's stream terminates when all senders are dropped. Every task has its own clone of
    // the sender, so we need to drop the original one.
    drop(tx);

    asynch::block_on(async move {
        let stream = rx.fold(0, |acc, x| async move { acc + x });
        let result = stream.await;
        assert_eq!(result, num_tasks * (num_tasks - 1) / 2);
    })
}

#[test]
fn mpsc_unbounded_stream_sum_1() {
    check_dfs(|| mpsc_stream_sum(1, mpsc::unbounded::<usize>), None)
}

#[test]
fn mpsc_unbounded_stream_sum_4() {
    check_dfs(|| mpsc_stream_sum(4, mpsc::unbounded::<usize>), None)
}

#[test]
fn mpsc_bounded_stream_sum_1() {
    check_dfs(|| mpsc_stream_sum(1, || mpsc::channel::<usize>(1)), None)
}

#[test]
fn mpsc_bounded_stream_sum_4() {
    check_dfs(|| mpsc_stream_sum(4, || mpsc::channel::<usize>(1)), None)
}

/// Validate that we see every permutation of message orderings at the receiver of the mpsc channel
fn mpsc_stream_permutations<Tx, Rx, F>(num_tasks: usize, creator: F)
where
    F: Fn() -> (Tx, Rx) + Send + Sync + 'static,
    Tx: Sink<usize> + Clone + Unpin + Send + 'static,
    Tx::Error: std::fmt::Debug,
    Rx: Stream<Item = usize> + Send + 'static,
{
    let permutations = std::sync::Arc::new(std::sync::Mutex::new(HashSet::new()));
    let permutations_clone = permutations.clone();

    check_dfs(
        move || {
            let (tx, rx) = creator();

            for i in 0..num_tasks {
                let mut tx = tx.clone();
                asynch::spawn(async move {
                    tx.send(i).await.expect("send should succeed");
                });
            }

            // A channel's stream terminates when all senders are dropped. Every task has its own clone of
            // the sender, so we need to drop the original one.
            drop(tx);

            let permutations = permutations_clone.clone();
            asynch::block_on(async move {
                let result = rx.collect::<Vec<_>>().await;
                let mut p = permutations.lock().unwrap();
                p.insert(result);
            })
        },
        None,
    );

    let p = permutations.lock().unwrap();
    // n tasks = n! permutations
    assert_eq!(p.len(), (1..(num_tasks + 1)).product::<usize>());
}

#[test]
fn mpsc_stream_permutations_unbounded_1() {
    mpsc_stream_permutations(1, mpsc::unbounded::<usize>);
}

#[test]
fn mpsc_stream_permutations_unbounded_4() {
    mpsc_stream_permutations(4, mpsc::unbounded::<usize>);
}

#[test]
fn mpsc_stream_permutations_bounded_1() {
    mpsc_stream_permutations(1, || mpsc::channel::<usize>(1));
}

#[test]
fn mpsc_stream_permutations_bounded_4() {
    mpsc_stream_permutations(4, || mpsc::channel::<usize>(1));
}

fn mpsc_stream_sender_maybe_deadlock(should_drop_sender: bool) {
    let (tx, rx) = mpsc::unbounded::<usize>();
    {
        let mut tx = tx.clone();
        asynch::spawn(async move {
            tx.send(42usize).await.unwrap();
        })
    };

    if should_drop_sender {
        drop(tx);
    }

    asynch::block_on(async move {
        // This will only complete if we dropped the sender above
        let ret = rx.collect::<Vec<_>>().await;
        assert_eq!(ret, vec![42]);
    });
}

#[test]
#[should_panic(expected = "deadlock")]
fn mpsc_stream_sender_deadlock() {
    check_dfs(|| mpsc_stream_sender_maybe_deadlock(false), None)
}

#[test]
fn mpsc_stream_sender_no_deadlock() {
    check_dfs(|| mpsc_stream_sender_maybe_deadlock(true), None)
}
