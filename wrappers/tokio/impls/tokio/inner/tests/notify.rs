use shuttle::future;
use shuttle_tokio_impl_inner::sync::notify::Notify;
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use test_log::test;

fn check_dfs<F>(f: F)
where
    F: Fn() + Send + Sync + 'static,
{
    use shuttle::scheduler::DfsScheduler;
    let mut config = shuttle::Config::default();
    config.max_steps = shuttle::MaxSteps::FailAfter(1_000);

    let scheduler = DfsScheduler::new(None, true);
    let runner = shuttle::Runner::new(scheduler, config);
    runner.run(f);
}

#[test]
fn notify_mpsc_channel() {
    // Example from the tokio docs for Notify

    // Unbound multi-producer single-consumer (mpsc) channel.
    //
    // No wakeups can be lost when using this channel because the call to
    // `notify_one()` will store a permit in the `Notify`, which the following call
    // to `notified()` will consume.
    struct Channel<T> {
        values: Mutex<VecDeque<T>>,
        notify: Notify,
    }

    impl<T> Channel<T> {
        fn new() -> Self {
            Self {
                values: Mutex::new(VecDeque::new()),
                notify: Notify::new(),
            }
        }
        pub fn send(&self, value: T) {
            let mut values = self.values.lock().unwrap();
            values.push_back(value);
            drop(values);

            // Notify the consumer a value is available
            self.notify.notify_one();
        }

        // This is a single-consumer channel, so several concurrent calls to
        // `recv` are not allowed.
        pub async fn recv(&self) -> T {
            loop {
                // Drain values
                {
                    let mut values = self.values.lock().unwrap();
                    if let Some(value) = values.pop_front() {
                        return value;
                    }
                    drop(values);
                }

                // Wait for values to be available
                self.notify.notified().await;
            }
        }
    }

    check_dfs(|| {
        future::block_on(async {
            let tx1 = Arc::new(Channel::new());
            let tx2 = tx1.clone();
            let rx = tx1.clone();
            future::spawn(async move {
                tx1.send(1);
            });
            future::spawn(async move {
                tx2.send(2);
            });
            let mut v = rx.recv().await;
            v += rx.recv().await;
            assert_eq!(v, 3);
        });
    });
}

#[test]
fn notify_mpmc_channel() {
    // Example from the tokio docs for Notify
    // A multi-producer, multi-consumer channel

    struct Channel<T> {
        messages: Mutex<VecDeque<T>>,
        notify_on_sent: Notify,
    }

    impl<T> Channel<T> {
        fn new() -> Self {
            Self {
                messages: Mutex::new(VecDeque::new()),
                notify_on_sent: Notify::new(),
            }
        }

        pub fn send(&self, msg: T) {
            let mut locked_queue = self.messages.lock().unwrap();
            locked_queue.push_back(msg);
            drop(locked_queue);

            // Send a notification to one of the calls currently
            // waiting in a call to `recv`.
            self.notify_on_sent.notify_one();
        }

        pub fn try_recv(&self) -> Option<T> {
            let mut locked_queue = self.messages.lock().unwrap();
            locked_queue.pop_front()
        }

        pub async fn recv(&self) -> T {
            let mut future = self.notify_on_sent.notified();
            let mut future = unsafe { std::pin::Pin::new_unchecked(&mut future) };

            loop {
                // Make sure that no wakeup is lost if we get
                // `None` from `try_recv`.
                future.as_mut().enable();

                if let Some(msg) = self.try_recv() {
                    return msg;
                }

                // Wait for a call to `notify_one`.
                //
                // This uses `.as_mut()` to avoid consuming the future,
                // which lets us call `Pin::set` below.
                future.as_mut().await;

                // Reset the future in case another call to
                // `try_recv` got the message before us.
                future.set(self.notify_on_sent.notified());
            }
        }
    }

    check_dfs(|| {
        future::block_on(async {
            let counter = Arc::new(AtomicUsize::new(0));
            let counter1 = counter.clone();
            let counter2 = counter.clone();

            let tx1 = Arc::new(Channel::new());
            let tx2 = tx1.clone();
            let rx1 = tx1.clone();
            let rx2 = tx1.clone();
            future::spawn(async move {
                tx1.send(1);
            });
            future::spawn(async move {
                tx2.send(2);
            });
            let r1 = future::spawn(async move {
                let x = rx1.recv().await;
                counter1.fetch_add(x, Ordering::SeqCst);
            });
            let r2 = future::spawn(async move {
                let x = rx2.recv().await;
                counter2.fetch_add(x, Ordering::SeqCst);
            });
            r1.await.unwrap();
            r2.await.unwrap();
            assert_eq!(counter.load(Ordering::SeqCst), 3);
        });
    });
}

fn notify_mpmc_channel_2_test(do_enable: bool) {
    // Example from the tokio docs

    // Unbound multi-producer multi-consumer (mpmc) channel.
    //
    // The call to `enable` is important because otherwise if you have two
    // calls to `recv` and two calls to `send` in parallel, the following could
    // happen:
    //
    //  1. Both calls to `try_recv` return `None`.
    //  2. Both new elements are added to the vector.
    //  3. The `notify_one` method is called twice, adding only a single
    //     permit to the `Notify`.
    //  4. Both calls to `recv` reach the `Notified` future. One of them
    //     consumes the permit, and the other sleeps forever.
    //
    // By adding the `Notified` futures to the list by calling `enable` before
    // `try_recv`, the `notify_one` calls in step three would remove the
    // futures from the list and mark them notified instead of adding a permit
    // to the `Notify`. This ensures that both futures are woken.
    struct Channel<T> {
        messages: Mutex<VecDeque<T>>,
        notify_on_sent: Notify,
        do_enable: bool,
    }

    impl<T> Channel<T> {
        fn new(do_enable: bool) -> Self {
            Self {
                messages: Mutex::new(VecDeque::new()),
                notify_on_sent: Notify::new(),
                do_enable,
            }
        }

        pub fn send(&self, msg: T) {
            let mut locked_queue = self.messages.lock().unwrap();
            locked_queue.push_back(msg);
            drop(locked_queue);

            // Send a notification to one of the calls currently
            // waiting in a call to `recv`.
            self.notify_on_sent.notify_one();
        }

        pub fn try_recv(&self) -> Option<T> {
            let mut locked_queue = self.messages.lock().unwrap();
            locked_queue.pop_front()
        }

        pub async fn recv(&self) -> T {
            let mut future = self.notify_on_sent.notified();
            let mut future = unsafe { std::pin::Pin::new_unchecked(&mut future) };

            loop {
                if self.do_enable {
                    // Make sure that no wakeup is lost if we get `None` from `try_recv`.
                    future.as_mut().enable();
                }

                if let Some(msg) = self.try_recv() {
                    return msg;
                }

                // Force context switch before the future is polled
                shuttle::future::yield_now().await;

                // Wait for a call to `notify_one`.
                //
                // This uses `.as_mut()` to avoid consuming the future,
                // which lets us call `Pin::set` below.
                future.as_mut().await;

                // Reset the future in case another call to
                // `try_recv` got the message before us.
                future.set(self.notify_on_sent.notified());
            }
        }
    }

    check_dfs(move || {
        future::block_on(async {
            let counter = Arc::new(AtomicUsize::new(0));
            let counter1 = counter.clone();
            let counter2 = counter.clone();

            let tx1 = Arc::new(Channel::new(do_enable));
            let tx2 = tx1.clone();
            let rx1 = tx1.clone();
            let rx2 = tx1.clone();

            let mut h = vec![];

            h.push(future::spawn(async move {
                let x = rx1.recv().await;
                counter1.fetch_add(x, Ordering::SeqCst);
            }));
            h.push(future::spawn(async move {
                let x = rx2.recv().await;
                counter2.fetch_add(x, Ordering::SeqCst);
            }));
            h.push(future::spawn(async move {
                tx1.send(1);
            }));
            h.push(future::spawn(async move {
                tx2.send(2);
            }));
            futures::future::join_all(h).await;
            assert_eq!(counter.load(Ordering::SeqCst), 3);
        });
    });
}

#[test]
fn notify_mpmc_no_deadlock() {
    notify_mpmc_channel_2_test(true);
}

#[test]
#[should_panic(expected = "deadlock")]
fn notify_mpmc_deadlock() {
    notify_mpmc_channel_2_test(false);
}
