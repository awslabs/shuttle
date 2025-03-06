use futures::{StreamExt, future::join_all, stream::FuturesUnordered};
use shuttle::{
    check_dfs,
    future::{JoinHandle, block_on, spawn},
};

fn empty_tasks() -> FuturesUnordered<JoinHandle<()>> {
    (0..2).map(|_| spawn(async move {})).collect()
}

#[test]
fn collect_empty_tasks() {
    check_dfs(
        || {
            block_on(async {
                let tasks = empty_tasks();

                let _ = tasks.collect::<Vec<_>>().await;
            })
        },
        None,
    );
}

#[test]
fn next_empty_tasks() {
    check_dfs(
        || {
            block_on(async {
                let mut tasks = empty_tasks();

                while let Some(result) = tasks.next().await {
                    result.unwrap();
                }
            })
        },
        None,
    );
}

#[test]
fn join_all_empty_tasks() {
    check_dfs(
        || {
            block_on(async {
                let tasks = empty_tasks();

                join_all(tasks)
                    .await
                    .into_iter()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
            })
        },
        None,
    );
}
