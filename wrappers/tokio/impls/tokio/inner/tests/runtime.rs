use shuttle::check_dfs;
use shuttle_tokio_impl_inner::runtime::{self, Handle};
use shuttle_tokio_impl_inner::sync::{mpsc, oneshot};
use shuttle_tokio_impl_inner::task::{self, JoinSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
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
        // 1000 tasks spawned per iteration; see the note on `async_mpsc_many_senders_with_blocking`.
        10,
    );
}

/// `num_alive_tasks` counts a task from its spawn until it finishes or is aborted. As in tokio,
/// tasks from `spawn_blocking` and `JoinSet::spawn_local` don't count.
#[test]
fn runtime_num_alive_tasks() {
    // The step bound stops each execution while a counted task is still alive, and Shuttle doesn't
    // drop the futures of a stopped execution, so each execution also checks that its count starts
    // at 0.
    let mut config = shuttle::Config::new();
    config.max_steps = shuttle::MaxSteps::ContinueAfter(1000);
    let scheduler = shuttle::scheduler::RandomScheduler::new(100);
    shuttle::Runner::new(scheduler, config).run(|| {
        let rt = runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(async {
            let metrics = Handle::current().metrics();
            assert_eq!(metrics.num_alive_tasks(), 0);

            let (tx, rx) = oneshot::channel();
            let finishes = task::spawn(async move { rx.await.unwrap() });
            let aborted = task::spawn(std::future::pending::<()>());
            let mut set = JoinSet::new();
            set.spawn(std::future::pending::<()>());
            assert_eq!(metrics.num_alive_tasks(), 3);

            let blocking = task::spawn_blocking(|| ());
            set.spawn_local(std::future::pending::<()>());
            assert_eq!(metrics.num_alive_tasks(), 3);

            tx.send(()).unwrap();
            finishes.await.unwrap();
            blocking.await.unwrap();
            assert_eq!(metrics.num_alive_tasks(), 2);

            aborted.abort();
            assert!(aborted.await.unwrap_err().is_cancelled());
            assert_eq!(metrics.num_alive_tasks(), 1);

            // Keep the set's tasks alive until the step bound stops the execution.
            loop {
                task::yield_now().await;
            }
        })
    });
}

/// Regression test: Shuttle must be able to schedule another task between a task's previous
/// operation and a spawn, which adds the new task to `num_alive_tasks`.
///
/// The test program wrongly assumes that once it sees `ready`, the new task is alive. That fails
/// when the reader runs between the store and the spawn, which Shuttle only explores if the count
/// goes up after the spawn's scheduling point.
#[test]
#[should_panic(expected = "spawn not done yet")]
fn spawn_after_store_is_a_separate_step() {
    check_dfs(
        || {
            let ready = Arc::new(shuttle::sync::atomic::AtomicBool::new(false));
            let reader = {
                let ready = ready.clone();
                shuttle::thread::spawn(move || {
                    if ready.load(Ordering::SeqCst) {
                        // BUG (in this test program): assumes the spawn has already happened.
                        let alive = Handle::current().metrics().num_alive_tasks();
                        assert_eq!(alive, 1, "spawn not done yet");
                    }
                })
            };

            ready.store(true, Ordering::SeqCst);
            let pending = task::spawn(std::future::pending::<()>());

            reader.join().unwrap();
            pending.abort();
        },
        None,
    );
}
