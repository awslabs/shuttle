use shuttle::{asynch, check_dfs, check_random, scheduler::PCTScheduler, thread, Runner};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use test_env_log::test;

async fn add(a: u32, b: u32) -> u32 {
    a + b
}

#[test]
fn async_fncall() {
    check_dfs(
        move || {
            let sum = add(3, 5);
            asynch::spawn(async move {
                let r = sum.await;
                assert_eq!(r, 8u32);
            });
        },
        None,
        None,
    );
}

#[test]
fn async_with_join() {
    check_dfs(
        move || {
            thread::spawn(|| {
                let join = asynch::spawn(async move { add(10, 32).await });

                asynch::spawn(async move {
                    assert_eq!(join.await.unwrap(), 42u32);
                });
            });
        },
        None,
        None,
    );
}

#[test]
fn async_with_threads() {
    check_dfs(
        move || {
            thread::spawn(|| {
                let v1 = async { 3u32 };
                let v2 = async { 2u32 };
                asynch::spawn(async move {
                    assert_eq!(5u32, v1.await + v2.await);
                });
            });
            thread::spawn(|| {
                let v1 = async { 5u32 };
                let v2 = async { 6u32 };
                asynch::spawn(async move {
                    assert_eq!(11u32, v1.await + v2.await);
                });
            });
        },
        None,
        None,
    );
}

#[test]
fn async_block_on() {
    check_dfs(
        || {
            let v = asynch::block_on(async { 42u32 });
            assert_eq!(v, 42u32);
        },
        None,
        None,
    );
}

#[test]
fn async_spawn() {
    check_dfs(
        || {
            let t = asynch::spawn(async { 42u32 });
            let v = asynch::block_on(async { t.await.unwrap() });
            assert_eq!(v, 42u32);
        },
        None,
        None,
    );
}

#[test]
fn async_spawn_chain() {
    check_dfs(
        || {
            let t1 = asynch::spawn(async { 1u32 });
            let t2 = asynch::spawn(async move { t1.await.unwrap() });
            let v = asynch::block_on(async move { t2.await.unwrap() });
            assert_eq!(v, 1u32);
        },
        None,
        None,
    );
}

#[test]
fn async_yield() {
    check_dfs(
        || {
            let v = asynch::block_on(async {
                asynch::yield_now().await;
                42u32
            });
            assert_eq!(v, 42u32);
        },
        None,
        None,
    )
}

fn async_counter() {
    let counter = Arc::new(AtomicUsize::new(0));

    let tasks: Vec<_> = (0..10)
        .map(|_| {
            let counter = Arc::clone(&counter);
            asynch::spawn(async move {
                let c = counter.load(Ordering::SeqCst);
                asynch::yield_now().await;
                counter.fetch_add(c, Ordering::SeqCst);
            })
        })
        .collect();

    asynch::block_on(async move {
        for t in tasks {
            t.await.unwrap();
        }
    });
}

#[test]
fn async_counter_random() {
    check_random(async_counter, 5000)
}

#[test]
fn async_counter_pct() {
    let scheduler = PCTScheduler::new(2, 5000);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(async_counter);
}
