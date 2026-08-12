//! A deliberately enormous failing schedule, for eyeballing what Shuttle prints when a test with
//! many tasks and many steps fails.
//!
//! The panic is placed after every task has finished, so the printed schedule covers essentially the
//! whole execution: 300 tasks and ~420,000 steps.
//!
//! For reference, the same failure printed with each combination:
//!
//! | encoding                            | lines | columns |   chars |
//! |-------------------------------------|-------|---------|---------|
//! | fixed width + hex (the old format)  |  8777 | 1053200 | 1053200 |
//! | move-to-front + hex                 |  6444 |  773280 |  773280 |
//! | move-to-front + unicode, no marks   |  1842 |  220942 |  220942 |
//! | move-to-front + unicode, 16 marks   |   182 |   21783 |  370310 |
//! | move-to-front + unicode, 255 marks  |    13 |    1506 |  385518 |
//! | move-to-front + unicode, default    |     1 |       1 |  386647 |
//!
//! "Columns" is what the schedule costs if the terminal honours zero-width combining marks. "Chars" is
//! what it costs if the terminal renders every mark as a cell of its own instead, which some do. The
//! dense form that `ScheduleTextEncoding::Auto` selects trades the second number for the first,
//! packing the whole schedule into one cell so that it prints on a single line however long it is.
//! `marks_per_cell: 0` is the setting whose cost does not depend on the terminal at all, and it still
//! beats hex better than three to one. Under a locale that does not claim UTF-8, `Auto` falls back to
//! the hex row on its own.
//!
//! The printed schedule replays as-is: paste it into `shuttle::replay` and the same panic comes back,
//! with no need for `ReplayScheduler::set_allow_incomplete`.

use shuttle::scheduler::RandomScheduler;
use shuttle::sync::Mutex;
use shuttle::{thread, Config, MaxSteps, Runner, ScheduleEncoding, ScheduleTextEncoding};
use std::sync::Arc;

const THREADS: usize = 300;
const ITERATIONS: usize = 400;

/// Contend a lock across many threads, then fail once they have all joined.
fn many_tasks_then_panic() {
    let counter = Arc::new(Mutex::new(0usize));

    let handles = (0..THREADS)
        .map(|_| {
            let counter = Arc::clone(&counter);
            thread::spawn(move || {
                for _ in 0..ITERATIONS {
                    *counter.lock().unwrap() += 1;
                    // Yield so that every iteration is a scheduling decision, which is what makes
                    // the schedule long.
                    thread::yield_now();
                }
            })
        })
        .collect::<Vec<_>>();

    for handle in handles {
        handle.join().unwrap();
    }

    // Deliberately wrong by one, so the test fails only at the very end.
    assert_eq!(
        *counter.lock().unwrap(),
        THREADS * ITERATIONS + 1,
        "failing on purpose, after {THREADS} tasks and {ITERATIONS} iterations each"
    );
}

fn run(encoding: ScheduleEncoding, text_encoding: ScheduleTextEncoding) {
    let mut config = Config::new();
    // This execution is far longer than the default bound of 1,000,000 steps.
    config.max_steps = MaxSteps::None;
    config.schedule_encoding = encoding;
    config.schedule_text_encoding = text_encoding;

    let runner = Runner::new(RandomScheduler::new(1), config);
    runner.run(many_tasks_then_panic);
}

/// Prints the schedule using the defaults: move-to-front payload, and the alphabet
/// [`ScheduleTextEncoding::Auto`] picks for wherever the output is going.
///
/// Run with:
///   cargo test --release -p shuttle --test mod -- --ignored --nocapture demo::large_schedule::prints_default
///
/// Prefix that with `LC_ALL=C` to watch `Auto` fall back to hex.
#[test]
#[ignore = "panics on purpose to print a very large schedule; run manually with --nocapture"]
fn prints_default() {
    run(ScheduleEncoding::default(), ScheduleTextEncoding::default());
}

/// The densest form, requested explicitly rather than left to `Auto`: one cell, one column, one line.
#[test]
#[ignore = "panics on purpose to print a very large schedule; run manually with --nocapture"]
fn prints_dense() {
    run(ScheduleEncoding::default(), ScheduleTextEncoding::DENSE);
}

/// The same schedule with no combining marks, so every character is one visible column.
#[test]
#[ignore = "panics on purpose to print a very large schedule; run manually with --nocapture"]
fn prints_unicode_no_marks() {
    run(
        ScheduleEncoding::default(),
        ScheduleTextEncoding::Unicode { marks_per_cell: 0 },
    );
}

/// The same schedule with shallower mark stacking than the default, for terminals that struggle to
/// attach an unbounded number of marks to one base character.
#[test]
#[ignore = "panics on purpose to print a very large schedule; run manually with --nocapture"]
fn prints_unicode_shallow_marks() {
    run(
        ScheduleEncoding::default(),
        ScheduleTextEncoding::Unicode { marks_per_cell: 16 },
    );
}

/// The same schedule as hex, for comparison.
#[test]
#[ignore = "panics on purpose to print a very large schedule; run manually with --nocapture"]
fn prints_hex() {
    run(ScheduleEncoding::default(), ScheduleTextEncoding::Hex);
}

/// The same schedule in the old format entirely: fixed-width payload rendered as hex.
#[test]
#[ignore = "panics on purpose to print a very large schedule; run manually with --nocapture"]
fn prints_legacy() {
    run(ScheduleEncoding::FixedWidth, ScheduleTextEncoding::Hex);
}
