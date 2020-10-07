use shuttle::{asynch, check, check_dfs, thread};
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn async_counting() {
    let counter = std::sync::Arc::new(AtomicUsize::new(0));
    let counter_1 = std::sync::Arc::clone(&counter);

    check(move || {
        let counter_2 = std::sync::Arc::clone(&counter_1);
        asynch::spawn(async move {
            counter_2.fetch_add(1, Ordering::SeqCst);
        });
        let counter_3 = std::sync::Arc::clone(&counter_1);
        asynch::spawn(async move {
            counter_3.fetch_add(1, Ordering::SeqCst);
        });
    });

    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

async fn add(a: u32, b: u32) -> u32 {
    a + b
}

#[test]
fn async_fncall() {
    check(move || {
        let sum = add(3, 5);
        asynch::spawn(async move {
            let r = sum.await;
            assert_eq!(r, 8u32);
        });
    });
}

#[test]
fn async_with_join() {
    check(move || {
        thread::spawn(|| {
            let join = asynch::spawn(async move { add(10, 32).await });

            asynch::spawn(async move {
                assert_eq!(join.await.unwrap(), 42u32);
            });
        });
    });
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
    );
}
