//! What a task does when it is next scheduled (see [`Event`]).

use crate::runtime::task::TaskId;
use std::fmt;
use std::panic::Location;

/// The kind of visible operation an [`Event`] describes.
///
/// New kinds may be added in minor releases, so match on this with a wildcard arm.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventKind {
    /// Shuttle doesn't know what the task does next. This is the event of a scheduling point that
    /// doesn't say (a bare [`switch`](crate::runtime::thread::switch)), of a task that hasn't started
    /// yet, and of a task waiting to poll a future again that no Shuttle semaphore has woken (see
    /// [`Task::next_event`](super::Task::next_event)). An unknown operation may conflict with any
    /// other.
    Unknown,
    /// Spawning a new task or thread.
    Spawn,
    /// The task finishing. The target is the finishing task.
    Exit,
    /// Waiting for a task to finish. The target is that task, if there is just one.
    Join,
    /// Aborting a task. The target is the task being aborted.
    Abort,
    /// Yielding to the scheduler. This touches nothing, so a primitive that yields as the
    /// scheduling point before an operation of its own should say so with
    /// [`switch_with`](crate::runtime::thread::switch_with) instead.
    Yield,
    /// Sleeping. Shuttle does not model time, so this is just a scheduling point, and like
    /// [`EventKind::Yield`] touches nothing.
    Sleep,
    /// Parking the current thread. The target is the parking task.
    Park,
    /// Unparking a thread. The target is the unparked task.
    Unpark,
    /// Loading an atomic.
    AtomicLoad,
    /// Storing to an atomic.
    AtomicStore,
    /// A read-modify-write on an atomic: a swap, compare-exchange or fetch-and-modify.
    AtomicReadModifyWrite,
    /// Acquiring permits from a semaphore, or waiting for them. Locks are built on semaphores, so
    /// this is also what locking a mutex or a reader-writer lock looks like.
    SemaphoreAcquire,
    /// Releasing permits to a semaphore, closing it, or giving up on an acquire that is queued.
    SemaphoreRelease,
    /// Waiting at a barrier.
    BarrierWait,
    /// Waiting on a condition variable.
    CondvarWait,
    /// Notifying a condition variable.
    CondvarNotify,
    /// Sending on a channel.
    ChannelSend,
    /// Receiving from a channel.
    ChannelRecv,
}

impl EventKind {
    /// Whether an operation of this kind leaves its target unchanged. Two operations on the same
    /// target commute when both are read-only.
    pub fn is_read_only(self) -> bool {
        matches!(self, EventKind::Join | EventKind::AtomicLoad)
    }
}

/// Identifies a resource, such as a semaphore, lock, atomic or channel, for the length of its life.
///
/// Two live resources never have the same `ResourceId`. A resource that has been dropped or moved
/// may give its id to a later one, but an [`Event`] only describes an operation a task is stopped
/// at, or the acquire a semaphore woke it for, and a resource can be neither dropped nor moved
/// while an operation on it is under way.
///
/// For identifying "the same" resource *across* executions, use [`ResourceId::signature_hash`]
/// instead, which is stable across executions but not unique (see [`crate::ResourceSignature`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ResourceId {
    address: usize,
    signature_hash: u64,
}

impl ResourceId {
    pub(crate) fn new(address: usize, signature_hash: u64) -> Self {
        Self {
            address,
            signature_hash,
        }
    }

    /// The hash of the resource's [`ResourceSignature`](crate::ResourceSignature), which is mostly
    /// stable across executions. It is not unique: resources built with a `const` constructor at
    /// the same source location share it.
    pub fn signature_hash(&self) -> u64 {
        self.signature_hash
    }
}

/// What an [`Event`] acts on.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventTarget {
    /// A synchronization resource.
    Resource(ResourceId),
    /// A task, for operations such as joining, aborting or unparking it.
    Task(TaskId),
}

/// The visible operation a task performs when it is next scheduled.
///
/// Scheduling points come *before* the visible operations they guard (see
/// [`switch`](crate::runtime::thread::switch)), so at every scheduling decision each runnable task
/// is stopped just before some operation. [`Task::next_event`](super::Task::next_event) describes
/// that operation, which lets a scheduler reason about the operations it is choosing between: for
/// example, a partial-order-aware scheduler can tell whether two runnable tasks are about to touch
/// the same resource (see [`Event::conflicts_with`]).
///
/// An `Event` holds only plain data: an [`EventKind`], the [`EventTarget`] the operation acts on,
/// if any, and the source location that performs it. It never borrows the resource.
///
/// An event describes only the first visible operation of a task's next step, and a step can do
/// more before the next scheduling point. For example, `Condvar::wait` releases its mutex at a
/// scheduling point labeled with the mutex, and registers as a waiter on the condvar in the same
/// step.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Event {
    kind: EventKind,
    target: Option<EventTarget>,
    location: Option<&'static Location<'static>>,
}

impl Event {
    /// The event that says nothing about what comes next.
    pub const UNKNOWN: Event = Event {
        kind: EventKind::Unknown,
        target: None,
        location: None,
    };

    /// An event of kind `kind` on `target`, performed at `location`.
    pub fn new(kind: EventKind, target: Option<EventTarget>, location: &'static Location<'static>) -> Self {
        Self {
            kind,
            target,
            location: Some(location),
        }
    }

    /// An event of kind `kind` on the resource with signature `signature`, performed by the caller.
    #[track_caller]
    pub fn on_resource(kind: EventKind, signature: &crate::ResourceSignature) -> Self {
        Self::new(
            kind,
            Some(EventTarget::Resource(signature.resource_id())),
            Location::caller(),
        )
    }

    /// An event of kind `kind` on the task `task`, performed by the caller.
    #[track_caller]
    pub fn on_task(kind: EventKind, task: TaskId) -> Self {
        Self::new(kind, Some(EventTarget::Task(task)), Location::caller())
    }

    /// An event of kind `kind` without a target, performed by the caller.
    #[track_caller]
    pub fn untargeted(kind: EventKind) -> Self {
        Self::new(kind, None, Location::caller())
    }

    /// The kind of operation.
    pub fn kind(&self) -> EventKind {
        self.kind
    }

    /// What the operation acts on, if anything.
    pub fn target(&self) -> Option<EventTarget> {
        self.target
    }

    /// The source location of the operation, if known. For operations performed through Shuttle's
    /// primitives this is usually the call site in the code under test.
    pub fn location(&self) -> Option<&'static Location<'static>> {
        self.location
    }

    /// Whether this operation and `other` may not commute, as far as their events tell: one of them
    /// is [`EventKind::Unknown`], or they act on the same target and at least one of them may change
    /// it. Known operations without a target, such as spawning, yielding and sleeping, conflict with
    /// nothing.
    ///
    /// This only compares the events: see
    /// [`Task::next_step_conflicts_with`](super::Task::next_step_conflicts_with) for comparing two
    /// tasks, and the [`Event`] documentation for what an event leaves out.
    pub fn conflicts_with(&self, other: &Event) -> bool {
        if self.kind == EventKind::Unknown || other.kind == EventKind::Unknown {
            return true;
        }
        self.target.is_some() && self.target == other.target && !(self.kind.is_read_only() && other.kind.is_read_only())
    }
}

impl Default for Event {
    fn default() -> Self {
        Self::UNKNOWN
    }
}

impl fmt::Debug for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.kind)?;
        match self.target {
            Some(EventTarget::Resource(id)) => write!(f, " on resource {:#x}", id.address)?,
            Some(EventTarget::Task(task)) => write!(f, " on {task:?}")?,
            None => {}
        }
        if let Some(location) = self.location {
            write!(f, " at {location}")?;
        }
        Ok(())
    }
}
