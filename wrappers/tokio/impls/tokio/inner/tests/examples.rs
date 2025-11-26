use futures::FutureExt;
use shuttle::check_dfs;
use shuttle::future;
use shuttle_tokio_impl_inner::sync::Mutex;
use test_log::test;

/// This function illustrates the danger of partially executing a set of futures
/// (e.g., in a `select!` statement), and then awaiting one of the futures without
/// dropping the other futures first. The other futures could still hold resources
/// that the awaited future needs, resulting in a deadlock.
async fn footgun(drop_a: bool) {
    let lock = Mutex::new(());

    let mut a = async {
        let _guard = lock.lock().await;
        future::yield_now().await;
    }
    .boxed();

    let mut b = async {
        let _guard = lock.lock().await;
    }
    .boxed();

    let mut c = async {}.boxed();

    // task `a` acquires the mutex and yields
    // task `b` becomes a waiter for the mutex
    // task `c` completes the select
    tokio::select! {
        biased;
        () = &mut a => {}
        () = &mut b => {}
        () = &mut c => {}
    }

    if drop_a {
        // dropping `a` releases the mutex so that `b` can acquire it
        drop(a);
    }

    // if `a` was not dropped, then `b` cannot acquire the mutex and we deadlock
    b.await;
}

#[test]
#[should_panic(expected = "deadlock")]
fn footgun_deadlock() {
    check_dfs(|| future::block_on(footgun(false)), None);
}

#[test]
fn footgun_averted() {
    check_dfs(|| future::block_on(footgun(true)), None);
}
