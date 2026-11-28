use std::fmt;
use std::ops::RangeBounds;

/// A random number generator: fastrand's, whose [`new`](Rng::new) forks the running task's generator, which
/// Shuttle seeds, where fastrand's forks the thread's.
///
/// Every other method is fastrand's own, so a generator made with [`with_seed`](Rng::with_seed) produces exactly
/// the values it produces with fastrand, and works outside of a Shuttle test too. The type converts to and from
/// fastrand's with `From`, for code that hands a generator to a crate built on fastrand.
#[derive(PartialEq, Eq)]
pub struct Rng(fastrand_orig::Rng);

impl Rng {
    /// Creates a new random number generator, forked from the running task's generator, which Shuttle seeds (see
    /// the [crate docs](crate)). fastrand's forks the thread's generator, which it seeds from the clock.
    ///
    /// Panics outside of a Shuttle test.
    #[inline]
    pub fn new() -> Rng {
        crate::global_rng::fork_task_rng()
    }

    /// Creates a new random number generator with the initial seed.
    #[inline]
    #[must_use = "this creates a new instance of `Rng`; if you want to initialize the thread-local generator, use `fastrand::seed()` instead"]
    pub const fn with_seed(seed: u64) -> Self {
        Rng(fastrand_orig::Rng::with_seed(seed))
    }

    /// Clones the generator by deterministically deriving a new generator based on the initial seed.
    #[inline]
    #[must_use = "this creates a new instance of `Rng`"]
    pub fn fork(&mut self) -> Self {
        Rng(self.0.fork())
    }

    /// Generates a random `char` in ranges a-z and A-Z.
    #[inline]
    pub fn alphabetic(&mut self) -> char {
        self.0.alphabetic()
    }

    /// Generates a random `char` in ranges a-z, A-Z and 0-9.
    #[inline]
    pub fn alphanumeric(&mut self) -> char {
        self.0.alphanumeric()
    }

    /// Generates a random `bool`.
    #[inline]
    pub fn bool(&mut self) -> bool {
        self.0.bool()
    }

    /// Generates a random digit in the given `base`.
    ///
    /// Digits are represented by `char`s in ranges 0-9 and a-z.
    ///
    /// Panics if the base is zero or greater than 36.
    #[inline]
    pub fn digit(&mut self, base: u32) -> char {
        self.0.digit(base)
    }

    /// Generates a random `f32` in range `0..=1`.
    #[inline]
    pub fn f32_inclusive(&mut self) -> f32 {
        self.0.f32_inclusive()
    }

    /// Generates a random `f32` in range `0..1`.
    #[inline]
    pub fn f32(&mut self) -> f32 {
        self.0.f32()
    }

    /// Generates a random `f64` in range `0..=1`.
    #[inline]
    pub fn f64_inclusive(&mut self) -> f64 {
        self.0.f64_inclusive()
    }

    /// Generates a random `f64` in range `0..1`.
    #[inline]
    pub fn f64(&mut self) -> f64 {
        self.0.f64()
    }

    /// Collects `amount` values at random from the iterable into a vector.
    ///
    /// The length of the returned vector equals `amount` unless the iterable contains insufficient elements, in
    /// which case it equals the number of elements available.
    #[inline]
    pub fn choose_multiple<I: IntoIterator>(&mut self, source: I, amount: usize) -> Vec<I::Item> {
        self.0.choose_multiple(source, amount)
    }

    /// Generates a random lowercase ASCII `char` in the range a-z.
    #[inline]
    pub fn lowercase(&mut self) -> char {
        self.0.lowercase()
    }

    /// Initializes this generator with the given seed.
    #[inline]
    pub fn seed(&mut self, seed: u64) {
        self.0.seed(seed)
    }

    /// Gives back **current** seed that is being held by this generator.
    #[inline]
    pub fn get_seed(&self) -> u64 {
        self.0.get_seed()
    }

    /// Choose an item from an iterator at random.
    ///
    /// This function may have an unexpected result if the `len()` property of the iterator does not match the
    /// actual number of items in the iterator. If the iterator is empty, this returns `None`.
    #[inline]
    pub fn choice<I>(&mut self, iter: I) -> Option<I::Item>
    where
        I: IntoIterator,
        I::IntoIter: ExactSizeIterator,
    {
        self.0.choice(iter)
    }

    /// Shuffles a slice randomly.
    #[inline]
    pub fn shuffle<T>(&mut self, slice: &mut [T]) {
        self.0.shuffle(slice)
    }

    /// Fill a byte slice with random data.
    #[inline]
    pub fn fill(&mut self, slice: &mut [u8]) {
        self.0.fill(slice)
    }

    /// Generates a random uppercase ASCII `char` in the range A-Z.
    #[inline]
    pub fn uppercase(&mut self) -> char {
        self.0.uppercase()
    }

    /// Generates a random `char` in the given range.
    ///
    /// Panics if the range is empty.
    #[inline]
    pub fn char(&mut self, range: impl RangeBounds<char>) -> char {
        self.0.char(range)
    }
}

macro_rules! integer {
    ($($t:tt),*) => {
        impl Rng {
            $(
                #[doc = concat!("Generates a random `", stringify!($t), "` in the given range.")]
                ///
                /// Panics if the range is empty.
                #[inline]
                pub fn $t(&mut self, range: impl RangeBounds<$t>) -> $t {
                    self.0.$t(range)
                }
            )*
        }
    };
}

integer!(i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize);

impl Clone for Rng {
    /// Clones the generator by creating a new generator with the same seed.
    fn clone(&self) -> Rng {
        Rng(self.0.clone())
    }
}

impl Default for Rng {
    /// Initialize the `Rng` from the running task's generator, as [`Rng::new`] does.
    #[inline]
    fn default() -> Rng {
        Rng::new()
    }
}

impl fmt::Debug for Rng {
    /// The same output as fastrand's, `Rng(<state>)`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<fastrand_orig::Rng> for Rng {
    fn from(rng: fastrand_orig::Rng) -> Self {
        Rng(rng)
    }
}

impl From<Rng> for fastrand_orig::Rng {
    fn from(rng: Rng) -> Self {
        rng.0
    }
}
