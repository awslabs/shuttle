//! An ordered set with the API of crossbeam-skiplist's `set` module. See [`SkipSet`].

use crate::map;
use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;
use std::ops::{Bound, RangeBounds};

/// An ordered set, with the API of [`crossbeam_skiplist::SkipSet`].
///
/// As crossbeam's, it is a [`SkipMap`](map::SkipMap) whose values are `()`, and every operation is
/// the map's.
///
/// [`crossbeam_skiplist::SkipSet`]: https://docs.rs/crossbeam-skiplist/0.1/crossbeam_skiplist/set/struct.SkipSet.html
pub struct SkipSet<T> {
    inner: map::SkipMap<T, ()>,
}

impl<T> SkipSet<T> {
    /// Returns a new, empty set.
    pub fn new() -> Self {
        Self {
            inner: map::SkipMap::new(),
        }
    }

    /// Returns `true` if the set is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Returns the number of entries in the set.
    pub fn len(&self) -> usize {
        self.inner.len()
    }
}

impl<T> SkipSet<T>
where
    T: Ord,
{
    /// Returns the entry with the smallest key.
    pub fn front(&self) -> Option<Entry<'_, T>> {
        self.inner.front().map(Entry::new)
    }

    /// Returns the entry with the largest key.
    pub fn back(&self) -> Option<Entry<'_, T>> {
        self.inner.back().map(Entry::new)
    }

    /// Returns `true` if the set contains a value for the specified key.
    pub fn contains<Q>(&self, key: &Q) -> bool
    where
        T: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.inner.contains_key(key)
    }

    /// Returns an entry with the specified `key`.
    pub fn get<Q>(&self, key: &Q) -> Option<Entry<'_, T>>
    where
        T: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.inner.get(key).map(Entry::new)
    }

    /// Returns an `Entry` pointing to the lowest element whose key is above the given bound.
    pub fn lower_bound<'a, Q>(&'a self, bound: Bound<&Q>) -> Option<Entry<'a, T>>
    where
        T: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.inner.lower_bound(bound).map(Entry::new)
    }

    /// Returns an `Entry` pointing to the highest element whose key is below the given bound.
    pub fn upper_bound<'a, Q>(&'a self, bound: Bound<&Q>) -> Option<Entry<'a, T>>
    where
        T: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.inner.upper_bound(bound).map(Entry::new)
    }

    /// Finds an entry with the specified key, or inserts a new `key`-`value` pair if none exist.
    pub fn get_or_insert(&self, key: T) -> Entry<'_, T> {
        Entry::new(self.inner.get_or_insert(key, ()))
    }

    /// Returns an iterator over all entries in the set.
    pub fn iter(&self) -> Iter<'_, T> {
        Iter {
            inner: self.inner.iter(),
        }
    }

    /// Returns an iterator over a subset of entries in the set.
    pub fn range<Q, R>(&self, range: R) -> Range<'_, Q, R, T>
    where
        T: Borrow<Q>,
        R: RangeBounds<Q>,
        Q: Ord + ?Sized,
    {
        Range {
            inner: self.inner.range(range),
        }
    }
}

impl<T> SkipSet<T>
where
    T: Ord + Send + 'static,
{
    /// Inserts a `key`-`value` pair into the set and returns the new entry.
    ///
    /// If there is an existing entry with this key, it will be removed before inserting the new one.
    pub fn insert(&self, key: T) -> Entry<'_, T> {
        Entry::new(self.inner.insert(key, ()))
    }

    /// Removes an entry with the specified key from the set and returns it.
    pub fn remove<Q>(&self, key: &Q) -> Option<Entry<'_, T>>
    where
        T: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.inner.remove(key).map(Entry::new)
    }

    /// Removes an entry from the front of the set. Returns the removed entry.
    pub fn pop_front(&self) -> Option<Entry<'_, T>> {
        self.inner.pop_front().map(Entry::new)
    }

    /// Removes an entry from the back of the set. Returns the removed entry.
    pub fn pop_back(&self) -> Option<Entry<'_, T>> {
        self.inner.pop_back().map(Entry::new)
    }

    /// Iterates over the set and removes every entry.
    pub fn clear(&self) {
        self.inner.clear();
    }
}

impl<T> Default for SkipSet<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> fmt::Debug for SkipSet<T>
where
    T: Ord + fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("SkipSet { .. }")
    }
}

impl<T> IntoIterator for SkipSet<T> {
    type Item = T;
    type IntoIter = IntoIter<T>;

    fn into_iter(self) -> IntoIter<T> {
        IntoIter {
            inner: self.inner.into_iter(),
        }
    }
}

impl<'a, T> IntoIterator for &'a SkipSet<T>
where
    T: Ord,
{
    type Item = Entry<'a, T>;
    type IntoIter = Iter<'a, T>;

    fn into_iter(self) -> Iter<'a, T> {
        self.iter()
    }
}

impl<T> FromIterator<T> for SkipSet<T>
where
    T: Ord,
{
    fn from_iter<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = T>,
    {
        Self {
            inner: iter.into_iter().map(|key| (key, ())).collect(),
        }
    }
}

/// A reference-counted entry in a set.
pub struct Entry<'a, T> {
    inner: map::Entry<'a, T, ()>,
}

impl<'a, T> Entry<'a, T> {
    fn new(inner: map::Entry<'a, T, ()>) -> Self {
        Self { inner }
    }

    /// Returns a reference to the key.
    pub fn value(&self) -> &T {
        self.inner.key()
    }

    /// Returns `true` if the entry is removed from the set.
    pub fn is_removed(&self) -> bool {
        self.inner.is_removed()
    }
}

impl<'a, T> Entry<'a, T>
where
    T: Ord,
{
    /// Moves to the next entry in the set.
    pub fn move_next(&mut self) -> bool {
        self.inner.move_next()
    }

    /// Moves to the previous entry in the set.
    pub fn move_prev(&mut self) -> bool {
        self.inner.move_prev()
    }

    /// Returns the next entry in the set.
    pub fn next(&self) -> Option<Entry<'a, T>> {
        self.inner.next().map(Entry::new)
    }

    /// Returns the previous entry in the set.
    pub fn prev(&self) -> Option<Entry<'a, T>> {
        self.inner.prev().map(Entry::new)
    }
}

impl<T> Entry<'_, T>
where
    T: Ord + Send + 'static,
{
    /// Removes the entry from the set.
    ///
    /// Returns `true` if this call removed the entry and `false` if it was already removed.
    pub fn remove(&self) -> bool {
        self.inner.remove()
    }
}

impl<T> Clone for Entry<'_, T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> fmt::Debug for Entry<'_, T>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Entry").field("value", self.value()).finish()
    }
}

impl<T> Deref for Entry<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.value()
    }
}

/// An owning iterator over the entries of a `SkipSet`.
pub struct IntoIter<T> {
    inner: map::IntoIter<T, ()>,
}

impl<T> Iterator for IntoIter<T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        self.inner.next().map(|(key, ())| key)
    }
}

impl<T> fmt::Debug for IntoIter<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("IntoIter { .. }")
    }
}

/// An iterator over the entries of a `SkipSet`.
pub struct Iter<'a, T> {
    inner: map::Iter<'a, T, ()>,
}

impl<'a, T> Iterator for Iter<'a, T>
where
    T: Ord,
{
    type Item = Entry<'a, T>;

    fn next(&mut self) -> Option<Entry<'a, T>> {
        self.inner.next().map(Entry::new)
    }
}

impl<'a, T> DoubleEndedIterator for Iter<'a, T>
where
    T: Ord,
{
    fn next_back(&mut self) -> Option<Entry<'a, T>> {
        self.inner.next_back().map(Entry::new)
    }
}

impl<T> fmt::Debug for Iter<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Iter { .. }")
    }
}

/// An iterator over a subset of entries of a `SkipSet`.
pub struct Range<'a, Q, R, T>
where
    T: Ord + Borrow<Q>,
    R: RangeBounds<Q>,
    Q: Ord + ?Sized,
{
    inner: map::Range<'a, Q, R, T, ()>,
}

impl<'a, Q, R, T> Iterator for Range<'a, Q, R, T>
where
    T: Ord + Borrow<Q>,
    R: RangeBounds<Q>,
    Q: Ord + ?Sized,
{
    type Item = Entry<'a, T>;

    fn next(&mut self) -> Option<Entry<'a, T>> {
        self.inner.next().map(Entry::new)
    }
}

impl<'a, Q, R, T> DoubleEndedIterator for Range<'a, Q, R, T>
where
    T: Ord + Borrow<Q>,
    R: RangeBounds<Q>,
    Q: Ord + ?Sized,
{
    fn next_back(&mut self) -> Option<Entry<'a, T>> {
        self.inner.next_back().map(Entry::new)
    }
}

impl<Q, R, T> fmt::Debug for Range<'_, Q, R, T>
where
    T: Ord + Borrow<Q> + fmt::Debug,
    R: RangeBounds<Q> + fmt::Debug,
    Q: Ord + ?Sized,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Range")
            .field("range", self.inner.range())
            .field("head", &self.inner.head_key())
            .field("tail", &self.inner.tail_key())
            .finish()
    }
}
