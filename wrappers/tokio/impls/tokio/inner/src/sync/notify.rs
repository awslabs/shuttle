/// Notifies a single task to wake up.
///
/// `Notify` provides a basic mechanism to notify a single task of an event.
/// `Notify` itself does not carry any data. Instead, it is to be used to signal
/// another task to perform an operation.
use crate::sync::oneshot;
use pin_project::{pin_project, pinned_drop};
use shuttle::rand::{rngs::ThreadRng, thread_rng, Rng};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tracing::trace;

#[derive(Debug)]
pub struct Notify {
    state: Mutex<NotifyState>,
}

#[derive(Debug)]
struct NotifyState {
    // For assigning unique ids to waiters
    next_id: usize,

    // Seed for selecting waiters to notify at random
    rng: ThreadRng,

    // Whether a notify is pending
    pending: bool,

    // List of waiters
    waiters: VecDeque<Waiter>,
}

impl NotifyState {
    fn new() -> Self {
        Self {
            next_id: 0,
            rng: thread_rng(),
            pending: false,
            waiters: VecDeque::new(),
        }
    }

    fn remove_waiter(&mut self, id: usize) -> Waiter {
        for i in 0..self.waiters.len() {
            if self.waiters[i].id == id {
                return self.waiters.remove(i).unwrap();
            }
        }
        panic!("could not find waiter with id {:?} among {:?}", id, self.waiters);
    }
}

// Encode worker state in usize
const INIT: usize = 0; // not yet polled or enabled
const ENABLED: usize = 1; // enabled but not notified
const NOTIFIED: usize = 2; // notified

#[derive(Debug)]
struct Waiter {
    flag: Arc<AtomicUsize>,
    id: usize,
    tx: oneshot::Sender<()>,
}

/// Future returned from [`Notify::notified()`].
///
/// This future is fused, so once it has completed, any future calls to poll
/// will immediately return `Poll::Ready`.
#[derive(Debug)]
#[pin_project(PinnedDrop)]
pub struct Notified<'a> {
    id: usize, // unique id for this waiter
    flag: Arc<AtomicUsize>,
    /// The `Notify` being received on.
    notify: &'a Notify,
    // oneshot being awaited on
    #[pin]
    rx: oneshot::Receiver<()>,
}

unsafe impl Send for Notified<'_> {}
unsafe impl Sync for Notified<'_> {}

impl Notify {
    /// Create a new `Notify`, initialized without a permit.
    pub fn new() -> Notify {
        Notify {
            state: Mutex::new(NotifyState::new()),
        }
    }

    /// Wait for a notification.
    pub fn notified(&self) -> Notified<'_> {
        let (tx, rx) = oneshot::channel();
        let mut state = self.state.lock().unwrap();
        let id = state.next_id;
        state.next_id += 1;
        let flag = Arc::new(AtomicUsize::new(INIT));
        let waiter = Waiter {
            flag: flag.clone(),
            id,
            tx,
        };
        state.waiters.push_back(waiter);
        trace!(
            "notified {:?} adding waiter {:?} to waiters {:?}",
            self,
            id,
            state.waiters,
        );
        drop(state);
        Notified {
            id,
            flag,
            notify: self,
            rx,
        }
    }

    /// Notifies a waiting task.
    ///
    /// If a task is currently waiting, that task is notified. Otherwise, a
    /// permit is stored in this `Notify` value and the **next** call to
    /// [`notified().await`] will complete immediately consuming the permit made
    /// available by this call to `notify_one()`.
    pub fn notify_one(&self) {
        let mut state = self.state.lock().unwrap();
        // Need to choose a waiter that is Pending
        let mut pending = Vec::with_capacity(state.waiters.len());
        for w in &state.waiters {
            let flag = w.flag.load(Ordering::SeqCst);
            assert!(flag == INIT || flag == ENABLED);
            if flag == ENABLED {
                pending.push(w.id);
            }
        }
        trace!("notify_one for {:p} notifying waiters {:?}", self, pending);
        if pending.is_empty() {
            // No pending waiters, so just record the fact that a notify is pending
            state.pending = true;
        } else {
            // Choose a pending waiter at random
            let index = state.rng.gen_range(0..pending.len());
            let id = pending[index];
            // Remove waiter and mark it notified
            let waiter = state.remove_waiter(id);
            state.pending = false;
            drop(state);
            trace!("notify_one for {:?} waking waiter {:?}", self, waiter.id);
            // Must set flag before notifying waiter
            waiter.flag.store(NOTIFIED, Ordering::SeqCst);
            waiter.tx.send(()).unwrap();
        }
    }

    /// Notifies all waiting tasks.
    ///
    /// If a task is currently waiting, that task is notified. Unlike with
    /// `notify_one()`, no permit is stored to be used by the next call to
    /// `notified().await`. The purpose of this method is to notify all
    /// already registered waiters. Registering for notification is done by
    /// acquiring an instance of the `Notified` future via calling `notified()`.    
    pub fn notify_waiters(&self) {
        let mut state = self.state.lock().unwrap();
        // Notify all waiters, including those not yet enabled
        let waiters = std::mem::take(&mut state.waiters);
        trace!("notify_waiters for {:p} notifying waiters {:?}", self, waiters);
        state.pending = false;
        drop(state);
        // Since we have removed all the waiters, we need to clear all the
        // flags first, before waking any of them.  This is because sending
        // a oneshot to wake a waiter will admit context switches, and we
        // could invoke the drop handler for a waiter whose flag has not been
        // cleared.
        for w in &waiters {
            // Must set flag before notifying waiter
            let flag = w.flag.swap(NOTIFIED, Ordering::SeqCst);
            assert!(flag == INIT || flag == ENABLED);
        }
        for w in waiters {
            let _ = w.tx.send(()); // Note this may fail if the waiter has dropped
        }
    }
}

impl Default for Notify {
    fn default() -> Notify {
        Notify::new()
    }
}

impl Notified<'_> {
    /// Adds this future to the list of futures that are ready to receive
    /// wakeups from calls to [`Notify::notify_one`].
    ///
    /// Polling the future also adds it to the list, so this method should only
    /// be used if you want to add the future to the list before the first call
    /// to `poll`. (In fact, this method is equivalent to calling `poll` except
    /// that no `Waker` is registered.)
    ///
    /// This has no effect on notifications sent using [`Notify::notify_waiters`], which
    /// are received as long as they happen after the creation of the `Notified`
    /// regardless of whether `enable` or `poll` has been called.
    ///
    /// This method returns true if the `Notified` is ready. This happens in the
    /// following situations:
    ///
    ///  1. The `notify_waiters` method was called between the creation of the
    ///     `Notified` and the call to this method.
    ///  2. This is the first call to `enable` or `poll` on this future, and the
    ///     `Notify` was holding a permit from a previous call to `notify_one`.
    ///     The call consumes the permit in that case.
    ///  3. The future has previously been enabled or polled, and it has since
    ///     then been marked ready by either consuming a permit from the
    ///     `Notify`, or by a call to `notify_one` or `notify_waiters` that
    ///     removed it from the list of futures ready to receive wakeups.
    ///
    /// If this method returns true, any future calls to poll on the same future
    /// will immediately return `Poll::Ready`.
    pub fn enable(self: Pin<&mut Self>) -> bool {
        self.poll_inner()
    }

    fn poll_inner(&self) -> bool {
        let flag = self.flag.load(Ordering::SeqCst);
        if flag == NOTIFIED {
            return true;
        }
        if flag == INIT {
            // Not yet polled or enabled, so mark it enabled
            self.flag.store(ENABLED, Ordering::SeqCst);
        }
        let mut state = self.notify.state.lock().unwrap();
        if std::mem::replace(&mut state.pending, false) {
            trace!("waiter {} in state {:?} consuming permit", self.id, flag);
            // We just consumed a permit, so mark this future ready and remove
            // the waiter
            let waiter = state.remove_waiter(self.id);
            drop(state);
            waiter.flag.store(NOTIFIED, Ordering::SeqCst);
            waiter.tx.send(()).unwrap();
            true
        } else {
            false
        }
    }
}

impl Future for Notified<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let enabled = self.poll_inner();
        if enabled {
            Poll::Ready(())
        } else {
            let mut this = self.project();
            match this.rx.as_mut().poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(_) => Poll::Ready(()),
            }
        }
    }
}

#[pinned_drop]
impl PinnedDrop for Notified<'_> {
    fn drop(self: Pin<&mut Self>) {
        trace!(
            "dropping waiter {:?} with flag {:?}",
            self.id,
            self.flag.load(Ordering::SeqCst)
        );
        // We're using std::sync::Atomics here, so no context switching will happen here
        if self.flag.load(Ordering::SeqCst) != NOTIFIED {
            // If the waiter hasn't been notified, remove it from the waiter queue
            let mut state = self.notify.state.lock().unwrap();
            let _ = state.remove_waiter(self.id);
        }
    }
}
