//! Code written against crossbeam-queue, as a crate that depends on `shuttle-crossbeam-queue` under
//! the name `crossbeam-queue` would have it. The same source is compiled against
//! `shuttle-crossbeam-queue` without the `shuttle` feature, where it is crossbeam-queue, and against
//! `shuttle-crossbeam-queue-inner`, which is what it is under `shuttle`. The test that includes it
//! provides `crate::thread`, std's threads or Shuttle's, and runs `exercise` under Shuttle when the
//! queues are Shuttle's.
//!
//! The checks hold whichever crate `crossbeam_queue` is.

use crossbeam_queue::{ArrayQueue, SegQueue};
use std::sync::Arc;

/// A queue that lives in a `static`, which crossbeam-queue allows for `SegQueue`.
static PENDING: SegQueue<u32> = SegQueue::new();

/// Exercises the queues' API.
pub fn exercise() {
    array_queue();
    array_queue_exclusive();
    seg_queue();
    seg_queue_exclusive();
    statics();
    dropping();
    formatting();
    concurrent();
}

fn array_queue() {
    let q = ArrayQueue::new(2);
    assert_eq!(q.capacity(), 2);
    assert!(q.is_empty());
    assert!(!q.is_full());
    assert_eq!(q.len(), 0);

    assert_eq!(q.push(1), Ok(()));
    assert_eq!(q.push(2), Ok(()));
    assert_eq!(q.push(3), Err(3));
    assert!(q.is_full());
    assert_eq!(q.len(), 2);

    // The oldest value makes room.
    assert_eq!(q.force_push(4), Some(1));
    assert_eq!(q.pop(), Some(2));
    assert_eq!(q.force_push(5), None);
    assert_eq!(q.pop(), Some(4));
    assert_eq!(q.pop(), Some(5));
    assert_eq!(q.pop(), None);
    assert!(q.is_empty());

    q.push(6).unwrap();
    q.push(7).unwrap();
    assert_eq!(q.into_iter().collect::<Vec<_>>(), vec![6, 7]);
}

fn array_queue_exclusive() {
    let mut q = ArrayQueue::new(1);
    assert_eq!(q.push_mut(10), Ok(()));
    assert_eq!(q.push_mut(20), Err(20));
    assert_eq!(q.pop_mut(), Some(10));
    assert!(q.pop_mut().is_none());
}

fn seg_queue() {
    let q = SegQueue::new();
    assert!(q.is_empty());
    q.push('a');
    q.push('b');
    assert_eq!(q.len(), 2);
    assert_eq!(q.pop(), Some('a'));
    assert_eq!(q.pop(), Some('b'));
    assert!(q.pop().is_none());

    let q: SegQueue<u64> = SegQueue::default();
    for i in 0..100 {
        q.push(i);
    }
    assert_eq!(q.len(), 100);
    assert_eq!(q.into_iter().sum::<u64>(), 4950);
}

fn seg_queue_exclusive() {
    let mut q = SegQueue::new();
    q.push_mut(10);
    q.push_mut(20);
    assert_eq!(q.pop_mut(), Some(10));
    assert_eq!(q.pop_mut(), Some(20));
    assert!(q.pop_mut().is_none());
}

/// Leaves the `static` as it found it, so that the checks hold however many times this runs in a
/// process, and whether or not the queue starts each run empty.
fn statics() {
    let before = PENDING.len();
    PENDING.push(1);
    PENDING.push(2);
    assert_eq!(PENDING.len(), before + 2);
    let mut drained = Vec::new();
    while let Some(value) = PENDING.pop() {
        drained.push(value);
    }
    assert_eq!(&drained[drained.len() - 2..], &[1, 2]);
}

fn dropping() {
    let token = Arc::new(());
    {
        let a = ArrayQueue::new(4);
        a.push(Arc::clone(&token)).unwrap();
        let s = SegQueue::new();
        s.push(Arc::clone(&token));
        s.push(Arc::clone(&token));
        assert_eq!(Arc::strong_count(&token), 4);
    }
    // Dropping a queue drops what is in it.
    assert_eq!(Arc::strong_count(&token), 1);

    let q = SegQueue::new();
    q.push(Arc::clone(&token));
    let mut values = q.into_iter();
    assert_eq!(Arc::strong_count(&token), 2);
    drop(values.next());
    assert_eq!(Arc::strong_count(&token), 1);
}

fn formatting() {
    assert_eq!(format!("{:?}", ArrayQueue::<u8>::new(1)), "ArrayQueue { .. }");
    assert_eq!(format!("{:?}", SegQueue::<u8>::new()), "SegQueue { .. }");
}

/// Two producers and a consumer. Whatever order the threads run in, every value arrives exactly
/// once, and each producer's values arrive in the order it pushed them.
fn concurrent() {
    let bounded = Arc::new(ArrayQueue::new(4));
    let unbounded = Arc::new(SegQueue::new());
    let producers: Vec<_> = (0..2)
        .map(|producer| {
            let bounded = Arc::clone(&bounded);
            let unbounded = Arc::clone(&unbounded);
            crate::thread::spawn(move || {
                for i in 0..2 {
                    bounded.push((producer, i)).unwrap();
                    unbounded.push((producer, i));
                }
            })
        })
        .collect();

    let mut received = Vec::new();
    while received.len() < 4 {
        if let Some(value) = bounded.pop() {
            received.push(value);
        } else {
            crate::thread::yield_now();
        }
    }
    for producer in producers {
        producer.join().unwrap();
    }

    for producer in 0..2 {
        let order: Vec<_> = received
            .iter()
            .filter(|(p, _)| *p == producer)
            .map(|(_, i)| *i)
            .collect();
        assert_eq!(order, vec![0, 1]);
    }
    let mut all: Vec<_> = std::iter::from_fn(|| unbounded.pop()).collect();
    all.sort_unstable();
    assert_eq!(all, vec![(0, 0), (0, 1), (1, 0), (1, 1)]);
    assert!(bounded.is_empty() && unbounded.is_empty());
}
