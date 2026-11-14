//! The garbage collector: [`Collector`] and [`LocalHandle`].

use crate::guard::{Deferred, Guard};
use crate::per_execution::PerExecution;
use shuttle_engine::runtime::thread::switch;
use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

/// A collector's state in one execution: the functions it was handed, which it never runs (see the
/// [crate documentation](crate)).
#[derive(Default)]
pub(crate) struct Bag {
    deferred: Vec<Deferred>,
}

/// What a collector is, behind the `Arc` its handles and guards share.
// Safety: the model's execution storage lets one task at a time reach the bag, and only
// `Send + Sync` values are reached through this type.
unsafe impl Send for Global {}
unsafe impl Sync for Global {}

pub(crate) struct Global {
    bag: PerExecution<Bag>,
}

impl Global {
    /// Stores `deferred`, to be dropped, without being called, when the execution ends.
    pub(crate) fn defer(&self, deferred: Deferred) {
        self.bag.get(Bag::default).borrow_mut().deferred.push(deferred);
    }
}

/// An epoch-based garbage collector, with the API of [`crossbeam_epoch::Collector`].
///
/// A collector's identity lasts for the whole process, as crossbeam's does, so that
/// [`default_collector`](crate::default_collector) can hand out a `&'static Collector`. Everything it
/// holds belongs to the running execution: the functions it is handed, and each of its handles'
/// pinning. See the [crate documentation](crate) for what the model does with them.
///
/// [`crossbeam_epoch::Collector`]: https://docs.rs/crossbeam-epoch/0.9/crossbeam_epoch/struct.Collector.html
pub struct Collector {
    pub(crate) global: Arc<Global>,
}

// Safety: a collector's state lives in the storage of the execution that reaches it, which one task
// at a time does.
unsafe impl Send for Collector {}
unsafe impl Sync for Collector {}

impl Default for Collector {
    fn default() -> Self {
        Self {
            global: Arc::new(Global {
                bag: PerExecution::new(),
            }),
        }
    }
}

impl Collector {
    /// Creates a new collector.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a new handle for the collector.
    ///
    /// Not a scheduling point: crossbeam's registration takes the global list's lock, but the model
    /// keeps nothing another task can see.
    pub fn register(&self) -> LocalHandle {
        LocalHandle {
            local: Arc::new(Local {
                collector: self.clone(),
                pins: PerExecution::new(),
            }),
            _marker: PhantomData,
        }
    }
}

impl Clone for Collector {
    /// Creates another reference to the same garbage collector.
    fn clone(&self) -> Self {
        Collector {
            global: self.global.clone(),
        }
    }
}

impl fmt::Debug for Collector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Collector { .. }")
    }
}

impl PartialEq for Collector {
    /// Checks if both handles point to the same collector.
    fn eq(&self, rhs: &Collector) -> bool {
        Arc::ptr_eq(&self.global, &rhs.global)
    }
}
impl Eq for Collector {}

/// A participant of a collector: what a [`LocalHandle`] and the [`Guard`]s it makes share.
pub(crate) struct Local {
    collector: Collector,
    /// How many of this participant's guards are alive in this execution.
    pins: PerExecution<usize>,
}

impl Local {
    pub(crate) fn collector(&self) -> &Collector {
        &self.collector
    }

    /// Pins the participant, and returns the guard that unpins it.
    ///
    /// One scheduling point unless the participant is pinned already, where crossbeam only bumps a
    /// counter it alone reads.
    pub(crate) fn pin(self: &Arc<Self>) -> Guard {
        self.pin_without_guard();
        Guard::new(Arc::clone(self))
    }

    /// Counts one more pinning, without making the guard that undoes it. [`Guard::repin_after`] uses
    /// it to restore the pinning it dropped.
    pub(crate) fn pin_without_guard(&self) {
        let was_pinned = {
            let pins = self.pins.get(|| 0);
            let mut pins = pins.borrow_mut();
            let was_pinned = *pins > 0;
            *pins += 1;
            was_pinned
        };
        if !was_pinned {
            // crossbeam's `pin` stores the global epoch into the participant's own, which the task
            // that advances the epoch reads.
            switch();
        }
    }

    /// Whether any of the participant's guards is alive.
    ///
    /// Not a scheduling point: crossbeam reads a counter that the participant alone writes.
    pub(crate) fn is_pinned(&self) -> bool {
        *self.pins.get(|| 0).borrow() > 0
    }

    /// Drops one guard's pinning. One scheduling point if it was the last guard, where crossbeam
    /// stores to the participant's epoch.
    pub(crate) fn unpin(&self) {
        let last = {
            let pins = self.pins.get(|| 0);
            let mut pins = pins.borrow_mut();
            *pins = pins.saturating_sub(1);
            *pins == 0
        };
        if last {
            // A guard can be dropped while the execution is being torn down, where a scheduling
            // point is not allowed.
            shuttle_engine::runtime::thread::switch_if_not_stopping();
        }
    }

    /// Repins the participant, as [`Guard::repin`] does. One scheduling point.
    pub(crate) fn repin(&self) {
        switch();
    }

    pub(crate) fn defer(&self, deferred: Deferred) {
        self.collector.global.defer(deferred);
    }
}

/// A handle to a garbage collector, with the API of [`crossbeam_epoch::LocalHandle`].
///
/// A handle belongs to the task that registered it, as crossbeam's belongs to its thread: it is
/// neither `Send` nor `Sync`. Its pinning belongs to the running execution.
///
/// [`crossbeam_epoch::LocalHandle`]: https://docs.rs/crossbeam-epoch/0.9/crossbeam_epoch/struct.LocalHandle.html
pub struct LocalHandle {
    pub(crate) local: Arc<Local>,
    /// crossbeam's handle holds a raw pointer, so it is neither `Send` nor `Sync`.
    _marker: PhantomData<*mut ()>,
}

impl LocalHandle {
    /// Pins the handle.
    #[inline]
    pub fn pin(&self) -> Guard {
        self.local.pin()
    }

    /// Returns `true` if the handle is pinned.
    #[inline]
    pub fn is_pinned(&self) -> bool {
        self.local.is_pinned()
    }

    /// Returns the `Collector` associated with this handle.
    #[inline]
    pub fn collector(&self) -> &Collector {
        self.local.collector()
    }
}

impl fmt::Debug for LocalHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("LocalHandle { .. }")
    }
}
