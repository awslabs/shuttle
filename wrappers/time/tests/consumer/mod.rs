//! Code written against `std::time`, as a crate that depends on `shuttle-time` and swapped
//! `std::time` for `shuttle_time::time` would have it. The same source is compiled against
//! `shuttle-time` without the `shuttle` feature, where that module is `std::time`, and against
//! `shuttle-time-inner`, which is what the module is under `shuttle`. The test that includes it
//! re-exports the module as `crate::time`, and provides `real_crate`, which stands in for a crate
//! built on `std::time` itself.
//!
//! The checks hold whichever module `time` is, so they assume nothing about the actual time.

use crate::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::collections::{BTreeSet, HashSet};

/// Exercises the clocks' API.
pub fn exercise() {
    instants();
    instant_arithmetic();
    system_times();
    system_time_arithmetic();
    ordering_and_hashing();
    durations_are_stds();
    boundary();
}

/// What a consumer most often writes: start a stopwatch and read it.
fn instants() {
    let start = Instant::now();
    let elapsed = start.elapsed();
    // Nothing here took a minute, whichever clock this is.
    assert!(elapsed < Duration::from_secs(60), "{elapsed:?}");

    let first = Instant::now();
    let second = Instant::now();
    assert!(second >= first);
    assert_eq!(second.duration_since(first), second - first);
    assert_eq!(second.saturating_duration_since(first), second - first);
    assert_eq!(second.checked_duration_since(first), Some(second - first));
    if second > first {
        assert!(first.checked_duration_since(second).is_none());
        assert_eq!(first.saturating_duration_since(second), Duration::ZERO);
        assert_eq!(first.duration_since(second), Duration::ZERO);
    } else {
        assert_eq!(first.checked_duration_since(second), Some(Duration::ZERO));
    }
}

fn instant_arithmetic() {
    let now = Instant::now();
    let minute = Duration::from_secs(60);

    let later = now + minute;
    assert_eq!(later.duration_since(now), minute);
    assert_eq!(later - minute, now);
    assert_eq!(now.checked_add(minute), Some(later));

    // A clock reading is far enough from the start of its own scale to subtract a minute from it.
    let earlier = now - minute;
    assert_eq!(now.duration_since(earlier), minute);
    assert_eq!(now.checked_sub(minute), Some(earlier));
    assert!(earlier < now);

    let mut moved = now;
    moved += minute;
    assert_eq!(moved, later);
    moved -= minute;
    assert_eq!(moved, now);
}

fn system_times() {
    let now = SystemTime::now();

    // The `shuttle` feature does not move the Unix epoch, only what `now()` reads.
    assert_eq!(UNIX_EPOCH, SystemTime::UNIX_EPOCH);
    let since_epoch = now.duration_since(UNIX_EPOCH).expect("now is after 1970");
    assert!(since_epoch > Duration::from_secs(1_600_000_000), "{since_epoch:?}");

    let elapsed = now.elapsed().expect("now does not go backwards");
    assert!(elapsed < Duration::from_secs(60), "{elapsed:?}");

    // Reading the clock again gives a time no earlier than this one, so asking for the duration
    // from the later reading to this one either is zero or fails.
    let later = SystemTime::now();
    match now.duration_since(later) {
        Ok(duration) => assert_eq!(duration, Duration::ZERO),
        Err(error) => assert!(error.duration() > Duration::ZERO),
    }
}

fn system_time_arithmetic() {
    let now = SystemTime::now();
    let minute = Duration::from_secs(60);

    let later = now + minute;
    assert_eq!(later.duration_since(now).expect("later is after now"), minute);
    assert_eq!(later - minute, now);
    assert_eq!(now.checked_add(minute), Some(later));

    let earlier = now - minute;
    assert_eq!(now.duration_since(earlier).expect("now is after a minute ago"), minute);
    assert_eq!(now.checked_sub(minute), Some(earlier));
    assert!(earlier < now);

    let mut moved = now;
    moved += minute;
    assert_eq!(moved, later);
    moved -= minute;
    assert_eq!(moved, now);
}

fn ordering_and_hashing() {
    let instants: Vec<Instant> = (0..4).map(|_| Instant::now()).collect();
    let system_times: Vec<SystemTime> = (0..4).map(|_| SystemTime::now()).collect();

    // Readings come out of a sorted set in the order they were taken, and equal ones collapse.
    let sorted: Vec<Instant> = instants.iter().copied().collect::<BTreeSet<_>>().into_iter().collect();
    let mut deduplicated = instants.clone();
    deduplicated.dedup();
    assert_eq!(sorted, deduplicated);
    assert_eq!(
        instants.iter().copied().collect::<HashSet<_>>().len(),
        deduplicated.len()
    );

    assert!(instants.windows(2).all(|pair| pair[0] <= pair[1]));
    assert!(system_times.windows(2).all(|pair| pair[0] <= pair[1]));
    assert_eq!(instants.iter().max(), instants.last());
    assert_eq!(system_times.iter().max(), system_times.last());

    // `Debug` says something, whichever clock this is.
    assert!(!format!("{:?} {:?}", instants[0], system_times[0]).is_empty());
}

/// `Duration` and the error types are `std::time`'s in both modes, so they need no conversion.
fn durations_are_stds() {
    let duration: std::time::Duration = Duration::from_millis(1);
    assert_eq!(duration, Duration::from_micros(1_000));

    let error: std::time::SystemTimeError = (SystemTime::now() - Duration::from_secs(1))
        .duration_since(SystemTime::now())
        .expect_err("a second ago is before now");
    assert!(error.duration() > Duration::ZERO);
}

/// Handing a system time to a crate built on `std::time`, and taking one back.
// Without the `shuttle` feature the conversions are from a type to itself, which Clippy reports.
#[allow(clippy::useless_conversion)]
fn boundary() {
    let now = SystemTime::now();

    let handed_over: std::time::SystemTime = now.into();
    let returned: SystemTime = crate::real_crate::echo(handed_over).into();
    assert_eq!(returned, now);

    let record = crate::real_crate::Record { at: now.into() };
    assert_eq!(SystemTime::from(record.at), now);
}
