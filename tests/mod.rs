#![deny(warnings)]

mod asynch;
mod basic;
mod data;
mod demo;

use regex::Regex;
use std::panic;
use std::panic::UnwindSafe;

fn check_replay_roundtrip<F, R>(test_func: F, replay_func: R)
where
    F: FnOnce() + UnwindSafe,
    R: FnOnce(&str) + UnwindSafe,
{
    let re = Regex::new("schedule: \"([0-9a-f]+)\"").unwrap();

    // Run the test that should fail and capture the schedule it prints
    let result = panic::catch_unwind(test_func).expect_err("test should panic");
    let output = result.downcast::<String>().unwrap();
    let schedule = &re.captures(output.as_str()).expect("output should contain a schedule")[1];

    // Now replay that schedule and make sure it still fails and outputs the same schedule
    let result = panic::catch_unwind(|| replay_func(schedule)).expect_err("replay should panic");
    let new_output = result.downcast::<String>().unwrap();
    let new_schedule = &re
        .captures(new_output.as_str())
        .expect("output should contain a schedule")[1];

    assert_eq!(new_schedule, schedule);
    // This might be too strong a check, but seems reasonable: the panics should be identical
    assert_eq!(new_output, output);
}
