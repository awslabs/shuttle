//! `try_thread_rng`, for code that can do without random data: it gets the generator wherever the
//! scheduler provides random data, and `None`, adding nothing to the schedule, where the scheduler
//! does not. A replay of a schedule gives the same answers at the same points.

use shuttle::rand::{thread_rng, try_thread_rng, Rng, RngCore};
use shuttle::scheduler::{
    DfsScheduler, PctScheduler, RandomScheduler, RoundRobinScheduler, UncontrolledNondeterminismCheckScheduler,
    UrwRandomScheduler,
};
use shuttle::sync::Mutex;
use shuttle::{check_dfs, check_random_with_seed, check_uncontrolled_nondeterminism, replay, thread};
use shuttle::{Config, PortfolioRunner, Runner};
use shuttle_engine::runtime::execution::CurrentSchedule;
use shuttle_engine::scheduler::serialization::serialize_schedule;
use shuttle_engine::scheduler::ScheduleStep;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use test_log::test;

/// The number of random steps in the schedule so far.
fn random_steps() -> usize {
    CurrentSchedule::get_schedule()
        .steps
        .iter()
        .filter(|step| **step == ScheduleStep::Random)
        .count()
}

/// The message of the panic that `f` fails with.
fn panic_message(f: impl FnOnce()) -> String {
    let payload = panic::catch_unwind(AssertUnwindSafe(f)).expect_err("expected a panic");
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => payload.downcast_ref::<&str>().unwrap().to_string(),
    }
}

#[test]
fn dfs_without_random_data_refuses_and_adds_nothing_to_the_schedule() {
    check_dfs(
        || {
            let before = CurrentSchedule::len();
            for _ in 0..10 {
                assert!(try_thread_rng().is_none());
            }
            assert_eq!(CurrentSchedule::len(), before);
        },
        None,
    );
}

#[test]
#[should_panic(expected = "requested random data from DFS scheduler")]
fn dfs_without_random_data_still_fails_a_draw_from_thread_rng() {
    check_dfs(
        || {
            assert!(try_thread_rng().is_none());
            let _ = thread_rng().next_u64();
        },
        None,
    );
}

/// How many executions `check_dfs` explores for two threads that take one lock each, asking for
/// random data around it if `asks`.
fn dfs_executions(asks: bool) -> usize {
    let executions = Arc::new(AtomicUsize::new(0));
    let counter = executions.clone();
    check_dfs(
        move || {
            counter.fetch_add(1, Ordering::SeqCst);
            let ask = move || {
                if asks {
                    assert!(try_thread_rng().is_none());
                }
            };
            let lock = shuttle::sync::Arc::new(Mutex::new(0));
            let theirs = lock.clone();
            let thread = thread::spawn(move || {
                ask();
                *theirs.lock().unwrap() += 1;
                ask();
            });
            ask();
            *lock.lock().unwrap() += 1;
            ask();
            thread.join().unwrap();
        },
        None,
    );
    executions.load(Ordering::SeqCst)
}

#[test]
fn refusals_add_nothing_to_the_search() {
    let without = dfs_executions(false);
    assert!(without > 1, "{without}");
    assert_eq!(dfs_executions(true), without);
}

#[test]
fn dfs_with_random_data_provides_it() {
    let runner = Runner::new(DfsScheduler::new(None, true), Config::new());
    runner.run(|| {
        let before = random_steps();
        let mut rng = try_thread_rng().expect("the scheduler provides random data");
        let _ = rng.next_u64();
        assert_eq!(random_steps(), before + 1);
    });
}

#[test]
fn every_other_scheduler_provides_random_data() {
    // PCT insists on a test with a scheduling choice: here, after the spawn, whether the main
    // thread or the spawned one runs first.
    fn asks() {
        thread::spawn(|| assert!(try_thread_rng().is_some()));
        assert!(try_thread_rng().is_some());
    }
    Runner::new(RandomScheduler::new(5), Config::new()).run(asks);
    Runner::new(PctScheduler::new(2, 5), Config::new()).run(asks);
    Runner::new(UrwRandomScheduler::new(5), Config::new()).run(asks);
    Runner::new(RoundRobinScheduler::new(5), Config::new()).run(asks);
}

/// Runs `draw` in each of 20 executions of a random scheduler seeded with `seed`, and returns what
/// it drew in each, with how many random steps the execution's schedule had by then.
fn draws_with_seed(seed: u64, draw: fn() -> Vec<u64>) -> Vec<(Vec<u64>, usize)> {
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = log.clone();
    check_random_with_seed(move || sink.lock().unwrap().push((draw(), random_steps())), seed, 20);
    let draws = log.lock().unwrap().clone();
    draws
}

#[test]
fn the_generator_draws_what_thread_rng_would_have() {
    // One step and one value per draw, the same as `thread_rng`'s.
    assert_eq!(
        draws_with_seed(7, || (0..5).map(|_| try_thread_rng().unwrap().next_u64()).collect()),
        draws_with_seed(7, || (0..5).map(|_| thread_rng().next_u64()).collect())
    );
    // And so the same bools, which take one draw each.
    assert_eq!(
        draws_with_seed(8, || (0..5)
            .map(|_| u64::from(try_thread_rng().unwrap().gen_bool(0.5)))
            .collect()),
        draws_with_seed(8, || (0..5).map(|_| u64::from(thread_rng().gen_bool(0.5))).collect())
    );
}

#[test]
fn a_clone_draws_its_own_values() {
    // The generator holds the first value; its clone draws the next one from the shared generator.
    assert_eq!(
        draws_with_seed(3, || {
            let mut rng = try_thread_rng().unwrap();
            let mut clone = rng.clone();
            let second = clone.next_u64();
            let first = rng.next_u64();
            vec![first, second, rng.next_u64()]
        }),
        draws_with_seed(3, || (0..3).map(|_| thread_rng().next_u64()).collect())
    );
}

/// Two threads each ask for random data and take a lock. Records, in `log`, what each ask returned,
/// in the order of the asks, and fails if the spawned thread took the lock first.
fn ask_then_lock(log: &'static std::sync::Mutex<(Vec<Option<u64>>, String)>) {
    log.lock().unwrap().0.clear();
    let order = shuttle::sync::Arc::new(Mutex::new(Vec::new()));
    let theirs = order.clone();
    let thread = thread::spawn(move || {
        let got = try_thread_rng().map(|mut rng| rng.next_u64());
        log.lock().unwrap().0.push(got);
        theirs.lock().unwrap().push("spawned");
    });
    let got = try_thread_rng().map(|mut rng| rng.next_u64());
    log.lock().unwrap().0.push(got);
    order.lock().unwrap().push("main");
    thread.join().unwrap();
    let first = order.lock().unwrap()[0];
    // The schedule so far, which is the failing schedule if the assertion below fails.
    log.lock().unwrap().1 = serialize_schedule(&CurrentSchedule::get_schedule());
    assert_eq!(first, "main", "the spawned thread took the lock first");
}

/// Fails `run_original` and replays the schedule it failed with, and returns what the asks of the
/// failing execution and of the replay returned.
fn fail_and_replay(
    log: &'static std::sync::Mutex<(Vec<Option<u64>>, String)>,
    run_original: impl FnOnce(),
) -> (Vec<Option<u64>>, Vec<Option<u64>>) {
    let message = panic_message(run_original);
    assert!(message.contains("the spawned thread took the lock first"), "{message}");
    let (original, schedule) = log.lock().unwrap().clone();

    let message = panic_message(|| replay(move || ask_then_lock(log), &schedule));
    assert!(message.contains("the spawned thread took the lock first"), "{message}");
    let replayed = log.lock().unwrap().0.clone();
    (original, replayed)
}

#[test]
fn a_replay_refuses_where_dfs_refused() {
    static LOG: std::sync::Mutex<(Vec<Option<u64>>, String)> = std::sync::Mutex::new((Vec::new(), String::new()));
    let (original, replayed) = fail_and_replay(&LOG, || check_dfs(|| ask_then_lock(&LOG), None));
    assert_eq!(original, vec![None, None]);
    assert_eq!(replayed, original);
}

#[test]
fn a_replay_provides_what_a_random_scheduler_provided() {
    static LOG: std::sync::Mutex<(Vec<Option<u64>>, String)> = std::sync::Mutex::new((Vec::new(), String::new()));
    let (original, replayed) = fail_and_replay(&LOG, || {
        Runner::new(RandomScheduler::new_from_seed(11, 1000), Config::new()).run(|| ask_then_lock(&LOG));
    });
    assert!(original.iter().all(Option::is_some), "{original:?}");
    assert_eq!(replayed, original);
}

#[test]
fn a_replay_provides_what_dfs_with_random_data_provided() {
    static LOG: std::sync::Mutex<(Vec<Option<u64>>, String)> = std::sync::Mutex::new((Vec::new(), String::new()));
    let (original, replayed) = fail_and_replay(&LOG, || {
        Runner::new(DfsScheduler::new(None, true), Config::new()).run(|| ask_then_lock(&LOG));
    });
    assert!(original.iter().all(Option::is_some), "{original:?}");
    assert_eq!(replayed, original);
}

/// Spawns a thread if `taken`, so that a schedule recorded with one answer does not replay with the
/// other.
fn spawn_if(taken: bool) {
    if taken {
        thread::spawn(|| {}).join().unwrap();
    }
}

#[test]
fn a_choice_made_without_random_data_is_controlled() {
    let scheduler = UncontrolledNondeterminismCheckScheduler::new(DfsScheduler::new(None, false));
    Runner::new(scheduler, Config::new()).run(|| {
        spawn_if(try_thread_rng().is_none());
        spawn_if(try_thread_rng().is_some());
    });
}

#[test]
fn a_choice_made_with_random_data_is_controlled() {
    check_uncontrolled_nondeterminism(
        || {
            let taken = match try_thread_rng() {
                Some(mut rng) => rng.gen_bool(0.5),
                None => true,
            };
            spawn_if(taken);
        },
        100,
    );
}

#[test]
#[should_panic(expected = "possible nondeterminism")]
fn a_refusal_that_does_not_happen_again_is_nondeterminism() {
    // Asks for random data in the first of each pair of executions, which the checker records, and
    // not in the second, which replays the recording.
    static EXECUTIONS: AtomicUsize = AtomicUsize::new(0);
    let scheduler = UncontrolledNondeterminismCheckScheduler::new(DfsScheduler::new(None, false));
    Runner::new(scheduler, Config::new()).run(|| {
        if EXECUTIONS.fetch_add(1, Ordering::SeqCst).is_multiple_of(2) {
            assert!(try_thread_rng().is_none());
        }
    });
}

#[test]
fn a_portfolio_passes_the_answer_on() {
    let mut portfolio = PortfolioRunner::new(true, Config::new());
    portfolio.add(DfsScheduler::new(None, false));
    portfolio.run(|| assert!(try_thread_rng().is_none()));

    let mut portfolio = PortfolioRunner::new(true, Config::new());
    portfolio.add(RandomScheduler::new(5));
    portfolio.add(DfsScheduler::new(None, true));
    portfolio.run(|| assert!(try_thread_rng().is_some()));
}

#[test]
fn teardown_provides_random_data_whatever_the_scheduler() {
    // Whether a destructor that ran in teardown got the generator, and whether its draw left the
    // schedule as it was.
    static ASKED: std::sync::Mutex<Vec<(bool, bool)>> = std::sync::Mutex::new(Vec::new());

    struct AsksOnDrop;

    impl Drop for AsksOnDrop {
        fn drop(&mut self) {
            let before = CurrentSchedule::len();
            let got = try_thread_rng().map(|mut rng| rng.next_u64());
            ASKED
                .lock()
                .unwrap()
                .push((got.is_some(), CurrentSchedule::len() == before));
        }
    }

    shuttle::lazy_static! {
        static ref STATIC: std::sync::Mutex<Option<AsksOnDrop>> = std::sync::Mutex::new(None);
    }

    check_dfs(
        || {
            *STATIC.lock().unwrap() = Some(AsksOnDrop);
            assert!(try_thread_rng().is_none());
        },
        None,
    );
    assert_eq!(*ASKED.lock().unwrap(), vec![(true, true)]);
}

#[test]
#[should_panic(expected = "Are you accessing a Shuttle primitive outside of a Shuttle test?")]
fn outside_of_shuttle_it_panics() {
    let _ = try_thread_rng();
}
