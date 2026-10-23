use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use shuttle::scheduler::{PctScheduler, RandomScheduler, RoundRobinScheduler, Scheduler};
use shuttle::sync::atomic::{AtomicUsize, Ordering};
use shuttle::{future, thread, Runner};
use std::sync::Arc;
use std::time::{Duration, Instant};

const NARROW_TASKS: u32 = 5;
const WIDE_TASKS: u32 = 100;
const ITERATIONS: usize = 100;

/// This benchmark creates a number of short (single event) tasks.
fn counter_async(scheduler: impl Scheduler + 'static, num_tasks: u32) {
    let runner = Runner::new(scheduler, Default::default());
    runner.run(move || {
        let counter = Arc::new(AtomicUsize::new(0usize));

        let tasks: Vec<_> = (0..num_tasks)
            .map(|_| {
                let counter = Arc::clone(&counter);
                future::spawn(async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                })
            })
            .collect();

        future::block_on(async move {
            for t in tasks {
                t.await.unwrap();
            }
        });
    });
}

/// A toy benchmark that runs a bunch of threads that just increment a counter
fn counter_sync(scheduler: impl Scheduler + 'static, num_tasks: u32) {
    let runner = Runner::new(scheduler, Default::default());
    runner.run(move || {
        let counter = Arc::new(AtomicUsize::new(0usize));

        let tasks: Vec<_> = (0..num_tasks)
            .map(|_| {
                let counter = Arc::clone(&counter);
                thread::spawn(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                })
            })
            .collect();

        for t in tasks {
            t.join().unwrap();
        }
    });
}

pub fn create_async_benchmark(c: &mut Criterion) {
    let mut g = c.benchmark_group("create async");

    g.bench_function("pct-narrow", |b| {
        b.iter(|| {
            let scheduler = PctScheduler::new_from_seed(0x12345678, 2, ITERATIONS);
            counter_async(scheduler, NARROW_TASKS);
        });
    });

    g.bench_function("random-narrow", |b| {
        b.iter(|| {
            let scheduler = RandomScheduler::new_from_seed(0x12345678, ITERATIONS);
            counter_async(scheduler, NARROW_TASKS);
        });
    });

    g.bench_function("pct-wide", |b| {
        b.iter(|| {
            let scheduler = PctScheduler::new_from_seed(0x12345678, 2, ITERATIONS);
            counter_async(scheduler, WIDE_TASKS);
        });
    });

    g.bench_function("random-wide", |b| {
        b.iter(|| {
            let scheduler = RandomScheduler::new_from_seed(0x12345678, ITERATIONS);
            counter_async(scheduler, WIDE_TASKS);
        });
    });
}

pub fn create_sync_benchmark(c: &mut Criterion) {
    let mut g = c.benchmark_group("create sync");

    g.bench_function("pct-narrow", |b| {
        b.iter(|| {
            let scheduler = PctScheduler::new_from_seed(0x12345678, 2, ITERATIONS);
            counter_sync(scheduler, NARROW_TASKS);
        });
    });

    g.bench_function("random-narrow", |b| {
        b.iter(|| {
            let scheduler = RandomScheduler::new_from_seed(0x12345678, ITERATIONS);
            counter_sync(scheduler, NARROW_TASKS);
        });
    });

    g.bench_function("pct-wide", |b| {
        b.iter(|| {
            let scheduler = PctScheduler::new_from_seed(0x12345678, 2, ITERATIONS);
            counter_sync(scheduler, WIDE_TASKS);
        });
    });

    g.bench_function("random-wide", |b| {
        b.iter(|| {
            let scheduler = RandomScheduler::new_from_seed(0x12345678, ITERATIONS);
            counter_sync(scheduler, WIDE_TASKS);
        });
    });
}

/// Measure scheduling after short-lived tasks have finished. Creating those tasks is setup,
/// so only the yields by the remaining task contribute to the reported duration.
pub fn yield_after_finished_tasks_benchmark(c: &mut Criterion) {
    const YIELDS: u64 = 100_000;
    let mut g = c.benchmark_group("yield after finished tasks");
    g.throughput(Throughput::Elements(YIELDS));
    for finished in [0, 1_000, 4_000] {
        g.bench_with_input(BenchmarkId::from_parameter(finished), &finished, |b, &finished| {
            b.iter_custom(|iterations| {
                let elapsed = Arc::new(std::sync::Mutex::new(Duration::ZERO));
                for _ in 0..iterations {
                    let elapsed = elapsed.clone();
                    Runner::new(RoundRobinScheduler::new(1), Default::default()).run(move || {
                        for _ in 0..finished {
                            thread::spawn(|| {}).join().unwrap();
                        }
                        let start = Instant::now();
                        for _ in 0..YIELDS {
                            thread::yield_now();
                        }
                        let duration = start.elapsed();
                        *elapsed.lock().unwrap() += duration;
                    });
                }
                let duration = *elapsed.lock().unwrap();
                duration
            });
        });
    }
}

criterion_group!(
    benches,
    create_async_benchmark,
    create_sync_benchmark,
    yield_after_finished_tasks_benchmark
);
criterion_main!(benches);
