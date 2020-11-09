use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use shuttle::scheduler::{PCTScheduler, RandomScheduler, Scheduler};
use shuttle::sync::{Condvar, Mutex};
use shuttle::{thread, Runner};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const ITERATIONS: usize = 1000;
const NUM_PRODUCERS: usize = 3;
const NUM_CONSUMERS: usize = 3;
const NUM_EVENTS: usize = NUM_PRODUCERS * NUM_CONSUMERS * 3;
const MAX_QUEUE_SIZE: usize = 3;

/// An implementation of a bounded concurrent queue, minus the actual queue part. Producers wait
/// until there's space in the queue, and then put their object in. Consumers wait until the queue
/// is non-empty, and then consume something from the queue.
fn bounded_buffer_check(scheduler: impl Scheduler + 'static) {
    let runner = Runner::new(scheduler);

    runner.run(move || {
        let lock = Arc::new(Mutex::new(()));
        let has_space = Arc::new(Condvar::new()); // count < MAX_QUEUE_SIZE
        let has_elements = Arc::new(Condvar::new()); // count > 0
        let count = Arc::new(AtomicUsize::new(0));

        let consumers = (0..NUM_CONSUMERS)
            .map(|_| {
                let lock = Arc::clone(&lock);
                let has_space = Arc::clone(&has_space);
                let has_elements = Arc::clone(&has_elements);
                let count = Arc::clone(&count);
                thread::spawn(move || {
                    let events = NUM_EVENTS / NUM_CONSUMERS;
                    for _ in 0..events {
                        let mut guard = lock.lock().unwrap();
                        while count.load(Ordering::SeqCst) == 0 {
                            guard = has_elements.wait(guard).unwrap();
                        }
                        // get()
                        count.fetch_sub(1, Ordering::SeqCst);
                        has_space.notify_one();
                        drop(guard);
                    }
                })
            })
            .collect::<Vec<_>>();

        let producers = (0..NUM_PRODUCERS)
            .map(|_| {
                let lock = Arc::clone(&lock);
                let has_space = Arc::clone(&has_space);
                let has_elements = Arc::clone(&has_elements);
                let count = Arc::clone(&count);
                thread::spawn(move || {
                    let events = NUM_EVENTS / NUM_PRODUCERS;
                    for _ in 0..events {
                        let mut guard = lock.lock().unwrap();
                        while count.load(Ordering::SeqCst) == MAX_QUEUE_SIZE {
                            guard = has_space.wait(guard).unwrap();
                        }
                        // put()
                        count.fetch_add(1, Ordering::SeqCst);
                        has_elements.notify_one();
                        drop(guard);
                    }
                })
            })
            .collect::<Vec<_>>();

        for consumer in consumers {
            consumer.join().unwrap();
        }
        for producer in producers {
            producer.join().unwrap();
        }
    });
}

pub fn bounded_buffer_benchmark(c: &mut Criterion) {
    let mut g = c.benchmark_group("buffer");
    g.throughput(Throughput::Elements(ITERATIONS as u64));

    g.bench_function("pct", |b| {
        b.iter(|| {
            let scheduler = PCTScheduler::new_from_seed(0x12345678, 2, ITERATIONS);
            bounded_buffer_check(scheduler);
        });
    });

    g.bench_function("random", |b| {
        b.iter(|| {
            let scheduler = RandomScheduler::new_from_seed(0x12345678, ITERATIONS);
            bounded_buffer_check(scheduler);
        });
    });
}

criterion_group!(benches, bounded_buffer_benchmark);
criterion_main!(benches);
