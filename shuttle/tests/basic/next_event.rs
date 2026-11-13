//! Tests for `Task::next_event`, the operation each runnable task performs if it is scheduled next.

use shuttle::future::batch_semaphore::{BatchSemaphore, Fairness};
use shuttle::scheduler::{Event, EventKind, EventTarget, RoundRobinScheduler, Schedule, Scheduler, Task, TaskId};
use shuttle::sync::atomic::{AtomicUsize, Ordering};
use shuttle::sync::{mpsc, Condvar, Mutex};
use shuttle::{future, thread, Config, Runner};
use std::future::{poll_fn, Future};
use std::sync::Arc;
use std::task::Poll;
use test_log::test;

/// The runnable tasks and their next events at one scheduling decision.
type Decision = Vec<(TaskId, Event)>;

/// A pair of runnable tasks whose next steps conflict, at the decision with this index.
type Conflict = (usize, TaskId, TaskId);

/// Wraps a scheduler, and records every decision it is asked to make, and the pairs of runnable
/// tasks whose next steps conflict at it.
struct RecordingScheduler<S> {
    inner: S,
    decisions: Arc<std::sync::Mutex<Vec<Decision>>>,
    conflicts: Arc<std::sync::Mutex<Vec<Conflict>>>,
}

impl<S: Scheduler> Scheduler for RecordingScheduler<S> {
    fn new_execution(&mut self) -> Option<Schedule> {
        self.inner.new_execution()
    }

    fn next_task(&mut self, runnable: &[&Task], current: Option<TaskId>, is_yielding: bool) -> Option<TaskId> {
        let decision = runnable.iter().map(|task| (task.id(), task.next_event())).collect();
        let index = {
            let mut decisions = self.decisions.lock().unwrap();
            decisions.push(decision);
            decisions.len() - 1
        };
        for a in runnable {
            for b in runnable {
                if a.id() != b.id() && a.next_step_conflicts_with(b) {
                    self.conflicts.lock().unwrap().push((index, a.id(), b.id()));
                }
            }
        }
        self.inner.next_task(runnable, current, is_yielding)
    }

    fn next_u64(&mut self) -> u64 {
        self.inner.next_u64()
    }
}

/// Runs `f` once, round-robin, and returns the decisions the scheduler made. One execution only, so
/// that resource ids, which are addresses, are comparable across decisions.
fn record<F>(f: F) -> Vec<Decision>
where
    F: Fn() + Send + Sync + 'static,
{
    record_under(RoundRobinScheduler::new(1), f).0
}

/// Like `record`, under `inner`, which must run a single execution. Also returns the pairs of tasks
/// whose next steps conflicted at a decision.
fn record_under<S, F>(inner: S, f: F) -> (Vec<Decision>, Vec<Conflict>)
where
    S: Scheduler + Send + 'static,
    F: Fn() + Send + Sync + 'static,
{
    let decisions = Arc::new(std::sync::Mutex::new(Vec::new()));
    let conflicts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let scheduler = RecordingScheduler {
        inner,
        decisions: Arc::clone(&decisions),
        conflicts: Arc::clone(&conflicts),
    };
    Runner::new(scheduler, Config::new()).run(f);
    let decisions = decisions.lock().unwrap().clone();
    let conflicts = conflicts.lock().unwrap().clone();
    (decisions, conflicts)
}

/// Every event of the given kind, in the order the decisions saw them.
fn events_of_kind(decisions: &[Decision], kind: EventKind) -> Vec<(TaskId, Event)> {
    let mut events: Vec<(TaskId, Event)> = Vec::new();
    for (task, event) in decisions.iter().flatten() {
        if event.kind() == kind && !events.contains(&(*task, *event)) {
            events.push((*task, *event));
        }
    }
    events
}

fn the_event_of_kind(decisions: &[Decision], kind: EventKind) -> (TaskId, Event) {
    let events = events_of_kind(decisions, kind);
    assert_eq!(events.len(), 1, "expected exactly one {kind:?} event, got {events:?}");
    events[0]
}

#[test]
fn atomic_operations_name_their_atomic() {
    let decisions = record(|| {
        let a = Arc::new(AtomicUsize::new(0));
        let b = Arc::new(AtomicUsize::new(0));
        let handle = {
            let a = Arc::clone(&a);
            thread::spawn(move || a.store(1, Ordering::SeqCst))
        };
        a.load(Ordering::SeqCst);
        b.fetch_add(1, Ordering::SeqCst);
        handle.join().unwrap();
    });

    let (_, store) = the_event_of_kind(&decisions, EventKind::AtomicStore);
    let (_, load) = the_event_of_kind(&decisions, EventKind::AtomicLoad);
    let (_, rmw) = the_event_of_kind(&decisions, EventKind::AtomicReadModifyWrite);

    assert!(matches!(store.target(), Some(EventTarget::Resource(_))));
    assert_eq!(store.target(), load.target());
    assert_ne!(store.target(), rmw.target());
    assert!(store.conflicts_with(&load));
    assert!(!store.conflicts_with(&rmw));
    assert!(!load.conflicts_with(&load), "two loads commute");

    // The event points at the operation in the code under test, not inside Shuttle.
    for event in [store, load, rmw] {
        assert_eq!(event.location().unwrap().file(), file!(), "{event}");
    }
}

#[test]
fn locking_a_mutex_acquires_its_semaphore() {
    let decisions = record(|| {
        let lock = Arc::new(Mutex::new(0));
        let handle = {
            let lock = Arc::clone(&lock);
            thread::spawn(move || *lock.lock().unwrap() += 1)
        };
        *lock.lock().unwrap() += 1;
        handle.join().unwrap();
    });

    let acquires = events_of_kind(&decisions, EventKind::SemaphoreAcquire);
    let releases = events_of_kind(&decisions, EventKind::SemaphoreRelease);
    assert!(!acquires.is_empty() && !releases.is_empty());
    let target = acquires[0].1.target();
    assert!(target.is_some());
    for (_, event) in acquires.iter().chain(&releases) {
        assert_eq!(event.target(), target, "{event}");
    }
    for (_, event) in &acquires {
        assert_eq!(event.location().unwrap().file(), file!(), "{event}");
    }
}

#[test]
fn a_task_blocked_on_a_semaphore_waits_on_it() {
    let decisions = record(|| {
        // An unfair semaphore that will block has no scheduling point before the first poll, so
        // every acquire event of the waiter comes from its pending wait.
        let semaphore = Arc::new(BatchSemaphore::new(0, Fairness::Unfair));
        let waiter = {
            let semaphore = Arc::clone(&semaphore);
            future::spawn(async move { semaphore.acquire(1).await.unwrap() })
        };
        thread::yield_now();
        semaphore.release(1);
        future::block_on(waiter).unwrap();
    });

    let (_, release) = the_event_of_kind(&decisions, EventKind::SemaphoreRelease);
    let waits = events_of_kind(&decisions, EventKind::SemaphoreAcquire);
    assert_eq!(waits.len(), 1, "{waits:?}");
    let (waiter, wait) = waits[0];
    assert_ne!(waiter, TaskId::from(0));
    assert_eq!(wait.target(), release.target());
    assert!(wait.conflicts_with(&release));
}

#[test]
fn a_dropped_acquire_names_nothing() {
    let decisions = record(|| {
        future::block_on(async {
            let semaphore = BatchSemaphore::new(0, Fairness::Unfair);
            {
                let mut acquire = Box::pin(semaphore.acquire(1));
                let polled = poll_fn(|cx| Poll::Ready(acquire.as_mut().poll(cx))).await;
                assert!(polled.is_pending());
            }
            drop(semaphore);

            // Wait for something that isn't a Shuttle semaphore. While the main task is switched
            // out here, its next event must not be the acquire, whose semaphore is gone.
            future::spawn(async { thread::yield_now() }).await.unwrap();
        })
    });

    let main = TaskId::from(0);
    let spawn = decisions
        .iter()
        .position(|decision| decision.iter().any(|(_, event)| event.kind() == EventKind::Spawn))
        .unwrap();
    let main_events: Vec<Event> = decisions[spawn + 1..]
        .iter()
        .flatten()
        .filter(|(task, _)| *task == main)
        .map(|(_, event)| *event)
        .collect();
    assert!(main_events.contains(&Event::UNKNOWN));
    for event in main_events {
        assert_ne!(event.kind(), EventKind::SemaphoreAcquire, "{event}");
    }
}

static FIRST_ACQUIRE_LINE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static SECOND_ACQUIRE_LINE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The lines of the main task's acquire events, in decision order.
fn main_acquire_lines(decisions: &[Decision]) -> Vec<u32> {
    decisions
        .iter()
        .flatten()
        .filter(|(task, event)| *task == TaskId::from(0) && event.kind() == EventKind::SemaphoreAcquire)
        .map(|(_, event)| event.location().unwrap().line())
        .collect()
}

#[test]
fn a_task_woken_by_a_semaphore_reports_the_acquire_it_was_woken_for() {
    use std::sync::atomic::Ordering::SeqCst;

    // The main task waits on two acquires at once, as in a `select!`, and the second one's
    // semaphore wakes it.
    let decisions = record(|| {
        let first_semaphore = Arc::new(BatchSemaphore::new(0, Fairness::Unfair));
        let second_semaphore = Arc::new(BatchSemaphore::new(0, Fairness::Unfair));
        let releaser = {
            let second_semaphore = Arc::clone(&second_semaphore);
            thread::spawn(move || second_semaphore.release(1))
        };
        future::block_on(async {
            FIRST_ACQUIRE_LINE.store(line!() + 1, SeqCst);
            let mut first = Box::pin(first_semaphore.acquire(1));
            SECOND_ACQUIRE_LINE.store(line!() + 1, SeqCst);
            let mut second = Box::pin(second_semaphore.acquire(1));
            poll_fn(|cx| {
                if first.as_mut().poll(cx).is_ready() || second.as_mut().poll(cx).is_ready() {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            })
            .await;
        });
        releaser.join().unwrap();
    });

    let lines = main_acquire_lines(&decisions);
    assert!(!lines.is_empty(), "the main task was never reported acquiring");
    assert!(
        lines.iter().all(|line| *line == SECOND_ACQUIRE_LINE.load(SeqCst)),
        "{lines:?}, first acquire at line {}",
        FIRST_ACQUIRE_LINE.load(SeqCst)
    );
}

static NESTED_ACQUIRE_LINE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[test]
fn a_blocking_acquire_inside_a_future_reports_itself() {
    use std::sync::atomic::Ordering::SeqCst;

    // The main task holds a pending acquire on one semaphore across polls, as a tokio `Receiver`
    // does, and then blocks in a nested `block_on` on another one, which wakes it.
    let decisions = record(|| {
        let held_semaphore = BatchSemaphore::new(0, Fairness::Unfair);
        let lock = Arc::new(BatchSemaphore::new(0, Fairness::Unfair));
        let releaser = {
            let lock = Arc::clone(&lock);
            thread::spawn(move || lock.release(1))
        };
        future::block_on(async {
            let mut held = Box::pin(held_semaphore.acquire(1));
            assert!(poll_fn(|cx| Poll::Ready(held.as_mut().poll(cx))).await.is_pending());
            NESTED_ACQUIRE_LINE.store(line!() + 1, SeqCst);
            lock.acquire_blocking(1).unwrap();
        });
        releaser.join().unwrap();
    });

    let lines = main_acquire_lines(&decisions);
    assert!(!lines.is_empty(), "the main task was never reported acquiring");
    assert!(
        lines.iter().all(|line| *line == NESTED_ACQUIRE_LINE.load(SeqCst)),
        "{lines:?}"
    );
}

/// Runs one execution, always picking the runnable task with the highest id.
struct NewestTaskFirst(bool);

impl Scheduler for NewestTaskFirst {
    fn new_execution(&mut self) -> Option<Schedule> {
        std::mem::replace(&mut self.0, false).then(|| Schedule::new(0))
    }

    fn next_task(&mut self, runnable: &[&Task], _current: Option<TaskId>, _is_yielding: bool) -> Option<TaskId> {
        runnable.iter().map(|task| task.id()).max()
    }

    fn next_u64(&mut self) -> u64 {
        0
    }
}

#[test]
fn a_task_woken_while_at_another_scheduling_point_keeps_its_event() {
    // The main task leaves an acquire pending and goes on, in the same poll, to load an atomic. The
    // releasing thread runs while the main task is stopped before the load, and wakes it: the load
    // is still what the main task does next.
    let (decisions, _) = record_under(NewestTaskFirst(true), || {
        let semaphore = Arc::new(BatchSemaphore::new(0, Fairness::Unfair));
        let flag = Arc::new(AtomicUsize::new(0));
        future::block_on(async {
            let mut acquire = Box::pin(semaphore.acquire(1));
            assert!(poll_fn(|cx| Poll::Ready(acquire.as_mut().poll(cx))).await.is_pending());
            let releaser = {
                let semaphore = Arc::clone(&semaphore);
                thread::spawn(move || semaphore.release(1))
            };
            flag.load(Ordering::SeqCst);
            acquire.await.unwrap();
            releaser.join().unwrap();
        });
    });

    let (main, releaser) = (TaskId::from(0), TaskId::from(1));
    let release = decisions
        .iter()
        .position(|decision| {
            decision
                .iter()
                .any(|(task, event)| *task == releaser && event.kind() == EventKind::SemaphoreRelease)
        })
        .unwrap();
    // The releaser runs next, being the newest task, and releases.
    let after_release = &decisions[release + 1];
    let main_event = after_release.iter().find(|(task, _)| *task == main).unwrap().1;
    assert_eq!(main_event.kind(), EventKind::AtomicLoad, "{main_event}");
}

#[test]
fn aborting_a_task_conflicts_with_its_next_step() {
    // The aborted task is stopped before a load of its own atomic when the main task aborts it. The
    // two events don't conflict, but the abort decides whether the aborted task runs at all.
    let (decisions, conflicts) = record_under(NewestTaskFirst(true), || {
        let flag = Arc::new(AtomicUsize::new(0));
        let handle = {
            let flag = Arc::clone(&flag);
            future::spawn(async move {
                flag.load(Ordering::SeqCst);
            })
        };
        handle.abort();
        let _ = future::block_on(handle);
    });

    let (main, aborted) = (TaskId::from(0), TaskId::from(1));
    let event_of = |decision: &Decision, task: TaskId| decision.iter().find(|(t, _)| *t == task).map(|(_, e)| *e);
    let decision = decisions
        .iter()
        .position(|decision| {
            event_of(decision, main).is_some_and(|event| event.kind() == EventKind::Abort)
                && event_of(decision, aborted).is_some_and(|event| event.kind() == EventKind::AtomicLoad)
        })
        .expect("the aborted task was never stopped at its load while the main task aborted it");
    let abort = event_of(&decisions[decision], main).unwrap();
    let load = event_of(&decisions[decision], aborted).unwrap();
    assert_eq!(abort.target(), Some(EventTarget::Task(aborted)));
    assert!(!abort.conflicts_with(&load));
    assert!(conflicts.contains(&(decision, main, aborted)));
    assert!(conflicts.contains(&(decision, aborted, main)));
}

#[test]
fn unknown_conflicts_with_everything() {
    let decisions = record(|| {
        let a = Arc::new(AtomicUsize::new(0));
        thread::spawn(move || a.store(1, Ordering::SeqCst)).join().unwrap();
        thread::yield_now();
    });
    let (_, store) = the_event_of_kind(&decisions, EventKind::AtomicStore);
    let (_, yield_now) = the_event_of_kind(&decisions, EventKind::Yield);
    assert!(Event::UNKNOWN.conflicts_with(&store));
    assert!(store.conflicts_with(&Event::UNKNOWN));
    assert!(Event::UNKNOWN.conflicts_with(&Event::UNKNOWN));
    // A yield touches nothing.
    assert!(!yield_now.conflicts_with(&store));
    assert!(!yield_now.conflicts_with(&yield_now));
}

#[test]
fn thread_operations_name_their_task() {
    let decisions = record(|| {
        let main = thread::current();
        let child = thread::spawn(move || {
            main.unpark();
        });
        thread::park();
        child.join().unwrap();
    });

    let main = TaskId::from(0);
    let (spawner, spawn) = the_event_of_kind(&decisions, EventKind::Spawn);
    assert_eq!(spawner, main);
    assert_eq!(spawn.target(), None);

    let (unparker, unpark) = the_event_of_kind(&decisions, EventKind::Unpark);
    assert_ne!(unparker, main);
    assert_eq!(unpark.target(), Some(EventTarget::Task(main)));

    let (_, join) = the_event_of_kind(&decisions, EventKind::Join);
    assert_eq!(join.target(), Some(EventTarget::Task(unparker)));

    for park in events_of_kind(&decisions, EventKind::Park) {
        assert_eq!(park.1.target(), Some(EventTarget::Task(main)));
        assert!(park.1.conflicts_with(&unpark));
    }
}

#[test]
fn condvar_and_channel_operations_name_their_resource() {
    let decisions = record(|| {
        let pair = Arc::new((Mutex::new(false), Condvar::new()));
        let (sender, receiver) = mpsc::channel();
        let child = {
            let pair = Arc::clone(&pair);
            thread::spawn(move || {
                let (lock, condvar) = &*pair;
                *lock.lock().unwrap() = true;
                condvar.notify_one();
                sender.send(1).unwrap();
            })
        };
        let (lock, condvar) = &*pair;
        let mut ready = lock.lock().unwrap();
        while !*ready {
            ready = condvar.wait(ready).unwrap();
        }
        drop(ready);
        assert_eq!(receiver.recv().unwrap(), 1);
        child.join().unwrap();
    });

    let (_, notify) = the_event_of_kind(&decisions, EventKind::CondvarNotify);
    for (_, wait) in events_of_kind(&decisions, EventKind::CondvarWait) {
        assert_eq!(wait.target(), notify.target());
        assert!(wait.conflicts_with(&notify));
    }

    let (_, send) = the_event_of_kind(&decisions, EventKind::ChannelSend);
    let receives = events_of_kind(&decisions, EventKind::ChannelRecv);
    assert!(!receives.is_empty());
    for (_, receive) in receives {
        assert_eq!(receive.target(), send.target());
    }
    assert_ne!(send.target(), notify.target());
}

#[test]
fn a_task_that_has_not_started_is_unknown() {
    let decisions = record(|| {
        let handle = thread::spawn(|| ());
        handle.join().unwrap();
    });
    // The spawned thread hasn't started when the main thread spawns or joins it.
    let child = TaskId::from(1);
    let child_event = decisions.iter().flatten().find(|(task, _)| *task == child).unwrap().1;
    assert_eq!(child_event, Event::UNKNOWN);
}
