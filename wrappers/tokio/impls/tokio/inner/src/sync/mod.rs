mod mutex;
pub use mutex::{Mutex, MutexGuard, OwnedMutexGuard, TryLockError};

pub use shuttle::future::batch_semaphore::{AcquireError, TryAcquireError};

mod semaphore;
pub use semaphore::{OwnedSemaphorePermit, Semaphore, SemaphorePermit};

mod rwlock;
pub use rwlock::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

pub mod broadcast;
pub mod mpsc;

pub mod notify;
pub use notify::Notify;

pub mod oneshot;
pub mod watch;

pub mod time {
    // Re-export for convenience
    #[doc(no_inline)]
    pub use std::time::Duration;
}

pub mod futures {
    pub use super::notify::Notified;
}

mod once_cell;
pub use self::once_cell::OnceCell;

#[cfg(test)]
mod test {
    /// This example demonstrates a historic bug in the interaction of Tokio's `select` and Shuttle's `Mutex`, which caused
    /// internal consistency violations in Shuttle. The same bug existed for `RwLock` as well.
    fn select_mutex_bug() {
        use crate::sync::mpsc;
        use shuttle::sync::{Arc, Mutex};

        // async wrapper for `Mutex::lock`
        async fn async_lock(m: Arc<Mutex<()>>) {
            *m.lock().unwrap();
        }

        shuttle::future::block_on(async {
            let (tx, mut rx) = mpsc::unbounded_channel();
            let mutex = Arc::new(Mutex::new(()));
            let mutex2 = mutex.clone();

            let h1 = shuttle::future::spawn(async move {
                tokio::select! {
                    biased;
                    _ = rx.recv() => {}
                    () = async_lock(mutex2) => {}
                }
            });

            let h2 = shuttle::future::spawn(async move {
                let _m = mutex.lock().unwrap();
                _ = tx.send(());
            });

            futures::future::join_all([h1, h2]).await;
        });
    }

    #[test_log::test]
    fn check_select_mutex_bug() {
        shuttle::check_dfs(select_mutex_bug, None);
    }

    /// A cancelled `recv()` must not leave a waiter registered in the channel's
    /// `recv_semaphore`.
    ///
    /// `Receiver::poll_recv` caches its `Acquire` future in `ReceiverInternal::
    /// pending_acquire` so the waiter survives across polls. The waiter records
    /// the `TaskId` of whichever task first polled it, and it is only removed
    /// when the *Receiver* is dropped/closed — not when the `recv()` future is
    /// dropped. So if a task cancels `recv()` (a documented cancel-safe
    /// operation in Tokio) and then exits while the `Receiver` lives on, a
    /// waiter belonging to a finished task stays queued. The next `send` calls
    /// `recv_semaphore.release(1)`, and Shuttle's
    /// `BatchSemaphoreState::unblock_waiters_from_front` trips
    /// `assert!(!task.finished())`.
    fn cancelled_recv_leaves_stale_waiter() {
        use crate::sync::mpsc;
        use shuttle::sync::{Arc, Mutex};

        shuttle::future::block_on(async {
            let (tx, rx) = mpsc::channel::<u32>(1);
            // The Receiver outlives the task that polls it.
            let rx = Arc::new(Mutex::new(Some(rx)));
            let rx2 = rx.clone();

            let h = shuttle::future::spawn(async move {
                let mut receiver = rx2.lock().unwrap().take().unwrap();
                {
                    let mut recv = Box::pin(receiver.recv());
                    // Channel is empty: the first poll enqueues a waiter for *this* task.
                    assert!(futures::poll!(recv.as_mut()).is_pending());
                    // Cancel the `recv()` (as `select!` would).
                }
                // Hand the Receiver back so it outlives this task, then finish
                // with the waiter still queued.
                *rx2.lock().unwrap() = Some(receiver);
            });
            h.await.unwrap();

            // Releases a permit on `recv_semaphore`, which tries to unblock the
            // waiter left behind by the finished task.
            tx.try_send(1).unwrap();

            // The cancelled `recv()` must not have consumed the message.
            let mut receiver = rx.lock().unwrap().take().unwrap();
            assert_eq!(receiver.recv().await, Some(1));
        });
    }

    #[test_log::test]
    fn check_cancelled_recv_leaves_stale_waiter() {
        shuttle::check_dfs(cancelled_recv_leaves_stale_waiter, None);
    }

    /// Control for `cancelled_recv_leaves_stale_waiter`: identical cancellation,
    /// except the `Receiver` is dropped inside the task. Dropping the Receiver
    /// drops the cached `Acquire`, whose `Drop` calls `remove_waiter`, so no
    /// stale waiter is left behind and the later `send` is fine. This shows the
    /// deregistration hook works — cancelling `recv()` just never reaches it,
    /// because the `Acquire` is owned by the `Receiver`, not by the `recv()`
    /// future that got dropped.
    fn cancelled_recv_with_receiver_dropped() {
        use crate::sync::mpsc;

        shuttle::future::block_on(async {
            let (tx, mut receiver) = mpsc::channel::<u32>(1);

            let h = shuttle::future::spawn(async move {
                {
                    let mut recv = Box::pin(receiver.recv());
                    assert!(futures::poll!(recv.as_mut()).is_pending());
                    // Cancel the `recv()` ...
                }
                // ... and drop the Receiver, which runs `Acquire::drop`.
                drop(receiver);
            });
            h.await.unwrap();

            // No waiter is queued, so this release has nobody to unblock.
            assert!(tx.try_send(1).is_err());
        });
    }

    #[test_log::test]
    fn check_cancelled_recv_with_receiver_dropped() {
        shuttle::check_dfs(cancelled_recv_with_receiver_dropped, None);
    }

    /// A `Receiver` moved to another task while its registering task is *still
    /// alive* must deliver to the new poller.
    ///
    /// Same setup as `cancelled_recv_leaves_stale_waiter`, except the first task
    /// never exits, so nothing is "finished" and no assertion fires. Instead the
    /// queued waiter still points at task A: `Acquire::poll` from task B hits the
    /// "fair semaphore, already queued" case and returns `Pending`, and a later
    /// `release` unblocks A (which is not waiting) rather than B. B is then never
    /// woken and Shuttle reports a deadlock. Shuttle's semaphore must re-point a
    /// queued waiter (and its waker) at whoever polls it, as tokio's own
    /// `batch_semaphore` does via its `will_wake` refresh.
    fn moved_recv_wakes_new_poller() {
        use crate::sync::mpsc;
        use shuttle::sync::{Arc, Mutex};

        shuttle::future::block_on(async {
            let (tx, rx) = mpsc::channel::<u32>(1);
            let rx = Arc::new(Mutex::new(Some(rx)));
            // Sequences task A's poll before task B's, and keeps A alive until
            // the end of the test.
            let (a_polled_tx, a_polled_rx) = mpsc::channel::<()>(1);
            let (release_a_tx, release_a_rx) = mpsc::channel::<()>(1);

            let rx_a = rx.clone();
            let a = shuttle::future::spawn(async move {
                let mut receiver = rx_a.lock().unwrap().take().unwrap();
                {
                    let mut recv = Box::pin(receiver.recv());
                    // Nothing has been sent yet, so this enqueues a waiter for
                    // task A rather than completing.
                    assert!(futures::poll!(recv.as_mut()).is_pending());
                    // Cancel the `recv()`, leaving the waiter queued.
                }
                *rx_a.lock().unwrap() = Some(receiver);
                a_polled_tx.send(()).await.unwrap();
                // Stay alive, but never poll the Receiver again.
                let mut release_a_rx = release_a_rx;
                let _ = release_a_rx.recv().await;
            });

            // Don't send anything until A has polled and handed the Receiver
            // back, so A's poll is deterministically the one that registers.
            let mut a_polled_rx = a_polled_rx;
            a_polled_rx.recv().await.unwrap();

            let rx_b = rx.clone();
            let b = shuttle::future::spawn(async move {
                let mut receiver = rx_b.lock().unwrap().take().unwrap();
                // Must be woken by the send below, even though the queued waiter
                // was registered by task A.
                let got = receiver.recv().await;
                *rx_b.lock().unwrap() = Some(receiver);
                got
            });

            tx.send(1).await.unwrap();
            assert_eq!(b.await.unwrap(), Some(1));
            release_a_tx.send(()).await.unwrap();
            a.await.unwrap();
        });
    }

    #[test_log::test]
    fn check_moved_recv_wakes_new_poller() {
        shuttle::check_random(moved_recv_wakes_new_poller, 200);
    }
}
