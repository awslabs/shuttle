//! Faults the simulated kernel injects: refusals a real kernel can post,
//! chosen with `shuttle::rand` so a schedule replays them.
//!
//! The faults apply to the rings of the Shuttle execution on the calling
//! thread, since Shuttle runs an execution's tasks on one thread; a test
//! sets them at the start of each execution, before it submits any op.

use std::cell::Cell;

use shuttle::rand::{Rng, thread_rng};

/// One in this many of the kernel's choices between I/O and a timer goes to
/// the timer, by default. A timeout is typically far longer than an I/O (a
/// ring's drain timer may be a minute, a bounded wait 100 ms), so a timer that
/// fires with I/O outstanding models a device that outlasts it, a hang, which
/// should be rare rather than a coin flip.
pub const DEFAULT_HANG_ODDS: u32 = 256;

/// What the kernel injects. The default injects nothing, and lets a timer beat
/// I/O one time in [`DEFAULT_HANG_ODDS`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Faults {
    /// The probability that a read or write submitted without
    /// `IOSQE_ASYNC` is refused with `-EAGAIN`, as the kernel refuses one it
    /// tried nonblocking and declined to reissue. A refused op may have
    /// moved part of its range first: a write some of its blocks to the
    /// file, a read some of its buffer.
    pub eagain: f64,
    /// One in this many of the kernel's choices between I/O in flight and a
    /// timer (a timeout SQE, or a timed enter) goes to the timer: the device
    /// hangs. `1` makes every such choice a hang, to drive a consumer's
    /// timeout paths; `0` counts as `1`.
    pub hang_odds: u32,
}

impl Default for Faults {
    fn default() -> Self {
        Self {
            eagain: 0.0,
            hang_odds: DEFAULT_HANG_ODDS,
        }
    }
}

thread_local! {
    static FAULTS: Cell<Faults> = const {
        Cell::new(Faults {
            eagain: 0.0,
            hang_odds: DEFAULT_HANG_ODDS,
        })
    };
}

/// One in how many choices between I/O and a timer goes to the timer.
pub(crate) fn hang_odds() -> u32 {
    faults().hang_odds.max(1)
}

/// Inject `faults` into the ops of this thread's rings from now on.
pub fn set_faults(faults: Faults) {
    FAULTS.with(|current| current.set(faults));
}

/// The faults this thread's rings inject.
pub fn faults() -> Faults {
    FAULTS.with(Cell::get)
}

/// Whether to refuse a read or write that may be refused.
pub(crate) fn refuse() -> bool {
    let p = faults().eagain;
    p > 0.0 && thread_rng().gen_bool(p.min(1.0))
}

/// Errors the kernel posts for ops it accepted, none by default. Unlike a
/// refusal, which the submitter resubmits, each surfaces to the caller, so a
/// test turns them on around the ops allowed to fail and off for the ones that
/// must not.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Errors {
    /// The probability a read fails with `-EIO`, having moved part of its
    /// range, as a device error mid-transfer would.
    pub eio: f64,
    /// The probability a read returns short: it moves a block-aligned part of
    /// its range, short of all of it, and reports that count.
    pub short_read: f64,
    /// The probability a `fallocate` fails with `-ENOSPC`.
    pub enospc: f64,
    /// The probability an enter that would block for completions is
    /// interrupted with `-EINTR`, as a signal arriving while it waits would.
    /// The kernel reports the entries the enter submitted, if it submitted
    /// any, so an interrupted wait surfaces only from an enter that submitted
    /// nothing.
    pub eintr: f64,
}

/// How many errors of each kind the kernel posted on this thread.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Posted {
    pub eio: u64,
    pub short_reads: u64,
    pub enospc: u64,
    /// Waits interrupted, whether or not the enter reported it.
    pub eintr: u64,
}

thread_local! {
    static ERRORS: Cell<Errors> = const {
        Cell::new(Errors {
            eio: 0.0,
            short_read: 0.0,
            enospc: 0.0,
            eintr: 0.0,
        })
    };
    static POSTED: Cell<Posted> = const {
        Cell::new(Posted {
            eio: 0,
            short_reads: 0,
            enospc: 0,
            eintr: 0,
        })
    };
}

/// Post `errors` for the ops of this thread's rings from now on.
pub fn set_errors(errors: Errors) {
    ERRORS.with(|current| current.set(errors));
}

/// The errors this thread's rings post.
pub fn errors() -> Errors {
    ERRORS.with(Cell::get)
}

/// The errors this thread's rings have posted, and reset the counts.
pub fn take_posted() -> Posted {
    POSTED.with(|posted| posted.replace(Posted::default()))
}

/// Draw with probability `p`, drawing nothing at 0 so a run without errors
/// schedules as it would without this.
fn draw(p: f64) -> bool {
    p > 0.0 && thread_rng().gen_bool(p.min(1.0))
}

fn note(count: impl FnOnce(&mut Posted)) {
    POSTED.with(|posted| {
        let mut current = posted.get();
        count(&mut current);
        posted.set(current);
    });
}

/// Whether to fail a read with `-EIO`.
pub(crate) fn fail_read() -> bool {
    let fail = draw(errors().eio);
    if fail {
        note(|posted| posted.eio += 1);
    }
    fail
}

/// Whether to return a read short.
pub(crate) fn short_read() -> bool {
    draw(errors().short_read)
}

/// Count a short read posted.
pub(crate) fn note_short_read() {
    note(|posted| posted.short_reads += 1);
}

/// Whether to fail a `fallocate` with `-ENOSPC`.
pub(crate) fn no_space() -> bool {
    let fail = draw(errors().enospc);
    if fail {
        note(|posted| posted.enospc += 1);
    }
    fail
}

/// Whether to interrupt a wait that would block.
pub(crate) fn interrupt() -> bool {
    let interrupt = draw(errors().eintr);
    if interrupt {
        note(|posted| posted.eintr += 1);
    }
    interrupt
}
