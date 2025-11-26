use shuttle::{check_dfs, future};
use shuttle_tokio_impl_inner::sync::Semaphore;
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use test_log::test;

#[test]
fn try_acquire() {
    check_dfs(
        || {
            let sem = Arc::new(Semaphore::new(1));
            {
                let p1 = sem.clone().try_acquire_owned();
                assert!(p1.is_ok());
                let p2 = sem.clone().try_acquire_owned();
                assert!(p2.is_err());
            }
            let p3 = sem.try_acquire_owned();
            assert!(p3.is_ok());
        },
        None,
    );
}

#[test]
fn try_acquire_many() {
    shuttle::check_dfs(
        || {
            let sem = Arc::new(Semaphore::new(42));
            {
                let p1 = sem.clone().try_acquire_many_owned(42);
                assert!(p1.is_ok());
                let p2 = sem.clone().try_acquire_owned();
                assert!(p2.is_err());
            }
            let p3 = sem.clone().try_acquire_many_owned(32);
            assert!(p3.is_ok());
            let p4 = sem.clone().try_acquire_many_owned(10);
            assert!(p4.is_ok());
            assert!(sem.try_acquire_owned().is_err());
        },
        None,
    );
}

#[test]
fn semaphore_acquire() {
    check_dfs(
        || {
            future::block_on(async move {
                let sem = Arc::new(Semaphore::new(1));
                let p1 = sem.clone().try_acquire_owned().unwrap();
                let sem_clone = sem.clone();
                let j = future::spawn(async move {
                    let _p2 = sem_clone.acquire_owned().await;
                });
                drop(p1);
                j.await.unwrap();
            });
        },
        None,
    );
}

async fn semtest(num_permits: usize, counts: Vec<usize>, states: &Arc<Mutex<HashSet<(usize, usize)>>>) {
    let s = Arc::new(Semaphore::new(num_permits));
    let r = Arc::new(AtomicUsize::new(0));
    let mut handles = vec![];
    for (i, &c) in counts.iter().enumerate() {
        let s = s.clone();
        let r = r.clone();
        let states = states.clone();
        let val = 1usize << i;
        handles.push(future::spawn(async move {
            let permit = s.acquire_many(c as u32).await.unwrap();
            let v = r.fetch_add(val, Ordering::SeqCst);
            future::yield_now().await;
            let _ = r.fetch_sub(val, Ordering::SeqCst);
            states.lock().unwrap().insert((i, v));
            drop(permit);
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
}

#[test]
fn semtest_1() {
    let states = Arc::new(Mutex::new(HashSet::new()));
    let states2 = states.clone();
    check_dfs(
        move || {
            let states2 = states2.clone();
            future::block_on(async move {
                semtest(5, vec![3, 3, 3], &states2).await;
            });
        },
        None,
    );

    let states = Arc::try_unwrap(states).unwrap().into_inner().unwrap();
    assert_eq!(states, HashSet::from([(0, 0), (1, 0), (2, 0)]));
}

#[test]
fn semtest_2() {
    let states = Arc::new(Mutex::new(HashSet::new()));
    let states2 = states.clone();
    check_dfs(
        move || {
            let states2 = states2.clone();
            future::block_on(async move {
                semtest(5, vec![3, 3, 2], &states2).await;
            });
        },
        None,
    );

    let states = Arc::try_unwrap(states).unwrap().into_inner().unwrap();
    assert_eq!(
        states,
        HashSet::from([(0, 0), (1, 0), (2, 0), (0, 4), (1, 4), (2, 1), (2, 2)])
    );
}

#[test]
fn add_permits() {
    check_dfs(
        || {
            future::block_on(async {
                let sem = Arc::new(Semaphore::new(0));
                let sem_clone = sem.clone();
                let j = future::spawn(async move {
                    let _p2 = sem_clone.acquire_owned().await;
                });
                sem.add_permits(1);
                j.await.unwrap();
            });
        },
        None,
    );
}

#[test]
fn forget() {
    check_dfs(
        || {
            let sem = Arc::new(Semaphore::new(1));
            {
                let p = sem.clone().try_acquire_owned().unwrap();
                assert_eq!(sem.available_permits(), 0);
                p.forget();
                assert_eq!(sem.available_permits(), 0);
            }
            assert_eq!(sem.available_permits(), 0);
            assert!(sem.try_acquire_owned().is_err());
        },
        None,
    );
}

#[test]
fn merge() {
    check_dfs(
        || {
            let sem = Arc::new(Semaphore::new(3));
            {
                let mut p1 = sem.try_acquire().unwrap();
                assert_eq!(sem.available_permits(), 2);
                let p2 = sem.try_acquire_many(2).unwrap();
                assert_eq!(sem.available_permits(), 0);
                p1.merge(p2);
                assert_eq!(sem.available_permits(), 0);
            }
            assert_eq!(sem.available_permits(), 3);
        },
        None,
    );
}

#[test]
#[should_panic(expected = "merging permits from different semaphore instances")]
fn merge_unrelated_permits() {
    check_dfs(
        || {
            let sem1 = Arc::new(Semaphore::new(3));
            let sem2 = Arc::new(Semaphore::new(3));
            let mut p1 = sem1.try_acquire().unwrap();
            let p2 = sem2.try_acquire().unwrap();
            p1.merge(p2);
        },
        None,
    );
}

#[test]
fn merge_owned() {
    check_dfs(
        || {
            let sem = Arc::new(Semaphore::new(3));
            {
                let mut p1 = sem.clone().try_acquire_owned().unwrap();
                assert_eq!(sem.available_permits(), 2);
                let p2 = sem.clone().try_acquire_many_owned(2).unwrap();
                assert_eq!(sem.available_permits(), 0);
                p1.merge(p2);
                assert_eq!(sem.available_permits(), 0);
            }
            assert_eq!(sem.available_permits(), 3);
        },
        None,
    );
}

#[test]
#[should_panic(expected = "merging permits from different semaphore instances")]
fn merge_unrelated_owned_permits() {
    check_dfs(
        || {
            let sem1 = Arc::new(Semaphore::new(3));
            let sem2 = Arc::new(Semaphore::new(3));
            let mut p1 = sem1.try_acquire_owned().unwrap();
            let p2 = sem2.try_acquire_owned().unwrap();
            p1.merge(p2);
        },
        None,
    );
}

#[test]
fn split() {
    check_dfs(
        || {
            let sem = Semaphore::new(5);
            let mut p1 = sem.try_acquire_many(3).unwrap();
            assert_eq!(sem.available_permits(), 2);
            assert_eq!(p1.num_permits(), 3);
            let mut p2 = p1.split(1).unwrap();
            assert_eq!(sem.available_permits(), 2);
            assert_eq!(p1.num_permits(), 2);
            assert_eq!(p2.num_permits(), 1);
            let p3 = p1.split(0).unwrap();
            assert_eq!(p3.num_permits(), 0);
            drop(p1);
            assert_eq!(sem.available_permits(), 4);
            let p4 = p2.split(1).unwrap();
            assert_eq!(p2.num_permits(), 0);
            assert_eq!(p4.num_permits(), 1);
            assert!(p2.split(1).is_none());
            drop(p2);
            assert_eq!(sem.available_permits(), 4);
            drop(p3);
            assert_eq!(sem.available_permits(), 4);
            drop(p4);
            assert_eq!(sem.available_permits(), 5);
        },
        None,
    );
}

#[test]
fn split_owned() {
    check_dfs(
        || {
            let sem = Arc::new(Semaphore::new(5));
            let mut p1 = sem.clone().try_acquire_many_owned(3).unwrap();
            assert_eq!(sem.available_permits(), 2);
            assert_eq!(p1.num_permits(), 3);
            let mut p2 = p1.split(1).unwrap();
            assert_eq!(sem.available_permits(), 2);
            assert_eq!(p1.num_permits(), 2);
            assert_eq!(p2.num_permits(), 1);
            let p3 = p1.split(0).unwrap();
            assert_eq!(p3.num_permits(), 0);
            drop(p1);
            assert_eq!(sem.available_permits(), 4);
            let p4 = p2.split(1).unwrap();
            assert_eq!(p2.num_permits(), 0);
            assert_eq!(p4.num_permits(), 1);
            assert!(p2.split(1).is_none());
            drop(p2);
            assert_eq!(sem.available_permits(), 4);
            drop(p3);
            assert_eq!(sem.available_permits(), 4);
            drop(p4);
            assert_eq!(sem.available_permits(), 5);
        },
        None,
    );
}

/*
#[test]
fn acquire_many() {
    check_dfs(
        || {
            future::block_on(async move {
                let semaphore = Arc::new(Semaphore::new(42));
                let permit32 = semaphore.clone().try_acquire_many_owned(32).unwrap();
                let (sender, receiver) = shuttle::sync::oneshot::channel();
                let join_handle = future::spawn(async move {
                    let _permit10 = semaphore.clone().acquire_many_owned(10).await.unwrap();
                    sender.send(()).unwrap();
                    let _permit32 = semaphore.acquire_many_owned(32).await.unwrap();
                });
                receiver.await.unwrap();
                drop(permit32);
                join_handle.await.unwrap();
            });
        },
        None,
    );
}

*/
