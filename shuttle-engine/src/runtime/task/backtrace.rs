//! Backtraces of a task's own stack, for the deadlock report.
//!
//! When [`crate::CAPTURE_BACKTRACE`] is set, Shuttle captures a backtrace every time a task parks
//! on a pending future, because that is the only moment its await chain is still on the stack (see
//! [`crate::await_backtrace`]). A test can do that millions of times, so capture has to be cheap,
//! and a DWARF unwind through [`std::backtrace::Backtrace`] costs tens of microseconds.
//!
//! On Apple arm64 we walk the task stack's frame-pointer chain instead, which costs nanoseconds,
//! and symbolize only when the backtrace is printed (see `frame_records`). Everywhere else, and
//! whenever we are not on a task stack whose bounds we know, we fall back to
//! [`std::backtrace::Backtrace::force_capture`].

use corosensei::stack::Stack;
use std::backtrace::Backtrace;
use std::fmt;

cfg_if::cfg_if! {
    if #[cfg(all(target_arch = "aarch64", target_vendor = "apple", target_pointer_width = "64"))] {
        mod frame_records;
        use frame_records as platform;
    } else {
        /// Stand-ins for targets where we cannot walk frame records.
        mod platform {
            use super::StackBounds;

            /// A walk is never taken here, so no value of this type exists.
            pub(super) type Frames = std::convert::Infallible;

            #[inline(always)]
            pub(super) fn capture() -> Option<Frames> {
                None
            }

            #[derive(Debug)]
            pub(crate) struct OnStack;

            impl OnStack {
                #[inline(always)]
                pub(crate) fn enter(_stack: StackBounds) -> Self {
                    Self
                }
            }
        }
    }
}

pub(crate) use platform::OnStack;

/// A backtrace of a task's stack, captured when [`crate::CAPTURE_BACKTRACE`] is set.
///
/// `Display` prints the numbered `N: function` / `at file:line:col` layout that panics use under
/// `RUST_BACKTRACE=1`, and `Debug` prints one `{ fn: .., file: .., line: .. }` record per frame, in
/// the same format as [`std::backtrace::Backtrace`]. A frame-pointer walk ends at the base of the
/// task's stack, so unlike std's backtrace it leaves out the executor that resumed the task.
pub struct TaskBacktrace(Repr);

enum Repr {
    /// Unsymbolized return addresses from a walk of the task stack's frame records.
    Walked(platform::Frames),
    Std(Backtrace),
}

impl TaskBacktrace {
    /// Capture a backtrace of the current stack. Its first frame is the caller of `capture`.
    //
    // Both kinds of backtrace must start at our caller. The frame-pointer walk skips the frame it
    // is called from, so there `capture` must not be inlined, or the walk would skip our caller's
    // frame instead of ours. std's backtrace starts at whatever calls `force_capture`, so elsewhere
    // `capture` must be inlined.
    #[cfg_attr(
        all(target_arch = "aarch64", target_vendor = "apple", target_pointer_width = "64"),
        inline(never)
    )]
    #[cfg_attr(
        not(all(target_arch = "aarch64", target_vendor = "apple", target_pointer_width = "64")),
        inline(always)
    )]
    pub fn capture() -> Self {
        match platform::capture() {
            Some(frames) => Self(Repr::Walked(frames)),
            None => Self(Repr::Std(Backtrace::force_capture())),
        }
    }
}

impl fmt::Display for TaskBacktrace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Repr::Walked(frames) => fmt::Display::fmt(frames, f),
            Repr::Std(backtrace) => fmt::Display::fmt(backtrace, f),
        }
    }
}

impl fmt::Debug for TaskBacktrace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Repr::Walked(frames) => fmt::Debug::fmt(frames, f),
            Repr::Std(backtrace) => fmt::Debug::fmt(backtrace, f),
        }
    }
}

/// The address range of a task's coroutine stack.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StackBounds {
    /// The lowest address of the allocation, guard page included.
    limit: usize,
    /// The highest address. The stack grows down from here.
    base: usize,
}

impl StackBounds {
    pub(crate) fn of(stack: &impl Stack) -> Self {
        Self {
            limit: stack.limit().get(),
            base: stack.base().get(),
        }
    }
}
