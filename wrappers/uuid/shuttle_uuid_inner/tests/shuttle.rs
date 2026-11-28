mod common;

use common::uuids_from_threads;
use shuttle::scheduler::{DfsScheduler, UncontrolledNondeterminismCheckScheduler};
use shuttle::sync::{Arc, Mutex};
use shuttle::{Config, Runner};
use shuttle::{check_dfs, check_random, check_random_with_seed, check_uncontrolled_nondeterminism, thread};
use shuttle_engine::runtime::execution::CurrentSchedule;
use shuttle_engine::scheduler::serialization::serialize_schedule;
use shuttle_uuid_inner::{Builder, ContextV1, ContextV7, NoContext, NonNilUuid, Timestamp, Uuid, Variant, Version};
use std::any::TypeId;
use std::collections::HashSet;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};

/// 2026-01-01T00:00:00Z, where the fake clock starts.
const EPOCH_SECONDS: u64 = 1_767_225_600;

/// Runs `check_random_with_seed` and returns the UUIDs that each execution created.
fn uuids_with_seed(seed: u64, iterations: usize, new_uuid: fn() -> Uuid) -> Vec<Vec<Uuid>> {
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log_clone = log.clone();
    check_random_with_seed(
        move || {
            let uuids = uuids_from_threads(3, new_uuid);
            log_clone.lock().unwrap().push(uuids);
        },
        seed,
        iterations,
    );
    log.lock().unwrap().clone()
}

#[test]
fn types_are_shuttles_own() {
    assert_ne!(TypeId::of::<Uuid>(), TypeId::of::<uuid_orig::Uuid>());
    assert_ne!(TypeId::of::<NonNilUuid>(), TypeId::of::<uuid_orig::NonNilUuid>());
    assert_ne!(TypeId::of::<Timestamp>(), TypeId::of::<uuid_orig::Timestamp>());
}

#[test]
fn new_v4_is_a_random_uuid() {
    check_random(
        || {
            let uuid = Uuid::new_v4();
            assert_eq!(uuid.get_version(), Some(Version::Random));
            assert_eq!(uuid.get_variant(), Variant::RFC4122);
            assert_ne!(uuid, Uuid::new_v4());
        },
        100,
    );
}

#[test]
fn same_seed_gives_the_same_uuids() {
    for new_uuid in [Uuid::new_v4, Uuid::now_v7] {
        let first = uuids_with_seed(0x5eed, 100, new_uuid);
        assert_eq!(uuids_with_seed(0x5eed, 100, new_uuid), first);
        assert_ne!(uuids_with_seed(0x5eed + 1, 100, new_uuid), first);

        // The UUIDs differ between threads and between executions.
        let distinct = first.iter().flatten().collect::<HashSet<_>>();
        assert_eq!(distinct.len(), 3 * 100);
    }
}

#[test]
fn fake_clock_starts_over_in_every_execution() {
    check_random(
        || {
            // The first reading of an execution is the epoch itself, whatever earlier executions did
            // to the clock.
            assert_eq!(Timestamp::now(NoContext).to_unix(), (EPOCH_SECONDS, 0));

            // Every later reading, through any of the constructors, is zero or one millisecond after
            // the one before.
            let mut last = 0;
            for reading in [
                millis_since_epoch(Timestamp::now(NoContext)),
                millis_since_epoch(Uuid::now_v7().get_timestamp().unwrap()),
                millis_since_epoch(Uuid::now_v1(&[0; 6]).get_timestamp().unwrap()),
                millis_since_epoch(Uuid::now_v6(&[0; 6]).get_timestamp().unwrap()),
            ] {
                assert!(reading == last || reading == last + 1, "{reading} ms after {last} ms");
                last = reading;
            }
        },
        50,
    );
}

/// How many milliseconds after the fake clock's epoch `ts` is.
fn millis_since_epoch(ts: Timestamp) -> u64 {
    let (seconds, subsec_nanos) = ts.to_unix();
    (seconds - EPOCH_SECONDS) * 1000 + u64::from(subsec_nanos) / 1_000_000
}

#[test]
fn clock_advance_is_scheduled_and_replays() {
    // The milliseconds of six version 7 UUIDs, per execution.
    fn run(seed: u64) -> Vec<Vec<u64>> {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log_clone = log.clone();
        check_random_with_seed(
            move || {
                let uuids = (0..6).map(|_| Uuid::now_v7()).collect::<Vec<_>>();
                // Whether the clock advanced or not, the UUIDs are ordered by creation: a shared
                // millisecond is what makes the context's counter order them.
                assert!(uuids.is_sorted(), "{uuids:?}");
                let millis = uuids
                    .iter()
                    .map(|uuid| millis_since_epoch(uuid.get_timestamp().unwrap()))
                    .collect::<Vec<_>>();
                log_clone.lock().unwrap().push(millis);
            },
            seed,
            50,
        );
        log.lock().unwrap().clone()
    }

    let readings = run(99);
    // The choices come from Shuttle, so the same seed gives the same readings.
    assert_eq!(run(99), readings);

    let steps = readings
        .iter()
        .flat_map(|millis| {
            millis
                .windows(2)
                .map(|pair| pair[1].checked_sub(pair[0]).expect("the clock never goes backwards"))
        })
        .collect::<Vec<_>>();
    assert!(
        steps.iter().all(|&step| step <= 1),
        "each step is zero or one millisecond"
    );
    assert!(
        steps.contains(&0),
        "some pair shares a millisecond, which is what runs the context's counter"
    );
    assert!(steps.contains(&1), "some pair is a millisecond apart");
}

#[test]
fn now_v7_is_ordered_across_tasks() {
    check_random(
        || {
            // Each thread records its UUID under the lock it creates it under, so the log is in
            // creation order.
            let log = Arc::new(Mutex::new(Vec::new()));
            let threads = (0..4)
                .map(|_| {
                    let log = log.clone();
                    thread::spawn(move || {
                        for _ in 0..3 {
                            let mut log = log.lock().unwrap();
                            log.push(Uuid::now_v7());
                        }
                    })
                })
                .collect::<Vec<_>>();
            for thread in threads {
                thread.join().unwrap();
            }
            let log = log.lock().unwrap();
            assert!(log.is_sorted(), "{log:?}");
        },
        100,
    );
}

#[test]
fn shared_contexts_start_over_in_every_execution() {
    let log = std::sync::Arc::new(std::sync::Mutex::new(HashSet::new()));
    let log_clone = log.clone();
    check_random_with_seed(
        move || {
            // If the clock or the contexts carried over from one execution to the next, the second
            // run below would start from what this one left behind, and differ.
            let first = Uuid::now_v7();
            let clock_seq = Uuid::now_v1(&[0; 6]).get_timestamp().unwrap().to_gregorian().1;
            log_clone.lock().unwrap().insert((first, clock_seq));
        },
        7,
        50,
    );
    let first_run = std::mem::take(&mut *log.lock().unwrap());
    let log_clone = log.clone();
    check_random_with_seed(
        move || {
            let first = Uuid::now_v7();
            let clock_seq = Uuid::now_v1(&[0; 6]).get_timestamp().unwrap().to_gregorian().1;
            log_clone.lock().unwrap().insert((first, clock_seq));
        },
        7,
        50,
    );
    assert_eq!(*log.lock().unwrap(), first_run);
}

/// Each of 10 threads creates a UUID with `new_uuid`, and takes the lock only if the UUID's last bit
/// is set, so the UUIDs decide which threads contend for the lock and how the schedule goes.
fn lock_if_last_bit_set(new_uuid: fn() -> [u8; 16]) {
    let lock = Arc::new(Mutex::new(0u64));
    let threads = (0..10)
        .map(|_| {
            let lock = lock.clone();
            thread::spawn(move || {
                if new_uuid()[15] & 1 == 1 {
                    *lock.lock().unwrap() += 1;
                }
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().unwrap();
    }
}

#[test]
fn schedule_that_depends_on_new_v4_replays() {
    check_uncontrolled_nondeterminism(|| lock_if_last_bit_set(|| Uuid::new_v4().into_bytes()), 1000);
}

#[test]
fn schedule_that_depends_on_now_v7_replays() {
    check_uncontrolled_nondeterminism(|| lock_if_last_bit_set(|| Uuid::now_v7().into_bytes()), 1000);
}

#[test]
#[should_panic(expected = "possible nondeterminism")]
fn schedule_that_depends_on_uuids_own_new_v4_does_not_replay() {
    check_uncontrolled_nondeterminism(|| lock_if_last_bit_set(|| uuid_orig::Uuid::new_v4().into_bytes()), 1000);
}

#[test]
#[should_panic(expected = "possible nondeterminism")]
fn schedule_that_depends_on_uuids_own_now_v7_does_not_replay() {
    check_uncontrolled_nondeterminism(|| lock_if_last_bit_set(|| uuid_orig::Uuid::now_v7().into_bytes()), 1000);
}

/// Makes one of each kind of UUID that draws randomness or reads the clock.
fn one_of_each() {
    let _ = Uuid::new_v4();
    let _ = Uuid::now_v7();
    let _ = Uuid::new_v7(Timestamp::from_unix(NoContext, 1_497_624_119, 0));
    let _ = Uuid::now_v1(&[0; 6]);
    let _ = Uuid::now_v6(&[0; 6]);
    let _ = NonNilUuid::new_v4();
    let _ = Timestamp::now(NoContext);
    let _ = ContextV1::new_random();
    let context = ContextV7::new();
    let _ = Uuid::new_v7(Timestamp::now(&context));
}

#[test]
fn under_dfs_uuids_are_made_without_random_data() {
    check_dfs(
        || {
            // Under `check_dfs` a draw from Shuttle's generator would fail the test, and a value from
            // it would be a step of the schedule.
            let before = CurrentSchedule::len();
            one_of_each();
            assert_eq!(CurrentSchedule::len(), before);
        },
        None,
    );
}

#[test]
fn dfs_with_random_data_draws_from_its_generator() {
    // The source of the bits follows the scheduler: a `DfsScheduler` that provides random data
    // provides them, as a step of the schedule each.
    Runner::new(DfsScheduler::new(None, true), Config::new()).run(|| {
        let before = CurrentSchedule::len();
        let _ = Uuid::new_v4();
        assert_eq!(CurrentSchedule::len(), before + 2, "16 bytes are two values");
    });
}

/// How many executions `check_dfs` explores for two threads that each take a lock twice, and make
/// UUIDs under it each time if `make`.
fn dfs_executions(make: bool) -> usize {
    let executions = std::sync::Arc::new(AtomicUsize::new(0));
    let counter = executions.clone();
    check_dfs(
        move || {
            counter.fetch_add(1, Ordering::SeqCst);
            let lock = Arc::new(Mutex::new(Vec::new()));
            let threads = (0..2)
                .map(|_| {
                    let lock = lock.clone();
                    thread::spawn(move || {
                        for _ in 0..2 {
                            let mut made = lock.lock().unwrap();
                            if make {
                                made.push(Uuid::new_v4());
                                made.push(Uuid::now_v7());
                            }
                        }
                    })
                })
                .collect::<Vec<_>>();
            for thread in threads {
                thread.join().unwrap();
            }
            let made = lock.lock().unwrap();
            assert_eq!(made.iter().collect::<HashSet<_>>().len(), made.len());
        },
        None,
    );
    executions.load(Ordering::SeqCst)
}

#[test]
fn under_dfs_making_uuids_adds_no_schedules() {
    let without = dfs_executions(false);
    assert!(without > 1, "{without}");
    assert_eq!(dfs_executions(true), without);
}

#[test]
fn under_dfs_the_uuids_of_an_execution_differ() {
    check_dfs(
        || {
            // Two threads, so that the UUIDs come from both, in the order the schedule decides.
            let made = uuids_from_threads(2, Uuid::new_v4)
                .into_iter()
                .chain((0..100_000).map(|_| Uuid::new_v4()))
                .chain((0..1_000).map(|_| Uuid::now_v7()))
                .collect::<Vec<_>>();
            assert_eq!(made.iter().collect::<HashSet<_>>().len(), made.len());
            assert!(made.iter().all(|uuid| uuid.get_variant() == Variant::RFC4122));
        },
        None,
    );
}

#[test]
fn under_dfs_the_executions_generator_starts_over() {
    // The execution's own generator is SplitMix64 seeded with zero, so the first version 4 UUID of
    // every execution has its first two values as its random bytes.
    let mut bytes = [0; 16];
    bytes[..8].copy_from_slice(&0xE220_A839_7B1D_CDAFu64.to_le_bytes());
    bytes[8..].copy_from_slice(&0x6E78_9E6A_A1B9_65F4u64.to_le_bytes());
    let first = Builder::from_random_bytes(bytes).into_uuid();

    let executions = std::sync::Arc::new(AtomicUsize::new(0));
    let counter = executions.clone();
    check_dfs(
        move || {
            counter.fetch_add(1, Ordering::SeqCst);
            assert_eq!(Uuid::new_v4(), first);
            // Threads that make UUIDs too, so that there are executions to start over in.
            let _ = uuids_from_threads(2, Uuid::new_v4);
        },
        None,
    );
    assert!(executions.load(Ordering::SeqCst) > 1);
}

/// Runs `check_dfs` and returns the UUIDs that each execution made, in the order it made them.
fn uuids_under_dfs(new_uuid: fn() -> Uuid) -> Vec<Vec<Uuid>> {
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = log.clone();
    check_dfs(move || sink.lock().unwrap().push(uuids_from_threads(3, new_uuid)), None);
    log.lock().unwrap().clone()
}

#[test]
fn under_dfs_the_same_schedule_gives_the_same_uuids() {
    for new_uuid in [Uuid::new_v4, Uuid::now_v7, || Uuid::now_v1(&[0; 6])] {
        let first = uuids_under_dfs(new_uuid);
        assert!(first.len() > 1);
        assert_eq!(uuids_under_dfs(new_uuid), first);
    }
}

#[test]
fn under_dfs_the_clock_moves_one_millisecond_per_reading() {
    check_dfs(
        || {
            let readings = [
                millis_since_epoch(Timestamp::now(NoContext)),
                millis_since_epoch(Uuid::now_v7().get_timestamp().unwrap()),
                millis_since_epoch(Uuid::now_v1(&[0; 6]).get_timestamp().unwrap()),
                millis_since_epoch(Uuid::now_v6(&[0; 6]).get_timestamp().unwrap()),
                millis_since_epoch(Timestamp::now(NoContext)),
            ];
            assert_eq!(readings, [0, 1, 2, 3, 4]);
        },
        None,
    );
}

#[test]
fn under_dfs_now_v7_is_ordered_across_tasks() {
    check_dfs(
        || {
            let log = Arc::new(Mutex::new(Vec::new()));
            let threads = (0..2)
                .map(|_| {
                    let log = log.clone();
                    thread::spawn(move || {
                        for _ in 0..2 {
                            let mut log = log.lock().unwrap();
                            log.push(Uuid::now_v7());
                        }
                    })
                })
                .collect::<Vec<_>>();
            for thread in threads {
                thread.join().unwrap();
            }
            let log = log.lock().unwrap();
            assert!(log.is_sorted(), "{log:?}");
            let millis = log
                .iter()
                .map(|uuid| millis_since_epoch(uuid.get_timestamp().unwrap()))
                .collect::<Vec<_>>();
            assert_eq!(millis, [0, 1, 2, 3], "no two share a millisecond");
        },
        None,
    );
}

#[test]
fn under_pct_the_clock_moves_by_zero_or_one_millisecond() {
    let steps = std::sync::Arc::new(std::sync::Mutex::new(HashSet::new()));
    let sink = steps.clone();
    shuttle::check_pct(
        move || {
            // PCT insists on a test with a scheduling choice.
            thread::spawn(|| {});
            let first = millis_since_epoch(Uuid::now_v7().get_timestamp().unwrap());
            let second = millis_since_epoch(Uuid::now_v7().get_timestamp().unwrap());
            sink.lock().unwrap().insert(second - first);
        },
        200,
        2,
    );
    assert_eq!(*steps.lock().unwrap(), HashSet::from([0, 1]));
}

#[test]
fn a_failing_dfs_schedule_replays_with_the_same_uuids() {
    // The UUIDs of the latest execution, and its schedule.
    static LOG: std::sync::Mutex<(Vec<Uuid>, String)> = std::sync::Mutex::new((Vec::new(), String::new()));

    // Fails in the schedules where the UUID that the first thread makes is the larger of the two.
    fn first_threads_uuid_is_smaller() {
        let made = Arc::new(Mutex::new([None, None]));
        let threads = (0..2)
            .map(|index| {
                let made = made.clone();
                thread::spawn(move || {
                    let mut made = made.lock().unwrap();
                    made[index] = Some(Uuid::new_v4());
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }
        let made = made.lock().unwrap().map(Option::unwrap).to_vec();
        *LOG.lock().unwrap() = (made.clone(), serialize_schedule(&CurrentSchedule::get_schedule()));
        assert!(made[0] < made[1], "the first thread's UUID is the larger");
    }

    panic::catch_unwind(AssertUnwindSafe(|| check_dfs(first_threads_uuid_is_smaller, None)))
        .expect_err("test should fail");
    let (failing, schedule) = LOG.lock().unwrap().clone();

    LOG.lock().unwrap().0.clear();
    panic::catch_unwind(|| shuttle::replay(first_threads_uuid_is_smaller, &schedule)).expect_err("replay should fail");
    assert_eq!(LOG.lock().unwrap().0, failing);
}

#[test]
fn under_dfs_a_schedule_that_depends_on_new_v4_replays() {
    // The first 500 of the schedules of ten threads, each of which the checker runs twice.
    let scheduler = UncontrolledNondeterminismCheckScheduler::new(DfsScheduler::new(Some(500), false));
    Runner::new(scheduler, Config::new()).run(|| lock_if_last_bit_set(|| Uuid::new_v4().into_bytes()));
}

/// Stands in for a crate built on uuid itself.
fn echo(uuid: uuid_orig::Uuid) -> uuid_orig::Uuid {
    uuid
}

#[test]
fn converts_to_and_from_uuids_own_types() {
    check_random(
        || {
            let uuid = Uuid::new_v4();
            let back: Uuid = echo(uuid.into()).into();
            assert_eq!(back, uuid);
            assert_eq!(uuid_orig::Uuid::from(uuid).as_bytes(), uuid.as_bytes());

            let non_nil = NonNilUuid::new_v4();
            assert_eq!(NonNilUuid::from(uuid_orig::NonNilUuid::from(non_nil)), non_nil);

            let hyphenated = uuid.hyphenated();
            let orig: uuid_orig::fmt::Hyphenated = hyphenated.into();
            assert_eq!(orig.to_string(), hyphenated.to_string());
            assert_eq!(shuttle_uuid_inner::fmt::Hyphenated::from(orig), hyphenated);

            let builder = shuttle_uuid_inner::Builder::from_random_bytes(*uuid.as_bytes());
            let orig: uuid_orig::Builder = builder.into();
            assert_eq!(
                orig.as_uuid().as_bytes(),
                uuid_orig::Builder::from_random_bytes(*uuid.as_bytes())
                    .as_uuid()
                    .as_bytes()
            );

            // A timestamp converts to uuid's with its counter, which uuid's `new_v7` places as this
            // crate's does.
            let context = ContextV7::new();
            let ts = Timestamp::from_unix(&context, 1_497_624_119, 1_234);
            let ours = Uuid::new_v7(Timestamp::from_unix(&context, 1_497_624_119, 1_234));
            let theirs = uuid_orig::Uuid::new_v7(ts.into());
            assert_eq!(ours.as_bytes()[..6], theirs.as_bytes()[..6]);
            assert!(
                theirs < uuid_orig::Uuid::from(ours),
                "the second counter is the larger one"
            );
        },
        10,
    );
}

#[test]
fn behaves_as_uuid_where_deterministic() {
    let text = "67e55044-10b1-426f-9247-bb680e5fe0c8";
    let ours = Uuid::parse_str(text).unwrap();
    let theirs = uuid_orig::Uuid::parse_str(text).unwrap();
    assert_eq!(
        serde_json::to_string(&ours).unwrap(),
        serde_json::to_string(&theirs).unwrap()
    );
    assert_eq!(
        format!("{ours:?} {ours:x} {ours:X}"),
        format!("{theirs:?} {theirs:x} {theirs:X}")
    );
    let hash = |value: &dyn Fn(&mut std::collections::hash_map::DefaultHasher)| {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        value(&mut hasher);
        std::hash::Hasher::finish(&hasher)
    };
    assert_eq!(
        hash(&|h| std::hash::Hash::hash(&ours, h)),
        hash(&|h| std::hash::Hash::hash(&theirs, h))
    );

    // Timestamps decoded from UUIDs of each time-based version match uuid's.
    for orig in [
        uuid_orig::Uuid::new_v1(
            uuid_orig::Timestamp::from_unix(uuid_orig::ContextV1::new(7), 1_497_624_119, 1_234),
            &[1; 6],
        ),
        uuid_orig::Uuid::new_v6(
            uuid_orig::Timestamp::from_unix(uuid_orig::ContextV1::new(7), 1_497_624_119, 1_234),
            &[1; 6],
        ),
        uuid_orig::Uuid::new_v7(uuid_orig::Timestamp::from_unix(
            uuid_orig::NoContext,
            1_497_624_119,
            1_234,
        )),
    ] {
        let ours = Uuid::from(orig);
        assert_eq!(
            uuid_orig::Timestamp::from(ours.get_timestamp().unwrap()),
            orig.get_timestamp().unwrap()
        );
    }

    // So do the deterministic constructors.
    let ts = Timestamp::from_unix(ContextV1::new(7), 1_497_624_119, 1_234);
    let orig_ts = uuid_orig::Timestamp::from_unix(uuid_orig::ContextV1::new(7), 1_497_624_119, 1_234);
    assert_eq!(uuid_orig::Timestamp::from(ts), orig_ts);
    assert_eq!(
        Uuid::new_v1(ts, &[1; 6]),
        Uuid::from(uuid_orig::Uuid::new_v1(orig_ts, &[1; 6]))
    );
    assert_eq!(
        Uuid::new_v6(ts, &[1; 6]),
        Uuid::from(uuid_orig::Uuid::new_v6(orig_ts, &[1; 6]))
    );
    let name = b"example.org";
    let orig = uuid_orig::Uuid::new_v5(&uuid_orig::Uuid::NAMESPACE_URL, name);
    assert_eq!(Uuid::new_v5(&Uuid::NAMESPACE_URL, name), Uuid::from(orig));
}

#[test]
#[should_panic(expected = "Are you accessing a Shuttle primitive outside of a Shuttle test?")]
fn new_v4_outside_of_shuttle_panics() {
    Uuid::new_v4();
}

#[test]
#[should_panic(expected = "Are you accessing a Shuttle primitive outside of a Shuttle test?")]
fn now_v7_outside_of_shuttle_panics() {
    Uuid::now_v7();
}

#[test]
#[should_panic(expected = "Are you accessing a Shuttle primitive outside of a Shuttle test?")]
fn timestamp_now_outside_of_shuttle_panics() {
    Timestamp::now(NoContext);
}

#[test]
#[should_panic(expected = "Are you accessing a Shuttle primitive outside of a Shuttle test?")]
fn new_random_context_outside_of_shuttle_panics() {
    ContextV1::new_random();
}

#[test]
fn deterministic_constructors_work_outside_of_shuttle() {
    let uuid = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"example.org");
    assert_eq!(uuid.to_string(), "aad03681-8b63-5304-89e0-8ca8f49461b5");
    let ts = Timestamp::from_unix(ContextV1::new(0), 1, 0);
    assert_eq!(Uuid::new_v1(ts, &[0; 6]).get_timestamp(), Some(ts));
}
