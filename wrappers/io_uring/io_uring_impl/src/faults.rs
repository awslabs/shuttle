//! Faults the simulated kernel injects: refusals a real kernel can post,
//! chosen with `shuttle::rand` so a schedule replays them.
//!
//! The settings belong to the running Shuttle execution, as its rings do:
//! they start at the defaults in every execution, and apply to the
//! execution's rings from the call that sets them on. A test therefore sets
//! them inside the test function, before it submits the ops they are for.
//! Like the rest of Shuttle, the functions here panic outside a Shuttle test.

use shuttle::rand::{Rng, thread_rng};

use crate::host::Host;

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

/// One in how many choices between I/O and a timer goes to the timer.
pub(crate) fn hang_odds(host: &Host) -> u32 {
    host.lock().faults.hang_odds.max(1)
}

/// Inject `faults` into the ops of this execution's rings from now on.
pub fn set_faults(faults: Faults) {
    Host::current().lock().faults = faults;
}

/// The faults this execution's rings inject.
pub fn faults() -> Faults {
    Host::current().lock().faults
}

/// Whether to refuse a read or write that may be refused.
pub(crate) fn refuse(host: &Host) -> bool {
    let p = host.lock().faults.eagain;
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

/// How many errors of each kind the kernel posted in this execution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Posted {
    pub eio: u64,
    pub short_reads: u64,
    pub enospc: u64,
    /// Waits interrupted, whether or not the enter reported it.
    pub eintr: u64,
}

/// Post `errors` for the ops of this execution's rings from now on.
pub fn set_errors(errors: Errors) {
    Host::current().lock().errors = errors;
}

/// The errors this execution's rings post.
pub fn errors() -> Errors {
    Host::current().lock().errors
}

/// The errors this execution's rings have posted, and reset the counts.
pub fn take_posted() -> Posted {
    std::mem::take(&mut Host::current().lock().posted)
}

/// Draw with probability `p`, drawing nothing at 0 so a run without errors
/// schedules as it would without this.
fn draw(p: f64) -> bool {
    p > 0.0 && thread_rng().gen_bool(p.min(1.0))
}

fn note(host: &Host, count: impl FnOnce(&mut Posted)) {
    count(&mut host.lock().posted);
}

/// Whether to fail a read with `-EIO`.
pub(crate) fn fail_read(host: &Host) -> bool {
    let p = host.lock().errors.eio;
    let fail = draw(p);
    if fail {
        note(host, |posted| posted.eio += 1);
    }
    fail
}

/// Whether to return a read short.
pub(crate) fn short_read(host: &Host) -> bool {
    let p = host.lock().errors.short_read;
    draw(p)
}

/// Count a short read posted.
pub(crate) fn note_short_read(host: &Host) {
    note(host, |posted| posted.short_reads += 1);
}

/// Whether to fail a `fallocate` with `-ENOSPC`.
pub(crate) fn no_space(host: &Host) -> bool {
    let p = host.lock().errors.enospc;
    let fail = draw(p);
    if fail {
        note(host, |posted| posted.enospc += 1);
    }
    fail
}

/// Whether to interrupt a wait that would block.
pub(crate) fn interrupt(host: &Host) -> bool {
    let p = host.lock().errors.eintr;
    let interrupt = draw(p);
    if interrupt {
        note(host, |posted| posted.eintr += 1);
    }
    interrupt
}
