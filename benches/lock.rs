use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use shuttle::scheduler::{PCTScheduler, RandomScheduler, Scheduler};
use shuttle::sync::Mutex;
use shuttle::{thread, Runner};
use std::sync::Arc;

/// A simple benchmark that just runs 3 threads incrementing a lock a bunch of times. This is a
/// stress test of our `Execution` logic, since the threads spend basically all their time taking
/// and dropping the lock.
fn basic_lock_check(scheduler: impl Scheduler + 'static) {
    const INNER_ITERATIONS: usize = 200;

    let runner = Runner::new(scheduler);
    runner.run(|| {
        let lock = Arc::new(Mutex::new(0usize));

        let thds: Vec<_> = (0..3)
            .map(|_| {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    for _ in 0..INNER_ITERATIONS {
                        *lock.lock().unwrap() += 1;
                    }
                })
            })
            .collect();

        for thd in thds {
            thd.join().unwrap();
        }
    });
}

pub fn basic_lock_benchmark(c: &mut Criterion) {
    const ITERATIONS: usize = 1000;

    let mut g = c.benchmark_group("lock");
    g.throughput(Throughput::Elements(ITERATIONS as u64));

    g.bench_function("pct", |b| {
        b.iter(|| {
            let scheduler = PCTScheduler::new_from_seed(0x12345678, 2, ITERATIONS);
            basic_lock_check(scheduler);
        });
    });

    g.bench_function("random", |b| {
        b.iter(|| {
            let scheduler = RandomScheduler::new_from_seed(0x12345678, ITERATIONS);
            basic_lock_check(scheduler);
        });
    });
}

criterion_group!(benches, basic_lock_benchmark);
criterion_main!(benches);
