use std::time::Duration;

use shuttle::crossbeam_channel::{Select, unbounded};
use shuttle::sync::{Arc, Barrier};
use shuttle::{check_dfs};
use test_log::test;
use shuttle::thread;

#[test]
fn selector_one_channel() {
    check_dfs(
        move || {
            let (s, r) = unbounded();

            let mut selector = Select::new();
            selector.recv(&r);

            s.send(5).unwrap();

            let op = selector.select();
            assert_eq!(op.index, 0);

            let val = r.recv().unwrap();
            assert_eq!(val, 5);
        },
        None,
    );
}

#[test]
fn selector_multi_channel() {
    check_dfs(
        move || {
            let (_, r1) = unbounded::<i32>();
            let (s2, r2) = unbounded();

            let mut selector = Select::new();
            selector.recv(&r1);
            selector.recv(&r2);

            s2.send(81).unwrap();

            let op = selector.select();
            assert_eq!(op.index, 1);

            let val = r2.recv().unwrap();
            assert_eq!(val, 81);
        },
        None,
    );
}

#[test]
fn try_select_empty_selector() {
    check_dfs(
        move || {
            assert!(Select::new().try_select().is_err())
        },
        None,
    );
}

#[test]
fn select_unused_channel_functional() {
    check_dfs(
        move || {
            let (s1, r1) = unbounded();
            let (s2, r2) = unbounded();

            let mut selector = Select::new();
            selector.recv(&r1);
            selector.recv(&r2);

            s2.send(81).unwrap();

            let op = selector.select();
            assert_eq!(op.index, 1);

            let val = r2.recv().unwrap();
            assert_eq!(val, 81);

            s1.send(198).unwrap();
            let val = r1.recv().unwrap();
            assert_eq!(val, 198);
        },
        None,
    );
}

#[test]
fn select_two_threads() {
    check_dfs(
        move || {
            let (_, r1) = unbounded::<i32>();
            let (s2, r2) = unbounded();

            let mut selector = Select::new();
            selector.recv(&r1);
            selector.recv(&r2);

            thread::spawn(move || {
                thread::sleep(Duration::from_millis(15));
                s2.send(5).unwrap();
            });

            let op = selector.select();
            assert_eq!(op.index, 1);

            let val = r2.recv().unwrap();
            assert_eq!(val, 5);
        },
        None,
    );
}

#[test]
fn select_multi_producer() {
    check_dfs(
        move || {
            let (s1, r1) = unbounded();
            let s1_clone = s1.clone();

            let mut selector = Select::new();
            selector.recv(&r1);

            thread::spawn(move || {
                thread::sleep(Duration::from_millis(15));
                s1.send(5).unwrap();
            });

            thread::spawn(move || {
                thread::sleep(Duration::from_millis(15));
                s1_clone.send(6).unwrap();
            });

            let op = selector.select();
            assert_eq!(op.index, 0);

            let val = r1.recv().unwrap();
            assert!(val == 5 || val == 6);
        },
        None,
    );
}

#[test]
fn select_multi_consumer() {
    check_dfs(
        move || {
            let (s1, r1) = unbounded();
            let r1_clone = r1.clone();

            // Create a barrier to enforce that the first send will match up to a receiver
            // before the selector begins receiving.
            // TODO: is this overengineering the test?
            let bar = Arc::new(Barrier::new(2));
            let bar_clone = bar.clone();
            
            thread::spawn(move || {
                r1.recv().unwrap();
                bar.wait();
            });

            s1.send(5).unwrap();
            bar_clone.wait();

            let mut selector = Select::new();
            selector.recv(&r1_clone);

            thread::spawn(move || {
                thread::sleep(Duration::from_millis(15));
                s1.send(6).unwrap();
            });

            let op = selector.select();
            assert_eq!(op.index, 0);

            let val = r1_clone.recv().unwrap();
            assert_eq!(val, 6);
        },
        None,
    );
}

#[test]
fn select_mpmc() {
    check_dfs(
        move || {
            let (s1, r1) = unbounded();
            let s1_clone = s1.clone();
            let r1_clone = r1.clone();

            // Create a barrier to enforce that all created threads complete before the test terminates.
            // This way, any errors caused due to MPMC will be discovered by Shuttle's DFS search.
            let bar = Arc::new(Barrier::new(4));
            let bar_clone1 = bar.clone();
            let bar_clone2 = bar.clone();
            let bar_clone3 = bar.clone();

            let mut selector = Select::new();
            selector.recv(&r1_clone);

            thread::spawn(move || {
                thread::sleep(Duration::from_millis(15));
                s1.send(6).unwrap();
                bar_clone1.wait();
            });

            thread::spawn(move || {
                thread::sleep(Duration::from_millis(15));
                s1_clone.send(5).unwrap();
                bar_clone2.wait();
            });

            let op = selector.select();
            assert_eq!(op.index, 0);

            thread::spawn(move || {
                r1.recv().unwrap();
                bar_clone3.wait();
            });

            let val = r1_clone.recv().unwrap();
            assert!(val == 5 || val == 6);
            bar.wait();
        },
        None,
    );
}

#[test]
fn select_many_threads() {
    check_dfs(
        move || {
            let num_channels = 6;
            let mut senders = Vec::new();
            let mut receivers = Vec::new();
            let mut selector = Select::new();

            for _ in 0..num_channels {
                let (s, r) = unbounded();
                senders.push(s);
                receivers.push(r);
            }
            for i in 0..num_channels {
                selector.recv(&receivers[i]);
            }

            let sender_indices = vec![1, 3, 5];
            for idx in sender_indices.clone() {
                let sender_clone = senders[idx].clone();
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(15));
                    sender_clone.send(idx * 2).unwrap();
                });
            }

            let op = selector.select();
            assert!(sender_indices.contains(&op.index));

            let val = receivers[op.index].recv().unwrap();
            assert_eq!(val, op.index * 2);
        },
        None,
    );
}