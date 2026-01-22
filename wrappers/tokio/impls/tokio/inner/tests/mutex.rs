use shuttle::future;
use shuttle::scheduler::PctScheduler;
use shuttle::{check_dfs, Runner};
use shuttle_tokio_impl_inner::sync::Mutex;
use std::collections::BTreeMap;
use std::sync::Arc;
use test_log::test;

#[test]
fn async_mutex_basic_test() {
    check_dfs(
        || {
            future::block_on(async {
                let lock = Arc::new(Mutex::new(0usize));
                let lock_clone = Arc::clone(&lock);

                future::spawn(async move {
                    let mut counter = lock_clone.lock().await;
                    *counter += 1;
                });

                let mut counter = lock.lock().await;
                *counter += 1;
            });
        },
        None,
    );

    // TODO would be cool if we were allowed to smuggle the lock out of the run,
    // TODO so we can assert invariants about it *after* execution ends
}

async fn deadlock() {
    let lock1 = Arc::new(Mutex::new(0usize));
    let lock2 = Arc::new(Mutex::new(0usize));
    let lock1_clone = Arc::clone(&lock1);
    let lock2_clone = Arc::clone(&lock2);

    future::spawn(async move {
        let _l1 = lock1_clone.lock().await;
        let _l2 = lock2_clone.lock().await;
    });

    let _l2 = lock2.lock().await;
    let _l1 = lock1.lock().await;
}

#[test]
#[should_panic(expected = "deadlock")]
fn async_mutex_deadlock() {
    check_dfs(|| future::block_on(async { deadlock().await }), None);
}

#[test]
#[should_panic(expected = "racing increments")]
fn async_mutex_concurrent_increment_buggy() {
    let scheduler = PctScheduler::new(2, 100);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(|| {
        future::block_on(async {
            let lock = Arc::new(Mutex::new(0usize));

            let tasks = (0..2)
                .map(|_| {
                    let lock = Arc::clone(&lock);
                    future::spawn(async move {
                        let curr = *lock.lock().await;
                        *lock.lock().await = curr + 1;
                    })
                })
                .collect::<Vec<_>>();

            for t in tasks {
                t.await.unwrap();
            }

            let counter = *lock.lock().await;
            assert_eq!(counter, 2, "racing increments");
        });
    });
}

// Create a BTreeMap of usize -> Node where each Node contains some value (usize), initially 0
// Spin up tasks that grab locks on individual nodes from the tree in some order
// Once all nodes are locked, the task increments the value of each node and then releases them
async fn owned_mutex_increment(num_locks: usize, mut locks: Vec<Vec<usize>>) -> Vec<usize> {
    #[derive(Debug)]
    struct Node(usize);

    let mut map = BTreeMap::new();
    for i in 0usize..num_locks {
        map.insert(i, Arc::new(Mutex::new(Node(0))));
    }
    let map = Arc::new(map);
    let mut handles = Vec::new();
    for lock in locks.drain(..) {
        let map = Arc::clone(&map);
        handles.push(future::spawn(async move {
            let mut nodes = Vec::new();
            for m in lock {
                let node = map.get(&m).unwrap().clone();
                nodes.push(node.lock_owned().await);
            }
            for mut n in nodes {
                n.0 += 1;
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    let mut values = Vec::new();
    for (_, n) in map.iter() {
        let v = n.lock().await.0;
        values.push(v);
    }
    values
}

#[test]
fn async_mutex_owned_1() {
    check_dfs(
        || {
            future::block_on(async move {
                let v = owned_mutex_increment(5usize, vec![vec![0, 2, 4], vec![1, 3]]).await;
                assert_eq!(v, [1, 1, 1, 1, 1]);
            });
        },
        None,
    );
}

#[test]
fn async_mutex_owned_2() {
    check_dfs(
        || {
            future::block_on(async move {
                let v = owned_mutex_increment(5usize, vec![vec![0, 2, 4], vec![1, 2, 3]]).await;
                assert_eq!(v, [1, 1, 2, 1, 1]);
            });
        },
        None,
    );
}
