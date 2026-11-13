//! This crate provides a Shuttle-compatible wrapper for [`std::thread`], [`std::thread_local!`] and
//! [`std::hint`], in order to make it more ergonomic to run a codebase under Shuttle.
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "shuttle-thread/shuttle",
//! ]
//!
//! [dependencies]
//! shuttle-thread = "VERSION_NUMBER"
//! ```
//!
//! Then write `shuttle_thread::thread` where you would write [`std::thread`],
//! `shuttle_thread::hint` where you would write [`std::hint`], and
//! [`shuttle_thread::thread_local!`](thread_local) where you would write [`std::thread_local!`].
//! The rest of the codebase then remains unchanged, and running with a Shuttle-compatible `thread`
//! can be done via the "shuttle" feature flag.
//!
//! # What changes under the feature
//!
//! A thread becomes a Shuttle task: the scheduler decides when it runs, and the whole execution
//! runs on one OS thread, so Shuttle can explore the interleavings of the threads instead of
//! leaving them to the OS. In particular:
//!
//! * [`thread::spawn`] and [`thread::Builder::spawn`] create a task that the current execution
//!   schedules, rather than an OS thread that runs outside Shuttle's control. Shuttle's primitives
//!   panic on a thread it does not know, so a test that spawns a real thread cannot use them there.
//! * [`thread::sleep`] does not sleep. Shuttle does not model time, so it takes a single scheduling
//!   point: other tasks may run, and the call returns however little time has passed. A test must
//!   not use it to wait for something to happen.
//! * [`thread::park`], [`thread::park_timeout`] and `Thread::unpark` go through Shuttle, so a parked
//!   task blocks and the unpark wakes it. As time is not modelled, `park_timeout` is `park`: the
//!   timeout never fires, and an execution in which every task is parked is a deadlock.
//! * [`thread::yield_now`] and [`hint::spin_loop`] take a scheduling point, so a spin loop lets the
//!   task it waits for run. On an OS thread neither is needed for that, since the other thread has
//!   its own core; under Shuttle a loop that takes neither never ends.
//! * [`thread::current`] reports the running task's id and name, so threads stay distinguishable,
//!   and the ids are the same in every execution of the same schedule.
//! * [`thread::available_parallelism`] reports what the test configured (4 by default) rather than
//!   the host's processor count, which would make the test's behavior depend on the machine.
//! * [`thread_local!`] gives each task of an execution its own value, and drops every value when
//!   the execution ends. [`std::thread_local!`] would instead give one value to the OS thread the
//!   execution runs on, so all the tasks would share it and it would survive into the next
//!   execution.
//!
//! Without the feature, every item is `std`'s own, and the macro expands to
//! [`std::thread_local!`].

/// Shuttle's [`std::thread`] under the `shuttle` feature, and `std`'s own without it.
pub mod thread {
    cfg_if::cfg_if! {
        if #[cfg(feature = "shuttle")] {
            pub use shuttle_thread_impl::thread::*;
        } else {
            pub use std::thread::*;
        }
    }
}

/// Shuttle's [`std::hint`] under the `shuttle` feature, and `std`'s own without it.
///
/// Only `spin_loop` differs: under Shuttle it takes a scheduling point, so that
/// a busy-wait lets the task it is waiting for run. The rest of `std::hint`, such as `black_box`,
/// is `std`'s in both cases.
pub mod hint {
    cfg_if::cfg_if! {
        if #[cfg(feature = "shuttle")] {
            pub use shuttle_thread_impl::hint::*;
        } else {
            pub use std::hint::*;
        }
    }
}

#[cfg(feature = "shuttle")]
pub use shuttle_thread_impl::thread_local;

/// Declare a new thread local storage key, of type [`thread::LocalKey`].
///
/// Without the `shuttle` feature this is [`std::thread_local!`]. With it, the key is Shuttle's: each
/// task of an execution gets its own value, and every value is dropped when the execution ends.
#[cfg(not(feature = "shuttle"))]
#[macro_export]
macro_rules! thread_local {
    ($($tokens:tt)*) => { ::std::thread_local! { $($tokens)* } };
}
