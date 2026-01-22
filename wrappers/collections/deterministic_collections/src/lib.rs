//! Provides HashMap and HashSet which has a deterministic iteration order.
//! This crate exists as the underlying implementation for
//! `determinizable-collections`, which allows switching between
//! implementations by setting the `deterministic` feature flag.

// For both HashMap and HashSet, we want to be able to use these methods:
//
// * inherent methods
//   * with an owned `self` receiver: re-implemented and forwarded
//   * with a `&self` receiver: accessible via `Deref`
//   * with a `&mut self` receiver: accessible via `DerefMut`
// * derivable trait methods: derived
// * non-derivable trait methods: re-implemented and forwarded
//
// Most implementations in this file forward calls to the underlying stdlib
// type. The exceptions are methods which somehow interact with the random
// state, where we need to choose our deterministic hasher state.

use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::collections::HashMap as StdHashMap;
use std::collections::HashSet as StdHashSet;
use std::hash::Hash;
use std::hash::RandomState;
use std::iter::Extend;
use std::ops::{BitAnd, BitOr, BitXor, Deref, DerefMut, Index, Sub};
use std::panic::UnwindSafe;

pub use siphasher::sip::SipHasher13 as DefaultHasher;

/// A fixed [`RandomState`] value to initialize deterministic hash collections with.
///
/// In the standard library [`RandomState`] is the default state for hash collections.
/// Every new [`RandomState`] is initialized with random keys for security.  This poses
/// a problem for testing, where we want deterministic behavior in order to be able to
/// replay test failures.
///
/// We provide deterministic hash collections by initializing them with this fixed
/// [`RandomState`] value. Since there is currently no public constructor for that,
/// we rely on the fact that the [`RandomState`] struct consists of two `u64` values
/// and transmute from a pair of `u64`s. This is unsafe, but we accept it for now,
/// while we try to get a deterministic constructor into the standard library.
const DETERMINISTIC_RANDOM_STATE: RandomState = unsafe { std::mem::transmute((0u64, 0u64)) };

// Note that previously we did not use `RandomState` but `SipHasher13` (which `RandomState`
// uses under the hood) with fixed keys. This is fine within a code base that uses our
// hash collections, but unfortunately we need to deal with code that uses libraries that
// have the default collection types (parameterized with `RandomState`) in their interface.
// If we wouldn't use `RandomState` we'd get a type incompatibility.
//
// We are using a new type in order to hijack `new` for deterministic construction.
// Unfortunately this means that we need to wrap/unwrap (via `From` impls below) at
// library boundaries. If there would be a way to avoid the new type, that would be great.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(bound(serialize = "K: Eq + Serialize, V: Serialize"))]
#[serde(bound(deserialize = "K: Eq + Hash + Deserialize<'de>, V: Deserialize<'de>"))]
pub struct HashMap<K, V>(StdHashMap<K, V, RandomState>);

impl<K, V> HashMap<K, V> {
    pub fn new() -> Self {
        Self(StdHashMap::with_hasher(DETERMINISTIC_RANDOM_STATE))
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self(StdHashMap::with_capacity_and_hasher(
            capacity,
            DETERMINISTIC_RANDOM_STATE,
        ))
    }

    pub fn into_keys(self) -> std::collections::hash_map::IntoKeys<K, V> {
        self.0.into_keys()
    }

    pub fn into_values(self) -> std::collections::hash_map::IntoValues<K, V> {
        self.0.into_values()
    }
}

impl<K: Eq + Hash, V> From<StdHashMap<K, V, RandomState>> for HashMap<K, V> {
    fn from(value: StdHashMap<K, V, RandomState>) -> Self {
        // We need to create a new map in order to gain control of the `RandomState`.
        HashMap::from_iter(value)
    }
}

impl<K, V> From<HashMap<K, V>> for StdHashMap<K, V, RandomState> {
    fn from(value: HashMap<K, V>) -> Self {
        value.0
    }
}

impl<K, V> Deref for HashMap<K, V> {
    type Target = StdHashMap<K, V, RandomState>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<K, V> DerefMut for HashMap<K, V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

// this is not derived, because HashMap does not impose that
// K: Default and V: Default, unlike the derived version
impl<K, V> Default for HashMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a, K: Eq + Hash + Copy, V: Copy> Extend<(&'a K, &'a V)> for HashMap<K, V> {
    fn extend<T: IntoIterator<Item = (&'a K, &'a V)>>(&mut self, iter: T) {
        self.0.extend(iter);
    }
}

impl<K: Eq + Hash, V: Eq> Eq for HashMap<K, V> {}

impl<K: Eq + Hash, V> Extend<(K, V)> for HashMap<K, V> {
    fn extend<T: IntoIterator<Item = (K, V)>>(&mut self, iter: T) {
        self.0.extend(iter);
    }
}

impl<K: Eq + Hash, V, const N: usize> From<[(K, V); N]> for HashMap<K, V> {
    fn from(arr: [(K, V); N]) -> Self {
        Self::from_iter(arr)
    }
}

impl<K: Eq + Hash, V> FromIterator<(K, V)> for HashMap<K, V> {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        let mut map = Self::new();
        map.extend(iter);
        map
    }
}

impl<K: Eq + Hash + Borrow<Q>, Q: Eq + Hash + ?Sized, V> Index<&Q> for HashMap<K, V> {
    type Output = V;
    fn index(&self, key: &Q) -> &Self::Output {
        self.0.index(key)
    }
}

impl<'a, K: Hash + Eq, V> IntoIterator for &'a HashMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = std::collections::hash_map::Iter<'a, K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a, K, V> IntoIterator for &'a mut HashMap<K, V> {
    type Item = (&'a K, &'a mut V);
    type IntoIter = std::collections::hash_map::IterMut<'a, K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}

impl<K, V> IntoIterator for HashMap<K, V> {
    type Item = (K, V);
    type IntoIter = std::collections::hash_map::IntoIter<K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<K: Eq + Hash, V: PartialEq> PartialEq for HashMap<K, V> {
    fn eq(&self, other: &HashMap<K, V>) -> bool {
        self.0.eq(&other.0)
    }
}

impl<K: UnwindSafe, V: UnwindSafe> UnwindSafe for HashMap<K, V> {}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(bound(serialize = "T: Eq + Serialize"))]
#[serde(bound(deserialize = "T: Eq + Hash + Deserialize<'de>"))]
pub struct HashSet<T>(StdHashSet<T, RandomState>);

impl<T> HashSet<T> {
    pub fn new() -> Self {
        Self(StdHashSet::with_hasher(DETERMINISTIC_RANDOM_STATE))
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self(StdHashSet::with_capacity_and_hasher(
            capacity,
            DETERMINISTIC_RANDOM_STATE,
        ))
    }
}

impl<T: Eq + Hash> From<StdHashSet<T, RandomState>> for HashSet<T> {
    fn from(value: StdHashSet<T, RandomState>) -> Self {
        // We need to create a new set in order to gain control of the `RandomState`.
        HashSet::from_iter(value)
    }
}

impl<T> From<HashSet<T>> for StdHashSet<T, RandomState> {
    fn from(value: HashSet<T>) -> Self {
        value.0
    }
}

impl<T> Deref for HashSet<T> {
    type Target = StdHashSet<T, RandomState>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for HashSet<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

// this is not derived, because HashSet does not impose that
// T: Default, unlike the derived version
impl<T> Default for HashSet<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Eq + Hash + Clone> BitAnd<&HashSet<T>> for &HashSet<T> {
    type Output = HashSet<T>;
    fn bitand(self, rhs: &HashSet<T>) -> HashSet<T> {
        HashSet(self.0.bitand(&rhs.0))
    }
}

impl<T: Eq + Hash + Clone> BitOr<&HashSet<T>> for &HashSet<T> {
    type Output = HashSet<T>;
    fn bitor(self, rhs: &HashSet<T>) -> HashSet<T> {
        HashSet(self.0.bitor(&rhs.0))
    }
}

impl<T: Eq + Hash + Clone> BitXor<&HashSet<T>> for &HashSet<T> {
    type Output = HashSet<T>;
    fn bitxor(self, rhs: &HashSet<T>) -> HashSet<T> {
        HashSet(self.0.bitxor(&rhs.0))
    }
}

impl<T: Eq + Hash> Eq for HashSet<T> {}

impl<'a, T: Eq + Hash + Copy> Extend<&'a T> for HashSet<T> {
    fn extend<I: IntoIterator<Item = &'a T>>(&mut self, iter: I) {
        self.0.extend(iter);
    }
}

impl<T: Eq + Hash> Extend<T> for HashSet<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        self.0.extend(iter);
    }
}

impl<T: Eq + Hash, const N: usize> From<[T; N]> for HashSet<T> {
    fn from(arr: [T; N]) -> Self {
        Self::from_iter(arr)
    }
}

impl<T: Eq + Hash> FromIterator<T> for HashSet<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> HashSet<T> {
        let mut set = Self::new();
        set.extend(iter);
        set
    }
}

impl<'a, T> IntoIterator for &'a HashSet<T> {
    type Item = &'a T;
    type IntoIter = std::collections::hash_set::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<T> IntoIterator for HashSet<T> {
    type Item = T;
    type IntoIter = std::collections::hash_set::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<T: Eq + Hash> PartialEq for HashSet<T> {
    fn eq(&self, other: &HashSet<T>) -> bool {
        self.0.eq(&other.0)
    }
}

impl<T: Eq + Hash + Clone> Sub<&HashSet<T>> for &HashSet<T> {
    type Output = HashSet<T>;
    fn sub(self, rhs: &HashSet<T>) -> HashSet<T> {
        HashSet(self.0.sub(&rhs.0))
    }
}

/// Tests to check that the interface works the same, not to test the actual
/// functionality thoroughly.
#[cfg(test)]
mod test {
    // These tests are written as macros, to avoid writing a method that is
    // generic in every trait that we are declaring above. The macros are
    // instantiated below, once for the deterministic collections, once for
    // the stdlib ones.

    use super::*;

    macro_rules! test_hashmap {
        ($hashmap:ident) => {
            // inherent methods
            let m: $hashmap<i32, i32> = $hashmap::new();
            assert!(m.into_keys().collect::<Vec<_>>().is_empty());
            let mut m: $hashmap<i32, i32> = $hashmap::with_capacity(5);

            // derivable traits, Eq, PartialEq
            let _ = m.clone();
            let _: $hashmap<i32, i32> = Default::default();
            assert_eq!(m, Default::default());

            // Deref (&self receiver)
            assert!(m.capacity() >= 5);

            // DerefMut (&mut self receiver)
            assert!(m.values_mut().next().is_none());
            m.reserve(10);

            // Extend
            m.extend([(1, 2)]);

            // From
            assert_eq!($hashmap::from([(1, 2)]).into_values().next(), Some(2));
            assert_eq!($hashmap::from([(1, 2)]), [(1, 2)].into());

            // FromIterator
            assert_eq!($hashmap::from_iter([(1, 2)]).into_values().next(), Some(2));

            // Index
            assert_eq!(m[&1], 2);

            // IntoIterator
            assert_eq!((&m).into_iter().next(), Some((&1, &2)));
            assert!((&mut m).into_iter().next().is_some());
            assert_eq!(m.into_iter().next(), Some((1, 2)));
        };
    }

    macro_rules! test_hashset {
        ($hashset:ident) => {
            // inherent methods
            let s: $hashset<i32> = $hashset::new();
            assert!(s.into_iter().collect::<Vec<_>>().is_empty());
            let mut s: $hashset<i32> = $hashset::with_capacity(5);

            // derivable traits, Eq, PartialEq
            let _ = s.clone();
            let _: $hashset<i32> = Default::default();
            assert_eq!(s, Default::default());

            // Deref (&self receiver)
            assert!(s.capacity() >= 5);

            // DerefMut (&mut self receiver)
            s.clear();
            s.reserve(10);

            // Extend
            s.extend([1]);

            // From
            assert_eq!($hashset::from([1]).into_iter().next(), Some(1));
            assert_eq!($hashset::from([1]), [(1)].into());

            // FromIterator
            assert_eq!($hashset::from_iter([(1)]).into_iter().next(), Some(1));

            // IntoIterator
            assert_eq!((&s).into_iter().next(), Some(&1));
            assert_eq!(s.into_iter().next(), Some(1));
        };
    }

    #[test]
    fn test_stdlib_hashmap() {
        type HashMap<K, V> = StdHashMap<K, V, RandomState>;
        test_hashmap!(HashMap);
    }

    #[test]
    fn test_stdlib_hashset() {
        type HashSet<T> = StdHashSet<T, RandomState>;
        test_hashset!(HashSet);
    }

    #[test]
    fn test_deterministic_hashmap() {
        use super::HashMap;
        test_hashmap!(HashMap);
    }

    #[test]
    fn test_deterministic_hashset() {
        use super::HashSet;
        test_hashset!(HashSet);
    }

    #[test]
    fn test_wrap_unwrap() {
        let std: StdHashMap<u64, u64> = StdHashMap::new();
        let deterministic: HashMap<_, _> = std.into();
        let _std: StdHashMap<_, _> = deterministic.into();

        let deterministic: HashMap<u64, u64> = HashMap::new();
        let std: StdHashMap<_, _> = deterministic.into();
        let _deterministic: HashMap<_, _> = std.into();

        let std: StdHashSet<u64> = StdHashSet::new();
        let deterministic: HashSet<_> = std.into();
        let _std: StdHashSet<_> = deterministic.into();

        let deterministic: HashSet<u64> = HashSet::new();
        let std: StdHashSet<_, _> = deterministic.into();
        let _deterministic: HashSet<_> = std.into();
    }
}
