//! Shuttle-aware replacement for `dashmap::DashSet`.
//!
//! Mirrors real dashmap's design: `DashSet<K>` is a thin wrapper around
//! `DashMap<K, ()>`, so it inherits all locking and determinism properties
//! from our `DashMap` shim (a single `shuttle::sync::RwLock` around a
//! deterministic `HashMap`). No additional unsafe code is needed here.
//!
//! As with our `DashMap` shim, the hasher type parameter `S` is omitted
//! because the underlying map always uses a deterministic hasher.

use crate::DashMap;
use std::borrow::Borrow;
use std::hash::Hash;

// ── DashSet ─────────────────────────────────────────────────────

pub struct DashSet<K> {
    inner: DashMap<K, ()>,
}

impl<K: std::fmt::Debug> std::fmt::Debug for DashSet<K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DashSet").finish_non_exhaustive()
    }
}

impl<K: Eq + Hash> Default for DashSet<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Eq + Hash + Clone> Clone for DashSet<K> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<K: Eq + Hash> From<DashMap<K, ()>> for DashSet<K> {
    fn from(inner: DashMap<K, ()>) -> Self {
        Self { inner }
    }
}

impl<K: Eq + Hash> FromIterator<K> for DashSet<K> {
    fn from_iter<I: IntoIterator<Item = K>>(iter: I) -> Self {
        Self {
            inner: iter.into_iter().map(|k| (k, ())).collect(),
        }
    }
}

impl<K: Eq + Hash> Extend<K> for DashSet<K> {
    fn extend<I: IntoIterator<Item = K>>(&mut self, iter: I) {
        self.inner.extend(iter.into_iter().map(|k| (k, ())));
    }
}

impl<K: Eq + Hash> IntoIterator for DashSet<K> {
    type Item = K;

    type IntoIter = OwningIter<K>;

    fn into_iter(self) -> Self::IntoIter {
        OwningIter(self.inner.into_iter())
    }
}

impl<K: Eq + Hash> DashSet<K> {
    pub fn new() -> Self {
        Self { inner: DashMap::new() }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: DashMap::with_capacity(capacity),
        }
    }

    /// Inserts a key into the set. Returns true if the key was not already
    /// in the set.
    pub fn insert(&self, key: K) -> bool {
        self.inner.insert(key, ()).is_none()
    }

    pub fn remove<Q>(&self, key: &Q) -> Option<K>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner.remove(key).map(|(k, _)| k)
    }

    pub fn remove_if<Q>(&self, key: &Q, f: impl FnOnce(&K) -> bool) -> Option<K>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner.remove_if(key, |k, _| f(k)).map(|(k, _)| k)
    }

    pub fn get<Q>(&self, key: &Q) -> Option<Ref<'_, K>>
    where
        K: Borrow<Q> + Clone,
        Q: Hash + Eq + ?Sized,
    {
        self.inner.get(key).map(Ref)
    }

    pub fn contains<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner.contains_key(key)
    }

    pub fn iter(&self) -> Iter<'_, K>
    where
        K: Clone,
    {
        Iter(self.inner.iter())
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn clear(&self) {
        self.inner.clear();
    }

    pub fn capacity(&self) -> usize {
        self.inner.capacity()
    }

    pub fn shrink_to_fit(&self) {
        self.inner.shrink_to_fit();
    }

    pub fn retain(&self, mut f: impl FnMut(&K) -> bool) {
        self.inner.retain(|k, _| f(k));
    }
}

// ── Ref (immutable guard) ───────────────────────────────────────

pub struct Ref<'a, K>(crate::Ref<'a, K, ()>);

impl<K: Eq + Hash> Ref<'_, K> {
    pub fn key(&self) -> &K {
        self.0.key()
    }
}

impl<K: Eq + Hash> std::ops::Deref for Ref<'_, K> {
    type Target = K;

    fn deref(&self) -> &K {
        self.key()
    }
}

// ── Iterators ───────────────────────────────────────────────────

pub struct Iter<'a, K>(crate::Iter<'a, K, ()>);

impl<'a, K: Eq + Hash> Iterator for Iter<'a, K> {
    type Item = RefMulti<'a, K>;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(RefMulti)
    }
}

pub struct OwningIter<K>(std::collections::hash_map::IntoIter<K, ()>);

impl<K> Iterator for OwningIter<K> {
    type Item = K;

    fn next(&mut self) -> Option<K> {
        self.0.next().map(|(k, ())| k)
    }
}

// ── RefMulti (iterator items) ───────────────────────────────────

pub struct RefMulti<'a, K>(crate::RefMulti<'a, K, ()>);

impl<K> RefMulti<'_, K> {
    pub fn key(&self) -> &K {
        self.0.key()
    }
}

impl<K> std::ops::Deref for RefMulti<'_, K> {
    type Target = K;

    fn deref(&self) -> &K {
        self.key()
    }
}
