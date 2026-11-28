//! An ordered map with the API of crossbeam-skiplist's `map` module. See [`SkipMap`].

use crate::per_execution::{PerExecution, in_execution, synchronize};
use shuttle_engine::runtime::task::clock::VectorClock;
use shuttle_engine::runtime::thread::switch;
use std::borrow::Borrow;
use std::collections::HashSet;
use std::fmt;
use std::marker::PhantomData;
use std::ops::{Bound, RangeBounds};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Source of the nodes' identifiers, which tell a removed node from a present one without `K: Ord`.
static NEXT_NODE: AtomicU64 = AtomicU64::new(0);

/// How many entries crossbeam's `clear` removes before it starts over from the front.
const CLEAR_BATCH: usize = 100;

/// An entry of the map. Entries share it, so that an entry stays readable after its removal, as
/// crossbeam's reference-counted nodes do.
pub(crate) struct Node<K, V> {
    id: u64,
    key: K,
    value: V,
}

impl<K, V> Node<K, V> {
    fn new(key: K, value: V) -> Arc<Self> {
        Arc::new(Self {
            id: NEXT_NODE.fetch_add(1, Ordering::Relaxed),
            key,
            value,
        })
    }
}

/// The map in one execution.
struct MapState<K, V> {
    /// The entries, sorted by key, with no two keys equal.
    entries: Vec<Arc<Node<K, V>>>,
    /// The nodes removed in this execution.
    removed: HashSet<u64>,
    clock: VectorClock,
}

impl<K, V> MapState<K, V> {
    fn new(seed: &[Arc<Node<K, V>>]) -> Self {
        Self {
            entries: seed.to_vec(),
            removed: HashSet::new(),
            clock: VectorClock::new(),
        }
    }

    fn is_removed(&self, node: &Node<K, V>) -> bool {
        self.removed.contains(&node.id)
    }

    fn take(&mut self, index: usize) -> Arc<Node<K, V>> {
        let node = self.entries.remove(index);
        self.removed.insert(node.id);
        node
    }
}

impl<K: Ord, V> MapState<K, V> {
    fn find<Q>(&self, key: &Q) -> Result<usize, usize>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.entries.binary_search_by(|node| node.key.borrow().cmp(key))
    }

    fn get<Q>(&self, key: &Q) -> Option<Arc<Node<K, V>>>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.find(key).ok().map(|index| Arc::clone(&self.entries[index]))
    }

    /// The first entry above `bound`.
    fn lower_bound<Q>(&self, bound: Bound<&Q>) -> Option<Arc<Node<K, V>>>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let index = match bound {
            Bound::Unbounded => 0,
            Bound::Included(bound) => self.entries.partition_point(|node| node.key.borrow() < bound),
            Bound::Excluded(bound) => self.entries.partition_point(|node| node.key.borrow() <= bound),
        };
        self.entries.get(index).cloned()
    }

    /// The last entry below `bound`.
    fn upper_bound<Q>(&self, bound: Bound<&Q>) -> Option<Arc<Node<K, V>>>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let end = match bound {
            Bound::Unbounded => self.entries.len(),
            Bound::Included(bound) => self.entries.partition_point(|node| node.key.borrow() <= bound),
            Bound::Excluded(bound) => self.entries.partition_point(|node| node.key.borrow() < bound),
        };
        end.checked_sub(1).map(|index| Arc::clone(&self.entries[index]))
    }

    /// The first entry after `key`, whether or not `key` is in the map, as crossbeam searches from a
    /// removed node.
    fn after(&self, key: &K) -> Option<Arc<Node<K, V>>> {
        self.lower_bound(Bound::Excluded(key))
    }

    /// The last entry before `key`.
    fn before(&self, key: &K) -> Option<Arc<Node<K, V>>> {
        self.upper_bound(Bound::Excluded(key))
    }

    /// Removes `node` if it is still in the map, and returns it.
    fn remove_node(&mut self, node: &Arc<Node<K, V>>) -> Option<Arc<Node<K, V>>> {
        match self.find(&node.key) {
            Ok(index) if Arc::ptr_eq(&self.entries[index], node) => Some(self.take(index)),
            _ => None,
        }
    }

    /// Inserts `node`, whose key must not be in the map.
    fn insert_new(&mut self, node: Arc<Node<K, V>>) {
        let index = self.find(&node.key).expect_err("the key is not in the map");
        self.entries.insert(index, node);
    }
}

/// An ordered map, with the API of [`crossbeam_skiplist::SkipMap`].
///
/// A sorted sequence of entries that each operation reads or changes at a scheduling point. See the
/// [crate documentation](crate) for the scheduling points, and for what is not modelled exactly.
///
/// [`crossbeam_skiplist::SkipMap`]: https://docs.rs/crossbeam-skiplist/0.1/crossbeam_skiplist/map/struct.SkipMap.html
pub struct SkipMap<K, V> {
    state: PerExecution<MapState<K, V>>,
    /// The entries every execution starts with: those `from_iter` put in outside of an execution.
    seed: Vec<Arc<Node<K, V>>>,
}

impl<K, V> SkipMap<K, V> {
    /// Returns a new, empty map.
    pub fn new() -> Self {
        Self {
            state: PerExecution::new(),
            seed: Vec::new(),
        }
    }

    fn with_state<R>(&self, f: impl FnOnce(&mut MapState<K, V>) -> R) -> R {
        let seed = &self.seed;
        let mut state = self.state.get(|| MapState::new(seed)).borrow_mut();
        synchronize(&mut state.clock);
        f(&mut state)
    }

    /// Returns `true` if the map is empty.
    pub fn is_empty(&self) -> bool {
        switch();
        self.with_state(|state| state.entries.is_empty())
    }

    /// Returns the number of entries in the map.
    ///
    /// crossbeam's is an approximation while the map is being modified; this one is exact.
    pub fn len(&self) -> usize {
        switch();
        self.with_state(|state| state.entries.len())
    }

    fn entry(&self, node: Arc<Node<K, V>>) -> Entry<'_, K, V> {
        Entry { map: self, node }
    }
}

impl<K, V> SkipMap<K, V>
where
    K: Ord,
{
    /// Returns the entry with the smallest key.
    pub fn front(&self) -> Option<Entry<'_, K, V>> {
        switch();
        self.with_state(|state| state.entries.first().cloned())
            .map(|node| self.entry(node))
    }

    /// Returns the entry with the largest key.
    pub fn back(&self) -> Option<Entry<'_, K, V>> {
        switch();
        self.with_state(|state| state.entries.last().cloned())
            .map(|node| self.entry(node))
    }

    /// Returns `true` if the map contains a value for the specified key.
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        switch();
        self.with_state(|state| state.find(key).is_ok())
    }

    /// Returns an entry with the specified `key`.
    pub fn get<Q>(&self, key: &Q) -> Option<Entry<'_, K, V>>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        switch();
        self.with_state(|state| state.get(key)).map(|node| self.entry(node))
    }

    /// Returns an `Entry` pointing to the lowest element whose key is above the given bound. If no
    /// such element is found then `None` is returned.
    pub fn lower_bound<'a, Q>(&'a self, bound: Bound<&Q>) -> Option<Entry<'a, K, V>>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        switch();
        self.with_state(|state| state.lower_bound(bound))
            .map(|node| self.entry(node))
    }

    /// Returns an `Entry` pointing to the highest element whose key is below the given bound. If no
    /// such element is found then `None` is returned.
    pub fn upper_bound<'a, Q>(&'a self, bound: Bound<&Q>) -> Option<Entry<'a, K, V>>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        switch();
        self.with_state(|state| state.upper_bound(bound))
            .map(|node| self.entry(node))
    }

    /// Finds an entry with the specified key, or inserts a new `key`-`value` pair if none exist.
    pub fn get_or_insert(&self, key: K, value: V) -> Entry<'_, K, V> {
        switch();
        let (node, unused) = self.with_state(|state| match state.get(&key) {
            Some(existing) => (existing, Some((key, value))),
            None => {
                let node = Node::new(key, value);
                state.insert_new(Arc::clone(&node));
                (node, None)
            }
        });
        // Dropped once the map's state is no longer borrowed, since its destructors may use the map.
        drop(unused);
        self.entry(node)
    }

    /// Finds an entry with the specified key, or inserts a new `key`-`value` pair if none exist,
    /// where value is calculated with a function.
    ///
    /// As in crossbeam, `value_fn` runs without the map being locked, at a scheduling point of its
    /// own, so another thread may insert the key first, in which case the value is discarded.
    pub fn get_or_insert_with<F>(&self, key: K, value_fn: F) -> Entry<'_, K, V>
    where
        F: FnOnce() -> V,
    {
        switch();
        if let Some(existing) = self.with_state(|state| state.get(&key)) {
            return self.entry(existing);
        }
        let value = value_fn();
        switch();
        let (node, unused) = self.with_state(|state| match state.get(&key) {
            Some(existing) => (existing, Some((key, value))),
            None => {
                let node = Node::new(key, value);
                state.insert_new(Arc::clone(&node));
                (node, None)
            }
        });
        drop(unused);
        self.entry(node)
    }

    /// Returns an iterator over all entries in the map, sorted by key.
    pub fn iter(&self) -> Iter<'_, K, V> {
        Iter {
            map: self,
            head: None,
            tail: None,
        }
    }

    /// Returns an iterator over a subset of entries in the map.
    pub fn range<Q, R>(&self, range: R) -> Range<'_, Q, R, K, V>
    where
        K: Borrow<Q>,
        R: RangeBounds<Q>,
        Q: Ord + ?Sized,
    {
        Range {
            map: self,
            head: None,
            tail: None,
            range,
            _marker: PhantomData,
        }
    }
}

impl<K, V> SkipMap<K, V>
where
    K: Ord + Send + 'static,
    V: Send + 'static,
{
    /// Inserts a `key`-`value` pair into the map and returns the new entry.
    ///
    /// If there is an existing entry with this key, it will be removed before inserting the new one.
    /// As in crossbeam, that takes two steps: for a moment, the map holds neither entry.
    pub fn insert(&self, key: K, value: V) -> Entry<'_, K, V> {
        self.compare_insert(key, value, |_| true)
    }

    /// Inserts a `key`-`value` pair into the skip list and returns the new entry.
    ///
    /// If there is an existing entry with this key and `compare_fn(entry.value)` returns true, it
    /// will be removed before inserting the new one, in two steps as in [`insert`](Self::insert).
    /// The closure will not be called if the key is not present.
    pub fn compare_insert<F>(&self, key: K, value: V, compare_fn: F) -> Entry<'_, K, V>
    where
        F: Fn(&V) -> bool,
    {
        let mut pair = Some((key, value));
        loop {
            switch();
            let found = self.with_state(|state| {
                let key = &pair.as_ref().expect("still ours").0;
                match state.get(key) {
                    Some(existing) => Err(existing),
                    None => {
                        let (key, value) = pair.take().expect("still ours");
                        let node = Node::new(key, value);
                        state.insert_new(Arc::clone(&node));
                        Ok(node)
                    }
                }
            });
            match found {
                Ok(inserted) => return self.entry(inserted),
                Err(existing) => {
                    // The closure runs while the map's state is not borrowed, so that it may use the
                    // map.
                    if compare_fn(&existing.value) {
                        let removed = self.with_state(|state| state.remove_node(&existing));
                        drop(removed);
                        // Then search again, at a scheduling point: crossbeam repeats its search.
                    } else {
                        return self.entry(existing);
                    }
                }
            }
        }
    }

    /// Removes an entry with the specified `key` from the map and returns it.
    ///
    /// The value will not actually be dropped until all references to it have gone out of scope.
    pub fn remove<Q>(&self, key: &Q) -> Option<Entry<'_, K, V>>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        switch();
        self.with_state(|state| state.find(key).ok().map(|index| state.take(index)))
            .map(|node| self.entry(node))
    }

    /// Removes the entry with the lowest key from the map. Returns the removed entry.
    pub fn pop_front(&self) -> Option<Entry<'_, K, V>> {
        switch();
        self.with_state(|state| (!state.entries.is_empty()).then(|| state.take(0)))
            .map(|node| self.entry(node))
    }

    /// Removes the entry with the greatest key from the map. Returns the removed entry.
    pub fn pop_back(&self) -> Option<Entry<'_, K, V>> {
        switch();
        self.with_state(|state| {
            let last = state.entries.len().checked_sub(1)?;
            Some(state.take(last))
        })
        .map(|node| self.entry(node))
    }

    /// Removes all entries from the map.
    ///
    /// As crossbeam's, it removes the entries one at a time from the front, each at a scheduling
    /// point, and starts over from the front after every 100, so entries that other threads insert
    /// meanwhile may survive.
    pub fn clear(&self) {
        loop {
            switch();
            let mut entry = self.with_state(|state| state.entries.first().cloned());
            for _ in 0..CLEAR_BATCH {
                let Some(current) = entry else {
                    return;
                };
                switch();
                let (next, removed) = self.with_state(|state| (state.after(&current.key), state.remove_node(&current)));
                drop((current, removed));
                entry = next;
            }
        }
    }
}

impl<K, V> Default for SkipMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> fmt::Debug for SkipMap<K, V>
where
    K: Ord + fmt::Debug,
    V: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("SkipMap { .. }")
    }
}

impl<K, V> IntoIterator for SkipMap<K, V> {
    type Item = (K, V);
    type IntoIter = IntoIter<K, V>;

    /// Consumes the map, yielding this execution's entries in key order; outside of an execution,
    /// the entries it was built with. Not a scheduling point.
    fn into_iter(mut self) -> IntoIter<K, V> {
        let seed = std::mem::take(&mut self.seed);
        let entries = match self.state.take() {
            Some(state) => {
                drop(seed);
                state.entries
            }
            None => seed,
        };
        IntoIter {
            entries: entries.into_iter(),
        }
    }
}

impl<'a, K, V> IntoIterator for &'a SkipMap<K, V>
where
    K: Ord,
{
    type Item = Entry<'a, K, V>;
    type IntoIter = Iter<'a, K, V>;

    fn into_iter(self) -> Iter<'a, K, V> {
        self.iter()
    }
}

impl<K, V> FromIterator<(K, V)> for SkipMap<K, V>
where
    K: Ord,
{
    /// Builds a map, keeping the first value of each key, as crossbeam's does.
    ///
    /// Inside an execution the entries are this execution's; outside of one they are those that
    /// every execution starts with. Not a scheduling point: nothing else can reach the map yet.
    fn from_iter<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
    {
        // Collected before the map's state is borrowed, since the iterator may use Shuttle.
        let pairs: Vec<(K, V)> = iter.into_iter().collect();
        let mut entries: Vec<Arc<Node<K, V>>> = Vec::with_capacity(pairs.len());
        let mut duplicates = Vec::new();
        for (key, value) in pairs {
            match entries.binary_search_by(|node| node.key.cmp(&key)) {
                Ok(_) => duplicates.push((key, value)),
                Err(index) => entries.insert(index, Node::new(key, value)),
            }
        }
        drop(duplicates);
        let mut map = Self::new();
        if in_execution() {
            map.state.get(|| MapState {
                entries,
                removed: HashSet::new(),
                clock: VectorClock::new(),
            });
        } else {
            map.seed = entries;
        }
        map
    }
}

/// A reference-counted entry in a map.
pub struct Entry<'a, K, V> {
    map: &'a SkipMap<K, V>,
    node: Arc<Node<K, V>>,
}

impl<'a, K, V> Entry<'a, K, V> {
    /// Returns a reference to the key.
    pub fn key(&self) -> &K {
        &self.node.key
    }

    /// Returns a reference to the value.
    pub fn value(&self) -> &V {
        &self.node.value
    }

    /// Returns `true` if the entry is removed from the map.
    pub fn is_removed(&self) -> bool {
        switch();
        self.map.with_state(|state| state.is_removed(&self.node))
    }
}

impl<'a, K, V> Entry<'a, K, V>
where
    K: Ord,
{
    /// Moves to the next entry in the map.
    pub fn move_next(&mut self) -> bool {
        match self.next() {
            None => false,
            Some(next) => {
                *self = next;
                true
            }
        }
    }

    /// Moves to the previous entry in the map.
    pub fn move_prev(&mut self) -> bool {
        match self.prev() {
            None => false,
            Some(prev) => {
                *self = prev;
                true
            }
        }
    }

    /// Returns the next entry in the map.
    pub fn next(&self) -> Option<Entry<'a, K, V>> {
        switch();
        let map = self.map;
        map.with_state(|state| state.after(&self.node.key))
            .map(|node| map.entry(node))
    }

    /// Returns the previous entry in the map.
    pub fn prev(&self) -> Option<Entry<'a, K, V>> {
        switch();
        let map = self.map;
        map.with_state(|state| state.before(&self.node.key))
            .map(|node| map.entry(node))
    }
}

impl<K, V> Entry<'_, K, V>
where
    K: Ord + Send + 'static,
    V: Send + 'static,
{
    /// Removes the entry from the map.
    ///
    /// Returns `true` if this call removed the entry and `false` if it was already removed.
    pub fn remove(&self) -> bool {
        switch();
        let removed = self.map.with_state(|state| state.remove_node(&self.node));
        removed.is_some()
    }
}

impl<K, V> Clone for Entry<'_, K, V> {
    fn clone(&self) -> Self {
        Self {
            map: self.map,
            node: Arc::clone(&self.node),
        }
    }
}

impl<K, V> fmt::Debug for Entry<'_, K, V>
where
    K: fmt::Debug,
    V: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Entry").field(self.key()).field(self.value()).finish()
    }
}

/// An owning iterator over the entries of a `SkipMap`.
pub struct IntoIter<K, V> {
    entries: std::vec::IntoIter<Arc<Node<K, V>>>,
}

impl<K, V> Iterator for IntoIter<K, V> {
    type Item = (K, V);

    fn next(&mut self) -> Option<(K, V)> {
        let node = self.entries.next()?;
        // Entries borrow the map, and the map was consumed, so nothing else holds the node.
        let node = Arc::try_unwrap(node)
            .ok()
            .expect("an entry of a consumed map is not shared");
        Some((node.key, node.value))
    }
}

impl<K, V> fmt::Debug for IntoIter<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("IntoIter { .. }")
    }
}

/// An iterator over the entries of a `SkipMap`.
///
/// As crossbeam's, it is not a snapshot: each step finds the entry after the last one it returned
/// in the map as it is then, so it sees entries that are inserted ahead of it, and skips those that
/// are removed.
pub struct Iter<'a, K, V> {
    map: &'a SkipMap<K, V>,
    head: Option<Arc<Node<K, V>>>,
    tail: Option<Arc<Node<K, V>>>,
}

impl<'a, K, V> Iterator for Iter<'a, K, V>
where
    K: Ord,
{
    type Item = Entry<'a, K, V>;

    fn next(&mut self) -> Option<Entry<'a, K, V>> {
        switch();
        let next = self.map.with_state(|state| match &self.head {
            Some(head) => state.after(&head.key),
            None => state.entries.first().cloned(),
        })?;
        if let Some(tail) = &self.tail
            && next.key >= tail.key
        {
            return None;
        }
        self.head = Some(Arc::clone(&next));
        Some(self.map.entry(next))
    }
}

impl<'a, K, V> DoubleEndedIterator for Iter<'a, K, V>
where
    K: Ord,
{
    fn next_back(&mut self) -> Option<Entry<'a, K, V>> {
        switch();
        let next = self.map.with_state(|state| match &self.tail {
            Some(tail) => state.before(&tail.key),
            None => state.entries.last().cloned(),
        })?;
        if let Some(head) = &self.head
            && head.key >= next.key
        {
            return None;
        }
        self.tail = Some(Arc::clone(&next));
        Some(self.map.entry(next))
    }
}

impl<K, V> fmt::Debug for Iter<'_, K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Iter { .. }")
    }
}

/// An iterator over a subset of entries of a `SkipMap`.
///
/// Like [`Iter`], it is not a snapshot.
pub struct Range<'a, Q, R, K, V>
where
    K: Ord + Borrow<Q>,
    R: RangeBounds<Q>,
    Q: Ord + ?Sized,
{
    map: &'a SkipMap<K, V>,
    head: Option<Arc<Node<K, V>>>,
    tail: Option<Arc<Node<K, V>>>,
    range: R,
    _marker: PhantomData<fn() -> Q>,
}

impl<'a, Q, R, K, V> Range<'a, Q, R, K, V>
where
    K: Ord + Borrow<Q>,
    R: RangeBounds<Q>,
    Q: Ord + ?Sized,
{
    pub(crate) fn range(&self) -> &R {
        &self.range
    }

    pub(crate) fn head_key(&self) -> Option<&K> {
        self.head.as_ref().map(|node| &node.key)
    }

    pub(crate) fn tail_key(&self) -> Option<&K> {
        self.tail.as_ref().map(|node| &node.key)
    }
}

impl<'a, Q, R, K, V> Iterator for Range<'a, Q, R, K, V>
where
    K: Ord + Borrow<Q>,
    R: RangeBounds<Q>,
    Q: Ord + ?Sized,
{
    type Item = Entry<'a, K, V>;

    fn next(&mut self) -> Option<Entry<'a, K, V>> {
        switch();
        let next = self.map.with_state(|state| match &self.head {
            Some(head) => state.after(&head.key),
            None => state.lower_bound(self.range.start_bound()),
        })?;
        let bound = match &self.tail {
            Some(tail) => Bound::Excluded(tail.key.borrow()),
            None => self.range.end_bound(),
        };
        if !below_upper_bound(&bound, next.key.borrow()) {
            return None;
        }
        self.head = Some(Arc::clone(&next));
        Some(self.map.entry(next))
    }
}

impl<'a, Q, R, K, V> DoubleEndedIterator for Range<'a, Q, R, K, V>
where
    K: Ord + Borrow<Q>,
    R: RangeBounds<Q>,
    Q: Ord + ?Sized,
{
    fn next_back(&mut self) -> Option<Entry<'a, K, V>> {
        switch();
        let next = self.map.with_state(|state| match &self.tail {
            Some(tail) => state.before(&tail.key),
            None => state.upper_bound(self.range.end_bound()),
        })?;
        let bound = match &self.head {
            Some(head) => Bound::Excluded(head.key.borrow()),
            None => self.range.start_bound(),
        };
        if !above_lower_bound(&bound, next.key.borrow()) {
            return None;
        }
        self.tail = Some(Arc::clone(&next));
        Some(self.map.entry(next))
    }
}

/// Prints a cursor of a range as crossbeam does, which shows its `RefEntry`.
struct RefEntry<'a, K, V>(&'a Node<K, V>);

impl<K: fmt::Debug, V: fmt::Debug> fmt::Debug for RefEntry<'_, K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("RefEntry")
            .field(&self.0.key)
            .field(&self.0.value)
            .finish()
    }
}

impl<Q, R, K, V> fmt::Debug for Range<'_, Q, R, K, V>
where
    K: Ord + Borrow<Q> + fmt::Debug,
    V: fmt::Debug,
    R: RangeBounds<Q> + fmt::Debug,
    Q: Ord + ?Sized,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Range")
            .field("range", &self.range)
            .field("head", &self.head.as_deref().map(RefEntry))
            .field("tail", &self.tail.as_deref().map(RefEntry))
            .finish()
    }
}

fn above_lower_bound<T: Ord + ?Sized>(bound: &Bound<&T>, other: &T) -> bool {
    match *bound {
        Bound::Unbounded => true,
        Bound::Included(key) => other >= key,
        Bound::Excluded(key) => other > key,
    }
}

fn below_upper_bound<T: Ord + ?Sized>(bound: &Bound<&T>, other: &T) -> bool {
    match *bound {
        Bound::Unbounded => true,
        Bound::Included(key) => other <= key,
        Bound::Excluded(key) => other < key,
    }
}
