//! A channel's state belongs to the execution that created it. One that outlives that execution, in
//! a `static` Shuttle does not reset, starts over in the next execution that uses it.

mod common;

use common::Counted;
use shuttle::check_random;
use shuttle_crossbeam_channel_inner::{Receiver, RecvTimeoutError, Sender, TryRecvError, after, never, unbounded};
use shuttle_time_inner::time::{Duration, Instant};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

#[test]
fn a_channel_in_a_real_static_starts_over_in_every_execution() {
    static CHANNEL: OnceLock<(Sender<Counted>, Receiver<Counted>)> = OnceLock::new();
    static DROPS: OnceLock<Arc<AtomicUsize>> = OnceLock::new();
    let drops = Arc::clone(DROPS.get_or_init(Default::default));
    let executions = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&executions);
    check_random(
        move || {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            let (s, r) = CHANNEL.get_or_init(unbounded);
            // Whatever the earlier executions left in the channel is gone.
            assert_eq!(s.len(), 0);
            assert_eq!(r.try_recv().err(), Some(TryRecvError::Empty));
            // ... and was dropped: two messages for every earlier execution.
            assert_eq!(DROPS.get().unwrap().load(Ordering::SeqCst), 2 * n);
            for _ in 0..2 {
                s.send(Counted(Arc::clone(DROPS.get().unwrap()))).unwrap();
            }
            assert_eq!(r.len(), 2);
        },
        10,
    );
    assert_eq!(executions.load(Ordering::SeqCst), 10);
    // The last execution's messages are still in the channel, as nothing used it since.
    assert_eq!(drops.load(Ordering::SeqCst), 18);
}

#[test]
fn a_static_never_works_in_every_execution() {
    static NEVER: Receiver<u8> = never();
    check_random(
        || {
            assert_eq!(NEVER.try_recv(), Err(TryRecvError::Empty));
            assert_eq!(
                NEVER.recv_timeout(Duration::from_millis(1)),
                Err(RecvTimeoutError::Timeout)
            );
        },
        10,
    );
}

#[test]
fn a_timer_in_a_real_static_fires_again_in_every_execution() {
    static TIMER: OnceLock<Receiver<Instant>> = OnceLock::new();
    check_random(
        || {
            let timer = TIMER.get_or_init(|| after(Duration::from_millis(5)));
            // Received in the earlier executions, but not yet in this one.
            assert!(timer.recv().is_ok());
            assert!(timer.try_recv().is_err());
        },
        10,
    );
}
