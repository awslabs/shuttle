/// Notifies a single task to wake up.
///
/// `Notify` provides a basic mechanism to notify a single task of an event.
/// `Notify` itself does not carry any data. Instead, it is to be used to signal
/// another task to perform an operation.
use futures::channel::oneshot;
use pin_project::{pin_project, pinned_drop};
use shuttle::rand::{rngs::ThreadRng, thread_rng, Rng};
use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::per_execution::PerExecution;
use shuttle_engine::runtime::thread;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tracing::trace;

/// Its waiters are kept once for each execution (see [`PerExecution`]), so that a `Notify` that
/// outlives an execution, as one in a `static` does, starts every execution with none.
#[derive(Debug)]
pub struct Notify {
    state: PerExecution<Mutex<NotifyState>>,
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

/// The state of a waiter, which its `Notified` and its entry in the waiter queue share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
enum WaiterState {
    /// Not yet polled or enabled
    Init = 0,
    /// Polled or enabled, but not notified
    Enabled = 1,
    /// Notified by `notify_one`
    NotifiedOne = 2,
    /// Notified by `notify_waiters`
    NotifiedAll = 3,
}

impl WaiterState {
    fn is_notified(self) -> bool {
        matches!(self, WaiterState::NotifiedOne | WaiterState::NotifiedAll)
    }
}

/// A `WaiterState` in a std atomic, so that it can be read without the lock of the waiter queue
#[derive(Debug)]
struct WaiterFlag(AtomicUsize);

impl WaiterFlag {
    fn new(state: WaiterState) -> Self {
        Self(AtomicUsize::new(state as usize))
    }

    fn decode(value: usize) -> WaiterState {
        match value {
            0 => WaiterState::Init,
            1 => WaiterState::Enabled,
            2 => WaiterState::NotifiedOne,
            3 => WaiterState::NotifiedAll,
            _ => unreachable!("invalid waiter state {value}"),
        }
    }

    fn load(&self) -> WaiterState {
        Self::decode(self.0.load(Ordering::SeqCst))
    }

    fn store(&self, state: WaiterState) {
        self.0.store(state as usize, Ordering::SeqCst);
    }

    fn swap(&self, state: WaiterState) -> WaiterState {
        Self::decode(self.0.swap(state as usize, Ordering::SeqCst))
    }
}

#[derive(Debug)]
struct Waiter {
    flag: Arc<WaiterFlag>,
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
    flag: Arc<WaiterFlag>,
    /// The `Notify` being received on.
    notify: &'a Notify,
    // oneshot that wakes the waiter. It's the `futures` oneshot, without the scheduling points of
    // this crate's `oneshot`.
    #[pin]
    rx: oneshot::Receiver<()>,
    // Whether the last call was a `poll` that returned `Pending`. The next poll then runs after a
    // wake-up, at the start of a step, so it takes no scheduling point.
    poll_pending: bool,
    // Whether the future has returned `Ready` (or `enable` has returned true)
    done: bool,
}

unsafe impl Send for Notified<'_> {}
unsafe impl Sync for Notified<'_> {}

impl Notify {
    /// Create a new `Notify`, initialized without a permit.
    pub fn new() -> Notify {
        Notify {
            state: PerExecution::new(),
        }
    }

    /// Create a new `Notify`, initialized without a permit.
    ///
    /// Equivalent to [`Notify::new`], except that it can be used in static variables.
    pub const fn const_new() -> Notify {
        Notify {
            state: PerExecution::new(),
        }
    }

    /// The waiters of the current execution, which the execution creates when it first uses the
    /// `Notify`.
    fn state(&self) -> &Mutex<NotifyState> {
        self.state.get(|| Mutex::new(NotifyState::new()))
    }

    /// Like `state`, but `None` where the state can't be reached: in an execution's storage while
    /// Shuttle holds its own state borrowed. Only the destructor of a `Notified` gets there, and
    /// there is nothing for it to remove.
    fn try_state(&self) -> Option<&Mutex<NotifyState>> {
        self.state.try_get(|| Mutex::new(NotifyState::new()))
    }

    /// Wait for a notification.
    pub fn notified(&self) -> Notified<'_> {
        // Scheduling point precedes the visible registration: from then on, the
        // waiter receives every `notify_waiters` call. Without it, no notifier
        // could run between a check of a condition and the `notified()` after
        // it, which is where the classic lost wakeup happens. `switch` rather
        // than `yield_now`, which also wakes the current task: a future that
        // never registers its waker would then spin instead of deadlocking.
        thread::switch();
        let (tx, rx) = oneshot::channel();
        let mut state = self.state().lock().unwrap();
        let id = state.next_id;
        state.next_id += 1;
        let flag = Arc::new(WaiterFlag::new(WaiterState::Init));
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
            poll_pending: false,
            done: false,
        }
    }

    /// Notifies a waiting task.
    ///
    /// If a task is currently waiting, that task is notified. Otherwise, a
    /// permit is stored in this `Notify` value and the **next** call to
    /// [`notified().await`] will complete immediately consuming the permit made
    /// available by this call to `notify_one()`.
    pub fn notify_one(&self) {
        // Scheduling point precedes the visible notification. The destructors of the channels built
        // on this one notify, and can run where the current task can't switch (see
        // `thread::switch_if_not_stopping`).
        thread::switch_if_not_stopping();
        self.notify_one_no_scheduling_point();
    }

    /// `notify_one` without its scheduling point. The drop of a `Notified` calls this after its own
    /// scheduling point.
    fn notify_one_no_scheduling_point(&self) {
        // Where the execution's state can't be reached, there is nobody to notify.
        let Some(state) = self.try_state() else {
            return;
        };
        let mut state = state.lock().unwrap();
        // Need to choose a waiter that is Pending
        let mut pending = Vec::with_capacity(state.waiters.len());
        for w in &state.waiters {
            let flag = w.flag.load();
            assert!(matches!(flag, WaiterState::Init | WaiterState::Enabled));
            if flag == WaiterState::Enabled {
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
            // Must set flag before notifying waiter.
            // The send fails if the `Notified` is dropped between the flag store and the send.
            // The flag is then `NotifiedOne`, so the drop passes the notification on.
            waiter.flag.store(WaiterState::NotifiedOne);
            let _ = waiter.tx.send(());
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
        // Scheduling point precedes the visible notification. As in `notify_one`, it is skipped
        // where the current task can't switch, and then there is nobody to notify either.
        thread::switch_if_not_stopping();
        let Some(state) = self.try_state() else {
            return;
        };
        let mut state = state.lock().unwrap();
        // Notify all waiters, including those not yet enabled
        let waiters = std::mem::take(&mut state.waiters);
        trace!("notify_waiters for {:p} notifying waiters {:?}", self, waiters);
        // Leave a permit that `notify_one` stored in place, as tokio does: it
        // is for the next `notified()`, and the waiters notified here don't
        // take it.
        drop(state);
        // Since we have removed all the waiters, we need to clear all the
        // flags first, before waking any of them, so that the drop handler of
        // a woken waiter sees its flag cleared.
        for w in &waiters {
            // Must set flag before notifying waiter
            let flag = w.flag.swap(WaiterState::NotifiedAll);
            assert!(matches!(flag, WaiterState::Init | WaiterState::Enabled));
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
    pub fn enable(mut self: Pin<&mut Self>) -> bool {
        // Scheduling point precedes the read of the waiter's flag. `enable` registers no waker, so
        // it never follows a wake-up.
        thread::switch();
        *self.as_mut().project().poll_pending = false;
        self.poll_inner()
    }

    fn poll_inner(self: Pin<&mut Self>) -> bool {
        let this = self.project();
        let flag = this.flag.load();
        if flag.is_notified() {
            *this.done = true;
            return true;
        }
        if flag == WaiterState::Init {
            // Not yet polled or enabled, so mark it enabled
            this.flag.store(WaiterState::Enabled);
        }
        let mut state = this.notify.state().lock().unwrap();
        if std::mem::replace(&mut state.pending, false) {
            trace!("waiter {} in state {:?} consuming permit", this.id, flag);
            // We just consumed a permit, so mark this future ready and remove
            // the waiter
            let waiter = state.remove_waiter(*this.id);
            drop(state);
            waiter.flag.store(WaiterState::NotifiedOne);
            waiter.tx.send(()).unwrap();
            *this.done = true;
            true
        } else {
            false
        }
    }
}

impl Future for Notified<'_> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        // Scheduling point precedes the read of the waiter's flag, except after a wake-up
        if !*self.as_mut().project().poll_pending {
            thread::switch();
        }
        let poll = if self.as_mut().poll_inner() {
            Poll::Ready(())
        } else {
            let mut this = self.as_mut().project();
            match this.rx.as_mut().poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(_) => {
                    *this.done = true;
                    Poll::Ready(())
                }
            }
        };
        *self.project().poll_pending = poll.is_pending();
        poll
    }
}

#[pinned_drop]
impl PinnedDrop for Notified<'_> {
    fn drop(self: Pin<&mut Self>) {
        trace!("dropping waiter {:?} with flag {:?}", self.id, self.flag.load());
        if self.done {
            return;
        }
        // Scheduling point precedes the read of the waiter's flag
        thread::switch_if_not_stopping();
        match self.flag.load() {
            // Not notified: remove the waiter from the waiter queue.
            WaiterState::Init | WaiterState::Enabled => {
                // Where the execution's state can't be reached, the waiter queue is gone with it.
                if let Some(state) = self.notify.try_state() {
                    let _ = state.lock().unwrap().remove_waiter(self.id);
                }
            }
            // Notified by `notify_one`, but not completed. As in tokio, pass the notification on to
            // another waiter, so that it is not lost. The drop passes the notification on also while
            // the task unwinds a panic, because the panic may be caught. It skips this outside of an
            // execution and after the execution stops (see `ExecutionState::should_stop`).
            WaiterState::NotifiedOne => {
                if !ExecutionState::should_stop() {
                    self.notify.notify_one_no_scheduling_point();
                }
            }
            // Notified by `notify_waiters`, but not completed. As in tokio, the notification is not
            // passed on.
            WaiterState::NotifiedAll => {}
        }
    }
}
