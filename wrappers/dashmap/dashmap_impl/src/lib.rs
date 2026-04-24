//! Shuttle-aware replacement for the `dashmap` crate.
//!
//! `DashMap` is replaced by an `RwLock<HashMap>` using `shuttle::sync::RwLock`.
//! This is more coarse-grained than real dashmap's per-shard `RwLock`
//! architecture, but it means shuttle's scheduler can observe and control
//! contention between any two concurrent operations.
//!
//! # Key cloning
//!
//! DashMap's API returns guard types (`Ref`, `RefMut`) that hold a lock while
//! exposing references to data inside the map. To implement this safely, our
//! guards store a **cloned copy of the key** alongside the lock guard, and
//! look up the value through the guard on each access. This avoids raw pointers
//! in the guard types at the cost of cloning the key on `get`/`get_mut`/`entry`.
//!
//! # Unsafe usage
//!
//! Unsafe is used in two specific places, each matching patterns used by
//! real dashmap and the standard library:
//!
//! - **`Send`/`Sync` impls** on `DashMap`, `Ref`, and `RefMut`: the inner
//!   `RwLockReadGuard`/`RwLockWriteGuard` may not auto-derive these traits.
//!   We manually assert them with the same bounds as real dashmap.
//!
//! - **Pointer casts in `Iter::next` and `IterMut::next`**: iterator items
//!   hold references into the `HashMap` that must outlive the `next()` call.
//!   The `RwLockWriteGuard` (owned by the iterator struct) keeps the `HashMap` alive
//!   for the iterator's lifetime `'a`, so extending the references to `'a` is
//!   sound. `IterMut` visits each key exactly once, preventing mutable aliasing.

use deterministic_collections::HashMap;
use shuttle::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::borrow::Borrow;
use std::hash::Hash;

// ── DashMap ──────────────────────────────────────────────────────

pub struct DashMap<K, V> {
    inner: RwLock<HashMap<K, V>>,
}

// SAFETY: RwLock provides the synchronization. These match real DashMap's bounds.
unsafe impl<K: Send, V: Send> Send for DashMap<K, V> {}
unsafe impl<K: Send + Sync, V: Send + Sync> Sync for DashMap<K, V> {}

impl<K: std::fmt::Debug, V: std::fmt::Debug> std::fmt::Debug for DashMap<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DashMap").finish_non_exhaustive()
    }
}

impl<K: Eq + Hash, V> Default for DashMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Eq + Hash + Clone, V: Clone> Clone for DashMap<K, V> {
    fn clone(&self) -> Self {
        Self {
            inner: RwLock::new(self.inner.read().unwrap().clone()),
        }
    }
}

impl<K: Eq + Hash, V> FromIterator<(K, V)> for DashMap<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Self {
            inner: RwLock::new(iter.into_iter().collect()),
        }
    }
}

impl<K: Eq + Hash, V> Extend<(K, V)> for DashMap<K, V> {
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        self.inner.write().unwrap().extend(iter);
    }
}

impl<K: Eq + Hash, V> IntoIterator for DashMap<K, V> {
    type Item = (K, V);

    type IntoIter = std::collections::hash_map::IntoIter<K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.inner.into_inner().unwrap().into_iter()
    }
}

impl<K: Eq + Hash, V> DashMap<K, V> {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: RwLock::new(HashMap::with_capacity(capacity)),
        }
    }

    pub fn with_shard_amount(_: usize) -> Self {
        Self::new()
    }

    pub fn with_capacity_and_shard_amount(cap: usize, _: usize) -> Self {
        Self::with_capacity(cap)
    }

    pub fn insert(&self, key: K, value: V) -> Option<V> {
        self.inner.write().unwrap().insert(key, value)
    }

    pub fn get<Q>(&self, key: &Q) -> Option<Ref<'_, K, V>>
    where
        K: Borrow<Q> + Clone,
        Q: Hash + Eq + ?Sized,
    {
        let guard = self.inner.read().unwrap();
        let k = guard.get_key_value(key)?.0.clone();
        Some(Ref { guard, key: k })
    }

    pub fn get_mut<Q>(&self, key: &Q) -> Option<RefMut<'_, K, V>>
    where
        K: Borrow<Q> + Clone,
        Q: Hash + Eq + ?Sized,
    {
        let guard = self.inner.write().unwrap();
        let k = guard.get_key_value(key)?.0.clone();
        Some(RefMut { guard, key: k })
    }

    pub fn try_get<Q>(&self, key: &Q) -> TryResult<Ref<'_, K, V>>
    where
        K: Borrow<Q> + Clone,
        Q: Hash + Eq + ?Sized,
    {
        let guard = match self.inner.try_read() {
            Ok(g) => g,
            Err(_) => return TryResult::Locked,
        };
        let k = match guard.get_key_value(key) {
            Some((k, _)) => k.clone(),
            None => return TryResult::Absent,
        };
        TryResult::Present(Ref { guard, key: k })
    }

    pub fn try_get_mut<Q>(&self, key: &Q) -> TryResult<RefMut<'_, K, V>>
    where
        K: Borrow<Q> + Clone,
        Q: Hash + Eq + ?Sized,
    {
        let guard = match self.inner.try_write() {
            Ok(g) => g,
            Err(_) => return TryResult::Locked,
        };
        let k = match guard.get_key_value(key) {
            Some((k, _)) => k.clone(),
            None => return TryResult::Absent,
        };
        TryResult::Present(RefMut { guard, key: k })
    }

    pub fn remove<Q>(&self, key: &Q) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner.write().unwrap().remove_entry(key)
    }

    pub fn remove_if<Q>(&self, key: &Q, f: impl FnOnce(&K, &V) -> bool) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let mut guard = self.inner.write().unwrap();
        if let Some((k, v)) = guard.remove_entry(key) {
            if f(&k, &v) {
                return Some((k, v));
            }
            guard.insert(k, v);
        }
        None
    }

    pub fn remove_if_mut<Q>(&self, key: &Q, f: impl FnOnce(&K, &mut V) -> bool) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let mut guard = self.inner.write().unwrap();
        if let Some((k, mut v)) = guard.remove_entry(key) {
            if f(&k, &mut v) {
                return Some((k, v));
            }
            guard.insert(k, v);
        }
        None
    }

    pub fn entry(&self, key: K) -> Entry<'_, K, V> {
        let guard = self.inner.write().unwrap();
        if guard.contains_key(&key) {
            Entry::Occupied(OccupiedEntry { guard, key })
        } else {
            Entry::Vacant(VacantEntry { guard, key })
        }
    }

    pub fn try_entry(&self, key: K) -> Option<Entry<'_, K, V>> {
        let guard = self.inner.try_write().ok()?;
        if guard.contains_key(&key) {
            Some(Entry::Occupied(OccupiedEntry { guard, key }))
        } else {
            Some(Entry::Vacant(VacantEntry { guard, key }))
        }
    }

    pub fn iter(&self) -> Iter<'_, K, V>
    where
        K: Clone,
    {
        let guard = self.inner.read().unwrap();
        let keys: Vec<K> = guard.keys().cloned().collect();
        Iter { guard, keys, index: 0 }
    }

    pub fn iter_mut(&self) -> IterMut<'_, K, V>
    where
        K: Clone,
    {
        let guard = self.inner.write().unwrap();
        let keys: Vec<K> = guard.keys().cloned().collect();
        IterMut { guard, keys, index: 0 }
    }

    pub fn len(&self) -> usize {
        self.inner.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.read().unwrap().is_empty()
    }

    pub fn clear(&self) {
        self.inner.write().unwrap().clear();
    }

    pub fn capacity(&self) -> usize {
        self.inner.read().unwrap().capacity()
    }

    pub fn shrink_to_fit(&self) {
        self.inner.write().unwrap().shrink_to_fit();
    }

    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner.read().unwrap().contains_key(key)
    }

    pub fn retain(&self, mut f: impl FnMut(&K, &mut V) -> bool) {
        self.inner.write().unwrap().retain(|k, v| f(k, v));
    }

    pub fn alter<Q>(&self, key: &Q, f: impl FnOnce(&K, V) -> V)
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let mut guard = self.inner.write().unwrap();
        if let Some((k, v)) = guard.remove_entry(key) {
            let new_v = f(&k, v);
            guard.insert(k, new_v);
        }
    }

    pub fn alter_all(&self, mut f: impl FnMut(&K, V) -> V) {
        let mut guard = self.inner.write().unwrap();
        let entries: Vec<(K, V)> = guard.drain().collect();
        for (k, v) in entries {
            let new_v = f(&k, v);
            guard.insert(k, new_v);
        }
    }

    pub fn view<Q, R>(&self, key: &Q, f: impl FnOnce(&K, &V) -> R) -> Option<R>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner.read().unwrap().get_key_value(key).map(|(k, v)| f(k, v))
    }
}

// ── TryResult ───────────────────────────────────────────────────

pub enum TryResult<R> {
    Present(R),
    Absent,
    Locked,
}

impl<R> TryResult<R> {
    pub fn is_present(&self) -> bool {
        matches!(self, TryResult::Present(_))
    }

    pub fn is_absent(&self) -> bool {
        matches!(self, TryResult::Absent)
    }

    pub fn is_locked(&self) -> bool {
        matches!(self, TryResult::Locked)
    }

    pub fn unwrap(self) -> R {
        match self {
            TryResult::Present(r) => r,
            TryResult::Absent => panic!("called unwrap on TryResult::Absent"),
            TryResult::Locked => panic!("called unwrap on TryResult::Locked"),
        }
    }

    pub fn try_unwrap(self) -> Option<R> {
        match self {
            TryResult::Present(r) => Some(r),
            _ => None,
        }
    }
}

// ── Ref (immutable guard) ───────────────────────────────────────

pub struct Ref<'a, K, V> {
    guard: RwLockReadGuard<'a, HashMap<K, V>>,
    key: K,
}

// SAFETY: the RwLock provides synchronization. These match real DashMap's Ref bounds.
unsafe impl<K: Send, V: Send> Send for Ref<'_, K, V> {}
unsafe impl<K: Sync, V: Sync> Sync for Ref<'_, K, V> {}

impl<K: Eq + Hash, V> Ref<'_, K, V> {
    pub fn key(&self) -> &K {
        &self.key
    }

    pub fn value(&self) -> &V {
        self.guard.get(&self.key).unwrap()
    }

    pub fn pair(&self) -> (&K, &V) {
        (&self.key, self.value())
    }
}

impl<K: Eq + Hash, V> std::ops::Deref for Ref<'_, K, V> {
    type Target = V;

    fn deref(&self) -> &V {
        self.value()
    }
}

// ── RefMut (mutable guard) ──────────────────────────────────────

pub struct RefMut<'a, K, V> {
    guard: RwLockWriteGuard<'a, HashMap<K, V>>,
    key: K,
}

// SAFETY: the RwLock provides synchronization. These match real DashMap's RefMut bounds.
unsafe impl<K: Send, V: Send> Send for RefMut<'_, K, V> {}
unsafe impl<K: Sync, V: Sync> Sync for RefMut<'_, K, V> {}

impl<K: Eq + Hash, V> RefMut<'_, K, V> {
    pub fn key(&self) -> &K {
        &self.key
    }

    pub fn value(&self) -> &V {
        self.guard.get(&self.key).unwrap()
    }

    pub fn value_mut(&mut self) -> &mut V {
        self.guard.get_mut(&self.key).unwrap()
    }

    pub fn pair(&self) -> (&K, &V) {
        (&self.key, self.value())
    }

    pub fn pair_mut(&mut self) -> (&K, &mut V) {
        let v = self.guard.get_mut(&self.key).unwrap();
        (&self.key, v)
    }
}

impl<K: Eq + Hash, V> std::ops::Deref for RefMut<'_, K, V> {
    type Target = V;

    fn deref(&self) -> &V {
        self.value()
    }
}

impl<K: Eq + Hash, V> std::ops::DerefMut for RefMut<'_, K, V> {
    fn deref_mut(&mut self) -> &mut V {
        self.value_mut()
    }
}

// ── Entry API ───────────────────────────────────────────────────

pub enum Entry<'a, K, V> {
    Occupied(OccupiedEntry<'a, K, V>),
    Vacant(VacantEntry<'a, K, V>),
}

impl<'a, K: Eq + Hash + Clone, V> Entry<'a, K, V> {
    pub fn or_insert(self, default: V) -> RefMut<'a, K, V> {
        match self {
            Entry::Occupied(e) => e.into_ref(),
            Entry::Vacant(e) => e.insert(default),
        }
    }

    pub fn or_insert_with(self, default: impl FnOnce() -> V) -> RefMut<'a, K, V> {
        match self {
            Entry::Occupied(e) => e.into_ref(),
            Entry::Vacant(e) => e.insert(default()),
        }
    }

    pub fn or_insert_with_key(self, default: impl FnOnce(&K) -> V) -> RefMut<'a, K, V> {
        match self {
            Entry::Occupied(e) => e.into_ref(),
            Entry::Vacant(e) => {
                let value = default(e.key());
                e.insert(value)
            }
        }
    }

    pub fn and_modify(self, f: impl FnOnce(&mut V)) -> Self {
        match self {
            Entry::Occupied(mut e) => {
                f(e.get_mut());
                Entry::Occupied(e)
            }
            Entry::Vacant(e) => Entry::Vacant(e),
        }
    }

    pub fn key(&self) -> &K {
        match self {
            Entry::Occupied(e) => e.key(),
            Entry::Vacant(e) => e.key(),
        }
    }
}

impl<'a, K: Eq + Hash + Clone, V: Default> Entry<'a, K, V> {
    pub fn or_default(self) -> RefMut<'a, K, V> {
        self.or_insert_with(V::default)
    }
}

pub struct OccupiedEntry<'a, K, V> {
    guard: RwLockWriteGuard<'a, HashMap<K, V>>,
    key: K,
}

impl<'a, K: Eq + Hash + Clone, V> OccupiedEntry<'a, K, V> {
    pub fn key(&self) -> &K {
        &self.key
    }

    pub fn get(&self) -> &V {
        self.guard.get(&self.key).unwrap()
    }

    pub fn get_mut(&mut self) -> &mut V {
        self.guard.get_mut(&self.key).unwrap()
    }

    pub fn into_ref(self) -> RefMut<'a, K, V> {
        let key = self.key;
        RefMut { guard: self.guard, key }
    }

    pub fn insert(&mut self, value: V) -> V {
        self.guard.insert(self.key.clone(), value).unwrap()
    }

    pub fn into_key(self) -> K {
        self.key
    }

    pub fn remove(mut self) -> V {
        self.guard.remove(&self.key).unwrap()
    }

    pub fn remove_entry(mut self) -> (K, V) {
        self.guard.remove_entry(&self.key).unwrap()
    }

    pub fn replace_entry(mut self, value: V) -> (K, V) {
        let (old_key, old_value) = self.guard.remove_entry(&self.key).unwrap();
        self.guard.insert(self.key, value);
        (old_key, old_value)
    }

    pub fn replace_entry_with(mut self, f: impl FnOnce(&K, V) -> Option<V>) -> Entry<'a, K, V> {
        let value = self.guard.remove(&self.key).unwrap();
        match f(&self.key, value) {
            Some(new_value) => {
                self.guard.insert(self.key.clone(), new_value);
                Entry::Occupied(self)
            }
            None => Entry::Vacant(VacantEntry {
                guard: self.guard,
                key: self.key,
            }),
        }
    }
}

pub struct VacantEntry<'a, K, V> {
    guard: RwLockWriteGuard<'a, HashMap<K, V>>,
    key: K,
}

impl<'a, K: Eq + Hash + Clone, V> VacantEntry<'a, K, V> {
    pub fn key(&self) -> &K {
        &self.key
    }

    pub fn into_key(self) -> K {
        self.key
    }

    pub fn insert(mut self, value: V) -> RefMut<'a, K, V> {
        self.guard.insert(self.key.clone(), value);
        let key = self.key;
        RefMut { guard: self.guard, key }
    }

    pub fn insert_entry(mut self, value: V) -> OccupiedEntry<'a, K, V> {
        self.guard.insert(self.key.clone(), value);
        OccupiedEntry {
            guard: self.guard,
            key: self.key,
        }
    }
}

// ── Iter (immutable) ────────────────────────────────────────────

pub struct Iter<'a, K, V> {
    guard: RwLockReadGuard<'a, HashMap<K, V>>,
    keys: Vec<K>,
    index: usize,
}

impl<'a, K: Eq + Hash, V> Iterator for Iter<'a, K, V> {
    type Item = RefMulti<'a, K, V>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.keys.len() {
            return None;
        }
        let i = self.index;
        self.index += 1;
        let (k, v) = self.guard.get_key_value(&self.keys[i]).unwrap();
        // SAFETY: the references are valid for 'a because the RwLockReadGuard
        // (which keeps the HashMap alive) lives for 'a in this struct.
        let k: &'a K = unsafe { &*(k as *const K) };
        let v: &'a V = unsafe { &*(v as *const V) };
        Some(RefMulti { key: k, value: v })
    }
}

// ── IterMut ─────────────────────────────────────────────────────

pub struct IterMut<'a, K, V> {
    guard: RwLockWriteGuard<'a, HashMap<K, V>>,
    keys: Vec<K>,
    index: usize,
}

impl<'a, K: Eq + Hash, V> Iterator for IterMut<'a, K, V> {
    type Item = RefMutMulti<'a, K, V>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.keys.len() {
            return None;
        }
        let i = self.index;
        self.index += 1;
        let k = self.guard.get_key_value(&self.keys[i]).unwrap().0 as *const K;
        let v = self.guard.get_mut(&self.keys[i]).unwrap() as *mut V;
        // SAFETY: guard keeps HashMap alive for 'a. Each key is visited
        // exactly once, so no mutable aliasing occurs.
        let k: &'a K = unsafe { &*k };
        let v: &'a mut V = unsafe { &mut *v };
        Some(RefMutMulti { key: k, value: v })
    }
}

// ── RefMulti / RefMutMulti (iterator items) ─────────────────────

pub struct RefMulti<'a, K, V> {
    key: &'a K,
    value: &'a V,
}

impl<K, V> RefMulti<'_, K, V> {
    pub fn key(&self) -> &K {
        self.key
    }
    pub fn value(&self) -> &V {
        self.value
    }
    pub fn pair(&self) -> (&K, &V) {
        (self.key, self.value)
    }
}

impl<K, V> std::ops::Deref for RefMulti<'_, K, V> {
    type Target = V;

    fn deref(&self) -> &V {
        self.value
    }
}

pub struct RefMutMulti<'a, K, V> {
    key: &'a K,
    value: &'a mut V,
}

impl<K, V> RefMutMulti<'_, K, V> {
    pub fn key(&self) -> &K {
        self.key
    }

    pub fn value(&self) -> &V {
        self.value
    }

    pub fn value_mut(&mut self) -> &mut V {
        self.value
    }

    pub fn pair(&self) -> (&K, &V) {
        (self.key, self.value)
    }

    pub fn pair_mut(&mut self) -> (&K, &mut V) {
        (self.key, self.value)
    }
}

impl<K, V> std::ops::Deref for RefMutMulti<'_, K, V> {
    type Target = V;

    fn deref(&self) -> &V {
        self.value
    }
}

impl<K, V> std::ops::DerefMut for RefMutMulti<'_, K, V> {
    fn deref_mut(&mut self) -> &mut V {
        self.value
    }
}

// ── Module re-exports for import compatibility ──────────────────

pub mod mapref {
    pub mod one {
        pub use crate::{Ref, RefMut};
    }

    pub mod entry {
        pub use crate::{Entry, OccupiedEntry, VacantEntry};
    }

    pub mod multiple {
        pub use crate::{RefMulti, RefMutMulti};
    }
}

pub mod try_result {
    pub use crate::TryResult;
}
