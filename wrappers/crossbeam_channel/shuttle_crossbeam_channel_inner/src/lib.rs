//! This crate contains Shuttle's implementation of the [`crossbeam-channel`] crate, version 0.5.
//! Do not depend on this crate directly. Use the [`shuttle-crossbeam-channel`] crate, which
//! conditionally exposes this implementation with the `shuttle` feature or the original crate
//! without it.
//!
//! [`crossbeam-channel`]: <https://crates.io/crates/crossbeam-channel>
//! [`shuttle-crossbeam-channel`]: <https://crates.io/crates/shuttle-crossbeam-channel>
//!
//! The API is crossbeam-channel 0.5.17's: `unbounded`, `bounded` (including zero capacity),
//! `after`, `at`, `tick`, `never`, `Sender`, `Receiver`, their iterators, `Select`,
//! `SelectedOperation`, and the `select!` and `select_biased!` macros. The error types are
//! crossbeam-channel's own. Every operation has to run inside a Shuttle test, and channels are
//! created there too, except `never()`, which is `const`.
//!
//! # Scheduling points
//!
//! An operation that another task can observe is a scheduling point, as each of crossbeam's
//! synchronizes:
//!
//! - Every send, receive and selection takes one at its start, whether it blocks or not:
//!   `try_send`, `send`, `send_timeout`, `send_deadline`, `try_recv`, `recv`, `recv_timeout`,
//!   `recv_deadline`, the iterators' `next`, `Select`'s `try_select`, `select`, `select_timeout`,
//!   `select_deadline`, `try_ready`, `ready`, `ready_timeout` and `ready_deadline`, and the
//!   `select!` and `select_biased!` macros. An operation that has to wait then blocks, and runs
//!   again when the scheduler picks it after something woke it.
//! - `len`, `is_empty` and `is_full` take one, as they read what other tasks change.
//! - Dropping the last `Sender` or the last `Receiver` of a channel takes one, as it disconnects the
//!   channel. Dropping any other handle, cloning one, `capacity` and `same_channel` take none.
//! - Completing a selected operation (`SelectedOperation::send` and `recv`) takes none: it is part
//!   of the selection. A zero-capacity receive whose sender is a `select!` still working out its
//!   message waits for it, as crossbeam's does.
//! - Creating a channel takes none.
//!
//! # Which task gets a message
//!
//! When a message arrives in an unbounded or bounded channel, every task blocked on receiving from
//! it wakes and tries again, in the order the scheduler runs them, so the scheduler decides which
//! of them gets the message, and a task that has not blocked yet may take it first. Likewise for
//! senders waiting for room in a bounded channel. crossbeam wakes the longest-waiting task, but it
//! does not promise that task the message.
//!
//! A zero-capacity channel hands a message over directly from one task to another, to the
//! operation on the other side that has waited longest, as crossbeam's does. A task never
//! rendezvouses with itself.
//!
//! # Selection
//!
//! [`Select`] and `select!` pick among the ready operations with Shuttle's random number generator
//! (`shuttle::rand`), which records the draw in the schedule, so that a failing schedule replays
//! and `shuttle::check_uncontrolled_nondeterminism` accepts the choice. They draw only when more
//! than one operation is ready. A biased selection (`Select::new_biased`, `select_biased!`) takes
//! the first ready operation and never draws. Like the rest of Shuttle's randomness, the draw is
//! refused by `shuttle::check_dfs`, which runs without random data: there, a selection with more
//! than one ready operation panics with "requested random data from DFS scheduler".
//!
//! # Time
//!
//! A channel's `Instant` is `shuttle-time`'s, which reads a clock that belongs to the running
//! execution, and its `Duration` is `std`'s. Code that passes a deadline takes its `Instant` from
//! `shuttle_time::time`, which `shuttle-crossbeam-channel`'s `shuttle` feature also switches to
//! Shuttle's clock.
//!
//! **A timeout fires only when no task can run.** A send, receive or selection that has to wait
//! for something (`send_timeout`, `recv_deadline`, `select!` with a `default(timeout)` case,
//! `ready_timeout` and the others) blocks like the operation without a timeout. If the execution
//! reaches a point where no task can run, and so would otherwise deadlock, the waits with the
//! earliest deadline time out: the clock moves forward to that deadline, and the waits go on as
//! they would at that time. Timers work the same way: a receive from `after`, `at` or `tick` waits
//! until no task can run, and then the clock moves to the earliest delivery time. So
//!
//! - a timeout never takes the place of work that can still happen: a long timeout does not fire
//!   in a schedule where its sender is merely slow, and code that treats a timeout as an error is
//!   not failed by Shuttle;
//! - timeouts and timers fire in the order of their deadlines, and a program that relies on one to
//!   make progress (a loop that polls a flag with `recv_timeout`, a `tick` that drives a flush) is
//!   not reported as deadlocked;
//! - after a timeout, the clock reads no earlier than its deadline, so a loop such as
//!   `while Instant::now() < deadline` around a timed receive ends.
//!
//! This is when the timers of Shuttle's time models fire (PR #217), and when tokio's paused clock
//! moves. What it does not explore is a timeout that fires while another task could still have
//! made progress, such as a reply that arrives just after its request timed out: that needs a
//! model of how long each step takes, which Shuttle does not have. Two more consequences:
//!
//! - A task that waits for a timeout to fire by spinning, without blocking, keeps a task runnable,
//!   so the timeout never fires and the execution hits its step limit.
//! - Checking a deadline reads the clock without advancing it, so it adds no step to the schedule,
//!   and works under `check_dfs`.
//!
//! # Differences from crossbeam-channel
//!
//! - Under Shuttle, a channel's `Instant` is `shuttle-time`'s, not `std::time::Instant` (see
//!   [Time](#time)).
//! - The state of a channel belongs to the execution it was created in. A channel that outlives it,
//!   for example in a `static` that Shuttle does not reset, starts over in the next execution that
//!   uses it: the messages in it are dropped, and the blocked operations forgotten.
//! - A bounded channel's `len` counts the messages in it, not the places that selected send arms
//!   have reserved and not filled yet. `is_full` counts both, as crossbeam's does.
//! - An unbounded channel drops its messages once its last receiver is dropped, as crossbeam's
//!   does; the other channels drop theirs with the channel.

mod chan;
mod channel;
mod erased;
mod generation;
mod select;
mod select_macro;
mod timer;
mod waiter;

/// Crate internals used by the `select!` macro.
#[doc(hidden)]
pub mod internal {
    pub use crate::select::{SelectHandle, Token, select, select_timeout, try_select};
    pub use crate::waiter::Waiter;
}

pub use crate::channel::{IntoIter, Iter, Receiver, Sender, TryIter, after, at, bounded, never, tick, unbounded};
pub use crate::select::{Select, SelectedOperation};
pub use crossbeam_channel::{
    ReadyTimeoutError, RecvError, RecvTimeoutError, SelectTimeoutError, SendError, SendTimeoutError, TryReadyError,
    TryRecvError, TrySelectError, TrySendError,
};
