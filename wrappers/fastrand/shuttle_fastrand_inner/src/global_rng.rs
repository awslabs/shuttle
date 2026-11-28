//! fastrand's free functions, on a generator that belongs to the running Shuttle task.
//!
//! fastrand keeps one generator per thread, in a `thread_local!`, seeded from the clock and the thread's id. Here
//! the generator lives in a Shuttle thread-local, so each task has its own, every execution starts without one,
//! and the first use in a task seeds it from Shuttle's random number generator. The functions then use it as
//! fastrand's do, so [`seed`] followed by a draw gives what `Rng::with_seed` followed by the same draw gives.

use crate::{Rng, source};
use std::cell::Cell;
use std::ops::RangeBounds;

shuttle::thread_local! {
    /// The state of the running task's generator, or `None` until the task first uses or seeds it.
    static RNG: Cell<Option<u64>> = Cell::new(None);
}

/// Runs `f` on the running task's generator, seeding it from Shuttle first if the task has not used it yet.
#[inline]
fn with_rng<R>(f: impl FnOnce(&mut fastrand_orig::Rng) -> R) -> R {
    RNG.with(|slot| run(slot, f))
}

#[inline]
fn run<R>(slot: &Cell<Option<u64>>, f: impl FnOnce(&mut fastrand_orig::Rng) -> R) -> R {
    // As fastrand does, leave a placeholder in the slot while `f` runs, and write the generator back when it is
    // done, also if `f` panics (on an empty range, say).
    let state = slot.replace(Some(0)).unwrap_or_else(source::next_u64);
    let mut restore = RestoreOnDrop {
        slot,
        current: fastrand_orig::Rng::with_seed(state),
    };
    f(&mut restore.current)
}

struct RestoreOnDrop<'a> {
    slot: &'a Cell<Option<u64>>,
    current: fastrand_orig::Rng,
}

impl Drop for RestoreOnDrop<'_> {
    fn drop(&mut self) {
        self.slot.set(Some(self.current.get_seed()));
    }
}

/// [`Rng::new`]: a fork of the running task's generator. Like fastrand's, it does not panic in a destructor that
/// runs after the task's thread-locals are gone; it seeds the new generator from Shuttle instead.
pub(crate) fn fork_task_rng() -> Rng {
    RNG.try_with(|slot| run(slot, |rng| Rng::from(rng.fork())))
        .unwrap_or_else(|_| Rng::with_seed(source::next_u64()))
}

/// Initializes the running task's generator with the given seed.
///
/// Unlike the other functions, this draws nothing from Shuttle, so it works under `shuttle::check_dfs`, and
/// so do the draws after it.
#[inline]
pub fn seed(seed: u64) {
    RNG.with(|slot| slot.set(Some(seed)));
}

/// Gives back **current** seed that is being held by the running task's generator.
#[inline]
pub fn get_seed() -> u64 {
    with_rng(|r| r.get_seed())
}

/// Generates a random `bool`.
#[inline]
pub fn bool() -> bool {
    with_rng(|r| r.bool())
}

/// Generates a random `char` in ranges a-z and A-Z.
#[inline]
pub fn alphabetic() -> char {
    with_rng(|r| r.alphabetic())
}

/// Generates a random `char` in ranges a-z, A-Z and 0-9.
#[inline]
pub fn alphanumeric() -> char {
    with_rng(|r| r.alphanumeric())
}

/// Generates a random `char` in range a-z.
#[inline]
pub fn lowercase() -> char {
    with_rng(|r| r.lowercase())
}

/// Generates a random `char` in range A-Z.
#[inline]
pub fn uppercase() -> char {
    with_rng(|r| r.uppercase())
}

/// Choose an item from an iterator at random.
///
/// This function may have an unexpected result if the `len()` property of the iterator does not match the actual
/// number of items in the iterator. If the iterator is empty, this returns `None`.
#[inline]
pub fn choice<I>(iter: I) -> Option<I::Item>
where
    I: IntoIterator,
    I::IntoIter: ExactSizeIterator,
{
    with_rng(|r| r.choice(iter))
}

/// Generates a random digit in the given `base`.
///
/// Digits are represented by `char`s in ranges 0-9 and a-z.
///
/// Panics if the base is zero or greater than 36.
#[inline]
pub fn digit(base: u32) -> char {
    with_rng(|r| r.digit(base))
}

/// Shuffles a slice randomly.
#[inline]
pub fn shuffle<T>(slice: &mut [T]) {
    with_rng(|r| r.shuffle(slice))
}

/// Fill a byte slice with random data.
#[inline]
pub fn fill(slice: &mut [u8]) {
    with_rng(|r| r.fill(slice))
}

macro_rules! integer {
    ($($t:tt),*) => {
        $(
            #[doc = concat!("Generates a random `", stringify!($t), "` in the given range.")]
            ///
            /// Panics if the range is empty.
            #[inline]
            pub fn $t(range: impl RangeBounds<$t>) -> $t {
                with_rng(|r| r.$t(range))
            }
        )*
    };
}

integer!(u8, i8, u16, i16, u32, i32, u64, i64, u128, i128, usize, isize, char);

/// Generates a random `f32` in range `0..1`.
pub fn f32() -> f32 {
    with_rng(|r| r.f32())
}

/// Generates a random `f32` in range `0..=1`.
pub fn f32_inclusive() -> f32 {
    with_rng(|r| r.f32_inclusive())
}

/// Generates a random `f64` in range `0..1`.
pub fn f64() -> f64 {
    with_rng(|r| r.f64())
}

/// Generates a random `f64` in range `0..=1`.
pub fn f64_inclusive() -> f64 {
    with_rng(|r| r.f64_inclusive())
}

/// Collects `amount` values at random from the iterable into a vector.
///
/// The length of the returned vector equals `amount` unless the iterable contains insufficient elements, in which
/// case it equals the number of elements available.
pub fn choose_multiple<I: IntoIterator>(source: I, amount: usize) -> Vec<I::Item> {
    with_rng(|rng| rng.choose_multiple(source, amount))
}
