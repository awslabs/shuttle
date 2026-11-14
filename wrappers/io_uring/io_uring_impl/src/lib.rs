// This file is derived from turmoil-io-uring 0.1.0 (crates/turmoil-io-uring in
// https://github.com/tokio-rs/turmoil, commit
// 684acc1a8eea3a9cf2c6959dc47b69dba981cac1), Copyright (c) 2022 Carl Lerche,
// and licensed under the MIT license: see LICENSE-MIT in this crate. The
// changes to it are licensed under the Apache License, Version 2.0, as the rest
// of Shuttle is.

//! A simulated `io_uring` kernel for [Shuttle](https://crates.io/crates/shuttle),
//! mirroring the [`io-uring` 0.7](https://docs.rs/io-uring/0.7) crate's API.
//!
//! Use it through the `shuttle-io-uring` wrapper, which re-exports this crate
//! under its `shuttle` feature and `io-uring` otherwise, so code built on the
//! ring runs unchanged under Shuttle.
//!
//! # Why
//!
//! `io-uring` is the kernel interface: it maps the kernel's queues and makes
//! the `io_uring_*` system calls, and the kernel completes ops on its own
//! schedule, which Shuttle can neither see nor control. Waiting for those
//! completions also needs tokio's I/O driver, which a Shuttle execution does
//! not have. A ring driver therefore cannot run under Shuttle at all, and its
//! hardest code (completions racing submissions, wakeups and cancellation)
//! is exactly what Shuttle is for.
//!
//! # The model
//!
//! Each ring has a kernel task. While ops are in flight it is runnable, and
//! each time Shuttle schedules it, it completes one of them, chosen by
//! Shuttle, by running the op's system call on the real file descriptor. So
//! ops stay in flight for as long as Shuttle keeps the kernel task off the
//! CPU, complete in any order, and a failing schedule replays.
//!
//! The semantics follow the kernel, not a simplification of it:
//!
//! - A read writes, and a write reads, the caller's buffer when the op
//!   completes. A buffer the submitter touches while the kernel owns it, or
//!   shares between two ops when either reads it, panics at the op that did
//!   it; see [`buffers`].
//! - A vectored op's `iovec` array is copied when the op is submitted
//!   (`IORING_FEAT_SUBMIT_STABLE`), so only the buffers it lists must outlive
//!   the submit.
//! - Per-op `RWF_*` flags reach the system call (`preadv2`/`pwritev2`), and
//!   offset −1 uses and advances the file position
//!   (`IORING_FEAT_RW_CUR_POS`).
//! - SQ entries reach the kernel only when the queue handle is synced or
//!   dropped, and a completion queue handle sees only what its last tail load
//!   exposed.
//! - A completion posts in the kernel's order: store the CQ tail, wake
//!   blocked enters, then load the CQ flags and, unless
//!   `IORING_CQ_EVENTFD_DISABLED` is set, add one to the registered eventfd's
//!   counter and signal it. The tail and flag words are
//!   Shuttle atomics, so every interleaving of that protocol is explored.
//! - An op may complete inline, during the enter that submits it.
//! - I/O completes before timers: a timer fires while I/O is in flight only
//!   one time in 256 by default ([`faults::Faults::hang_odds`]), which models
//!   a device that outlasts the timeout.
//! - `submit_and_wait` and timed enters block the calling task. A bounded
//!   wait is one more timer among the ops in flight: Shuttle expires it at the
//!   rate it fires a timeout SQE, and at once when nothing in flight could
//!   complete it. A blocked wait may be interrupted with `EINTR` when a test
//!   asks ([`faults::Errors::eintr`]).
//! - An enter reports the entries it submitted, if it submitted any, whatever
//!   became of its wait, and a wait cut short by its timeout or a signal
//!   returns success if any completion is visible, as `io_uring_enter` does.
//! - A cancel of an in-flight op either cancels it or reports `-EALREADY`
//!   and lets it complete; Shuttle chooses.
//! - Fixed files resolve when an op is submitted, as the kernel takes its
//!   file reference then.
//! - Completions that find the CQ full are held back and flushed by the next
//!   enter (`IORING_FEAT_NODROP`); an enter with work to submit meanwhile
//!   fails with `EBUSY`, as the kernel's does.
//! - Dropping a ring with ops in flight panics, as the kernel would keep
//!   using their buffers after the ring is gone, unless a timer has fired
//!   with I/O outstanding: an owner that saw the device hang may abandon the
//!   ring and leak its buffers.
//!
//! [`AsyncFd`] mirrors tokio's for the fds the simulated kernel signals: a
//! ring's own fd and eventfds registered with one, plain or async.
//!
//! A test can run its rings as Linux 5.4 instead of a current kernel, to
//! drive a consumer's fallbacks for what an old kernel lacks; see
//! [`version`].
//!
//! Everything the simulation keeps belongs to the running Shuttle execution:
//! the settings of [`faults`], [`buffers`] and [`version`], the ops in flight,
//! and the fds an [`AsyncFd`] can wait on. Every execution starts from the
//! defaults, whatever an earlier one did, so a failing execution replays on
//! its own as it ran in the test.
//!
//! # Not modelled
//!
//! - `IORING_SETUP_SQPOLL` and `IORING_SETUP_IOPOLL` (rejected at build).
//! - Linked and drained SQEs, provided buffers, and `CQE_SKIP_SUCCESS`
//!   (`-EINVAL` completions).
//! - Opcodes other than those in [`opcode`].
//! - Deferred task work: with `COOP_TASKRUN` or `DEFER_TASKRUN` the kernel
//!   posts some completions only when the submitter next enters. Here every
//!   completion posts when it happens, and `taskrun()` is always `false`.
//! - Faults and short transfers beyond what the real file returns.
//! - Kernels other than a current one and Linux 5.4.
//! - Writes an application makes to an eventfd: only the simulated kernel's
//!   signals wake an [`AsyncFd`].
//!
//! # Provenance
//!
//! Forked from `turmoil-io-uring` 0.1.0 (MIT, Copyright (c) 2022 Carl Lerche;
//! see `LICENSE-MIT`), which simulates the ring on turmoil's clock and
//! filesystem. This crate keeps its API mirroring and replaces the clock with
//! Shuttle's scheduler, the simulated filesystem with real file descriptors,
//! and its simplified semantics with the kernel's. Each source file says in its
//! header whether it is derived from that crate or new here. What is new, and
//! the changes to what is derived, are licensed under the Apache License,
//! Version 2.0, so the crate is `Apache-2.0 AND MIT`.

// io_uring, and the system calls the simulated kernel makes, are Linux's. On
// other targets the crate is empty, so the workspace still builds there.
#![cfg(target_os = "linux")]

mod async_fd;
pub mod buffers;
pub mod cqueue;
pub mod faults;
mod host;
mod kernel;
pub mod opcode;
pub mod register;
pub mod squeue;
mod submit;
pub mod types;
pub mod version;

use std::io;
use std::marker::PhantomData;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::sync::Arc;

pub use async_fd::{AsyncFd, AsyncFdReadyGuard, Interest, TryIoError};
pub use cqueue::CompletionQueue;
pub use register::Probe;
pub use squeue::SubmissionQueue;
pub use submit::{EnterFlags, Submitter};

use host::Host;
use kernel::Ring;
use version::{Kernel, Release, since};

/// A simulated ring, mirroring the real crate's `IoUring`.
pub struct IoUring<S = squeue::Entry, C = cqueue::Entry>
where
    S: squeue::EntryMarker,
    C: cqueue::EntryMarker,
{
    ring: Arc<Ring>,
    params: Parameters,
    _markers: PhantomData<(S, C)>,
}

/// Ring parameters, mirroring the real crate's `Parameters`. The simulated
/// kernel reports the features of a current Linux.
#[derive(Clone, Debug)]
pub struct Parameters {
    sq_entries: u32,
    cq_entries: u32,
    single_issuer: bool,
    kernel: Kernel,
}

impl Parameters {
    pub fn sq_entries(&self) -> u32 {
        self.sq_entries
    }

    pub fn cq_entries(&self) -> u32 {
        self.cq_entries
    }

    /// Always zero: there is no SQ polling thread.
    pub fn sq_thread_idle(&self) -> u32 {
        0
    }

    pub fn is_setup_sqpoll(&self) -> bool {
        false
    }

    pub fn is_setup_iopoll(&self) -> bool {
        false
    }

    pub fn is_setup_single_issuer(&self) -> bool {
        self.single_issuer
    }

    pub fn is_feature_single_mmap(&self) -> bool {
        true
    }

    /// Completions that find the CQ full are held back, not dropped.
    pub fn is_feature_nodrop(&self) -> bool {
        self.kernel.has(since::NODROP)
    }

    pub fn is_feature_submit_stable(&self) -> bool {
        self.kernel.has(since::SUBMIT_STABLE)
    }

    pub fn is_feature_rw_cur_pos(&self) -> bool {
        self.kernel.has(since::RW_CUR_POS)
    }

    pub fn is_feature_cur_personality(&self) -> bool {
        self.kernel.has(since::CUR_PERSONALITY)
    }

    pub fn is_feature_fast_poll(&self) -> bool {
        self.kernel.has(since::FAST_POLL)
    }

    /// The CQ ring has its flags word (`IORING_CQ_EVENTFD_DISABLED`).
    pub fn is_feature_poll_32bits(&self) -> bool {
        self.kernel.has(since::POLL_32BITS)
    }

    pub fn is_feature_sqpoll_nonfixed(&self) -> bool {
        self.kernel.has(since::SQPOLL_NONFIXED)
    }

    /// Enters take [`SubmitArgs`](types::SubmitArgs) timeouts.
    pub fn is_feature_ext_arg(&self) -> bool {
        self.kernel.has(since::EXT_ARG)
    }

    pub fn is_feature_native_workers(&self) -> bool {
        self.kernel.has(since::NATIVE_WORKERS)
    }

    /// `false`: registered resources, and so their tags, are not simulated.
    pub fn is_feature_resource_tagging(&self) -> bool {
        false
    }

    /// `false`: `IOSQE_CQE_SKIP_SUCCESS` completes with `-EINVAL`.
    pub fn is_feature_skip_cqe_on_success(&self) -> bool {
        false
    }

    /// `false`: linked SQEs complete with `-EINVAL`.
    pub fn is_feature_linked_file(&self) -> bool {
        false
    }

    /// `false`: the simulation has no socket opcodes.
    pub fn is_feature_recvsend_bundle(&self) -> bool {
        false
    }

    /// `false`: a minimum wait needs a clock, which Shuttle does not have.
    pub fn is_feature_min_timeout(&self) -> bool {
        false
    }

    /// `false`: `IORING_ENTER_NO_IOWAIT` is not simulated.
    pub fn is_feature_no_iowait(&self) -> bool {
        false
    }
}

/// Ring setup options, mirroring the real crate's `Builder`.
///
/// `SQPOLL` and `IOPOLL` fail the build with `EINVAL`; the task-run flags are
/// accepted (see the crate docs on deferred task work), and so are the options
/// that make no difference to the simulation.
#[derive(Clone)]
pub struct Builder<S = squeue::Entry, C = cqueue::Entry>
where
    S: squeue::EntryMarker,
    C: cqueue::EntryMarker,
{
    sqpoll: bool,
    iopoll: bool,
    single_issuer: bool,
    cq_entries: Option<u32>,
    /// The newest release among the setup flags asked for: an older kernel
    /// rejects the setup with `EINVAL`.
    needs: Option<Release>,
    _markers: PhantomData<(S, C)>,
}

impl<S: squeue::EntryMarker, C: cqueue::EntryMarker> Builder<S, C> {
    /// Accepted for source compatibility: the simulation maps no memory for a
    /// child process to inherit.
    pub fn dontfork(&mut self) -> &mut Self {
        self
    }

    pub fn setup_sqpoll(&mut self, _idle: u32) -> &mut Self {
        self.sqpoll = true;
        self
    }

    /// Fails the build with `EINVAL`: the CPU is the SQ polling thread's,
    /// which needs `SQPOLL`.
    pub fn setup_sqpoll_cpu(&mut self, _cpu: u32) -> &mut Self {
        self.sqpoll = true;
        self
    }

    pub fn setup_iopoll(&mut self) -> &mut Self {
        self.iopoll = true;
        self
    }

    pub fn setup_submit_all(&mut self) -> &mut Self {
        self.need(since::SETUP_SUBMIT_ALL);
        self
    }

    pub fn setup_coop_taskrun(&mut self) -> &mut Self {
        self.need(since::SETUP_COOP_TASKRUN);
        self
    }

    pub fn setup_taskrun_flag(&mut self) -> &mut Self {
        self.need(since::SETUP_TASKRUN_FLAG);
        self
    }

    pub fn setup_defer_taskrun(&mut self) -> &mut Self {
        self.need(since::SETUP_DEFER_TASKRUN);
        self
    }

    pub fn setup_single_issuer(&mut self) -> &mut Self {
        self.need(since::SETUP_SINGLE_ISSUER);
        self.single_issuer = true;
        self
    }

    pub fn setup_clamp(&mut self) -> &mut Self {
        self.need(since::SETUP_CLAMP);
        self
    }

    pub fn setup_cqsize(&mut self, entries: u32) -> &mut Self {
        self.need(since::SETUP_CQSIZE);
        self.cq_entries = Some(entries);
        self
    }

    /// Accepted: the simulation has no worker pool to share.
    pub fn setup_attach_wq(&mut self, _fd: RawFd) -> &mut Self {
        self.need(since::SETUP_ATTACH_WQ);
        self
    }

    /// Accepted: the simulation has no SQ array.
    pub fn setup_no_sqarray(&mut self) -> &mut Self {
        self.need(since::SETUP_NO_SQARRAY);
        self
    }

    /// Record that the setup needs what `release` added.
    fn need(&mut self, release: Release) {
        self.needs = Some(self.needs.map_or(release, |needs| needs.max(release)));
    }

    /// Set the ring up, as the kernel this execution's rings behave as (see
    /// [`version`]): one older than a setup flag asked for fails with `EINVAL`.
    /// Panics outside a Shuttle test.
    pub fn build(&self, entries: u32) -> io::Result<IoUring<S, C>> {
        let host = Host::current();
        let kernel = host.lock().kernel;
        if self.sqpoll || self.iopoll || entries == 0 || self.needs.is_some_and(|needs| !kernel.has(needs)) {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        let sq_entries = entries.next_power_of_two();
        let cq_entries = self.cq_entries.map_or(sq_entries * 2, u32::next_power_of_two);
        Ok(IoUring {
            ring: Ring::start(host, sq_entries, cq_entries, kernel),
            params: Parameters {
                sq_entries,
                cq_entries,
                single_issuer: self.single_issuer,
                kernel,
            },
            _markers: PhantomData,
        })
    }
}

impl IoUring<squeue::Entry, cqueue::Entry> {
    /// A ring with `entries` SQ entries and twice as many CQ entries.
    pub fn new(entries: u32) -> io::Result<Self> {
        Self::builder().build(entries)
    }
}

impl<S: squeue::EntryMarker, C: cqueue::EntryMarker> IoUring<S, C> {
    pub fn builder() -> Builder<S, C> {
        Builder {
            sqpoll: false,
            iopoll: false,
            single_issuer: false,
            cq_entries: None,
            needs: None,
            _markers: PhantomData,
        }
    }

    pub fn params(&self) -> &Parameters {
        &self.params
    }

    pub fn submitter(&self) -> Submitter<'_> {
        Submitter::new(&self.ring)
    }

    pub fn submit(&self) -> io::Result<usize> {
        self.submitter().submit()
    }

    pub fn submit_and_wait(&self, want: usize) -> io::Result<usize> {
        self.submitter().submit_and_wait(want)
    }

    pub fn submission(&mut self) -> squeue::SubmissionQueue<'_, S> {
        squeue::SubmissionQueue::new(&self.ring)
    }

    /// # Safety
    ///
    /// As in the real crate: the caller must be the queue's only writer.
    pub unsafe fn submission_shared(&self) -> squeue::SubmissionQueue<'_, S> {
        squeue::SubmissionQueue::new(&self.ring)
    }

    pub fn completion(&mut self) -> cqueue::CompletionQueue<'_, C> {
        cqueue::CompletionQueue::new(&self.ring)
    }

    /// # Safety
    ///
    /// As in the real crate: the caller must be the queue's only reader.
    pub unsafe fn completion_shared(&self) -> cqueue::CompletionQueue<'_, C> {
        cqueue::CompletionQueue::new(&self.ring)
    }

    pub fn split(
        &mut self,
    ) -> (
        Submitter<'_>,
        squeue::SubmissionQueue<'_, S>,
        cqueue::CompletionQueue<'_, C>,
    ) {
        (
            Submitter::new(&self.ring),
            squeue::SubmissionQueue::new(&self.ring),
            cqueue::CompletionQueue::new(&self.ring),
        )
    }
}

impl<S: squeue::EntryMarker, C: cqueue::EntryMarker> Drop for IoUring<S, C> {
    fn drop(&mut self) {
        self.ring.close();
    }
}

impl<S: squeue::EntryMarker, C: cqueue::EntryMarker> AsRawFd for IoUring<S, C> {
    /// The simulated ring fd, which an [`AsyncFd`] can wait on.
    fn as_raw_fd(&self) -> RawFd {
        self.ring.fd
    }
}

impl<S: squeue::EntryMarker, C: cqueue::EntryMarker> AsFd for IoUring<S, C> {
    /// The simulated ring fd. It names no open file: use it only to wait on
    /// it with an [`AsyncFd`], never in a system call.
    fn as_fd(&self) -> BorrowedFd<'_> {
        // SAFETY: the fd is the simulated ring's number, valid for as long
        // as the ring; nothing reads it as a kernel descriptor.
        unsafe { BorrowedFd::borrow_raw(self.ring.fd) }
    }
}
