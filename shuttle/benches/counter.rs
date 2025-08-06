use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, SamplingMode, Throughput};
use shuttle::scheduler::{PctScheduler, RandomScheduler, Scheduler};
use shuttle::sync::atomic::{AtomicUsize, Ordering};
use shuttle::{future, thread, Runner};
use std::sync::Arc;
use std::time::Duration;

const TOTAL_EVENTS: u32 = 10000;
const NARROW_TASKS: u32 = 5;
const WIDE_TASKS: u32 = 100;
const ITERATIONS: usize = 1;
const WIDE_EVENTS_PER_TASK: u32 = TOTAL_EVENTS / WIDE_TASKS;
const NARROW_EVENTS_PER_TASK: u32 = TOTAL_EVENTS / NARROW_TASKS;

/// A toy benchmark that runs a bunch of tasks that just increment a counter
fn counter_async(scheduler: impl Scheduler + 'static, num_tasks: u32, num_events_per_task: u32) {
    let runner = Runner::new(scheduler, Default::default());
    runner.run(move || {
        let counter = Arc::new(AtomicUsize::new(0usize));

        let tasks: Vec<_> = (0..num_tasks)
            .map(|_| {
                let counter = Arc::clone(&counter);
                future::spawn(async move {
                    for _ in 0..num_events_per_task {
                        counter.fetch_add(1, Ordering::SeqCst);
                    }
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
fn counter_sync(scheduler: impl Scheduler + 'static, num_tasks: u32, num_events_per_task: u32) {
    let runner = Runner::new(scheduler, Default::default());
    runner.run(move || {
        let counter = Arc::new(AtomicUsize::new(0usize));

        let tasks: Vec<_> = (0..num_tasks)
            .map(|_| {
                let counter = Arc::clone(&counter);
                thread::spawn(move || {
                    for _ in 0..num_events_per_task {
                        counter.fetch_add(1, Ordering::SeqCst);
                    }
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
    g.throughput(Throughput::Elements((ITERATIONS * TOTAL_EVENTS as usize) as u64));
    g.warm_up_time(Duration::from_secs(1));

    g.bench_function("pct-narrow", |b| {
        b.iter(|| {
            let scheduler = PctScheduler::new_from_seed(0x12345678, 2, ITERATIONS);
            counter_async(scheduler, NARROW_TASKS, NARROW_EVENTS_PER_TASK);
        });
    });

    g.bench_function("random-narrow", |b| {
        b.iter(|| {
            let scheduler = RandomScheduler::new_from_seed(0x12345678, ITERATIONS);
            counter_async(scheduler, NARROW_TASKS, NARROW_EVENTS_PER_TASK);
        });
    });

    g.bench_function("pct-wide", |b| {
        b.iter(|| {
            let scheduler = PctScheduler::new_from_seed(0x12345678, 2, ITERATIONS);
            counter_async(scheduler, WIDE_TASKS, WIDE_EVENTS_PER_TASK);
        });
    });

    g.bench_function("random-wide", |b| {
        b.iter(|| {
            let scheduler = RandomScheduler::new_from_seed(0x12345678, ITERATIONS);
            counter_async(scheduler, WIDE_TASKS, WIDE_EVENTS_PER_TASK);
        });
    });
}

pub fn counter_sync_benchmark(c: &mut Criterion) {
    let mut g = c.benchmark_group("counter sync");
    g.throughput(Throughput::Elements((ITERATIONS * TOTAL_EVENTS as usize) as u64));
    g.warm_up_time(Duration::from_secs(1));

    g.bench_function("pct-narrow", |b| {
        b.iter(|| {
            let scheduler = PctScheduler::new_from_seed(0x12345678, 2, ITERATIONS);
            counter_sync(scheduler, NARROW_TASKS, NARROW_EVENTS_PER_TASK);
        });
    });

    g.bench_function("random-narrow", |b| {
        b.iter(|| {
            let scheduler = RandomScheduler::new_from_seed(0x12345678, ITERATIONS);
            counter_sync(scheduler, NARROW_TASKS, NARROW_EVENTS_PER_TASK);
        });
    });

    g.bench_function("pct-wide", |b| {
        b.iter(|| {
            let scheduler = PctScheduler::new_from_seed(0x12345678, 2, ITERATIONS);
            counter_sync(scheduler, WIDE_TASKS, WIDE_EVENTS_PER_TASK);
        });
    });

    g.bench_function("random-wide", |b| {
        b.iter(|| {
            let scheduler = RandomScheduler::new_from_seed(0x12345678, ITERATIONS);
            counter_sync(scheduler, WIDE_TASKS, WIDE_EVENTS_PER_TASK);
        });
    });
}

const SCALING_TOTAL_EVENTS: [u32; 3] = [1000, 10000, 100000];
const SCALING_TASKS: [u32; 6] = [4, 16, 32, 64, 128, 1024];

pub fn counter_scaling_async_benchmark(c: &mut Criterion) {
    let mut g = c.benchmark_group("counter scaling async");
    g.sample_size(20)
        .warm_up_time(Duration::from_millis(100))
        .sampling_mode(SamplingMode::Flat);

    for num_tasks in SCALING_TASKS {
        for num_total_events in SCALING_TOTAL_EVENTS {
            // skip configs without meaningful number of events per thread
            if num_tasks * 10 >= num_total_events {
                continue;
            }
            let num_events_per_task = num_total_events / num_tasks;
            let input = (num_tasks, num_events_per_task);
            let parameter_string = format!("{{tasks:{num_tasks},events:{num_total_events}}}");
            g.bench_with_input(
                BenchmarkId::new("RW", parameter_string),
                &input,
                |b, (num_tasks, num_events_per_task)| {
                    b.iter(|| {
                        let scheduler = RandomScheduler::new_from_seed(0x12345678, ITERATIONS);
                        counter_async(scheduler, *num_tasks, *num_events_per_task);
                    })
                },
            );
        }
    }
}

criterion_group!(
    benches,
    counter_async_benchmark,
    counter_sync_benchmark,
    counter_scaling_async_benchmark
);
criterion_main!(benches);
