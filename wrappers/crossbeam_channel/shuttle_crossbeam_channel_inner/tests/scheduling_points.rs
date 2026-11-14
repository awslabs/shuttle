//! The scheduling points of each operation, counted with `current::context_switches()` in an
//! execution with a single task, where nothing blocks unless the test says so.

use shuttle::check_dfs;
use shuttle::current::context_switches;
use shuttle_crossbeam_channel_inner::{Select, after, at, bounded, never, select, select_biased, tick, unbounded};
use shuttle_time_inner::time::Duration;
use shuttle_time_inner::timer_support::peek;

/// The number of scheduling points `f` takes. What `f` returns is dropped after the count, so that
/// dropping it does not count.
fn points<R>(f: impl FnOnce() -> R) -> usize {
    let before = context_switches();
    let result = f();
    let points = context_switches() - before;
    drop(result);
    points
}

#[test]
fn operations_that_do_not_block_take_one() {
    check_dfs(
        || {
            for (s, r) in [unbounded(), bounded(4)] {
                assert_eq!(points(|| s.send(1).unwrap()), 1);
                assert_eq!(points(|| s.try_send(2).unwrap()), 1);
                assert_eq!(points(|| s.send_timeout(3, Duration::from_secs(1)).unwrap()), 1);
                assert_eq!(points(|| r.recv().unwrap()), 1);
                assert_eq!(points(|| r.try_recv().unwrap()), 1);
                assert_eq!(points(|| r.recv_timeout(Duration::from_secs(1)).unwrap()), 1);
                // Failing without blocking is one too.
                assert_eq!(points(|| r.try_recv().unwrap_err()), 1);
                assert_eq!(points(|| r.recv_timeout(Duration::ZERO).unwrap_err()), 1);
                s.send(4).unwrap();
                assert_eq!(points(|| r.iter().next().unwrap()), 1);
                assert_eq!(points(|| r.try_iter().next()), 1);
            }
            let (s, r) = bounded::<u8>(0);
            assert_eq!(points(|| s.try_send(1).unwrap_err()), 1);
            assert_eq!(points(|| r.try_recv().unwrap_err()), 1);
        },
        None,
    );
}

#[test]
fn queries_take_one_except_capacity_and_same_channel() {
    check_dfs(
        || {
            let (s, r) = bounded::<u8>(2);
            assert_eq!(points(|| s.len()), 1);
            assert_eq!(points(|| s.is_empty()), 1);
            assert_eq!(points(|| s.is_full()), 1);
            assert_eq!(points(|| r.len()), 1);
            assert_eq!(points(|| r.is_empty()), 1);
            assert_eq!(points(|| r.is_full()), 1);
            assert_eq!(points(|| s.capacity()), 0);
            assert_eq!(points(|| r.capacity()), 0);
            assert_eq!(points(|| s.same_channel(&s)), 0);
            assert_eq!(points(|| r.same_channel(&r)), 0);
            let timer = after(Duration::from_secs(1));
            assert_eq!(points(|| timer.len()), 1);
            assert_eq!(points(|| never::<u8>().is_empty()), 1);
        },
        None,
    );
}

#[test]
fn creating_cloning_and_dropping_handles() {
    check_dfs(
        || {
            assert_eq!(points(unbounded::<u8>), 0);
            assert_eq!(points(|| bounded::<u8>(0)), 0);
            assert_eq!(points(|| after(Duration::from_secs(1))), 0);
            assert_eq!(points(|| at(peek())), 0);
            assert_eq!(points(|| tick(Duration::from_secs(1))), 0);
            assert_eq!(points(never::<u8>), 0);

            let (s, r) = unbounded::<u8>();
            assert_eq!(points(|| s.clone()), 0);
            assert_eq!(points(|| r.clone()), 0);
            let s2 = s.clone();
            let r2 = r.clone();
            // Dropping a handle that is not its side's last is invisible to other tasks.
            assert_eq!(points(|| drop(s2)), 0);
            assert_eq!(points(|| drop(r2)), 0);
            // Dropping the last one disconnects the channel.
            assert_eq!(points(|| drop(s)), 1);
            assert_eq!(points(|| drop(r)), 1);
        },
        None,
    );
}

#[test]
fn a_selection_takes_one_and_completing_it_none() {
    check_dfs(
        || {
            let (s1, r1) = unbounded::<u8>();
            let (_s2, r2) = unbounded::<u8>();
            s1.send(1).unwrap();
            let mut sel = Select::new();
            sel.recv(&r1);
            sel.recv(&r2);
            let before = context_switches();
            let oper = sel.select();
            assert_eq!(context_switches() - before, 1);
            assert_eq!(points(|| oper.recv(&r1).unwrap()), 0);

            assert_eq!(points(|| sel.try_select().unwrap_err()), 1);
            assert_eq!(points(|| sel.try_ready().unwrap_err()), 1);
            assert_eq!(
                points(|| select_biased! {
                    recv(r1) -> _ => {}
                    recv(r2) -> _ => {}
                    default => {}
                }),
                1
            );
            s1.send(2).unwrap();
            assert_eq!(
                points(|| select! {
                    recv(r1) -> msg => msg.unwrap(),
                    recv(r2) -> _ => unreachable!(),
                }),
                1
            );
        },
        None,
    );
}

// A wait that times out blocks once more: the scheduling decision that finds no task to run times
// it out.
#[test]
fn a_timeout_takes_one_more() {
    check_dfs(
        || {
            let (_s, r) = unbounded::<u8>();
            assert_eq!(points(|| r.recv_timeout(Duration::from_millis(1)).unwrap_err()), 2);
            let timer = after(Duration::from_millis(1));
            assert_eq!(points(|| timer.recv().unwrap()), 2);
        },
        None,
    );
}
