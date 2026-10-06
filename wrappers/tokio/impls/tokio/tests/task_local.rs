//! `shuttle-tokio-impl` used to re-export tokio's own `task_local!`, which does not keep the values
//! of different Shuttle tasks apart (see `inner/tests/task_local.rs`). Check that it exposes the
//! Shuttle implementation instead.

use shuttle::future::block_on;
use shuttle_tokio_impl::task::futures::TaskLocalFuture;
use shuttle_tokio_impl::task::{self, LocalKey};
use shuttle_tokio_impl::task_local;
use std::future::Ready;

task_local! {
    static KEY: u32;
}

#[test]
fn task_local_is_shuttles() {
    // These only compile if the macro and the types are the inner crate's, and not tokio's.
    let _: &'static shuttle_tokio_impl_inner::task::LocalKey<u32> = &KEY;
    let _: &'static LocalKey<u32> = &KEY;
    let fut: TaskLocalFuture<u32, Ready<()>> = KEY.scope(1, std::future::ready(()));
    drop(fut);
}

#[test]
fn spawned_task_does_not_see_parents_value() {
    shuttle::check_dfs(
        || {
            block_on(KEY.scope(1, async {
                let child = task::spawn(async { KEY.try_get().ok() });
                // A scheduling point in the middle of this poll, at which `child` can run
                drop(shuttle_tokio_impl::sync::Mutex::new(()).lock().await);
                assert_eq!(child.await.unwrap(), None);
                assert_eq!(KEY.get(), 1);
            }))
        },
        None,
    );
}
