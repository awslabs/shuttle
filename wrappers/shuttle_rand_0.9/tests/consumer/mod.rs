//! Code written against rand 0.9, as a crate that depends on `shuttle-rand_0_9` under the name `rand` would have
//! it. The same source is compiled against `shuttle-rand_0_9` without the `shuttle` feature, where it is rand,
//! and against `shuttle-rand_0_9-inner`, which is what `shuttle-rand_0_9` is under `shuttle`. The test that
//! includes it names the crate `rand`, and has rand itself as `rand_orig`.
//!
//! The checks hold whichever crate `rand` is, so they assume nothing about the values drawn, only about their
//! ranges and about what seeded generators produce.

use rand::distr::{Alphanumeric, Distribution, SampleString, StandardUniform, Uniform};
use rand::rngs::{OsRng, SmallRng, StdRng, ThreadRng};
use rand::seq::{IndexedRandom, IteratorRandom, SliceRandom};
use rand::{CryptoRng, Rng, RngCore, SeedableRng, TryCryptoRng, TryRngCore};

/// Exercises rand's API.
pub fn exercise() {
    signatures();
    traits();
    entry_points();
    os_rng();
    seeded_generators_are_rands();
    seeding_from_entropy();
    a_generator_of_our_own();
    distributions_and_sequences();
    prelude_user::use_the_prelude();
    assert_eq!(mock_rng(), 11);
}

/// Every entropy entry point, with the signature it has in rand.
fn signatures() {
    let _: fn() -> ThreadRng = rand::rng;
    #[allow(deprecated)]
    let _: fn() -> ThreadRng = rand::thread_rng;
    let _: fn() -> u64 = rand::random::<u64>;
    let _: fn() -> rand::distr::Iter<StandardUniform, ThreadRng, u8> = rand::random_iter::<u8>;
    let _: fn(std::ops::Range<u32>) -> u32 = rand::random_range::<u32, std::ops::Range<u32>>;
    let _: fn(f64) -> bool = rand::random_bool;
    let _: fn(u32, u32) -> bool = rand::random_ratio;
    let _: fn(&mut [u8]) = rand::fill::<[u8]>;
    let _: fn(&mut ThreadRng) -> Result<(), rand::rand_core::OsError> = ThreadRng::reseed;
    let _: fn() -> StdRng = <StdRng as SeedableRng>::from_os_rng;
    let _: fn() -> Result<StdRng, getrandom::Error> = <StdRng as SeedableRng>::try_from_os_rng;
    let _: fn(u64) -> StdRng = <StdRng as SeedableRng>::seed_from_u64;
    let _: fn(&mut OsRng) -> Result<u64, rand::rand_core::OsError> = <OsRng as TryRngCore>::try_next_u64;
}

/// The traits the generators implement in rand, including the auto traits.
fn traits() {
    fn thread_rng_like<T: RngCore + CryptoRng + Rng + Clone + Default + std::fmt::Debug>() {}
    fn os_rng_like<T: TryRngCore + TryCryptoRng + Clone + Copy + Default + std::fmt::Debug + Send + Sync>() {}
    fn seedable<T: SeedableRng + RngCore + Clone + PartialEq + std::fmt::Debug + Send + Sync>() {}
    thread_rng_like::<ThreadRng>();
    os_rng_like::<OsRng>();
    seedable::<StdRng>();
    seedable::<SmallRng>();

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
    #[allow(deprecated)]
    let _: u32 = rand::thread_rng().next_u32();

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
    rand::fill(&mut bytes[..]);
    assert_ne!(bytes, [0u8; 64]);
    let mut words = [0u32; 8];
    rand::fill(&mut words);
    assert_ne!(words, [0u32; 8]);
}

fn os_rng() {
    assert_eq!(format!("{:?}", OsRng), "OsRng");
    let mut os = OsRng;
    let (first, second) = (os.try_next_u64().unwrap(), OsRng.try_next_u64().unwrap());
    assert_ne!(first, second);
    let mut key = [0u8; 32];
    OsRng.try_fill_bytes(&mut key).unwrap();
    assert_ne!(key, [0u8; 32]);
    let _: u32 = OsRng.unwrap_err().next_u32();
    let mut from_os = StdRng::try_from_rng(&mut OsRng).unwrap();
    let _: u64 = from_os.random();
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

    let mut ours = StdRng::from_seed([3; 32]);
    let mut rands = <rand_orig::rngs::StdRng as rand_orig::SeedableRng>::from_seed([3; 32]);
    assert_eq!(ours.random_range(0..1000), rands.random_range(0..1000));

    // Seeding from another generator takes its seed from that generator only.
    let mut parent = StdRng::seed_from_u64(1);
    let mut child = SmallRng::from_rng(&mut parent);
    let mut rands_parent = <rand_orig::rngs::StdRng as rand_orig::SeedableRng>::seed_from_u64(1);
    let mut rands_child = <rand_orig::rngs::SmallRng as rand_orig::SeedableRng>::from_rng(&mut rands_parent);
    assert_eq!(child.next_u64(), rands_child.next_u64());
    let mut child = StdRng::try_from_rng(&mut parent).unwrap();
    let mut rands_child = <rand_orig::rngs::StdRng as rand_orig::SeedableRng>::try_from_rng(&mut rands_parent).unwrap();
    assert_eq!(child.next_u64(), rands_child.next_u64());

    assert_eq!(seeded_pair::<StdRng>().0, seeded_pair::<StdRng>().1);
}

fn seeded_pair<R: SeedableRng + RngCore>() -> (u64, u64) {
    (R::seed_from_u64(5).next_u64(), R::seed_from_u64(5).next_u64())
}

fn from_os<R: SeedableRng>() -> R {
    R::from_os_rng()
}

/// Generators seeded from entropy differ from each other.
fn seeding_from_entropy() {
    let mut a = StdRng::from_os_rng();
    let mut b: StdRng = from_os();
    assert_ne!(a.random::<u128>(), b.random::<u128>());
    let mut c = SmallRng::try_from_os_rng().unwrap();
    let mut d = SmallRng::from_os_rng();
    assert_ne!(c.random::<u128>(), d.random::<u128>());
    let mut e = StdRng::from_rng(&mut rand::rng());
    let _: u64 = e.random();
}

/// A generator that implements rand's traits itself, as a consumer crate may.
#[derive(Debug, Clone, PartialEq)]
pub struct Counter {
    seed: [u8; 12],
    state: u64,
}

impl RngCore for Counter {
    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(1);
        self.state
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        rand::rand_core::impls::fill_bytes_via_next(self, dest)
    }
}

impl SeedableRng for Counter {
    type Seed = [u8; 12];

    fn from_seed(seed: [u8; 12]) -> Self {
        let state = u64::from_le_bytes(seed[..8].try_into().unwrap());
        Counter { seed, state }
    }
}

/// The same seed type, implementing rand_core's trait, which is rand's.
struct Probe([u8; 12]);

impl rand_orig::rand_core::SeedableRng for Probe {
    type Seed = [u8; 12];

    fn from_seed(seed: [u8; 12]) -> Self {
        Probe(seed)
    }
}

fn a_generator_of_our_own() {
    // The provided methods behave as rand_core's.
    let ours = Counter::seed_from_u64(9);
    let rand_cores = <Probe as rand_orig::rand_core::SeedableRng>::seed_from_u64(9);
    assert_eq!(ours.seed, rand_cores.0);

    let mut counter = Counter::from_seed([0; 12]);
    assert_eq!(counter.random::<u64>(), 1);
    let from_rng = Counter::from_rng(&mut counter.clone());
    assert_eq!(&from_rng.seed[..8], &2u64.to_le_bytes());
    let _ = Counter::try_from_rng(&mut OsRng).unwrap();
    assert_ne!(Counter::from_os_rng(), Counter::from_os_rng());
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
    let picked: Vec<u32> = deck.choose_multiple(&mut rng, 5).copied().collect();
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

// Deprecated in rand 0.9.2; the warning is the same in both modes.
#[allow(deprecated)]
fn mock_rng() -> u64 {
    let mut step = rand::rngs::mock::StepRng::new(5, 1);
    step.next_u64() + step.next_u64()
}
