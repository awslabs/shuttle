//! Code written against crossbeam-channel, as a crate that depends on `shuttle-crossbeam-channel`
//! under the name `crossbeam_channel` would have it. The same source is compiled against the
//! wrapper without the `shuttle` feature, where it is crossbeam-channel, against the wrapper with
//! the feature, and against `shuttle-crossbeam-channel-inner`, which is what the wrapper is under
//! `shuttle`. The test that includes it provides `crate::crossbeam_channel`, `crate::thread` (std's
//! or Shuttle's) and `crate::time` (std's or `shuttle-time`'s).
//!
//! The checks hold for real threads and time as well as under Shuttle, so they assume nothing about
//! how long anything takes, except that a short timeout on a channel that nobody sends to expires.

use crate::crossbeam_channel::{
    self, Receiver, RecvError, RecvTimeoutError, Select, SendError, Sender, TryRecvError, TrySendError, after, bounded,
    never, select, select_biased, tick, unbounded,
};
use crate::thread;
use crate::time::{Duration, Instant};

/// Exercises the channel API.
pub fn exercise() {
    unbounded_channel();
    bounded_channel();
    rendezvous();
    disconnection();
    iterators();
    selection();
    select_api();
    timeouts();
    timers();
    worker_pool();
}

fn unbounded_channel() {
    let (s, r) = unbounded();
    for i in 0..3 {
        s.send(i).unwrap();
    }
    assert_eq!(s.len(), 3);
    assert!(!r.is_empty());
    assert!(!s.is_full());
    assert_eq!(r.capacity(), None);
    assert_eq!((0..3).map(|_| r.recv().unwrap()).collect::<Vec<_>>(), [0, 1, 2]);
    assert_eq!(r.try_recv(), Err(TryRecvError::Empty));
}

fn bounded_channel() {
    let (s, r) = bounded(1);
    s.send(1).unwrap();
    assert!(s.is_full());
    assert_eq!(s.try_send(2), Err(TrySendError::Full(2)));
    let sender = thread::spawn(move || s.send(2).unwrap());
    assert_eq!(r.recv(), Ok(1));
    assert_eq!(r.recv(), Ok(2));
    sender.join().unwrap();
    assert_eq!(r.capacity(), Some(1));
}

fn rendezvous() {
    let (s, r) = bounded::<&'static str>(0);
    assert_eq!(s.capacity(), Some(0));
    assert!(s.is_empty() && s.is_full());
    // Nobody waits on the other side, so neither side can go on without blocking.
    assert_eq!(s.try_send("lost"), Err(TrySendError::Full("lost")));
    assert_eq!(r.try_recv(), Err(TryRecvError::Empty));
    let receiver = thread::spawn(move || r.recv().unwrap());
    s.send("hello").unwrap();
    assert_eq!(receiver.join().unwrap(), "hello");
}

fn disconnection() {
    let (s, r) = unbounded::<u32>();
    let s2 = s.clone();
    assert!(s.same_channel(&s2));
    s.send(7).unwrap();
    drop(s);
    drop(s2);
    // Messages sent before the disconnection are still received.
    assert_eq!(r.recv(), Ok(7));
    assert_eq!(r.recv(), Err(RecvError));
    assert_eq!(r.try_recv(), Err(TryRecvError::Disconnected));

    let (s, r) = bounded::<u32>(0);
    drop(r);
    assert_eq!(s.send(1), Err(SendError(1)));

    // A blocked receiver wakes up when the last sender goes.
    let (s, r) = bounded::<u32>(1);
    let receiver = thread::spawn(move || r.recv());
    drop(s);
    assert_eq!(receiver.join().unwrap(), Err(RecvError));
}

fn iterators() {
    let (s, r) = unbounded();
    let sender = thread::spawn(move || {
        for i in 0..4 {
            s.send(i).unwrap();
        }
    });
    let all: Vec<i32> = r.iter().collect();
    assert_eq!(all, [0, 1, 2, 3]);
    sender.join().unwrap();

    let (s, r) = unbounded();
    s.send(1).unwrap();
    s.send(2).unwrap();
    assert_eq!(r.try_iter().collect::<Vec<_>>(), [1, 2]);
    drop(s);
    assert_eq!(r.into_iter().count(), 0);
}

fn selection() {
    let (s1, r1) = unbounded();
    let (s2, r2) = unbounded();
    s1.send(10).unwrap();
    // Both operations are ready, so either may run.
    select! {
        recv(r1) -> msg => assert_eq!(msg, Ok(10)),
        send(s2, 20) -> res => {
            assert_eq!(res, Ok(()));
            assert_eq!(r2.recv(), Ok(20));
        }
    }

    // A biased selection takes the first ready operation.
    let (s3, r3) = unbounded::<u8>();
    s3.send(1).unwrap();
    s2.send(2).unwrap();
    select_biased! {
        recv(r3) -> msg => assert_eq!(msg, Ok(1)),
        recv(r2) -> _ => panic!("the first ready operation is selected"),
    }

    // Nothing is ready, so the default case runs.
    let empty: Receiver<u8> = never();
    select! {
        recv(empty) -> _ => panic!("`never` delivers nothing"),
        default => {}
    }

    // A selection waits for a sender on another thread.
    let (s4, r4) = bounded(0);
    let sender = thread::spawn(move || s4.send("handed over").unwrap());
    select! {
        recv(r4) -> msg => assert_eq!(msg, Ok("handed over")),
    }
    sender.join().unwrap();
}

fn select_api() {
    let (s1, r1) = unbounded::<i32>();
    let (s2, r2) = unbounded::<i32>();
    s2.send(5).unwrap();
    let mut sel = Select::new();
    let i1 = sel.recv(&r1);
    let i2 = sel.recv(&r2);
    let oper = sel.select();
    assert_eq!(oper.index(), i2);
    assert_eq!(oper.recv(&r2), Ok(5));
    assert!(sel.try_select().is_err());
    assert_eq!(sel.try_ready().ok(), None);
    s1.send(6).unwrap();
    assert_eq!(sel.ready(), i1);
    assert_eq!(r1.try_recv(), Ok(6));
    sel.remove(i1);
    assert!(sel.select_timeout(Duration::from_millis(1)).is_err());
    drop(s1);
    drop(s2);
}

fn timeouts() {
    // Nobody sends, so the receive times out: here after a millisecond, under Shuttle once no task
    // can run.
    let (s, r) = unbounded::<u8>();
    let start = Instant::now();
    assert_eq!(r.recv_timeout(Duration::from_millis(1)), Err(RecvTimeoutError::Timeout));
    assert!(start.elapsed() >= Duration::from_millis(1));
    assert_eq!(
        r.recv_deadline(Instant::now() + Duration::from_millis(1)),
        Err(RecvTimeoutError::Timeout)
    );
    s.send(1).unwrap();
    assert_eq!(r.recv_timeout(Duration::from_secs(3600)), Ok(1));

    // A full channel's send times out, and hands the message back.
    let (s, _r) = bounded(1);
    s.send(1).unwrap();
    let err = s.send_timeout(2, Duration::from_millis(1)).unwrap_err();
    assert!(err.is_timeout());
    assert_eq!(err.into_inner(), 2);

    // A selection's default case after a timeout.
    select! {
        recv(r) -> _ => panic!("nothing was sent"),
        default(Duration::from_millis(1)) => {}
    }
}

fn timers() {
    let start = Instant::now();
    let fired = after(Duration::from_millis(1)).recv().unwrap();
    assert!(fired >= start + Duration::from_millis(1));
    assert!(Instant::now() >= fired);

    let ticker = tick(Duration::from_millis(1));
    let first = ticker.recv().unwrap();
    let second = ticker.recv().unwrap();
    assert!(second >= first + Duration::from_millis(1));

    // A timer as the timeout of a selection.
    let (_s, r) = unbounded::<u8>();
    select! {
        recv(r) -> _ => panic!("nothing was sent"),
        recv(after(Duration::from_millis(1))) -> fired => assert!(fired.is_ok()),
    }

    let nothing: Receiver<u8> = never();
    assert_eq!(
        nothing.recv_timeout(Duration::from_millis(1)),
        Err(RecvTimeoutError::Timeout)
    );
}

/// Several workers share one receiver, and the results come back on another channel.
fn worker_pool() {
    let (jobs, queue): (Sender<u32>, Receiver<u32>) = bounded(2);
    let (results, done) = unbounded();
    let workers = (0..2)
        .map(|_| {
            let queue = queue.clone();
            let results = results.clone();
            thread::spawn(move || {
                for job in queue {
                    results.send(job * 2).unwrap();
                }
            })
        })
        .collect::<Vec<_>>();
    drop(queue);
    drop(results);
    for job in 1..=4 {
        jobs.send(job).unwrap();
    }
    drop(jobs);
    let mut all: Vec<u32> = done.iter().collect();
    all.sort_unstable();
    assert_eq!(all, [2, 4, 6, 8]);
    for worker in workers {
        worker.join().unwrap();
    }
    let _ = crossbeam_channel::bounded::<()>(1);
}
