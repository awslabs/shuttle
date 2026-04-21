use std::iter::Iterator;
use std::time::Duration;

/// A retry strategy driven by the fibonacci series.
///
/// Each retry uses a delay which is the sum of the two previous delays.
///
/// Depending on the problem at hand, a fibonacci retry strategy might
/// perform better and lead to better throughput than the `ExponentialBackoff`
/// strategy.
///
/// See ["A Performance Comparison of Different Backoff Algorithms under Different Rebroadcast Probabilities for MANETs."](http://www.comp.leeds.ac.uk/ukpew09/papers/12.pdf)
/// for more details.
#[derive(Debug, Clone)]
pub struct FibonacciBackoff {
    curr: u64,
    next: u64,
    factor: u64,
    max_delay: Option<Duration>,
}

impl FibonacciBackoff {
    /// Constructs a new fibonacci back-off strategy,
    /// given a base duration in milliseconds.
    pub fn from_millis(millis: u64) -> FibonacciBackoff {
        FibonacciBackoff {
            curr: millis,
            next: millis,
            factor: 1u64,
            max_delay: None,
        }
    }

    /// A multiplicative factor that will be applied to the retry delay.
    ///
    /// For example, using a factor of `1000` will make each delay in units of seconds.
    ///
    /// Default factor is `1`.
    pub fn factor(mut self, factor: u64) -> FibonacciBackoff {
        self.factor = factor;
        self
    }

    /// Apply a maximum delay. No retry delay will be longer than this `Duration`.
    pub fn max_delay(mut self, duration: Duration) -> FibonacciBackoff {
        self.max_delay = Some(duration);
        self
    }
}

impl Iterator for FibonacciBackoff {
    type Item = Duration;

    fn next(&mut self) -> Option<Duration> {
        // set delay duration by applying factor
        let duration = if let Some(duration) = self.curr.checked_mul(self.factor) {
            Duration::from_millis(duration)
        } else {
            Duration::from_millis(u64::MAX)
        };

        // check if we reached max delay
        if let Some(ref max_delay) = self.max_delay {
            if duration > *max_delay {
                return Some(*max_delay);
            }
        }

        if let Some(next_next) = self.curr.checked_add(self.next) {
            self.curr = self.next;
            self.next = next_next;
        } else {
            self.curr = self.next;
            self.next = u64::MAX;
        }

        Some(duration)
    }
}

#[test]
fn returns_the_fibonacci_series_starting_at_10() {
    let mut iter = FibonacciBackoff::from_millis(10);
    assert_eq!(iter.next(), Some(Duration::from_millis(10)));
    assert_eq!(iter.next(), Some(Duration::from_millis(10)));
    assert_eq!(iter.next(), Some(Duration::from_millis(20)));
    assert_eq!(iter.next(), Some(Duration::from_millis(30)));
    assert_eq!(iter.next(), Some(Duration::from_millis(50)));
    assert_eq!(iter.next(), Some(Duration::from_millis(80)));
}

#[test]
fn saturates_at_maximum_value() {
    let mut iter = FibonacciBackoff::from_millis(u64::MAX);
    assert_eq!(iter.next(), Some(Duration::from_millis(u64::MAX)));
    assert_eq!(iter.next(), Some(Duration::from_millis(u64::MAX)));
}

#[test]
fn stops_increasing_at_max_delay() {
    let mut iter = FibonacciBackoff::from_millis(10).max_delay(Duration::from_millis(50));
    assert_eq!(iter.next(), Some(Duration::from_millis(10)));
    assert_eq!(iter.next(), Some(Duration::from_millis(10)));
    assert_eq!(iter.next(), Some(Duration::from_millis(20)));
    assert_eq!(iter.next(), Some(Duration::from_millis(30)));
    assert_eq!(iter.next(), Some(Duration::from_millis(50)));
    assert_eq!(iter.next(), Some(Duration::from_millis(50)));
}

#[test]
fn returns_max_when_max_less_than_base() {
    let mut iter = FibonacciBackoff::from_millis(20).max_delay(Duration::from_millis(10));

    assert_eq!(iter.next(), Some(Duration::from_millis(10)));
    assert_eq!(iter.next(), Some(Duration::from_millis(10)));
}

#[test]
fn can_use_factor_to_get_seconds() {
    let factor = 1000;
    let mut s = FibonacciBackoff::from_millis(1).factor(factor);

    assert_eq!(s.next(), Some(Duration::from_secs(1)));
    assert_eq!(s.next(), Some(Duration::from_secs(1)));
    assert_eq!(s.next(), Some(Duration::from_secs(2)));
}
