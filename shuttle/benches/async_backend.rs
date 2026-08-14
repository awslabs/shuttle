//! Compares the two task backends on identical async workloads.
//!
//! Every benchmark here runs the *same* async test body twice, changing only
//! `Config::backend`, so the difference is purely the cost of suspending and resuming a task:
//! switching stacks (`Stackful`) versus returning `Poll::Pending` and being polled again
//! (`Futures`).

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use shuttle::future::batch_semaphore::BatchSemaphore;
use shuttle::future::{self, yield_now};
use shuttle::scheduler::RandomScheduler;
use shuttle::{Config, Runner, TaskBackend};
use shuttle_engine::future::batch_semaphore::Fairness;
use std::sync::Arc;
use std::time::Duration;

const ITERATIONS: usize = 1;

fn config(backend: TaskBackend) -> Config {
    Config::new().with_backend(backend)
}

/// Tasks that only ever yield. Isolates the cost of a context switch with no synchronization.
fn yield_workload(backend: TaskBackend, num_tasks: u32, yields_per_task: u32) {
    let runner = Runner::new(RandomScheduler::new_from_seed(0x12345678, ITERATIONS), config(backend));
    runner.run_async(move || async move {
        let handles = (0..num_tasks)
            .map(|_| {
                future::spawn(async move {
                    for _ in 0..yields_per_task {
                        yield_now().await;
                    }
                })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            handle.await.unwrap();
        }
    });
}

/// Tasks contending on an async mutex. Exercises blocking and unblocking as well as switching.
fn semaphore_workload(backend: TaskBackend, num_tasks: u32, acquires_per_task: u32) {
    let runner = Runner::new(RandomScheduler::new_from_seed(0x12345678, ITERATIONS), config(backend));
    runner.run_async(move || async move {
        let semaphore = Arc::new(BatchSemaphore::new(1, Fairness::Unfair));
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let handles = (0..num_tasks)
            .map(|_| {
                let semaphore = semaphore.clone();
                let counter = counter.clone();
                future::spawn(async move {
                    for _ in 0..acquires_per_task {
                        semaphore.acquire(1).await.unwrap();
                        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        semaphore.release(1);
                    }
                })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            handle.await.unwrap();
        }
    });
}

/// Spawning and immediately finishing tasks. Isolates task creation, which on the stackful backend
/// means acquiring a stack from the continuation pool.
fn spawn_workload(backend: TaskBackend, num_tasks: u32) {
    let runner = Runner::new(RandomScheduler::new_from_seed(0x12345678, ITERATIONS), config(backend));
    runner.run_async(move || async move {
        let handles = (0..num_tasks).map(|i| future::spawn(async move { i })).collect::<Vec<_>>();

        for handle in handles {
            handle.await.unwrap();
        }
    });
}

const BACKENDS: [(&str, TaskBackend); 2] = [("stackful", TaskBackend::Stackful), ("futures", TaskBackend::Futures)];

pub fn yield_benchmark(c: &mut Criterion) {
    let mut g = c.benchmark_group("async yield");
    g.warm_up_time(Duration::from_millis(500)).sample_size(30);

    for (num_tasks, total_yields) in [(4u32, 4000u32), (32, 8000), (256, 8192)] {
        for (name, backend) in BACKENDS {
            let params = format!("tasks:{num_tasks},yields:{total_yields}");
            g.bench_with_input(BenchmarkId::new(name, params), &backend, |b, backend| {
                b.iter(|| yield_workload(*backend, num_tasks, total_yields / num_tasks))
            });
        }
    }
}

pub fn semaphore_benchmark(c: &mut Criterion) {
    let mut g = c.benchmark_group("async semaphore");
    g.warm_up_time(Duration::from_millis(500)).sample_size(30);

    for (num_tasks, total_acquires) in [(4u32, 2000u32), (32, 4000), (256, 4096)] {
        for (name, backend) in BACKENDS {
            let params = format!("tasks:{num_tasks},acquires:{total_acquires}");
            g.bench_with_input(BenchmarkId::new(name, params), &backend, |b, backend| {
                b.iter(|| semaphore_workload(*backend, num_tasks, total_acquires / num_tasks))
            });
        }
    }
}

pub fn spawn_benchmark(c: &mut Criterion) {
    let mut g = c.benchmark_group("async spawn");
    g.warm_up_time(Duration::from_millis(500)).sample_size(30);

    for num_tasks in [64u32, 1024, 8192] {
        for (name, backend) in BACKENDS {
            let params = format!("tasks:{num_tasks}");
            g.bench_with_input(BenchmarkId::new(name, params), &backend, |b, backend| {
                b.iter(|| spawn_workload(*backend, num_tasks))
            });
        }
    }
}

criterion_group!(benches, yield_benchmark, semaphore_benchmark, spawn_benchmark);
criterion_main!(benches);
