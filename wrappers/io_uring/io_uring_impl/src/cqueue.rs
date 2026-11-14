//! Completion queue, mirroring [`io_uring::cqueue`](https://docs.rs/io-uring/0.7/io_uring/cqueue).

use std::fmt;

use crate::kernel::Ring;

/// Selects the CQE shape. Only the default 16-byte [`Entry`] is simulated.
pub trait EntryMarker: private::Sealed + Clone + fmt::Debug + Send + Sync + 'static {}

/// One completion queue entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub(crate) user_data: u64,
    pub(crate) result: i32,
    pub(crate) flags: u32,
}

impl Entry {
    /// The `user_data` of the SQE this completes.
    pub fn user_data(&self) -> u64 {
        self.user_data
    }

    /// The op's result: non-negative on success, `-errno` on failure.
    pub fn result(&self) -> i32 {
        self.result
    }

    /// The CQE flags. Always zero: the simulation has no multishot ops,
    /// provided buffers, or notification CQEs.
    pub fn flags(&self) -> u32 {
        self.flags
    }
}

impl EntryMarker for Entry {}

/// The completion queue, mirroring the real crate's `CompletionQueue`.
///
/// As on a real ring, the handle sees the completions posted by the time it
/// was created or last [`sync`](Self::sync)ed: loading the kernel's CQ tail
/// is a Shuttle scheduling point, so the kernel can post between a consumer's
/// loads, and a consumer that forgets to `sync` misses completions here as it
/// would on Linux.
pub struct CompletionQueue<'a, E: EntryMarker = Entry> {
    ring: &'a Ring,
    /// Completions the last tail load exposed and not yet taken.
    visible: u32,
    _marker: std::marker::PhantomData<E>,
}

impl<'a, E: EntryMarker> CompletionQueue<'a, E> {
    pub(crate) fn new(ring: &'a Ring) -> Self {
        let mut queue = Self {
            ring,
            visible: 0,
            _marker: std::marker::PhantomData,
        };
        queue.sync();
        queue
    }

    /// Load the kernel's CQ tail.
    pub fn sync(&mut self) {
        self.visible = self.ring.cq_visible();
    }

    /// Completions visible to this handle.
    pub fn len(&self) -> usize {
        self.visible as usize
    }

    /// Whether no completion is visible.
    pub fn is_empty(&self) -> bool {
        self.visible == 0
    }

    /// Whether the visible completions fill the queue.
    pub fn is_full(&self) -> bool {
        self.len() >= self.capacity()
    }

    /// The number of entries the queue holds.
    pub fn capacity(&self) -> usize {
        self.ring.cq_entries as usize
    }

    /// Completions the kernel dropped for lack of room. Zero on a kernel with
    /// `IORING_FEAT_NODROP` (5.5), which holds them back instead, to be flushed
    /// by the next enter; an older one drops them, and nothing posts them.
    pub fn overflow(&self) -> u32 {
        self.ring.dropped()
    }

    /// Whether `IORING_CQ_EVENTFD_DISABLED` is set: the kernel then posts
    /// completions without signalling the registered eventfd. Loading the
    /// flag word is a Shuttle scheduling point.
    pub fn eventfd_disabled(&self) -> bool {
        self.ring.eventfd_disabled()
    }

    /// Set `IORING_CQ_EVENTFD_DISABLED`: the kernel stops signalling the
    /// registered eventfd. A Shuttle scheduling point.
    pub fn disable_eventfd(&self) {
        self.ring.set_eventfd_disabled(true);
    }

    /// Clear `IORING_CQ_EVENTFD_DISABLED`. The kernel loads the flag after
    /// storing each new tail, so a consumer that clears it and then loads the
    /// tail races the kernel exactly as on Linux; both are Shuttle scheduling
    /// points.
    pub fn enable_eventfd(&self) {
        self.ring.set_eventfd_disabled(false);
    }
}

impl<E: EntryMarker> Iterator for CompletionQueue<'_, E> {
    type Item = E;

    fn next(&mut self) -> Option<E> {
        if self.visible == 0 {
            return None;
        }
        let entry = self.ring.take_cqe();
        self.visible -= 1;
        let any: Box<dyn std::any::Any> = Box::new(entry);
        Some(*any.downcast::<E>().expect("Entry is the only simulated CQE shape"))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.len(), Some(self.len()))
    }
}

impl<E: EntryMarker> ExactSizeIterator for CompletionQueue<'_, E> {}

mod private {
    pub trait Sealed {}
    impl Sealed for super::Entry {}
}
