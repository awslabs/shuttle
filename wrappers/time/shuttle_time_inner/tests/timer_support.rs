//! The hooks that Shuttle's other wrappers model timers with: reading the clock without advancing
//! it, and moving it forward to a deadline.

use shuttle_engine::runtime::execution::CurrentSchedule;
use shuttle_time_inner::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use shuttle_time_inner::timer_support::{advance_to, peek, since_origin};

#[test]
fn peeking_neither_advances_the_clock_nor_draws() {
    shuttle::check_random(
        || {
            let before = CurrentSchedule::get_schedule().steps.len();
            let first = peek();
            for _ in 0..10 {
                assert_eq!(peek(), first);
            }
            assert_eq!(CurrentSchedule::get_schedule().steps.len(), before);
            // The next reading is the one the peeks saw.
            assert_eq!(Instant::now(), first);
        },
        20,
    );
}

#[test]
fn advancing_moves_the_clock_forward_only() {
    shuttle::check_random(
        || {
            let start = Instant::now();
            let deadline = start + Duration::from_secs(5);
            advance_to(deadline);
            assert_eq!(peek(), deadline);
            let after = Instant::now();
            assert!(after >= deadline);
            assert!(after.duration_since(start) >= Duration::from_secs(5));

            // Both clocks read the same counter, so the system clock moved too.
            let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
            let start_since_epoch = Duration::from_secs(1_767_225_600);
            assert!(since_epoch >= start_since_epoch + Duration::from_secs(5));

            // A deadline the clock has passed already leaves it where it is.
            let now = peek();
            advance_to(start);
            assert_eq!(peek(), now);
        },
        20,
    );
}

#[test]
fn the_clock_starts_over_after_an_advance() {
    for _ in 0..2 {
        shuttle::check_random(
            || {
                assert_eq!(
                    SystemTime::now().duration_since(UNIX_EPOCH).unwrap(),
                    Duration::from_secs(1_767_225_600)
                );
                advance_to(peek() + Duration::from_secs(3600));
            },
            5,
        );
    }
}

#[test]
fn deadlines_order_by_their_distance_from_the_origin() {
    shuttle::check_random(
        || {
            let now = Instant::now();
            let soon = now + Duration::from_millis(1);
            let later = now + Duration::from_secs(1);
            assert!(since_origin(now) < since_origin(soon));
            assert!(since_origin(soon) < since_origin(later));
            assert_eq!(
                since_origin(later) - since_origin(soon),
                Duration::from_secs(1) - Duration::from_millis(1)
            );
        },
        5,
    );
}
