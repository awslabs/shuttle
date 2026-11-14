//! Code written against fastrand, as a crate that depends on `shuttle-fastrand` under the name `fastrand` would
//! have it. The same source is compiled against `shuttle-fastrand` without the `shuttle` feature, where it is
//! fastrand, and against `shuttle-fastrand-inner`, which is what `shuttle-fastrand` is under `shuttle`. The test
//! that includes it names the crate `fastrand`, and has fastrand itself as `fastrand_orig`.
//!
//! The checks hold whichever crate `fastrand` is, so they assume nothing about the values drawn, only about their
//! ranges and about what seeded generators produce.

use fastrand::Rng;
use std::ops::Range;

/// A seeded generator, built in a constant as `Rng::with_seed` allows.
const SEEDED: Rng = Rng::with_seed(0x5eed);

/// Exercises fastrand's API.
pub fn exercise() {
    signatures();
    traits();
    free_functions();
    seeded_generators_are_fastrands();
    seed_and_the_free_functions();
    new_forks_the_generator_behind_the_free_functions();
    boundary();
}

/// The free functions and constructors, with the signatures they have in fastrand.
fn signatures() {
    let _: fn(u64) = fastrand::seed;
    let _: fn() -> u64 = fastrand::get_seed;
    let _: fn() -> bool = fastrand::bool;
    let _: fn() -> char = fastrand::alphabetic;
    let _: fn() -> char = fastrand::alphanumeric;
    let _: fn() -> char = fastrand::lowercase;
    let _: fn() -> char = fastrand::uppercase;
    let _: fn(u32) -> char = fastrand::digit;
    let _: fn(&mut [u8]) = fastrand::fill;
    let _: fn(&mut [u32]) = fastrand::shuffle::<u32>;
    let _: fn(Range<u8>) -> Option<u8> = fastrand::choice::<Range<u8>>;
    let _: fn(Vec<u8>, usize) -> Vec<u8> = fastrand::choose_multiple::<Vec<u8>>;
    let _: fn() -> f32 = fastrand::f32;
    let _: fn() -> f32 = fastrand::f32_inclusive;
    let _: fn() -> f64 = fastrand::f64;
    let _: fn() -> f64 = fastrand::f64_inclusive;
    let _: fn(Range<u8>) -> u8 = fastrand::u8;
    let _: fn(Range<i8>) -> i8 = fastrand::i8;
    let _: fn(Range<u16>) -> u16 = fastrand::u16;
    let _: fn(Range<i16>) -> i16 = fastrand::i16;
    let _: fn(Range<u32>) -> u32 = fastrand::u32;
    let _: fn(Range<i32>) -> i32 = fastrand::i32;
    let _: fn(std::ops::RangeFull) -> u64 = fastrand::u64;
    let _: fn(Range<i64>) -> i64 = fastrand::i64;
    let _: fn(Range<u128>) -> u128 = fastrand::u128;
    let _: fn(Range<i128>) -> i128 = fastrand::i128;
    let _: fn(Range<usize>) -> usize = fastrand::usize;
    let _: fn(Range<isize>) -> isize = fastrand::isize;
    let _: fn(Range<char>) -> char = fastrand::char;

    let _: fn() -> Rng = Rng::new;
    let _: fn(u64) -> Rng = Rng::with_seed;
    let _: fn(&mut Rng) -> Rng = Rng::fork;
    let _: fn(&mut Rng, u64) = Rng::seed;
    let _: fn(&Rng) -> u64 = Rng::get_seed;
    let _: fn(&mut Rng, &mut [u8]) = Rng::fill;
    let _: fn(&mut Rng, Range<usize>) -> usize = Rng::usize;
}

/// The traits `Rng` implements in fastrand, including the auto traits.
fn traits() {
    fn rng_like<
        T: Clone
            + std::fmt::Debug
            + PartialEq
            + Eq
            + Default
            + Send
            + Sync
            + Unpin
            + std::panic::UnwindSafe
            + std::panic::RefUnwindSafe,
    >() {
    }
    rng_like::<Rng>();
    assert_eq!(format!("{:?}", Rng::with_seed(42)), "Rng(42)");
    assert_eq!(SEEDED.clone(), Rng::with_seed(0x5eed));
}

fn free_functions() {
    assert_ne!(fastrand::u128(..), fastrand::u128(..));
    for _ in 0..20 {
        assert!((1..=6).contains(&fastrand::u8(1..=6)));
        assert!((-3..3).contains(&fastrand::i32(-3..3)));
        assert!(fastrand::usize(..10) < 10);
        assert!(('a'..='f').contains(&fastrand::char('a'..='f')));
        assert!((0.0..1.0).contains(&fastrand::f64()));
        assert!((0.0..=1.0).contains(&fastrand::f32_inclusive()));
        assert!(fastrand::alphabetic().is_ascii_alphabetic());
        assert!(fastrand::alphanumeric().is_ascii_alphanumeric());
        assert!(fastrand::lowercase().is_ascii_lowercase());
        assert!(fastrand::uppercase().is_ascii_uppercase());
        assert!(fastrand::digit(16).is_ascii_hexdigit());
    }
    let _: bool = fastrand::bool();
    let _: (i8, u16, i16, u32, i64, i128, isize) = (
        fastrand::i8(..),
        fastrand::u16(..),
        fastrand::i16(..),
        fastrand::u32(..),
        fastrand::i64(..),
        fastrand::i128(..),
        fastrand::isize(..),
    );
    let _: (f32, f64) = (fastrand::f32(), fastrand::f64_inclusive());

    let mut deck: Vec<u32> = (0..52).collect();
    fastrand::shuffle(&mut deck);
    let mut sorted = deck.clone();
    sorted.sort();
    assert_eq!(sorted, (0..52).collect::<Vec<_>>());
    assert!(fastrand::choice(&deck).is_some());
    assert_eq!(fastrand::choice(Vec::<u8>::new()), None);
    assert_eq!(fastrand::choose_multiple(0..100, 5).len(), 5);
    let mut bytes = [0u8; 64];
    fastrand::fill(&mut bytes);
    assert_ne!(bytes, [0u8; 64]);
}

/// Seeded generators produce exactly what they produce in fastrand.
fn seeded_generators_are_fastrands() {
    let mut ours = Rng::with_seed(7);
    let mut fastrands = fastrand_orig::Rng::with_seed(7);
    for _ in 0..4 {
        assert_eq!(ours.u64(..), fastrands.u64(..));
        assert_eq!(ours.i32(-5..5), fastrands.i32(-5..5));
        assert_eq!(ours.usize(..=9), fastrands.usize(..=9));
        assert_eq!(ours.u128(..), fastrands.u128(..));
        assert_eq!(ours.char('a'..='z'), fastrands.char('a'..='z'));
        assert_eq!(ours.bool(), fastrands.bool());
        assert_eq!(ours.f64().to_bits(), fastrands.f64().to_bits());
        assert_eq!(ours.f32_inclusive().to_bits(), fastrands.f32_inclusive().to_bits());
        assert_eq!(ours.alphanumeric(), fastrands.alphanumeric());
        assert_eq!(ours.digit(36), fastrands.digit(36));
        assert_eq!(ours.choice(0..10), fastrands.choice(0..10));
        assert_eq!(ours.choose_multiple(0..50, 3), fastrands.choose_multiple(0..50, 3));
        assert_eq!(ours.get_seed(), fastrands.get_seed());
    }
    let (mut a, mut b) = ([0u8; 13], [0u8; 13]);
    ours.fill(&mut a);
    fastrands.fill(&mut b);
    assert_eq!(a, b);
    let (mut a, mut b): (Vec<u32>, Vec<u32>) = ((0..30).collect(), (0..30).collect());
    ours.shuffle(&mut a);
    fastrands.shuffle(&mut b);
    assert_eq!(a, b);
    assert_eq!(ours.fork().u64(..), fastrands.fork().u64(..));
    ours.seed(99);
    fastrands.seed(99);
    assert_eq!(ours.lowercase(), fastrands.lowercase());
}

/// `seed` makes the free functions draw what a generator with that seed draws.
fn seed_and_the_free_functions() {
    fastrand::seed(7);
    let mut expected = Rng::with_seed(7);
    assert_eq!(fastrand::get_seed(), 7);
    assert_eq!(fastrand::u64(..), expected.u64(..));
    assert_eq!(fastrand::alphanumeric(), expected.alphanumeric());
    let mut deck: Vec<u32> = (0..10).collect();
    let mut expected_deck = deck.clone();
    fastrand::shuffle(&mut deck);
    expected.shuffle(&mut expected_deck);
    assert_eq!(deck, expected_deck);
    assert_eq!(fastrand::get_seed(), expected.get_seed());
}

/// `Rng::new` forks the generator behind the free functions, and advances it.
fn new_forks_the_generator_behind_the_free_functions() {
    fastrand::seed(9);
    let mut expected = Rng::with_seed(9);
    assert_eq!(Rng::new(), expected.fork());
    assert_eq!(Rng::default(), expected.fork());
    assert_eq!(fastrand::get_seed(), expected.get_seed());
    // Unseeded, two forks differ.
    assert_ne!(Rng::new().u128(..), Rng::new().u128(..));
}

/// Stands in for a crate built on fastrand itself.
mod real_crate {
    pub fn next(rng: &mut fastrand_orig::Rng) -> u64 {
        rng.u64(..)
    }
}

// Without the `shuttle` feature, these convert a type to itself.
#[allow(clippy::useless_conversion)]
fn boundary() {
    let mut real: fastrand_orig::Rng = Rng::with_seed(3).into();
    let mut back: Rng = fastrand_orig::Rng::with_seed(3).into();
    assert_eq!(real_crate::next(&mut real), back.u64(..));
}
