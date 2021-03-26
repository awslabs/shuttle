// For symmetry we clone some `Arc`s even though we could just move them
#![allow(clippy::redundant_clone)]

use rand::Rng;
use shuttle::rand::thread_rng;
use shuttle::sync::{Condvar, Mutex};
use shuttle::{check_random, replay, thread};
use std::sync::Arc;
use test_env_log::test;

/// This file implements the example from a blog post about Coyote (P#):
///   https://cloudblogs.microsoft.com/opensource/2020/07/14/extreme-programming-meets-systematic-testing-using-coyote/
/// The comments in this file are quotes from that blog post.

/// Let’s walk through how Coyote can easily solve the programming problem posed by Tom Cargill. He
/// shared a BoundedBuffer implementation written in Java with a known, but tricky, deadlock bug.
///
/// The BoundedBuffer implements a buffer of fixed length with concurrent writers adding items to
/// the buffer and readers consuming items from the buffer. The readers wait if there are no items
/// in the buffer and writers wait if the buffer is full, resuming only once a slot has been
/// consumed by a reader. This is also known as a producer/consumer queue.
///
/// The concrete ask was for the community to find a particular bug that Cargill knew about in the
/// above program. The meta-ask was to come up with a methodology for catching such bugs rapidly.
// Unlike in the C# and Java code, Rust doesn't have monitors. But a monitor is just a Mutex and a
// Condvar anyway, so we implement it that way instead. Some of this code is not idiomatic Rust,
// but is written this way to match the C# code more closely.
#[derive(Clone)]
struct BoundedBuffer<T: Copy> {
    inner: Arc<Mutex<Inner<T>>>,
    cond: Arc<Condvar>,
}

struct Inner<T: Copy> {
    buffer: Box<[T]>,
    buffer_size: usize,
    put_at: usize,
    take_at: usize,
    occupied: usize,
}

impl<T: Copy + Default> BoundedBuffer<T> {
    fn new(buffer_size: usize) -> Self {
        let inner = Inner {
            buffer: vec![T::default(); buffer_size].into_boxed_slice(),
            buffer_size,
            put_at: 0,
            take_at: 0,
            occupied: 0,
        };

        BoundedBuffer {
            inner: Arc::new(Mutex::new(inner)),
            cond: Arc::new(Condvar::new()),
        }
    }

    fn put(&self, x: T) {
        let mut this = self.inner.lock().unwrap();
        while this.occupied == this.buffer_size {
            this = self.cond.wait(this).unwrap();
        }

        this.occupied += 1;
        this.put_at %= this.buffer_size;
        let put_at = this.put_at;
        this.buffer[put_at] = x;
        this.put_at += 1;

        self.cond.notify_one();
    }

    fn take(&self) -> T {
        let mut this = self.inner.lock().unwrap();
        while this.occupied == 0 {
            this = self.cond.wait(this).unwrap();
        }

        this.occupied -= 1;
        this.take_at %= this.buffer_size;
        let result = this.buffer[this.take_at];
        this.take_at += 1;

        self.cond.notify_one();

        result
    }
}

fn reader(buffer: BoundedBuffer<usize>, iterations: usize) {
    for _ in 0..iterations {
        let _ = buffer.take();
    }
}

fn writer(buffer: BoundedBuffer<usize>, iterations: usize) {
    for i in 0..iterations {
        buffer.put(i);
    }
}

/// We’ll now write a small test driver program, which Coyote can use to find the bug.
///
/// The first test you write might look like this. Here we setup two tasks. First is a reader
/// calling Take and the other is a Writer calling Put.
///
/// Clearly, we have to Put the same number of items as we Take.
///
/// Otherwise, there will be a trivial deadlock waiting for more items.
///
/// We have matched both in this test with 10 iterations of each Put and Take. We find no deadlock
/// when we run the test above, despite Coyote systematically exploring different possible
/// interleavings between the Put and Take calls.
#[test]
fn test_bounded_buffer_trivial() {
    check_random(
        || {
            let buffer = BoundedBuffer::new(1);

            let reader = {
                let buffer = buffer.clone();
                thread::spawn(move || reader(buffer, 10))
            };

            let writer = {
                let buffer = buffer.clone();
                thread::spawn(move || writer(buffer, 10))
            };

            reader.join().unwrap();
            writer.join().unwrap();
        },
        1000,
    )
}

/// The bug might only trigger in certain configurations, but not in all configurations. Can we use
/// Coyote to explore the state space of the configurations? Luckily, we can.
///
/// We can generate a random number of readers, writers, buffer length, and iterations, letting
/// Coyote explore these configurations. Coyote will also explore the Task interleavings in each
/// configuration. The following slightly more interesting Coyote test explores these
/// configurations, letting Coyote control the non-determinism introduced by these random variables
/// and the scheduling of the resulting number of tasks.
#[test]
#[should_panic(expected = "deadlock")]
fn test_bounded_buffer_find_deadlock_configuration() {
    check_random(
        move || {
            let mut rng = thread_rng();

            let buffer_size = rng.gen_range(0usize, 5) + 1;
            let readers = rng.gen_range(0usize, 5) + 1;
            let writers = rng.gen_range(0usize, 5) + 1;
            let iterations = rng.gen_range(0usize, 10) + 1;
            let total_iterations = iterations * readers;
            let writer_iterations = total_iterations / writers;
            let remainder = total_iterations % writers;

            tracing::info!(buffer_size, readers, writers, iterations);

            let buffer = BoundedBuffer::new(buffer_size);

            let mut tasks = vec![];

            for _ in 0..readers {
                let buffer = buffer.clone();
                tasks.push(thread::spawn(move || reader(buffer, iterations)));
            }

            for i in 0..writers {
                let buffer = buffer.clone();
                let mut w = writer_iterations;
                if i == writers - 1 {
                    w += remainder;
                }
                tasks.push(thread::spawn(move || writer(buffer, w)));
            }

            for task in tasks {
                task.join().unwrap();
            }
        },
        1000,
    )
}

/// Indeed, we now see clearly that there is a minimal test with two readers and one writer. We also
/// see all these deadlocks can be found with a buffer size of one and a small number of iterations.
#[allow(clippy::vec_init_then_push)]
fn bounded_buffer_minimal() {
    let buffer = BoundedBuffer::new(1);

    let mut tasks = vec![];

    tasks.push({
        let buffer = buffer.clone();
        thread::spawn(move || reader(buffer, 5))
    });

    tasks.push({
        let buffer = buffer.clone();
        thread::spawn(move || reader(buffer, 5))
    });

    tasks.push({
        let buffer = buffer.clone();
        thread::spawn(move || writer(buffer, 10))
    });

    for task in tasks {
        task.join().unwrap();
    }
}

/// Now we can write the minimal test. We’ll use 10 iterations just to be sure it deadlocks often.
#[test]
#[should_panic(expected = "deadlock")]
fn test_bounded_buffer_minimal_deadlock() {
    check_random(bounded_buffer_minimal, 1000)
}

/// Fortunately Coyote also produces another log file called BoundedBuffer_0_0.schedule. This is a
/// magic file that Coyote can use to replay the bug. With this, you can step through your program
/// in the debugger, take as long as you want, and the bug will always be found. This is a HUGE
/// advantage to anyone debugging these kinds of Heisenbugs.
#[test]
#[should_panic(expected = "deadlock")]
fn test_bounded_buffer_minimal_deadlock_replay() {
    replay(bounded_buffer_minimal, "91022600006c50a6699b246d92166d5ba22801")
}
