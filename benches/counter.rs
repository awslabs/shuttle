use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use shuttle::scheduler::{PctScheduler, RandomScheduler, Scheduler};
use shuttle::{asynch, thread, Runner};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const NUM_TASKS: usize = 10;
const ITERATIONS: usize = 100;

/// A toy benchmark that runs a bunch of tasks that just increment a counter
fn counter_async(scheduler: impl Scheduler + 'static) {
    let runner = Runner::new(scheduler, Default::default());
    runner.run(|| {
        let counter = Arc::new(AtomicUsize::new(0usize));

        let tasks: Vec<_> = (0..NUM_TASKS)
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
    });
}

/// A toy benchmark that runs a bunch of threads that just increment a counter
fn counter_sync(scheduler: impl Scheduler + 'static) {
    let runner = Runner::new(scheduler, Default::default());
    runner.run(|| {
        let counter = Arc::new(AtomicUsize::new(0usize));

        let tasks: Vec<_> = (0..NUM_TASKS)
            .map(|_| {
                let counter = Arc::clone(&counter);
                thread::spawn(move || {
                    let c = counter.load(Ordering::SeqCst);
                    thread::yield_now();
                    counter.fetch_add(c, Ordering::SeqCst);
                })
            })
            .collect();

        for t in tasks {
            t.join().unwrap();
        }
    });
}

pub fn counter_async_benchmark(c: &mut Criterion) {
    let mut g = c.benchmark_group("counter async");
    g.throughput(Throughput::Elements(ITERATIONS as u64));

    g.bench_function("pct", |b| {
        b.iter(|| {
            let scheduler = PctScheduler::new_from_seed(0x12345678, 2, ITERATIONS);
            counter_async(scheduler);
        });
    });

    g.bench_function("random", |b| {
        b.iter(|| {
            let scheduler = RandomScheduler::new_from_seed(0x12345678, ITERATIONS);
            counter_async(scheduler);
        });
    });
}

pub fn counter_sync_benchmark(c: &mut Criterion) {
    let mut g = c.benchmark_group("counter sync");
    g.throughput(Throughput::Elements(ITERATIONS as u64));

    g.bench_function("pct", |b| {
        b.iter(|| {
            let scheduler = PctScheduler::new_from_seed(0x12345678, 2, ITERATIONS);
            counter_sync(scheduler);
        });
    });

    g.bench_function("random", |b| {
        b.iter(|| {
            let scheduler = RandomScheduler::new_from_seed(0x12345678, ITERATIONS);
            counter_sync(scheduler);
        });
    });
}

criterion_group!(benches, counter_async_benchmark, counter_sync_benchmark);
criterion_main!(benches);
