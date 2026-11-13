//! Tests for the scheduling-point checker (see `ExecutionState::record_access`), which fails a test
//! when a step accesses two shared resources, at least one of them for writing, with no scheduling
//! point in between. The checker is on with the `check-scheduling-points` feature.

use shuttle::future::batch_semaphore::{Acquire, BatchSemaphore, Fairness};
use shuttle::future::block_on;
use shuttle::sync::atomic::{AtomicBool, Ordering};
use shuttle::sync::{Arc, ResourceSignature, ResourceType};
use shuttle::{check_dfs, thread};
use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::thread::switch;
use shuttle_engine::AccessKind;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A stand-in for a primitive with its own shared state, which reports its accesses to the checker.
struct Resource {
    signature: ResourceSignature,
}

impl Resource {
    fn new() -> Self {
        Self {
            signature: ExecutionState::new_resource_signature(ResourceType::Atomic),
        }
    }

    fn read(&self) {
        ExecutionState::record_access(&self.signature, AccessKind::Read);
    }

    fn write(&self) {
        ExecutionState::record_access(&self.signature, AccessKind::Write);
    }
}

fn write_two_resources_in_one_step() {
    let a = Resource::new();
    let b = Resource::new();
    switch();
    a.write();
    b.write();
}

#[cfg(feature = "check-scheduling-points")]
#[test]
#[should_panic(expected = "missing scheduling point")]
fn two_resources_in_one_step_fail() {
    check_dfs(write_two_resources_in_one_step, None);
}

#[cfg(feature = "check-scheduling-points")]
#[test]
#[should_panic(expected = "missing scheduling point")]
fn read_then_write_of_two_resources_fails() {
    check_dfs(
        || {
            let a = Resource::new();
            let b = Resource::new();
            switch();
            a.read();
            b.write();
        },
        None,
    );
}

/// The checker sees accesses in any task, not just the main one, and the step it reports begins at
/// the task's own last scheduling point.
#[cfg(feature = "check-scheduling-points")]
#[test]
#[should_panic(expected = "missing scheduling point")]
fn two_resources_in_one_step_of_spawned_task_fail() {
    check_dfs(
        || {
            let handle = thread::spawn(write_two_resources_in_one_step);
            switch();
            handle.join().unwrap();
        },
        None,
    );
}

#[cfg(not(feature = "check-scheduling-points"))]
#[test]
fn two_resources_in_one_step_pass_without_checker() {
    check_dfs(write_two_resources_in_one_step, None);
}

#[test]
fn accesses_separated_by_scheduling_point_pass() {
    check_dfs(
        || {
            let a = Resource::new();
            let b = Resource::new();
            switch();
            a.write();
            switch();
            b.write();
        },
        None,
    );
}

#[test]
fn accesses_to_same_resource_pass() {
    check_dfs(
        || {
            let a = Resource::new();
            switch();
            a.read();
            a.write();
            a.write();
        },
        None,
    );
}

#[test]
fn reads_of_two_resources_pass() {
    check_dfs(
        || {
            let a = Resource::new();
            let b = Resource::new();
            switch();
            a.read();
            b.read();
        },
        None,
    );
}

#[test]
fn atomic_step_of_two_resources_passes() {
    check_dfs(
        || {
            let a = Resource::new();
            let b = Resource::new();
            switch();
            // This test's operation is a write to both resources at once.
            ExecutionState::atomic_step(|| {
                a.write();
                b.write();
            });
        },
        None,
    );
}

/// An `atomic_step` counts as one operation, so another operation in the same step still fails.
#[cfg(feature = "check-scheduling-points")]
#[test]
#[should_panic(expected = "missing scheduling point")]
fn access_after_atomic_step_fails() {
    check_dfs(
        || {
            let a = Resource::new();
            let b = Resource::new();
            let c = Resource::new();
            switch();
            ExecutionState::atomic_step(|| {
                a.write();
                b.write();
            });
            c.write();
        },
        None,
    );
}

/// Steps of different tasks don't share accesses, even when a task's step ends as it finishes and
/// another's begins without a scheduling point in between.
#[test]
fn accesses_in_different_tasks_pass() {
    check_dfs(
        || {
            let handles = (0..2)
                .map(|_| {
                    thread::spawn(|| {
                        let a = Resource::new();
                        switch();
                        a.write();
                    })
                })
                .collect::<Vec<_>>();
            let b = Resource::new();
            for handle in handles {
                handle.join().unwrap();
            }
            b.write();
        },
        None,
    );
}

/// A first poll of an `Acquire` of an unfair semaphore that has to block takes no scheduling point
/// before it blocks, because blocking commutes with every other operation on the semaphore. So it
/// can share a step with the task's previous operation, here the store.
#[test]
fn unfair_acquire_that_blocks_shares_step_with_previous_operation() {
    check_dfs(
        || {
            let semaphore = Arc::new(BatchSemaphore::new(1, Fairness::Unfair));
            let flag = Arc::new(AtomicBool::new(false));
            semaphore.acquire_blocking(1).unwrap();
            let handle = {
                let semaphore = Arc::clone(&semaphore);
                let flag = Arc::clone(&flag);
                thread::spawn(move || {
                    flag.store(true, Ordering::SeqCst);
                    semaphore.acquire_blocking(1).unwrap();
                    semaphore.release(1);
                })
            };
            semaphore.release(1);
            handle.join().unwrap();
        },
        None,
    );
}

/// A future that stores to `flag`, and then polls `acquire`, in one poll.
struct StoreThenAcquire<'a> {
    flag: &'a AtomicBool,
    acquire: Pin<Box<Acquire<'a>>>,
}

impl Future for StoreThenAcquire<'_> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.flag.store(true, Ordering::SeqCst);
        self.acquire.as_mut().poll(cx).map(|result| result.unwrap())
    }
}

/// Once `release` has granted a fair semaphore's permits to a waiter, the poll that finds them is
/// not an operation on the semaphore: no other operation can take them back. So it can share a
/// step with the task's previous operation, here the store.
#[test]
fn poll_of_granted_acquire_shares_step_with_previous_operation() {
    check_dfs(
        || {
            let semaphore = Arc::new(BatchSemaphore::new(1, Fairness::StrictlyFair));
            semaphore.acquire_blocking(1).unwrap();
            let handle = {
                let semaphore = Arc::clone(&semaphore);
                thread::spawn(move || {
                    let flag = AtomicBool::new(false);
                    block_on(StoreThenAcquire {
                        flag: &flag,
                        acquire: Box::pin(semaphore.acquire(1)),
                    });
                    semaphore.release(1);
                })
            };
            semaphore.release(1);
            handle.join().unwrap();
        },
        None,
    );
}
