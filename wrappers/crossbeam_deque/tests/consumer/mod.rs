//! Code written against crossbeam-deque, as a crate that depends on `shuttle-crossbeam-deque` under
//! the name `crossbeam-deque` would have it. The same source is compiled against
//! `shuttle-crossbeam-deque` without the `shuttle` feature, where it is crossbeam-deque, and against
//! `shuttle-crossbeam-deque-inner`, which is what it is under `shuttle`. The test that includes it
//! provides `crate::thread`, std's threads or Shuttle's, and runs `exercise` under Shuttle when the
//! queues are Shuttle's.
//!
//! The single-threaded checks pin down what crossbeam-deque 0.8.8 does, including which tasks a
//! batch steal takes, so they also check that the model does the same.

use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use std::iter;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Exercises the queues' API.
pub fn exercise() {
    workers();
    stealers();
    batches();
    batches_between_flavors();
    injector();
    injector_batches();
    steal_results();
    dropping();
    formatting();
    scheduler();
}

fn workers() {
    let w = Worker::new_fifo();
    assert!(w.is_empty());
    w.push(1);
    w.push(2);
    w.push(3);
    assert_eq!(w.len(), 3);
    assert_eq!(w.pop(), Some(1));
    assert_eq!(w.pop(), Some(2));

    let w = Worker::new_lifo();
    w.push(1);
    w.push(2);
    w.push(3);
    assert_eq!(w.pop(), Some(3));
    assert_eq!(w.pop(), Some(2));
    assert_eq!(w.pop(), Some(1));
    assert_eq!(w.pop(), None);

    // Past crossbeam's first buffer of 64, and back.
    let w = Worker::new_lifo();
    for i in 0..200 {
        w.push(i);
    }
    assert_eq!(w.len(), 200);
    assert_eq!(
        (0..200).rev().collect::<Vec<_>>(),
        iter::from_fn(|| w.pop()).collect::<Vec<_>>()
    );
}

fn stealers() {
    // Stealers take from the end opposite to where tasks are pushed.
    let w = Worker::new_fifo();
    let s = w.stealer();
    w.push(1);
    w.push(2);
    w.push(3);
    assert_eq!(s.len(), 3);
    assert_eq!(s.steal(), Steal::Success(1));
    assert_eq!(w.pop(), Some(2));
    assert_eq!(w.pop(), Some(3));
    assert!(s.is_empty());
    assert_eq!(s.steal(), Steal::Empty);

    let w = Worker::new_lifo();
    let s = w.stealer();
    w.push(1);
    w.push(2);
    w.push(3);
    let t = s.clone();
    assert_eq!(t.steal(), Steal::Success(1));
    assert_eq!(w.pop(), Some(3));
    assert_eq!(s.steal(), Steal::Success(2));
    assert_eq!(s.steal(), Steal::Empty);
}

/// crossbeam-deque's documented examples of batch steals, which pin down how many tasks it takes.
fn batches() {
    let w1 = Worker::new_fifo();
    for i in 1..=4 {
        w1.push(i);
    }
    let s = w1.stealer();
    let w2 = Worker::new_fifo();
    assert_eq!(s.steal_batch(&w2), Steal::Success(()));
    assert_eq!(w2.pop(), Some(1));
    assert_eq!(w2.pop(), Some(2));
    assert_eq!(w2.pop(), None);

    let w1 = Worker::new_fifo();
    for i in 1..=6 {
        w1.push(i);
    }
    let s = w1.stealer();
    let w2 = Worker::new_fifo();
    assert_eq!(s.steal_batch_with_limit(&w2, 2), Steal::Success(()));
    assert_eq!(w2.pop(), Some(1));
    assert_eq!(w2.pop(), Some(2));
    assert_eq!(w2.pop(), None);
    w1.push(7);
    w1.push(8);
    assert_eq!(s.steal_batch_with_limit(&w2, usize::MAX), Steal::Success(()));
    assert_eq!(w2.len(), 3);

    let w1 = Worker::new_fifo();
    for i in 1..=4 {
        w1.push(i);
    }
    let s = w1.stealer();
    let w2 = Worker::new_fifo();
    assert_eq!(s.steal_batch_and_pop(&w2), Steal::Success(1));
    assert_eq!(w2.pop(), Some(2));
    assert_eq!(w2.pop(), None);

    let w1 = Worker::new_fifo();
    for i in 1..=6 {
        w1.push(i);
    }
    let s = w1.stealer();
    let w2 = Worker::new_fifo();
    assert_eq!(s.steal_batch_with_limit_and_pop(&w2, 2), Steal::Success(1));
    assert_eq!(w2.pop(), Some(2));
    assert_eq!(w2.pop(), None);
    w1.push(7);
    w1.push(8);
    assert_eq!(s.steal_batch_with_limit_and_pop(&w2, usize::MAX), Steal::Success(3));
    assert_eq!(w2.pop(), Some(4));
    assert_eq!(w2.pop(), Some(5));
    assert_eq!(w2.pop(), None);

    // A batch steal into the stealer's own worker steals nothing.
    let w = Worker::new_fifo();
    let s = w.stealer();
    assert_eq!(s.steal_batch(&w), Steal::Empty);
    w.push(1);
    assert_eq!(s.steal_batch(&w), Steal::Success(()));
    assert_eq!(s.steal_batch_and_pop(&w), Steal::Success(1));
    assert_eq!(s.steal_batch_and_pop(&w), Steal::Empty);
    assert_eq!(s.steal_batch(&Worker::new_fifo()), Steal::Empty);
}

/// The order in which a batch lands in the destination depends on both queues' flavors.
fn batches_between_flavors() {
    fn batch_order(source: Worker<u32>, dest: Worker<u32>) -> (Vec<u32>, Option<u32>) {
        for i in 1..=6 {
            source.push(i);
        }
        let s = source.stealer();
        assert_eq!(s.steal_batch(&dest), Steal::Success(()));
        let landed = iter::from_fn(|| dest.pop()).collect();
        let popped = s.steal_batch_and_pop(&dest).success();
        (landed, popped)
    }
    assert_eq!(
        batch_order(Worker::new_fifo(), Worker::new_fifo()),
        (vec![1, 2, 3], Some(4))
    );
    assert_eq!(
        batch_order(Worker::new_fifo(), Worker::new_lifo()),
        (vec![1, 2, 3], Some(4))
    );
    // From a LIFO source, `steal_batch_and_pop` returns the newest task it took, here the second.
    assert_eq!(
        batch_order(Worker::new_lifo(), Worker::new_lifo()),
        (vec![3, 2, 1], Some(5))
    );
    assert_eq!(
        batch_order(Worker::new_lifo(), Worker::new_fifo()),
        (vec![3, 2, 1], Some(5))
    );

    // A LIFO source's batch-and-pop keeps the newest task it took.
    let source = Worker::new_lifo();
    for i in 1..=8 {
        source.push(i);
    }
    let dest = Worker::new_fifo();
    assert_eq!(source.stealer().steal_batch_and_pop(&dest), Steal::Success(4));
    assert_eq!(iter::from_fn(|| dest.pop()).collect::<Vec<_>>(), vec![3, 2, 1]);
}

fn injector() {
    let q = Injector::new();
    assert!(q.is_empty());
    q.push(1);
    q.push(2);
    assert_eq!(q.len(), 2);
    assert_eq!(q.steal(), Steal::Success(1));
    assert_eq!(q.steal(), Steal::Success(2));
    assert_eq!(q.steal(), Steal::Empty);

    let q: Injector<usize> = Injector::default();
    for i in 0..200 {
        q.push(i);
    }
    assert_eq!(q.len(), 200);
    assert_eq!(
        iter::from_fn(|| q.steal().success()).collect::<Vec<_>>(),
        (0..200).collect::<Vec<_>>()
    );
}

/// crossbeam-deque's documented examples of the injector's batch steals, and its blocks of 63.
fn injector_batches() {
    let q = Injector::new();
    for i in 1..=4 {
        q.push(i);
    }
    let w = Worker::new_fifo();
    assert_eq!(q.steal_batch(&w), Steal::Success(()));
    assert_eq!(w.pop(), Some(1));
    assert_eq!(w.pop(), Some(2));

    let q = Injector::new();
    for i in 1..=6 {
        q.push(i);
    }
    let w = Worker::new_fifo();
    assert_eq!(q.steal_batch_with_limit(&w, 2), Steal::Success(()));
    assert_eq!(w.pop(), Some(1));
    assert_eq!(w.pop(), Some(2));
    assert_eq!(w.pop(), None);
    q.push(7);
    q.push(8);
    assert_eq!(q.steal_batch_with_limit(&w, usize::MAX), Steal::Success(()));
    assert_eq!(w.len(), 3);

    let q = Injector::new();
    for i in 1..=4 {
        q.push(i);
    }
    let w = Worker::new_fifo();
    assert_eq!(q.steal_batch_and_pop(&w), Steal::Success(1));
    assert_eq!(w.pop(), Some(2));

    let q = Injector::new();
    for i in 1..=6 {
        q.push(i);
    }
    let w = Worker::new_fifo();
    assert_eq!(q.steal_batch_with_limit_and_pop(&w, 2), Steal::Success(1));
    assert_eq!(w.pop(), Some(2));
    assert_eq!(w.pop(), None);
    q.push(7);
    assert_eq!(q.steal_batch_with_limit_and_pop(&w, usize::MAX), Steal::Success(3));
    assert_eq!(w.pop(), Some(4));
    assert_eq!(w.pop(), Some(5));
    assert_eq!(w.pop(), None);

    // With the tail in a later block, a batch takes the rest of the head's block of 63.
    let q = Injector::new();
    for i in 0..100 {
        q.push(i);
    }
    for _ in 0..60 {
        let _ = q.steal();
    }
    let w = Worker::new_lifo();
    assert_eq!(q.steal_batch_with_limit(&w, usize::MAX), Steal::Success(()));
    assert_eq!(iter::from_fn(|| w.pop()).collect::<Vec<_>>(), vec![60, 61, 62]);
    assert_eq!(q.len(), 37);
}

fn steal_results() {
    use Steal::{Empty, Retry, Success};
    let collect = |v: Vec<Steal<i32>>| v.into_iter().collect::<Steal<i32>>();
    assert_eq!(collect(vec![Empty, Retry, Empty]), Retry);
    assert_eq!(collect(vec![Retry, Success(1), Empty]), Success(1));
    assert_eq!(Empty.or_else(|| Retry), Retry::<i32>);
    assert!(Success(7).is_success() && Retry::<i32>.is_retry() && Empty::<i32>.is_empty());
}

fn dropping() {
    let token = Arc::new(());
    {
        let w = Worker::new_fifo();
        w.push(Arc::clone(&token));
        let s = w.stealer();
        drop(w);
        // The stealer keeps the queue alive.
        assert_eq!(Arc::strong_count(&token), 2);
        let q = Injector::new();
        q.push(Arc::clone(&token));
        assert_eq!(Arc::strong_count(&token), 3);
        drop(s);
    }
    assert_eq!(Arc::strong_count(&token), 1);
}

fn formatting() {
    let w = Worker::<u8>::new_lifo();
    assert_eq!(format!("{w:?}"), "Worker { .. }");
    assert_eq!(format!("{:?}", w.stealer()), "Stealer { .. }");
    assert_eq!(format!("{:?}", Injector::<u8>::new()), "Injector { .. }");
    assert_eq!(format!("{:?}", Steal::Success(1)), "Success(..)");
}

/// crossbeam-deque's own example of finding a task, in a scheduler with a global injector and two
/// workers: every task runs exactly once, whatever the interleaving, and however many steals retry.
fn find_task<T>(local: &Worker<T>, global: &Injector<T>, stealers: &[Stealer<T>]) -> Option<T> {
    local.pop().or_else(|| {
        iter::repeat_with(|| {
            global
                .steal_batch_and_pop(local)
                .or_else(|| stealers.iter().map(|s| s.steal()).collect())
        })
        .find(|s| !s.is_retry())
        .and_then(|s| s.success())
    })
}

fn scheduler() {
    const TASKS: usize = 6;
    let global = Arc::new(Injector::new());
    for task in 0..TASKS {
        global.push(task);
    }
    let workers: Vec<Worker<usize>> = (0..2).map(|_| Worker::new_lifo()).collect();
    let stealers: Arc<Vec<Stealer<usize>>> = Arc::new(workers.iter().map(Worker::stealer).collect());
    let ran = Arc::new(AtomicUsize::new(0));
    let threads: Vec<_> = workers
        .into_iter()
        .map(|local| {
            let global = Arc::clone(&global);
            let stealers = Arc::clone(&stealers);
            let ran = Arc::clone(&ran);
            crate::thread::spawn(move || {
                let mut mine = Vec::new();
                while ran.load(Ordering::SeqCst) < TASKS {
                    match find_task(&local, &global, &stealers) {
                        Some(task) => {
                            mine.push(task);
                            ran.fetch_add(1, Ordering::SeqCst);
                            // Half of the tasks spawn no more work; each one runs once.
                        }
                        None => crate::thread::yield_now(),
                    }
                }
                mine
            })
        })
        .collect();
    let mut all: Vec<usize> = threads.into_iter().flat_map(|t| t.join().unwrap()).collect();
    all.sort_unstable();
    assert_eq!(all, (0..TASKS).collect::<Vec<_>>());
}
