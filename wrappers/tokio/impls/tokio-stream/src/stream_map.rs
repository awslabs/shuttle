//! This file is lifted from [tokio-stream/src/stream_map.rs](https://github.com/tokio-rs/tokio/blob/9e94fa7e15cfe6ebbd06e9ebad4642896620d924/tokio-stream/src/stream_map.rs), and has had the following changes applied to it:
//! 1. Examples removed.
//! 2. Custom rand implementation removed. See CHANGED below.
use crate::Stream;

use shuttle::rand::Rng;
use std::borrow::Borrow;
use std::hash::Hash;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Combine many streams into one, indexing each source stream with a unique
/// key.
///
/// `StreamMap` is similar to [`StreamExt::merge`] in that it combines source
/// streams into a single merged stream that yields values in the order that
/// they arrive from the source streams. However, `StreamMap` has a lot more
/// flexibility in usage patterns.
///
/// `StreamMap` can:
///
/// * Merge an arbitrary number of streams.
/// * Track which source stream the value was received from.
/// * Handle inserting and removing streams from the set of managed streams at
///   any point during iteration.
///
/// All source streams held by `StreamMap` are indexed using a key. This key is
/// included with the value when a source stream yields a value. The key is also
/// used to remove the stream from the `StreamMap` before the stream has
/// completed streaming.
///
/// # `Unpin`
///
/// Because the `StreamMap` API moves streams during runtime, both streams and
/// keys must be `Unpin`. In order to insert a `!Unpin` stream into a
/// `StreamMap`, use [`pin!`] to pin the stream to the stack or [`Box::pin`] to
/// pin the stream in the heap.
///
/// # Implementation
///
/// `StreamMap` is backed by a `Vec<(K, V)>`. There is no guarantee that this
/// internal implementation detail will persist in future versions, but it is
/// important to know the runtime implications. In general, `StreamMap` works
/// best with a "smallish" number of streams as all entries are scanned on
/// insert, remove, and polling. In cases where a large number of streams need
/// to be merged, it may be advisable to use tasks sending values on a shared
/// [`mpsc`] channel.
///
/// # Notes
///
/// `StreamMap` removes finished streams automatically, without alerting the user.
/// In some scenarios, the caller would want to know on closed streams.
/// To do this, use [`StreamNotifyClose`] as a wrapper to your stream.
/// It will return None when the stream is closed.
///
/// [`StreamExt::merge`]: crate::StreamExt::merge
/// [`mpsc`]: https://docs.rs/tokio/1.0/tokio/sync/mpsc/index.html
/// [`pin!`]: https://docs.rs/tokio/1.0/tokio/macro.pin.html
/// [`Box::pin`]: std::boxed::Box::pin
/// [`StreamNotifyClose`]: crate::StreamNotifyClose

#[derive(Debug)]
pub struct StreamMap<K, V> {
    /// Streams stored in the map
    entries: Vec<(K, V)>,
}

impl<K, V> StreamMap<K, V> {
    /// An iterator visiting all key-value pairs in arbitrary order.
    ///
    /// The iterator element type is `&'a (K, V)`.
    pub fn iter(&self) -> impl Iterator<Item = &(K, V)> {
        self.entries.iter()
    }

    /// An iterator visiting all key-value pairs mutably in arbitrary order.
    ///
    /// The iterator element type is `&'a mut (K, V)`.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut (K, V)> {
        self.entries.iter_mut()
    }

    /// Creates an empty `StreamMap`.
    ///
    /// The stream map is initially created with a capacity of `0`, so it will
    /// not allocate until it is first inserted into.
    pub fn new() -> StreamMap<K, V> {
        StreamMap { entries: vec![] }
    }

    /// Creates an empty `StreamMap` with the specified capacity.
    ///
    /// The stream map will be able to hold at least `capacity` elements without
    /// reallocating. If `capacity` is 0, the stream map will not allocate.
    pub fn with_capacity(capacity: usize) -> StreamMap<K, V> {
        StreamMap {
            entries: Vec::with_capacity(capacity),
        }
    }

    /// Returns an iterator visiting all keys in arbitrary order.
    ///
    /// The iterator element type is `&'a K`.
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.iter().map(|(k, _)| k)
    }

    /// An iterator visiting all values in arbitrary order.
    ///
    /// The iterator element type is `&'a V`.
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.iter().map(|(_, v)| v)
    }

    /// An iterator visiting all values mutably in arbitrary order.
    ///
    /// The iterator element type is `&'a mut V`.
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.iter_mut().map(|(_, v)| v)
    }

    /// Returns the number of streams the map can hold without reallocating.
    ///
    /// This number is a lower bound; the `StreamMap` might be able to hold
    /// more, but is guaranteed to be able to hold at least this many.
    pub fn capacity(&self) -> usize {
        self.entries.capacity()
    }

    /// Returns the number of streams in the map.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if the map contains no elements.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Clears the map, removing all key-stream pairs. Keeps the allocated
    /// memory for reuse.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Insert a key-stream pair into the map.
    ///
    /// If the map did not have this key present, `None` is returned.
    ///
    /// If the map did have this key present, the new `stream` replaces the old
    /// one and the old stream is returned.
    pub fn insert(&mut self, k: K, stream: V) -> Option<V>
    where
        K: Hash + Eq,
    {
        let ret = self.remove(&k);
        self.entries.push((k, stream));

        ret
    }

    /// Removes a key from the map, returning the stream at the key if the key was previously in the map.
    ///
    /// The key may be any borrowed form of the map's key type, but `Hash` and
    /// `Eq` on the borrowed form must match those for the key type.
    pub fn remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        for i in 0..self.entries.len() {
            if self.entries[i].0.borrow() == k {
                return Some(self.entries.swap_remove(i).1);
            }
        }

        None
    }

    /// Returns `true` if the map contains a stream for the specified key.
    ///
    /// The key may be any borrowed form of the map's key type, but `Hash` and
    /// `Eq` on the borrowed form must match those for the key type.
    pub fn contains_key<Q>(&self, k: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        for i in 0..self.entries.len() {
            if self.entries[i].0.borrow() == k {
                return true;
            }
        }

        false
    }
}

impl<K, V> StreamMap<K, V>
where
    K: Unpin,
    V: Stream + Unpin,
{
    /// Polls the next value, includes the vec entry index
    fn poll_next_entry(&mut self, cx: &mut Context<'_>) -> Poll<Option<(usize, V::Item)>> {
        // CHANGED wrt Tokio
        let start = if self.entries.len() == 0 {
            0
        } else {
            shuttle::rand::thread_rng().gen::<usize>() % self.entries.len()
        };
        let mut idx = start;

        for _ in 0..self.entries.len() {
            let (_, stream) = &mut self.entries[idx];

            match Pin::new(stream).poll_next(cx) {
                Poll::Ready(Some(val)) => return Poll::Ready(Some((idx, val))),
                Poll::Ready(None) => {
                    // Remove the entry
                    self.entries.swap_remove(idx);

                    // Check if this was the last entry, if so the cursor needs
                    // to wrap
                    if idx == self.entries.len() {
                        idx = 0;
                    } else if idx < start && start <= self.entries.len() {
                        // The stream being swapped into the current index has
                        // already been polled, so skip it.
                        idx = idx.wrapping_add(1) % self.entries.len();
                    }
                }
                Poll::Pending => {
                    idx = idx.wrapping_add(1) % self.entries.len();
                }
            }
        }

        // If the map is empty, then the stream is complete.
        if self.entries.is_empty() {
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    }
}

impl<K, V> Default for StreamMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> Stream for StreamMap<K, V>
where
    K: Clone + Unpin,
    V: Stream + Unpin,
{
    type Item = (K, V::Item);

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some((idx, val)) = ready!(self.poll_next_entry(cx)) {
            let key = self.entries[idx].0.clone();
            Poll::Ready(Some((key, val)))
        } else {
            Poll::Ready(None)
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let mut ret = (0, Some(0));

        for (_, stream) in &self.entries {
            let hint = stream.size_hint();

            ret.0 += hint.0;

            match (ret.1, hint.1) {
                (Some(a), Some(b)) => ret.1 = Some(a + b),
                (Some(_), None) => ret.1 = None,
                _ => {}
            }
        }

        ret
    }
}

impl<K, V> FromIterator<(K, V)> for StreamMap<K, V>
where
    K: Hash + Eq,
{
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        let iterator = iter.into_iter();
        let (lower_bound, _) = iterator.size_hint();
        let mut stream_map = Self::with_capacity(lower_bound);

        for (key, value) in iterator {
            stream_map.insert(key, value);
        }

        stream_map
    }
}

impl<K, V> Extend<(K, V)> for StreamMap<K, V> {
    fn extend<T>(&mut self, iter: T)
    where
        T: IntoIterator<Item = (K, V)>,
    {
        self.entries.extend(iter);
    }
}
