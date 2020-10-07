use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use shuttle::scheduler::PCTScheduler;
use shuttle::sync::Mutex;
use shuttle::{thread, Runner};
use std::sync::Arc;

/// A simple benchmark that just runs 3 threads incrementing a lock a bunch of times. This is a
/// stress test of our `Execution` logic, since the threads spend basically all their time taking
/// and dropping the lock.
pub fn pct_throughput_benchmark(c: &mut Criterion) {
    const ITERATIONS: usize = 1000;
    const INNER_ITERATIONS: usize = 200;

    let mut g = c.benchmark_group("throughput");
    g.throughput(Throughput::Elements(ITERATIONS as u64));

    g.bench_function("pct", |b| {
        b.iter(|| {
            let scheduler = PCTScheduler::new_from_seed(0x12345678, 2, ITERATIONS);
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
        });
    });
}

criterion_group!(benches, pct_throughput_benchmark);
criterion_main!(benches);
