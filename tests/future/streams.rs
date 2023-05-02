use futures::{future::join_all, stream::FuturesUnordered, StreamExt};
use shuttle::{
    check_random,
    future::{block_on, spawn},
};

fn futures_unordered_collect() {
    block_on(async {
        let tasks = (0..100).map(|_| spawn(async move {})).collect::<FuturesUnordered<_>>();

        let _ = tasks.collect::<Vec<_>>().await;
    });
}

#[test]
fn collect_empty_tasks() {
    check_random(futures_unordered_collect, 100);
}

fn futures_unordered_next() {
    block_on(async {
        let mut tasks = (0..100).map(|_| spawn(async move {})).collect::<FuturesUnordered<_>>();

        while let Some(result) = tasks.next().await {
            result.unwrap();
        }
    });
}

#[test]
fn next_empty_tasks() {
    check_random(futures_unordered_next, 100);
}

fn futures_join_all() {
    block_on(async {
        let tasks = (0..100).map(|_| spawn(async move {})).collect::<FuturesUnordered<_>>();

        join_all(tasks)
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
    });
}

#[test]
fn join_all_empty_tasks() {
    check_random(futures_join_all, 100);
}
