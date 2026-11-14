//! Code written against rand 0.10, as a crate that depends on `shuttle-rand_0_10` under the name `rand` would
//! have it. The same source is compiled against `shuttle-rand_0_10` without the `shuttle` feature, where it is
//! rand, and against `shuttle-rand_0_10-inner`, which is what `shuttle-rand_0_10` is under `shuttle`. The test
//! that includes it names the crate `rand`, and has rand itself as `rand_orig`.
//!
//! The checks hold whichever crate `rand` is, so they assume nothing about the values drawn, only about their
//! ranges and about what seeded generators produce.

use rand::distr::{Alphanumeric, Distribution, SampleString, StandardUniform, Uniform};
use rand::rngs::{
    ChaCha8Rng, ChaCha12Rng, ChaCha20Rng, SmallRng, StdRng, SysError, SysRng, ThreadRng, Xoshiro128PlusPlus,
    Xoshiro256PlusPlus,
};
use rand::seq::{IndexedRandom, IteratorRandom, SliceRandom};
use rand::{CryptoRng, Rng, RngExt, RngReader, SeedableRng, TryCryptoRng, TryRng};
use std::convert::Infallible;
use std::io::Read;

/// Exercises rand's API.
pub fn exercise() {
    signatures();
    traits();
    entry_points();
    sys_rng();
    make_rng();
    seeded_generators_are_rands();
    a_generator_of_our_own();
    distributions_and_sequences();
    prelude_user::use_the_prelude();
}

/// Every entropy entry point, with the signature it has in rand.
fn signatures() {
    let _: fn() -> ThreadRng = rand::rng;
    let _: fn() -> u64 = rand::random::<u64>;
    let _: fn() -> rand::distr::Iter<StandardUniform, ThreadRng, u8> = rand::random_iter::<u8>;
    let _: fn(std::ops::Range<u32>) -> u32 = rand::random_range::<u32, std::ops::Range<u32>>;
    let _: fn(f64) -> bool = rand::random_bool;
    let _: fn(u32, u32) -> bool = rand::random_ratio;
    let _: fn(&mut [u8]) = rand::fill::<u8>;
    let _: fn() -> StdRng = rand::make_rng::<StdRng>;
    let _: fn(&mut ThreadRng) -> Result<(), SysError> = ThreadRng::reseed;
    let _: fn(&mut SysRng) -> Result<u64, SysError> = <SysRng as TryRng>::try_next_u64;
    let _: fn(u64) -> StdRng = <StdRng as SeedableRng>::seed_from_u64;
}

/// The traits the generators implement in rand, including the auto traits.
fn traits() {
    fn thread_rng_like<
        T: Rng + RngExt + CryptoRng + TryRng<Error = Infallible> + TryCryptoRng + Clone + Default + std::fmt::Debug,
    >() {
    }
    fn sys_rng_like<
        T: TryRng<Error = SysError> + TryCryptoRng + Clone + Copy + Default + std::fmt::Debug + Send + Sync,
    >() {
    }
    fn seedable<T: SeedableRng + Rng + PartialEq + std::fmt::Debug + Send + Sync>() {}
    thread_rng_like::<ThreadRng>();
    sys_rng_like::<SysRng>();
    seedable::<StdRng>();
    seedable::<SmallRng>();
    seedable::<Xoshiro128PlusPlus>();
    seedable::<Xoshiro256PlusPlus>();
    seedable::<ChaCha8Rng>();
    seedable::<ChaCha12Rng>();
    seedable::<ChaCha20Rng>();

    // `ThreadRng` is neither `Send` nor `Sync`: if it were, `_` below would be ambiguous.
    trait AmbiguousIfSend<A> {
        fn some_item() {}
    }
    impl<T: ?Sized> AmbiguousIfSend<()> for T {}
    impl<T: ?Sized + Send> AmbiguousIfSend<u8> for T {}
    trait AmbiguousIfSync<A> {
        fn some_item() {}
    }
    impl<T: ?Sized> AmbiguousIfSync<()> for T {}
    impl<T: ?Sized + Sync> AmbiguousIfSync<u8> for T {}
    <ThreadRng as AmbiguousIfSend<_>>::some_item();
    <ThreadRng as AmbiguousIfSync<_>>::some_item();
}

fn entry_points() {
    let mut rng = rand::rng();
    assert_eq!(format!("{rng:?}"), "ThreadRng { .. }");
    assert!(rng.reseed().is_ok());
    assert_ne!(rng.random::<u128>(), rng.random::<u128>());
    let _: u64 = ThreadRng::default().next_u64();
    let _: u32 = rng.try_next_u32().unwrap();

    assert_ne!(rand::random::<u128>(), rand::random::<u128>());
    let (byte, word, pair): (u8, u32, (bool, char)) = rand::random();
    let _ = (byte, word, pair);
    assert_eq!(rand::random_iter::<u64>().take(5).count(), 5);
    for _ in 0..20 {
        assert!((1..=6).contains(&rand::random_range(1..=6)));
        assert!((0.0..1.0).contains(&rand::random_range(0.0..1.0)));
    }
    assert!(rand::random_bool(1.0));
    assert!(!rand::random_bool(0.0));
    assert!(rand::random_ratio(3, 3));
    assert!(!rand::random_ratio(0, 3));
    let mut bytes = [0u8; 64];
    rand::fill(&mut bytes);
    assert_ne!(bytes, [0u8; 64]);
    let mut words = [0u32; 8];
    rand::fill(&mut words[..]);
    assert_ne!(words, [0u32; 8]);

    let mut read = [0u8; 16];
    RngReader(rand::rng()).read_exact(&mut read).unwrap();
    assert_ne!(read, [0u8; 16]);
}

fn sys_rng() {
    assert_eq!(format!("{:?}", SysRng), "SysRng");
    let mut sys = SysRng;
    let (first, second) = (sys.try_next_u64().unwrap(), SysRng.try_next_u64().unwrap());
    assert_ne!(first, second);
    let mut key = [0u8; 32];
    SysRng.try_fill_bytes(&mut key).unwrap();
    assert_ne!(key, [0u8; 32]);
    let _: u32 = rand::rand_core::UnwrapErr(SysRng).next_u32();
    let mut from_sys = StdRng::try_from_rng(&mut SysRng).unwrap();
    let _: u64 = from_sys.random();
}

fn make_rng() {
    let mut a: StdRng = rand::make_rng();
    let mut b = rand::make_rng::<StdRng>();
    assert_ne!(a.random::<u128>(), b.random::<u128>());
    let mut small: SmallRng = rand::make_rng();
    let _: u64 = small.random();
    let mut forked = small.fork();
    let _: u64 = forked.random();
}

/// Seeded generators produce exactly what they produce in rand.
fn seeded_generators_are_rands() {
    let mut ours = StdRng::seed_from_u64(7);
    let mut rands = <rand_orig::rngs::StdRng as rand_orig::SeedableRng>::seed_from_u64(7);
    for _ in 0..8 {
        assert_eq!(ours.next_u64(), rands.next_u64());
    }

    let mut ours = SmallRng::seed_from_u64(7);
    let mut rands = <rand_orig::rngs::SmallRng as rand_orig::SeedableRng>::seed_from_u64(7);
    assert_eq!(ours.random::<[u32; 4]>(), rands.random::<[u32; 4]>());

    let mut ours = ChaCha8Rng::from_seed([3; 32]);
    let mut rands = <rand_orig::rngs::ChaCha8Rng as rand_orig::SeedableRng>::from_seed([3; 32]);
    assert_eq!(ours.random_range(0..1000), rands.random_range(0..1000));

    let mut parent = StdRng::seed_from_u64(1);
    let mut child = Xoshiro256PlusPlus::from_rng(&mut parent);
    let mut rands_parent = <rand_orig::rngs::StdRng as rand_orig::SeedableRng>::seed_from_u64(1);
    let mut rands_child = <rand_orig::rngs::Xoshiro256PlusPlus as rand_orig::SeedableRng>::from_rng(&mut rands_parent);
    assert_eq!(child.next_u64(), rands_child.next_u64());
    assert_eq!(parent.fork().next_u64(), rands_parent.fork().next_u64());
}

/// A generator that implements rand's traits itself, as a consumer crate may.
#[derive(Debug, Clone, PartialEq)]
pub struct Counter {
    seed: [u8; 12],
    state: u64,
}

impl TryRng for Counter {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        self.try_next_u64().map(|x| x as u32)
    }

    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        self.state = self.state.wrapping_add(1);
        Ok(self.state)
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Infallible> {
        rand::rand_core::utils::fill_bytes_via_next_word(dest, || self.try_next_u64())
    }
}

impl SeedableRng for Counter {
    type Seed = [u8; 12];

    fn from_seed(seed: [u8; 12]) -> Self {
        let state = u64::from_le_bytes(seed[..8].try_into().unwrap());
        Counter { seed, state }
    }
}

fn a_generator_of_our_own() {
    let mut counter = Counter::from_seed([0; 12]);
    assert_eq!(counter.random::<u64>(), 1);
    let seeded = Counter::seed_from_u64(9);
    let rands = <Counter as rand_orig::SeedableRng>::seed_from_u64(9);
    assert_eq!(seeded, rands);
    let made: Counter = rand::make_rng();
    let _ = made;
    let _ = Counter::try_from_rng(&mut SysRng).unwrap();
}

fn distributions_and_sequences() {
    let mut rng = rand::rng();
    let die = Uniform::new_inclusive(1, 6).unwrap();
    assert!((1..=6).contains(&die.sample(&mut rng)));
    assert_eq!(Alphanumeric.sample_string(&mut rng, 12).len(), 12);
    let mut deck: Vec<u32> = (0..52).collect();
    deck.shuffle(&mut rng);
    assert_eq!(deck.len(), 52);
    assert!(deck.choose(&mut rng).is_some());
    assert!((0..10).choose(&mut rng).is_some());
    let picked: Vec<u32> = deck.sample(&mut rng, 5).copied().collect();
    assert_eq!(picked.len(), 5);

    // The same calls on a seeded generator give rand's results.
    let mut ours = StdRng::seed_from_u64(11);
    let mut rands = <rand_orig::rngs::StdRng as rand_orig::SeedableRng>::seed_from_u64(11);
    let mut a: Vec<u32> = (0..20).collect();
    let mut b = a.clone();
    a.shuffle(&mut ours);
    rand_orig::seq::SliceRandom::shuffle(&mut b[..], &mut rands);
    assert_eq!(a, b);
}

mod prelude_user {
    use rand::prelude::*;

    pub fn use_the_prelude() {
        let mut rng: ThreadRng = rand::rng();
        let mut seeded = StdRng::seed_from_u64(3);
        let mut small = SmallRng::seed_from_u64(3);
        let items = [1, 2, 3];
        assert!(items.choose(&mut rng).is_some());
        let _: u8 = seeded.random();
        let _: u8 = small.next_u32() as u8;
        let _ = rand::distr::StandardUniform
            .sample_iter(&mut rng)
            .take(2)
            .collect::<Vec<u8>>();
    }
}
