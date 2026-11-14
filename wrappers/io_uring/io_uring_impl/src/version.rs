//! Which kernel the simulation behaves as.
//!
//! io_uring grew over many releases, and a system may still run an old one. A
//! consumer that probes for features and falls back without them can run its
//! fallback under the simulation by choosing the kernel for an execution, as it
//! sets [`crate::faults`]: on the calling thread, before it creates the rings
//! the choice applies to. A ring keeps the kernel it was created under.
//!
//! What an older kernel lacks, the simulation lacks too, as the kernel shows
//! it:
//!
//! - Opcodes newer than the kernel complete with `-EINVAL`.
//! - Setup flags newer than the kernel fail [`Builder::build`] with `EINVAL`,
//!   and registrations newer than it fail with `EINVAL`.
//! - [`Parameters`] reports only the features the kernel has.
//! - Without `IORING_FEAT_EXT_ARG` (5.11), a timed enter fails with `EINVAL`.
//! - Without `IORING_FEAT_NODROP` (5.5), a completion that finds the CQ full is
//!   dropped and counted in [`CompletionQueue::overflow`], rather than held
//!   back; nothing ever posts it.
//! - Without `IORING_FEAT_SUBMIT_STABLE` (5.5), the kernel reads a vectored
//!   op's `iovec` array when it runs the op, not when it is submitted.
//! - Without `IORING_FEAT_RW_CUR_POS` (5.6), an op at offset −1 completes with
//!   `-EINVAL` rather than use the file position.
//! - Without `IORING_CQ_EVENTFD_DISABLED` (5.8), the flag cannot be set: the
//!   kernel signals the registered eventfd for every completion.
//!
//! [`Builder::build`]: crate::Builder::build
//! [`Parameters`]: crate::Parameters
//! [`CompletionQueue::overflow`]: crate::cqueue::CompletionQueue::overflow

use std::cell::Cell;

/// A kernel release, as `(major, minor)`.
pub type Release = (u32, u32);

/// Which kernel the simulation behaves as. The default is a current one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Kernel {
    /// A current kernel: every feature the simulation models.
    #[default]
    Current,
    /// Linux 5.4, which some systems still run: the ring, vectored reads and
    /// writes, `fsync` and timeouts, and little else.
    Linux5_4,
}

impl Kernel {
    /// The release, or `None` for [`Kernel::Current`], which has every
    /// release's features.
    pub fn release(self) -> Option<Release> {
        match self {
            Kernel::Current => None,
            Kernel::Linux5_4 => Some((5, 4)),
        }
    }

    /// Whether the kernel has what `release` added.
    pub(crate) fn has(self, release: Release) -> bool {
        self.release().is_none_or(|own| own >= release)
    }
}

thread_local! {
    static KERNEL: Cell<Kernel> = const { Cell::new(Kernel::Current) };
}

/// Create this thread's rings as `kernel` from now on.
pub fn set_kernel(kernel: Kernel) {
    KERNEL.with(|current| current.set(kernel));
}

/// The kernel this thread's new rings behave as.
pub fn kernel() -> Kernel {
    KERNEL.with(Cell::get)
}

/// What each feature the simulation models arrived in.
pub(crate) mod since {
    use super::Release;

    pub(crate) const NODROP: Release = (5, 5);
    pub(crate) const SUBMIT_STABLE: Release = (5, 5);
    pub(crate) const RW_CUR_POS: Release = (5, 6);
    pub(crate) const CUR_PERSONALITY: Release = (5, 6);
    pub(crate) const FAST_POLL: Release = (5, 7);
    pub(crate) const CQ_EVENTFD_DISABLED: Release = (5, 8);
    pub(crate) const POLL_32BITS: Release = (5, 9);
    pub(crate) const SQPOLL_NONFIXED: Release = (5, 11);
    pub(crate) const EXT_ARG: Release = (5, 11);
    pub(crate) const NATIVE_WORKERS: Release = (5, 12);

    pub(crate) const SETUP_CQSIZE: Release = (5, 5);
    pub(crate) const SETUP_CLAMP: Release = (5, 6);
    pub(crate) const SETUP_SUBMIT_ALL: Release = (5, 18);
    pub(crate) const SETUP_COOP_TASKRUN: Release = (5, 19);
    pub(crate) const SETUP_TASKRUN_FLAG: Release = (5, 19);
    pub(crate) const SETUP_SINGLE_ISSUER: Release = (6, 0);
    pub(crate) const SETUP_DEFER_TASKRUN: Release = (6, 1);

    pub(crate) const REGISTER_FILES_UPDATE: Release = (5, 5);
    pub(crate) const REGISTER_EVENTFD_ASYNC: Release = (5, 6);
    pub(crate) const REGISTER_PROBE: Release = (5, 6);
    pub(crate) const REGISTER_FILES_SPARSE: Release = (5, 19);
}
