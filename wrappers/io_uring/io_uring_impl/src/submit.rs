//! Submitting and registering, mirroring the real crate's `Submitter`.

use std::io;
use std::mem;
use std::os::fd::RawFd;

use bitflags::bitflags;

use crate::kernel::Ring;
use crate::register::Probe;
use crate::types::SubmitArgs;
use crate::version::{Release, since};

bitflags! {
    /// `io_uring_enter` flags, mirroring the real crate's `EnterFlags`.
    #[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct EnterFlags: u32 {
        const GETEVENTS = 1;
        const SQ_WAKEUP = 1 << 1;
        const SQ_WAIT = 1 << 2;
        const EXT_ARG = 1 << 3;
        const REGISTERED_RING = 1 << 4;
        const ABS_TIMER = 1 << 5;
    }
}

/// The ring's syscall interface: `io_uring_enter` and
/// `io_uring_register`.
pub struct Submitter<'a> {
    ring: &'a Ring,
}

impl<'a> Submitter<'a> {
    pub(crate) fn new(ring: &'a Ring) -> Self {
        Self { ring }
    }

    /// `EINVAL` unless the ring's kernel has what `release` added.
    fn need(&self, release: Release) -> io::Result<()> {
        if self.ring.kernel.has(release) {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(libc::EINVAL))
        }
    }

    /// `io_uring_enter`.
    ///
    /// With `EXT_ARG`, `arg` must be a [`SubmitArgs`], as the kernel requires
    /// of `io_uring_getevents_arg`; its timeout bounds the wait.
    ///
    /// # Safety
    ///
    /// As in the real crate: `arg` must point at the argument type `flag`
    /// selects.
    pub unsafe fn enter<T: Sized>(
        &self,
        to_submit: u32,
        min_complete: u32,
        flag: u32,
        arg: Option<&T>,
    ) -> io::Result<usize> {
        let flags = EnterFlags::from_bits_retain(flag);
        if flags.contains(EnterFlags::EXT_ARG) {
            self.need(since::EXT_ARG)?;
        }
        let timeout = match arg {
            Some(arg) if flags.contains(EnterFlags::EXT_ARG) => {
                if mem::size_of::<T>() != mem::size_of::<SubmitArgs<'_, '_>>() {
                    return Err(io::Error::from_raw_os_error(libc::EINVAL));
                }
                // SAFETY: with `EXT_ARG` the caller passes a `SubmitArgs`
                // (checked by size, as the kernel checks `argsz`).
                let args = unsafe { &*(arg as *const T).cast::<SubmitArgs<'_, '_>>() };
                args.timeout
            }
            _ => None,
        };
        self.ring
            .enter(to_submit, min_complete, flags.contains(EnterFlags::GETEVENTS), timeout)
    }

    /// Submit every published entry.
    pub fn submit(&self) -> io::Result<usize> {
        self.submit_and_wait(0)
    }

    /// Submit every published entry, then block until `want` completions are
    /// visible.
    pub fn submit_and_wait(&self, want: usize) -> io::Result<usize> {
        self.ring
            .enter(self.ring.sq_published() as u32, want as u32, want > 0, None)
    }

    /// Submit, then block until `want` completions are visible or the wait
    /// times out with `ETIME`; a bounded wait can time out at any point
    /// Shuttle chooses.
    ///
    /// A kernel without `IORING_FEAT_EXT_ARG` (before 5.11) has no timed
    /// enter, and fails it with `EINVAL`.
    pub fn submit_with_args(&self, want: usize, args: &SubmitArgs<'_, '_>) -> io::Result<usize> {
        self.need(since::EXT_ARG)?;
        self.ring
            .enter(self.ring.sq_published() as u32, want as u32, want > 0, args.timeout)
    }

    /// For SQ polling, which the simulation does not have: returns at once.
    pub fn squeue_wait(&self) -> io::Result<usize> {
        Ok(0)
    }

    pub fn register_files(&self, fds: &[RawFd]) -> io::Result<()> {
        self.ring.register_files(fds.to_vec())
    }

    pub fn register_files_sparse(&self, nr: u32) -> io::Result<()> {
        self.need(since::REGISTER_FILES_SPARSE)?;
        self.ring.register_files(vec![-1; nr as usize])
    }

    pub fn register_files_update(&self, offset: u32, fds: &[RawFd]) -> io::Result<usize> {
        self.need(since::REGISTER_FILES_UPDATE)?;
        self.ring.register_files_update(offset, fds)
    }

    pub fn unregister_files(&self) -> io::Result<()> {
        self.ring.unregister_files()
    }

    /// Have the kernel signal `eventfd` as it posts completions, unless
    /// `IORING_CQ_EVENTFD_DISABLED` is set. An `AsyncFd` on `eventfd` then
    /// waits on those signals.
    pub fn register_eventfd(&self, eventfd: RawFd) -> io::Result<()> {
        self.ring.register_eventfd(eventfd, false)
    }

    /// As [`register_eventfd`](Self::register_eventfd), but the kernel signals
    /// `eventfd` only for completions that did not post inline, during the
    /// enter that submitted their op (`IORING_REGISTER_EVENTFD_ASYNC`).
    pub fn register_eventfd_async(&self, eventfd: RawFd) -> io::Result<()> {
        self.need(since::REGISTER_EVENTFD_ASYNC)?;
        self.ring.register_eventfd(eventfd, true)
    }

    pub fn unregister_eventfd(&self) -> io::Result<()> {
        self.ring.unregister_eventfd()
    }

    /// Report the simulated opcodes. A kernel before 5.6 has no probe.
    pub fn register_probe(&self, probe: &mut Probe) -> io::Result<()> {
        self.need(since::REGISTER_PROBE)?;
        probe.fill();
        Ok(())
    }
}
