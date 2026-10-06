//! Tests for `task_local!`.
//!
//! * `api` checks that this crate's `task_local!` behaves like tokio's. It ports tokio's own
//!   `tests/task_local.rs`, and covers the rest of the behavior that tokio documents, all under
//!   Shuttle.
//! * `isolation` runs a set of scenarios against both this crate's `task_local!` and tokio's.
//!   tokio's keeps a scope's value in a `std::thread_local!` while the scope is polled, and every
//!   Shuttle task runs on the same OS thread, so the tasks that Shuttle switches to in the middle
//!   of that poll find the value as well. Each scenario passes with this crate's implementation
//!   and fails with tokio's, which is what `shuttle-tokio` used to re-export.
//! * `teardown` covers the ends of an execution that leave a scoped future unfinished, where
//!   Shuttle drops the future without any task running.

use futures::FutureExt;
use shuttle::future::block_on;
use shuttle::scheduler::RandomScheduler;
use shuttle::{check_dfs, check_random, Config, MaxSteps, Runner};
use shuttle_tokio_impl_inner::sync::{oneshot, Mutex};
use shuttle_tokio_impl_inner::task::futures::TaskLocalFuture;
use shuttle_tokio_impl_inner::task::{self, LocalKey};
use shuttle_tokio_impl_inner::task_local;
use std::future::Future;
use std::pin::{pin, Pin};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use tracing_subscriber::layer::SubscriberExt;

/// Acquires an uncontended lock. That is a scheduling point that does not return `Poll::Pending`,
/// so Shuttle can switch to another task here, *in the middle of* the calling task's poll. Most
/// operations on a concurrency primitive are like this: sending on a channel, spawning a task,
/// taking a lock that is free, and so on.
async fn scheduling_point() {
    drop(Mutex::new(()).lock().await);
}

mod api {
    use super::*;
    use test_log::test;

    // Ported from tokio's `tests/task_local.rs`. The only changes are using Shuttle's primitives,
    // and dropping the parts that rely on `JoinError::try_into_panic`, which is not implemented
    // under Shuttle.

    #[test]
    fn local() {
        task_local! {
            static REQ_ID: u32;
            pub static FOO: bool;
        }

        check_dfs(
            || {
                block_on(async {
                    let j1 = task::spawn(REQ_ID.scope(1, async move {
                        assert_eq!(REQ_ID.get(), 1);
                        assert_eq!(REQ_ID.get(), 1);
                    }));

                    let j2 = task::spawn(REQ_ID.scope(2, async move {
                        REQ_ID.with(|v| {
                            assert_eq!(REQ_ID.get(), 2);
                            assert_eq!(*v, 2);
                        });

                        shuttle_tokio_impl_inner::time::sleep(std::time::Duration::from_millis(10)).await;

                        assert_eq!(REQ_ID.get(), 2);
                    }));

                    let j3 = task::spawn(FOO.scope(true, async move {
                        assert!(FOO.get());
                    }));

                    j1.await.unwrap();
                    j2.await.unwrap();
                    j3.await.unwrap();
                })
            },
            None,
        );
    }

    #[test]
    fn task_local_available_on_abort() {
        task_local! {
            static KEY: u32;
        }

        struct MyFuture {
            tx_poll: Option<oneshot::Sender<()>>,
            tx_drop: Option<oneshot::Sender<u32>>,
        }
        impl Future for MyFuture {
            type Output = ();

            fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
                if let Some(tx_poll) = self.tx_poll.take() {
                    let _ = tx_poll.send(());
                }
                Poll::Pending
            }
        }
        impl Drop for MyFuture {
            fn drop(&mut self) {
                let _ = self.tx_drop.take().unwrap().send(KEY.get());
            }
        }

        check_dfs(
            || {
                block_on(async {
                    let (tx_drop, rx_drop) = oneshot::channel();
                    let (tx_poll, rx_poll) = oneshot::channel();

                    let h = task::spawn(KEY.scope(
                        42,
                        MyFuture {
                            tx_poll: Some(tx_poll),
                            tx_drop: Some(tx_drop),
                        },
                    ));

                    rx_poll.await.unwrap();
                    h.abort();
                    assert_eq!(rx_drop.await.unwrap(), 42);

                    let err = h.await.unwrap_err();
                    assert!(err.is_cancelled());
                })
            },
            None,
        );
    }

    #[test]
    fn task_local_available_on_completion_drop() {
        task_local! {
            static KEY: u32;
        }

        struct MyFuture {
            tx: Option<oneshot::Sender<u32>>,
        }
        impl Future for MyFuture {
            type Output = ();

            fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
                Poll::Ready(())
            }
        }
        impl Drop for MyFuture {
            fn drop(&mut self) {
                let _ = self.tx.take().unwrap().send(KEY.get());
            }
        }

        check_dfs(
            || {
                block_on(async {
                    let (tx, rx) = oneshot::channel();

                    let h = task::spawn(KEY.scope(42, MyFuture { tx: Some(tx) }));

                    assert_eq!(rx.await.unwrap(), 42);
                    h.await.unwrap();
                })
            },
            None,
        );
    }

    #[test]
    fn take_value() {
        task_local! {
            static KEY: u32
        }

        check_dfs(
            || {
                let fut = KEY.scope(1, async {});
                let mut pinned = Box::pin(fut);
                assert_eq!(pinned.as_mut().take_value(), Some(1));
                assert_eq!(pinned.as_mut().take_value(), None);
            },
            None,
        );
    }

    #[test]
    fn poll_after_take_value_should_fail() {
        task_local! {
            static KEY: u32
        }

        check_dfs(
            || {
                block_on(async {
                    let fut = KEY.scope(1, async {
                        let result = KEY.try_with(|_| {});
                        // The task local value no longer exists.
                        assert!(result.is_err());
                    });
                    let mut fut = Box::pin(fut);
                    fut.as_mut().take_value();

                    // Poll the future after `take_value` has been called
                    fut.await;
                })
            },
            None,
        );
    }

    #[test]
    fn get_value() {
        task_local! {
            static KEY: u32
        }

        check_dfs(
            || {
                block_on(async {
                    KEY.scope(1, async {
                        assert_eq!(KEY.get(), 1);
                        assert_eq!(KEY.try_get().unwrap(), 1);
                    })
                    .await;

                    let fut = KEY.scope(1, async {
                        let result = KEY.try_get();
                        // The task local value no longer exists.
                        assert!(result.is_err());
                    });
                    let mut fut = Box::pin(fut);
                    fut.as_mut().take_value();

                    // Poll the future after `take_value` has been called
                    fut.await;
                })
            },
            None,
        );
    }

    // The rest of tokio's documented behavior.

    #[test]
    fn not_set_outside_a_scope() {
        task_local! {
            static KEY: u32;
        }

        check_dfs(
            || {
                assert!(KEY.try_get().is_err());
                assert!(KEY.try_with(|_| ()).is_err());

                block_on(async {
                    assert!(KEY.try_get().is_err());
                    KEY.scope(1, async {}).await;
                    assert!(KEY.try_get().is_err(), "the value must not outlive its scope");
                });

                KEY.sync_scope(1, || {});
                assert!(KEY.try_get().is_err(), "the value must not outlive its scope");
            },
            None,
        );
    }

    #[test]
    #[should_panic(expected = "cannot access a task-local storage value without setting it first")]
    fn with_panics_outside_a_scope() {
        task_local! {
            static KEY: u32;
        }

        check_dfs(|| KEY.with(|_| ()), None);
    }

    #[test]
    #[should_panic(expected = "cannot access a task-local storage value without setting it first")]
    fn get_panics_outside_a_scope() {
        task_local! {
            static KEY: u32;
        }

        check_dfs(
            || {
                KEY.get();
            },
            None,
        );
    }

    #[test]
    fn access_error() {
        task_local! {
            static KEY: u32;
        }

        check_dfs(
            || {
                let err = KEY.try_get().unwrap_err();
                assert_eq!(err.to_string(), "task-local value not set");
                assert_eq!(format!("{err:?}"), "AccessError");
                #[allow(clippy::clone_on_copy)]
                let cloned = err.clone();
                assert_eq!(err, cloned);
                let _: Box<dyn std::error::Error> = Box::new(err);
            },
            None,
        );
    }

    #[test]
    fn nested_scopes() {
        task_local! {
            static KEY: u32;
        }

        check_dfs(
            || {
                block_on(KEY.scope(1, async {
                    assert_eq!(KEY.get(), 1);

                    KEY.scope(2, async {
                        assert_eq!(KEY.get(), 2);
                        task::yield_now().await;
                        assert_eq!(KEY.get(), 2);
                    })
                    .await;
                    assert_eq!(
                        KEY.get(),
                        1,
                        "the outer value must be restored when the inner scope ends"
                    );

                    KEY.sync_scope(3, || {
                        assert_eq!(KEY.get(), 3);
                        KEY.sync_scope(4, || assert_eq!(KEY.get(), 4));
                        assert_eq!(KEY.get(), 3);
                    });
                    assert_eq!(KEY.get(), 1);

                    task::yield_now().await;
                    assert_eq!(KEY.get(), 1);
                }));
                assert!(KEY.try_get().is_err());
            },
            None,
        );
    }

    #[test]
    fn sync_scope() {
        task_local! {
            static KEY: u32;
        }

        check_dfs(
            || {
                assert_eq!(KEY.sync_scope(1, || KEY.get() + 1), 2);
                assert!(KEY.try_get().is_err());

                // A `sync_scope` around a `block_on` covers the future.
                KEY.sync_scope(1, || {
                    block_on(async {
                        assert_eq!(KEY.get(), 1);
                        task::yield_now().await;
                        assert_eq!(KEY.get(), 1);
                    })
                });
            },
            None,
        );
    }

    #[test]
    fn keys_are_independent() {
        task_local! {
            static NUMBER: u32;
            static NAME: String;
        }

        check_dfs(
            || {
                block_on(NUMBER.scope(
                    1,
                    NAME.scope("outer".to_string(), async {
                        assert_eq!(NUMBER.get(), 1);
                        assert_eq!(NAME.get(), "outer");

                        NUMBER
                            .scope(2, async {
                                assert_eq!(NUMBER.get(), 2);
                                assert_eq!(NAME.get(), "outer");
                            })
                            .await;

                        NAME.sync_scope("inner".to_string(), || {
                            assert_eq!(NUMBER.get(), 1);
                            assert_eq!(NAME.get(), "inner");
                        });

                        assert_eq!(NUMBER.get(), 1);
                        assert_eq!(NAME.get(), "outer");
                    }),
                ));
            },
            None,
        );
    }

    /// tokio's task-locals are not inherited: a spawned task starts without a value.
    #[test]
    fn spawned_tasks_start_without_a_value() {
        task_local! {
            static KEY: u32;
        }

        check_dfs(
            || {
                block_on(KEY.scope(1, async {
                    let spawned = task::spawn(async { KEY.try_get().ok() });
                    let blocking = task::spawn_blocking(|| KEY.try_get().ok());
                    let mut set = task::JoinSet::new();
                    set.spawn(async { KEY.try_get().ok() });
                    scheduling_point().await;

                    assert_eq!(spawned.await.unwrap(), None);
                    assert_eq!(blocking.await.unwrap(), None);
                    assert_eq!(set.join_next().await.unwrap().unwrap(), None);
                    assert_eq!(KEY.get(), 1);
                }));
            },
            None,
        );
    }

    #[test]
    #[should_panic(expected = "cannot enter a task-local scope while the task-local storage is borrowed")]
    fn sync_scope_inside_with_panics() {
        task_local! {
            static KEY: u32;
        }

        check_dfs(|| KEY.sync_scope(1, || KEY.with(|_| KEY.sync_scope(2, || {}))), None);
    }

    #[test]
    #[should_panic(expected = "cannot enter a task-local scope while the task-local storage is borrowed")]
    fn scope_polled_inside_with_panics() {
        task_local! {
            static KEY: u32;
        }

        check_dfs(
            || {
                KEY.sync_scope(1, || {
                    KEY.with(|_| KEY.scope(2, async {}).now_or_never());
                })
            },
            None,
        );
    }

    #[test]
    #[should_panic(expected = "`TaskLocalFuture` polled after completion")]
    fn polled_after_completion_panics() {
        task_local! {
            static KEY: u32;
        }

        check_dfs(
            || {
                let mut fut = Box::pin(KEY.scope(1, async {}));
                assert_eq!(fut.as_mut().now_or_never(), Some(()));
                fut.as_mut().now_or_never();
            },
            None,
        );
    }

    /// Counts how often it is dropped.
    #[derive(Debug)]
    struct DropCounter(Arc<AtomicUsize>);

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// The value belongs to the `TaskLocalFuture`, which drops it when it is dropped (and not when
    /// it completes), unless `take_value` hands it out first.
    #[test]
    fn value_is_dropped_with_the_future() {
        task_local! {
            static KEY: DropCounter;
        }

        check_dfs(
            || {
                let drops = Arc::new(AtomicUsize::new(0));

                let mut fut = Box::pin(KEY.scope(DropCounter(drops.clone()), task::yield_now()));
                block_on(fut.as_mut());
                assert_eq!(drops.load(Ordering::SeqCst), 0);
                drop(fut);
                assert_eq!(drops.load(Ordering::SeqCst), 1);

                let mut fut = Box::pin(KEY.scope(DropCounter(drops.clone()), task::yield_now()));
                let value = fut.as_mut().take_value();
                drop(fut);
                assert_eq!(drops.load(Ordering::SeqCst), 1);
                drop(value);
                assert_eq!(drops.load(Ordering::SeqCst), 2);

                // Dropped without ever being polled
                drop(KEY.scope(DropCounter(drops.clone()), async {}));
                assert_eq!(drops.load(Ordering::SeqCst), 3);
            },
            None,
        );
    }

    /// A future that records the value of `key` when it is dropped.
    struct RecordOnDrop {
        key: &'static LocalKey<u32>,
        seen: Arc<std::sync::Mutex<Vec<Option<u32>>>>,
        polls_until_ready: usize,
    }

    impl Future for RecordOnDrop {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.polls_until_ready == 0 {
                return Poll::Ready(());
            }
            self.polls_until_ready -= 1;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }

    impl Drop for RecordOnDrop {
        fn drop(&mut self) {
            self.seen.lock().unwrap().push(self.key.try_get().ok());
        }
    }

    /// The future inside a scope is dropped with the value set, whether it has completed, is still
    /// pending, or was never polled.
    #[test]
    fn inner_future_is_dropped_inside_the_scope() {
        task_local! {
            static KEY: u32;
        }

        check_dfs(
            || {
                let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
                let record = |polls_until_ready| RecordOnDrop {
                    key: &KEY,
                    seen: seen.clone(),
                    polls_until_ready,
                };

                // Never polled
                drop(KEY.scope(1, record(0)));

                // Pending
                let mut fut = Box::pin(KEY.scope(2, record(1)));
                assert!(fut.as_mut().now_or_never().is_none());
                drop(fut);

                // Completed. The future is dropped as soon as it completes, inside its last poll.
                block_on(KEY.scope(3, record(1)));

                assert_eq!(*seen.lock().unwrap(), [Some(1), Some(2), Some(3)]);
                assert!(KEY.try_get().is_err());
            },
            None,
        );
    }

    /// A scope that panics restores the value it shadowed, so that the panic can be caught.
    #[test]
    fn panicking_scope_restores_the_outer_value() {
        task_local! {
            static KEY: u32;
        }

        check_dfs(
            || {
                let result = std::panic::catch_unwind(|| KEY.sync_scope(1, || panic!("expected panic")));
                assert!(result.is_err());
                assert!(KEY.try_get().is_err());

                block_on(KEY.scope(1, async {
                    let result = KEY
                        .scope(2, async {
                            task::yield_now().await;
                            panic!("expected panic");
                        })
                        .catch_unwind()
                        .await;
                    assert!(result.is_err());
                    assert_eq!(KEY.get(), 1);
                }));
                assert!(KEY.try_get().is_err());
            },
            None,
        );
    }

    #[test]
    fn debug() {
        task_local! {
            static KEY: u32;
        }

        check_dfs(
            || {
                assert_eq!(format!("{:?}", KEY), "LocalKey { .. }");

                let mut fut = Box::pin(KEY.scope(5, async {}));
                assert_eq!(format!("{fut:?}"), "TaskLocalFuture { value: 5 }");
                fut.as_mut().take_value();
                assert_eq!(format!("{fut:?}"), "TaskLocalFuture { value: <missing> }");
            },
            None,
        );
    }

    /// The types have the same auto traits as tokio's.
    #[test]
    fn auto_traits() {
        use std::panic::{RefUnwindSafe, UnwindSafe};
        use std::rc::Rc;

        fn send_sync<T: Send + Sync>() {}
        fn unwind_safe<T: UnwindSafe + RefUnwindSafe>() {}

        // A `LocalKey` is a handle to a slot that every task has its own copy of, so it can be
        // shared regardless of what it holds.
        send_sync::<LocalKey<Rc<u32>>>();
        send_sync::<tokio::task::LocalKey<Rc<u32>>>();
        unwind_safe::<LocalKey<std::cell::Cell<u32>>>();
        unwind_safe::<tokio::task::LocalKey<std::cell::Cell<u32>>>();

        send_sync::<TaskLocalFuture<u32, std::future::Ready<()>>>();
        send_sync::<tokio::task::futures::TaskLocalFuture<u32, std::future::Ready<()>>>();
        unwind_safe::<TaskLocalFuture<u32, std::future::Ready<()>>>();
        unwind_safe::<tokio::task::futures::TaskLocalFuture<u32, std::future::Ready<()>>>();

        // A `TaskLocalFuture` is `!Unpin` even if the future it scopes is `Unpin`. For a type that
        // is `Unpin`, both impls below apply, and the call does not compile.
        trait AmbiguousIfUnpin<A> {
            fn some_item() {}
        }
        impl<T: ?Sized> AmbiguousIfUnpin<()> for T {}
        impl<T: ?Sized + Unpin> AmbiguousIfUnpin<u8> for T {}
        <TaskLocalFuture<u32, std::future::Ready<()>> as AmbiguousIfUnpin<_>>::some_item();
        <tokio::task::futures::TaskLocalFuture<u32, std::future::Ready<()>> as AmbiguousIfUnpin<_>>::some_item();
    }

    /// Outside of a Shuttle test, a `LocalKey` behaves like tokio's: there is no Shuttle task, so
    /// the slot is a plain thread-local, like tokio's.
    #[test]
    fn outside_of_a_shuttle_test() {
        task_local! {
            static KEY: u32;
        }

        assert!(KEY.try_get().is_err());
        assert_eq!(KEY.sync_scope(1, || KEY.get()), 1);
        assert!(KEY.try_get().is_err());

        // `futures::executor::block_on` is used because Shuttle's `block_on` needs a Shuttle test.
        let value = futures::executor::block_on(KEY.scope(2, async {
            let inner = KEY.scope(3, async { KEY.get() }).await;
            (KEY.get(), inner)
        }));
        assert_eq!(value, (2, 3));
        assert!(KEY.try_get().is_err());

        // Dropping a scope that has not completed re-enters it to drop the future.
        let mut fut = Box::pin(KEY.scope(4, std::future::pending::<()>()));
        assert!(fut.as_mut().now_or_never().is_none());
        drop(fut);
        assert!(KEY.try_get().is_err());
    }

    /// A `tracing` subscriber that reads a task-local on every event. Besides the events of the
    /// test, it gets Shuttle's own (at every level down to TRACE), some of which Shuttle emits
    /// outside of any task, or while it is updating its own state. None of those must panic.
    #[test]
    fn read_from_a_tracing_subscriber() {
        task_local! {
            static KEY: u32;
        }

        struct ReadsKey {
            seen: Arc<std::sync::Mutex<Vec<Option<u32>>>>,
        }

        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for ReadsKey {
            fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
                // Only record the test's own events; Shuttle's just need to not panic.
                if event.metadata().target() == "task_local_test" {
                    self.seen.lock().unwrap().push(KEY.try_get().ok());
                }
            }
        }

        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(ReadsKey { seen: seen.clone() });

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "task_local_test", "outside of the test");

            check_dfs(
                || {
                    tracing::info!(target: "task_local_test", "in the test, outside of a scope");
                    block_on(async {
                        let tasks = (1..=2)
                            .map(|value| {
                                task::spawn(KEY.scope(value, async move {
                                    scheduling_point().await;
                                    tracing::info!(target: "task_local_test", "in a scope");
                                }))
                            })
                            .collect::<Vec<_>>();
                        for task in tasks {
                            task.await.unwrap();
                        }
                    });
                },
                None,
            );

            tracing::info!(target: "task_local_test", "after the test");
        });

        let seen = seen.lock().unwrap();
        assert_eq!(seen[0], None);
        assert_eq!(*seen.last().unwrap(), None);
        // The test's events, in each iteration: the one outside of a scope, and one per task
        let in_iterations = &seen[1..seen.len() - 1];
        assert_eq!(in_iterations.len() % 3, 0);
        for iteration in in_iterations.chunks(3) {
            assert_eq!(iteration[0], None);
            let mut in_scopes = [iteration[1], iteration[2]];
            in_scopes.sort();
            assert_eq!(in_scopes, [Some(1), Some(2)]);
        }
    }

    /// Scopes nested inside each other in several tasks, with the tasks switched out in the middle
    /// of polls and between them.
    #[test]
    fn concurrent_nested_scopes() {
        task_local! {
            static KEY: u32;
        }

        async fn nested(base: u32) {
            KEY.scope(base, async move {
                scheduling_point().await;
                assert_eq!(KEY.get(), base);

                KEY.scope(base + 1, async move {
                    scheduling_point().await;
                    assert_eq!(KEY.get(), base + 1);
                    task::yield_now().await;
                    assert_eq!(KEY.get(), base + 1);

                    KEY.sync_scope(base + 2, || {
                        shuttle::thread::yield_now();
                        assert_eq!(KEY.get(), base + 2);
                    });
                    assert_eq!(KEY.get(), base + 1);
                })
                .await;

                scheduling_point().await;
                assert_eq!(KEY.get(), base);
            })
            .await;
            assert!(KEY.try_get().is_err());
        }

        check_random(
            || {
                block_on(async {
                    let tasks = (0..3).map(|i| task::spawn(nested(10 * i))).collect::<Vec<_>>();
                    nested(100).await;
                    for t in tasks {
                        t.await.unwrap();
                    }
                })
            },
            1000,
        );
    }
}

mod isolation {
    use super::*;

    /// The operations of a task-local key that the scenarios use, so that every scenario can be
    /// run against both this crate's `LocalKey` and tokio's.
    trait TaskLocal: Sync + 'static {
        fn scope<F>(&'static self, value: u32, f: F) -> impl Future<Output = F::Output> + Send + 'static
        where
            F: Future + Send + 'static;

        /// Runs `f` in a scope, and then returns the value that `TaskLocalFuture::take_value`
        /// hands back.
        fn scope_then_take_value<F>(
            &'static self,
            value: u32,
            f: F,
        ) -> impl Future<Output = Option<u32>> + Send + 'static
        where
            F: Future + Send + 'static;

        fn sync_scope<R>(&'static self, value: u32, f: impl FnOnce() -> R) -> R;

        fn with<R>(&'static self, f: impl FnOnce(&u32) -> R) -> R;

        fn try_get(&'static self) -> Option<u32>;
    }

    macro_rules! impl_task_local {
        ($key:ty) => {
            impl TaskLocal for $key {
                fn scope<F>(&'static self, value: u32, f: F) -> impl Future<Output = F::Output> + Send + 'static
                where
                    F: Future + Send + 'static,
                {
                    <$key>::scope(self, value, f)
                }

                fn scope_then_take_value<F>(
                    &'static self,
                    value: u32,
                    f: F,
                ) -> impl Future<Output = Option<u32>> + Send + 'static
                where
                    F: Future + Send + 'static,
                {
                    async move {
                        let mut fut = pin!(<$key>::scope(self, value, f));
                        fut.as_mut().await;
                        fut.as_mut().take_value()
                    }
                }

                fn sync_scope<R>(&'static self, value: u32, f: impl FnOnce() -> R) -> R {
                    <$key>::sync_scope(self, value, f)
                }

                fn with<R>(&'static self, f: impl FnOnce(&u32) -> R) -> R {
                    <$key>::with(self, f)
                }

                fn try_get(&'static self) -> Option<u32> {
                    <$key>::try_get(self).ok()
                }
            }
        };
    }

    impl_task_local!(LocalKey<u32>);
    impl_task_local!(tokio::task::LocalKey<u32>);

    /// Declares the tests for the scenario `$scenario`: one that runs it against this crate's
    /// `task_local!`, and one that runs it against tokio's and expects it to fail with
    /// `$tokio_failure`.
    macro_rules! scenario {
        ($scenario:ident, tokio fails with $tokio_failure:literal) => {
            mod $scenario {
                use test_log::test;

                #[test]
                fn passes_with_shuttle_task_local() {
                    shuttle_tokio_impl_inner::task_local! {
                        static KEY: u32;
                    }
                    super::$scenario(&KEY);
                }

                #[test]
                #[should_panic(expected = $tokio_failure)]
                fn fails_with_tokio_task_local() {
                    tokio::task_local! {
                        static KEY: u32;
                    }
                    super::$scenario(&KEY);
                }
            }
        };
        ($scenario:ident, tokio passes) => {
            mod $scenario {
                use test_log::test;

                #[test]
                fn passes_with_shuttle_task_local() {
                    shuttle_tokio_impl_inner::task_local! {
                        static KEY: u32;
                    }
                    super::$scenario(&KEY);
                }

                #[test]
                fn passes_with_tokio_task_local() {
                    tokio::task_local! {
                        static KEY: u32;
                    }
                    super::$scenario(&KEY);
                }
            }
        };
    }

    /// The control: one task at a time. tokio's implementation is only broken by the interleaving
    /// of tasks, so it passes this one, which shows that the scenarios below fail for that reason
    /// and no other.
    fn one_task_at_a_time<K: TaskLocal>(key: &'static K) {
        check_dfs(
            move || {
                block_on(async move {
                    let value = key
                        .scope(1, async move {
                            scheduling_point().await;
                            let inner = key.scope(2, async move { key.try_get() }).await;
                            task::yield_now().await;
                            (key.try_get(), inner)
                        })
                        .await;
                    assert_eq!(value, (Some(1), Some(2)));
                    assert_eq!(key.try_get(), None);

                    // The tasks run one after the other
                    for value in 1..=2 {
                        let task = task::spawn(key.scope_then_take_value(value, scheduling_point()));
                        assert_eq!(task.await.unwrap(), Some(value));
                    }

                    assert_eq!(key.sync_scope(3, || key.try_get()), Some(3));
                    assert_eq!(key.try_get(), None);
                })
            },
            None,
        );
    }
    scenario!(one_task_at_a_time, tokio passes);

    /// A task spawned inside a scope does not inherit the value.
    ///
    /// With tokio's implementation, the spawned task can run while its parent is switched out in
    /// the middle of a poll of the scope, and then finds the parent's value.
    fn spawned_task_does_not_inherit_value<K: TaskLocal>(key: &'static K) {
        check_dfs(
            move || {
                block_on(key.scope(1, async move {
                    let child = task::spawn(async move { key.try_get() });
                    scheduling_point().await;
                    assert_eq!(
                        child.await.unwrap(),
                        None,
                        "spawned task saw the task-local value of the task that spawned it"
                    );
                }));
            },
            None,
        );
    }
    scenario!(spawned_task_does_not_inherit_value, tokio fails with "spawned task saw the task-local value of the task that spawned it");

    /// Tasks that are in scopes of the same key at the same time each see their own value.
    ///
    /// With tokio's implementation, the second task to enter its scope, while the first is
    /// switched out in the middle of a poll of its own, replaces the first task's value with its
    /// own, and the first task finds that when it resumes.
    fn concurrent_scopes_are_isolated<K: TaskLocal>(key: &'static K) {
        check_dfs(
            move || {
                block_on(async move {
                    let tasks = (1..=2)
                        .map(|value| {
                            task::spawn(key.scope(value, async move {
                                scheduling_point().await;
                                key.try_get()
                            }))
                        })
                        .collect::<Vec<_>>();

                    for (value, task) in (1..=2).zip(tasks) {
                        assert_eq!(
                            task.await.unwrap(),
                            Some(value),
                            "a task saw another task's task-local value"
                        );
                    }
                })
            },
            None,
        );
    }
    scenario!(concurrent_scopes_are_isolated, tokio fails with "a task saw another task's task-local value");

    /// A task sees its own value in every poll of its scope, even after the polls of several
    /// tasks' scopes have overlapped.
    ///
    /// With tokio's implementation, every poll of a scope ends by moving the value in the
    /// thread-local back into the scope. When the polls of two scopes overlap and do not end in the
    /// opposite order they started in, each of them takes away the other's value, and uses it from
    /// then on.
    fn value_survives_overlapping_polls<K: TaskLocal>(key: &'static K) {
        check_dfs(
            move || {
                block_on(async move {
                    let tasks = (1..=2)
                        .map(|value| {
                            task::spawn(key.scope(value, async move {
                                scheduling_point().await;
                                // End this poll; the value is read in the next one.
                                task::yield_now().await;
                                key.try_get()
                            }))
                        })
                        .collect::<Vec<_>>();

                    for (value, task) in (1..=2).zip(tasks) {
                        assert_eq!(
                            task.await.unwrap(),
                            Some(value),
                            "a task saw another task's task-local value in a later poll"
                        );
                    }
                })
            },
            None,
        );
    }
    scenario!(value_survives_overlapping_polls, tokio fails with "a task saw another task's task-local value in a later poll");

    /// `TaskLocalFuture::take_value` hands back the value that the scope was created with.
    ///
    /// With tokio's implementation, overlapping polls (see `value_survives_overlapping_polls`) can
    /// leave the value of one task's scope in another task's scope, or in neither.
    fn take_value_returns_the_scopes_own_value<K: TaskLocal>(key: &'static K) {
        check_dfs(
            move || {
                block_on(async move {
                    let tasks = (1..=2)
                        .map(|value| task::spawn(key.scope_then_take_value(value, scheduling_point())))
                        .collect::<Vec<_>>();

                    for (value, task) in (1..=2).zip(tasks) {
                        assert_eq!(
                            task.await.unwrap(),
                            Some(value),
                            "`take_value` handed back another scope's value"
                        );
                    }
                })
            },
            None,
        );
    }
    scenario!(take_value_returns_the_scopes_own_value, tokio fails with "`take_value` handed back another scope's value");

    /// Once every scope has ended, no task sees a value.
    ///
    /// With tokio's implementation, overlapping polls (see `value_survives_overlapping_polls`) can
    /// leave a value in the thread-local after every scope has ended, for every task to see.
    fn value_does_not_outlive_its_scope<K: TaskLocal>(key: &'static K) {
        check_dfs(
            move || {
                block_on(async move {
                    let tasks = (1..=2)
                        .map(|value| task::spawn(key.scope(value, scheduling_point())))
                        .collect::<Vec<_>>();
                    for task in tasks {
                        task.await.unwrap();
                    }

                    assert_eq!(
                        key.try_get(),
                        None,
                        "a task-local value is still set after all of its scopes have ended"
                    );
                })
            },
            None,
        );
    }
    scenario!(value_does_not_outlive_its_scope, tokio fails with "a task-local value is still set after all of its scopes have ended");

    /// A task can hold on to its value with `with` across a scheduling point.
    ///
    /// With tokio's implementation, the `RefCell` that holds the value is shared by every task, so
    /// a task that enters a scope while another one is inside `with` panics.
    fn with_across_a_scheduling_point<K: TaskLocal>(key: &'static K) {
        check_dfs(
            move || {
                block_on(async move {
                    let log = Arc::new(shuttle::sync::Mutex::new(Vec::new()));
                    let tasks = (1..=2)
                        .map(|value| {
                            let log = log.clone();
                            task::spawn(key.scope(value, async move {
                                // Taking this lock is a scheduling point.
                                key.with(|value| log.lock().unwrap().push(*value));
                            }))
                        })
                        .collect::<Vec<_>>();
                    for task in tasks {
                        task.await.unwrap();
                    }

                    let mut log = log.lock().unwrap().clone();
                    log.sort();
                    assert_eq!(log, [1, 2]);
                })
            },
            None,
        );
    }
    scenario!(with_across_a_scheduling_point, tokio fails with "cannot enter a task-local scope while the task-local storage is borrowed");

    /// Threads that are in a `sync_scope` of the same key at the same time each see their own
    /// value.
    ///
    /// tokio's implementation is correct for real threads, which each have their own
    /// thread-locals, but every Shuttle thread runs on the same OS thread.
    fn sync_scopes_in_threads_are_isolated<K: TaskLocal>(key: &'static K) {
        check_dfs(
            move || {
                let threads = (1..=2)
                    .map(|value| {
                        shuttle::thread::spawn(move || {
                            key.sync_scope(value, || {
                                shuttle::thread::yield_now();
                                key.try_get()
                            })
                        })
                    })
                    .collect::<Vec<_>>();

                for (value, thread) in (1..=2).zip(threads) {
                    assert_eq!(
                        thread.join().unwrap(),
                        Some(value),
                        "a thread saw another thread's task-local value"
                    );
                }
            },
            None,
        );
    }
    scenario!(sync_scopes_in_threads_are_isolated, tokio fails with "a thread saw another thread's task-local value");

    /// An execution that ends in the middle of a scope does not leave its value behind.
    ///
    /// When Shuttle stops an execution, for instance because it reached `MaxSteps::ContinueAfter`,
    /// it discards the tasks that are still running without unwinding them. With tokio's
    /// implementation, a task that is in the middle of a poll of a scope at that point never puts
    /// its value back, so the next execution starts out with the value set. A failure that depends
    /// on that cannot be replayed on its own.
    fn value_does_not_leak_into_the_next_execution<K: TaskLocal>(key: &'static K) {
        let mut config = Config::new();
        config.max_steps = MaxSteps::ContinueAfter(100);
        Runner::new(RandomScheduler::new(2), config).run(move || {
            assert_eq!(
                key.try_get(),
                None,
                "a task-local value leaked in from an earlier execution"
            );

            // Runs until Shuttle stops the execution
            block_on(key.scope(1, async move {
                loop {
                    scheduling_point().await;
                }
            }));
        });
    }
    scenario!(value_does_not_leak_into_the_next_execution, tokio fails with "a task-local value leaked in from an earlier execution");
}

mod teardown {
    //! When the test returns, Shuttle drops the tasks that have not finished, such as a detached
    //! task that is still pending. It does so with no task running, as a tokio runtime that shuts
    //! down drops its tasks outside of any of them. The futures of those tasks can still see their
    //! own task-local values while they are dropped, like they can in tokio.

    use super::*;
    use std::sync::Mutex as StdMutex;
    use test_log::test;

    /// A future that records the value of `key` when it is dropped. Its poll returns `Pending`
    /// forever, unless it has a `lock` to take, in which case it blocks on the lock in the
    /// middle of the poll.
    struct RecordOnDrop {
        key: &'static LocalKey<u32>,
        seen: &'static StdMutex<Vec<Option<u32>>>,
        lock: Option<Arc<shuttle::sync::Mutex<()>>>,
    }

    impl Future for RecordOnDrop {
        type Output = ();

        fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
            if let Some(lock) = &self.lock {
                drop(lock.lock().unwrap());
            }
            Poll::Pending
        }
    }

    impl Drop for RecordOnDrop {
        fn drop(&mut self) {
            self.seen.lock().unwrap().push(self.key.try_get().ok());
        }
    }

    /// A detached task that is pending in a scope at the end of the execution.
    #[test]
    fn task_pending_in_a_scope() {
        task_local! {
            static KEY: u32;
        }
        static SCOPED: StdMutex<Vec<Option<u32>>> = StdMutex::new(Vec::new());
        static UNSCOPED: StdMutex<Vec<Option<u32>>> = StdMutex::new(Vec::new());

        check_dfs(
            || {
                block_on(async {
                    drop(task::spawn(KEY.scope(
                        7,
                        RecordOnDrop {
                            key: &KEY,
                            seen: &SCOPED,
                            lock: None,
                        },
                    )));
                    // Dropped after the scoped task, so it checks that the scope did not leave its
                    // value behind.
                    drop(task::spawn(RecordOnDrop {
                        key: &KEY,
                        seen: &UNSCOPED,
                        lock: None,
                    }));
                })
            },
            None,
        );

        let scoped = SCOPED.lock().unwrap();
        assert!(!scoped.is_empty());
        assert!(scoped.iter().all(|seen| *seen == Some(7)), "{scoped:?}");
        let unscoped = UNSCOPED.lock().unwrap();
        assert_eq!(unscoped.len(), scoped.len());
        assert!(unscoped.iter().all(|seen| seen.is_none()), "{unscoped:?}");
    }

    /// A detached task that is blocked in the middle of a poll of a scope at the end of the
    /// execution. Shuttle unwinds the task's stack, and the scope has to move its value from the
    /// task's storage back into the `TaskLocalFuture`, even though no task is running.
    #[test]
    fn task_blocked_in_the_middle_of_a_poll_of_a_scope() {
        task_local! {
            static KEY: u32;
        }
        static SCOPED: StdMutex<Vec<Option<u32>>> = StdMutex::new(Vec::new());
        static UNSCOPED: StdMutex<Vec<Option<u32>>> = StdMutex::new(Vec::new());

        check_dfs(
            || {
                let lock = Arc::new(shuttle::sync::Mutex::new(()));
                // Never released, so that the task below blocks forever
                std::mem::forget(lock.lock().unwrap());

                block_on(async move {
                    drop(task::spawn(KEY.scope(
                        7,
                        RecordOnDrop {
                            key: &KEY,
                            seen: &SCOPED,
                            lock: Some(lock),
                        },
                    )));
                    drop(task::spawn(RecordOnDrop {
                        key: &KEY,
                        seen: &UNSCOPED,
                        lock: None,
                    }));
                })
            },
            None,
        );

        let scoped = SCOPED.lock().unwrap();
        assert!(!scoped.is_empty());
        assert!(scoped.iter().all(|seen| *seen == Some(7)), "{scoped:?}");
        let unscoped = UNSCOPED.lock().unwrap();
        assert_eq!(unscoped.len(), scoped.len());
        assert!(unscoped.iter().all(|seen| seen.is_none()), "{unscoped:?}");
    }

    /// Two detached tasks, each pending inside two nested scopes of the same key, and one outside
    /// of any scope. While Shuttle drops them, the scopes of all of them use the same slot (the one
    /// for when no task runs). Every value they hold must see the innermost scope it is in, and
    /// never a value of another task.
    #[test]
    fn tasks_pending_in_nested_scopes() {
        task_local! {
            static KEY: u32;
        }

        /// What a `Recorder` saw when it was dropped
        #[derive(Debug)]
        struct Record {
            /// The value of its task's outer scope, or `None` for the task outside of any scope
            task: Option<u32>,
            in_inner_scope: bool,
            task_started: bool,
            seen: Option<u32>,
        }
        static RECORDS: StdMutex<Vec<Record>> = StdMutex::new(Vec::new());

        struct Recorder {
            task: Option<u32>,
            in_inner_scope: bool,
            task_started: Arc<AtomicBool>,
        }

        impl Drop for Recorder {
            fn drop(&mut self) {
                RECORDS.lock().unwrap().push(Record {
                    task: self.task,
                    in_inner_scope: self.in_inner_scope,
                    task_started: self.task_started.load(Ordering::SeqCst),
                    seen: KEY.try_get().ok(),
                });
            }
        }

        check_dfs(
            || {
                block_on(async {
                    for outer in [10, 20] {
                        let task_started = Arc::new(AtomicBool::new(false));
                        let recorder = |in_inner_scope| Recorder {
                            task: Some(outer),
                            in_inner_scope,
                            task_started: task_started.clone(),
                        };
                        let for_outer_scope = recorder(false);
                        let for_inner_scope = recorder(true);

                        drop(task::spawn(KEY.scope(outer, async move {
                            task_started.store(true, Ordering::SeqCst);
                            let _held = for_outer_scope;
                            let _inner = KEY.scope(outer + 1, async move {
                                let _held = for_inner_scope;
                                std::future::pending::<()>().await
                            });
                            std::future::pending::<()>().await
                        })));
                    }

                    let for_no_scope = Recorder {
                        task: None,
                        in_inner_scope: false,
                        task_started: Arc::new(AtomicBool::new(false)),
                    };
                    drop(task::spawn(async move {
                        let _held = for_no_scope;
                        std::future::pending::<()>().await
                    }));
                })
            },
            None,
        );

        let records = RECORDS.lock().unwrap();
        for record in records.iter() {
            let expected = match record {
                Record { task: None, .. } => None,
                // The inner scope only exists once the task has started. Before that, the value
                // meant for it is held by the outer scope's future.
                Record {
                    task: Some(outer),
                    in_inner_scope: true,
                    task_started: true,
                    ..
                } => Some(outer + 1),
                Record { task: Some(outer), .. } => Some(*outer),
            };
            assert_eq!(record.seen, expected, "{record:?}");
        }

        // DFS explores both a task that never starts and one that is pending in both scopes.
        for outer in [10, 20] {
            for task_started in [false, true] {
                assert!(
                    records
                        .iter()
                        .any(|r| r.task == Some(outer) && r.in_inner_scope && r.task_started == task_started),
                    "no record for task {outer} with task_started = {task_started}"
                );
            }
        }
    }
}
