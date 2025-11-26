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
}
