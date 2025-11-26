use shuttle_tokio_impl_inner::runtime::{self, Handle};
use shuttle_tokio_impl_inner::sync::mpsc;
use test_log::test;

async fn mpsc_senders_with_blocking_inner(num_senders: usize, channel_size: usize) {
    let rt = Handle::current();

    assert!(num_senders >= channel_size);
    let num_receives = num_senders - channel_size;
    let (tx, mut rx) = mpsc::channel::<usize>(channel_size);
    let senders = (0..num_senders)
        .map(move |i| {
            let tx = tx.clone();
            rt.spawn(async move {
                tx.send(i).await.unwrap();
            })
        })
        .collect::<Vec<_>>();

    // Receive enough messages to ensure no sender will block
    for _ in 0..num_receives {
        let _ = rx.recv().await.unwrap();
    }
    for sender in senders {
        sender.await.unwrap();
    }
}

#[test]
fn runtime_mpsc_many_senders_with_blocking() {
    shuttle::check_random(
        || {
            let rt = runtime::Builder::new_current_thread()
                .enable_time()
                .start_paused(true)
                .build()
                .unwrap();

            rt.block_on(async {
                mpsc_senders_with_blocking_inner(1000, 500).await;
            });
        },
        1000,
    );
}
