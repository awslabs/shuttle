//! The signatures the simulation mirrors, built against whichever crate
//! `crate::io_uring` is: `shuttle-io-uring` without the `shuttle` feature,
//! which is io-uring 0.7.15 or later, and `shuttle-io-uring-impl`. Each `let`
//! coerces an item to the type io-uring gives it, so a signature that differs
//! fails to build the test that includes this module. Nothing here runs an op.

#![allow(clippy::type_complexity)]

use std::fmt::Debug;
use std::io;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, RawFd};
use std::time::Duration;

use crate::io_uring::squeue::PushError;
use crate::io_uring::types::{Fd, Fixed, FsyncFlags, SubmitArgs, TimeoutFlags, Timespec};
use crate::io_uring::{
    Builder, CompletionQueue, EnterFlags, IoUring, Parameters, Probe, SubmissionQueue, Submitter, cqueue, opcode,
    register, squeue,
};

fn entry_marker<T: squeue::EntryMarker>() {}
fn completion_marker<T: cqueue::EntryMarker>() {}
fn debug<T: Debug>() {}
fn copy<T: Copy>() {}
fn send_sync<T: Send + Sync>() {}
fn as_raw_fd<T: AsRawFd>() {}
fn error<T: std::error::Error>() {}
fn default<T: Default>() {}

/// Coerces every mirrored item to its io-uring signature.
pub fn signatures() {
    ring();
    builder();
    parameters();
    // The ring's lifetime, which the queue and submitter signatures below are
    // generic over. It is in each signature because clippy's
    // `extra_unused_lifetimes` looks only there.
    submitter(PhantomData);
    submission_queue(PhantomData);
    completion_queue(PhantomData);
    opcodes();
    types();
    flags();
    traits();
}

fn ring() {
    let _: fn(u32) -> io::Result<IoUring> = IoUring::new;
    let _: fn() -> Builder = IoUring::<squeue::Entry, cqueue::Entry>::builder;
    let _: for<'a> fn(&'a IoUring) -> Submitter<'a> = IoUring::submitter;
    let _: for<'a> fn(&'a IoUring) -> &'a Parameters = IoUring::params;
    let _: fn(&IoUring) -> io::Result<usize> = IoUring::submit;
    let _: fn(&IoUring, usize) -> io::Result<usize> = IoUring::submit_and_wait;
    let _: for<'a> fn(&'a mut IoUring) -> (Submitter<'a>, SubmissionQueue<'a>, CompletionQueue<'a>) = IoUring::split;
    let _: for<'a> fn(&'a mut IoUring) -> SubmissionQueue<'a> = IoUring::submission;
    let _: for<'a> unsafe fn(&'a IoUring) -> SubmissionQueue<'a> = IoUring::submission_shared;
    let _: for<'a> fn(&'a mut IoUring) -> CompletionQueue<'a> = IoUring::completion;
    let _: for<'a> unsafe fn(&'a IoUring) -> CompletionQueue<'a> = IoUring::completion_shared;
}

fn builder() {
    type Setter = for<'a> fn(&'a mut Builder) -> &'a mut Builder;
    type SetterWith<T> = for<'a> fn(&'a mut Builder, T) -> &'a mut Builder;
    let _: Setter = Builder::dontfork;
    let _: Setter = Builder::setup_iopoll;
    let _: SetterWith<u32> = Builder::setup_sqpoll;
    let _: SetterWith<u32> = Builder::setup_sqpoll_cpu;
    let _: SetterWith<u32> = Builder::setup_cqsize;
    let _: Setter = Builder::setup_clamp;
    let _: SetterWith<RawFd> = Builder::setup_attach_wq;
    let _: Setter = Builder::setup_submit_all;
    let _: Setter = Builder::setup_coop_taskrun;
    let _: Setter = Builder::setup_taskrun_flag;
    let _: Setter = Builder::setup_defer_taskrun;
    let _: Setter = Builder::setup_single_issuer;
    let _: Setter = Builder::setup_no_sqarray;
    let _: fn(&Builder, u32) -> io::Result<IoUring> = Builder::build;
}

fn parameters() {
    type Flag = fn(&Parameters) -> bool;
    let flags: [Flag; 19] = [
        Parameters::is_setup_sqpoll,
        Parameters::is_setup_iopoll,
        Parameters::is_setup_single_issuer,
        Parameters::is_feature_single_mmap,
        Parameters::is_feature_nodrop,
        Parameters::is_feature_submit_stable,
        Parameters::is_feature_rw_cur_pos,
        Parameters::is_feature_cur_personality,
        Parameters::is_feature_fast_poll,
        Parameters::is_feature_poll_32bits,
        Parameters::is_feature_sqpoll_nonfixed,
        Parameters::is_feature_ext_arg,
        Parameters::is_feature_native_workers,
        Parameters::is_feature_resource_tagging,
        Parameters::is_feature_skip_cqe_on_success,
        Parameters::is_feature_linked_file,
        Parameters::is_feature_recvsend_bundle,
        Parameters::is_feature_min_timeout,
        Parameters::is_feature_no_iowait,
    ];
    let _ = flags;
    let _: fn(&Parameters) -> u32 = Parameters::sq_entries;
    let _: fn(&Parameters) -> u32 = Parameters::sq_thread_idle;
    let _: fn(&Parameters) -> u32 = Parameters::cq_entries;
}

fn submitter<'a>(_: PhantomData<&'a ()>) {
    type Call<'a, A, R> = for<'b> fn(&'b Submitter<'a>, A) -> io::Result<R>;
    type Args = SubmitArgs<'static, 'static>;
    let _: for<'b, 'c> unsafe fn(&'b Submitter<'a>, u32, u32, u32, Option<&'c Args>) -> io::Result<usize> =
        Submitter::enter::<Args>;
    let _: for<'b> fn(&'b Submitter<'a>) -> io::Result<usize> = Submitter::submit;
    let _: Call<'a, usize, usize> = Submitter::submit_and_wait;
    let _: for<'b, 'c> fn(&'b Submitter<'a>, usize, &'c Args) -> io::Result<usize> = Submitter::submit_with_args;
    let _: for<'b> fn(&'b Submitter<'a>) -> io::Result<usize> = Submitter::squeue_wait;
    let _: for<'b, 'c> fn(&'b Submitter<'a>, &'c [RawFd]) -> io::Result<()> = Submitter::register_files;
    let _: Call<'a, u32, ()> = Submitter::register_files_sparse;
    let _: for<'b, 'c> fn(&'b Submitter<'a>, u32, &'c [RawFd]) -> io::Result<usize> = Submitter::register_files_update;
    let _: for<'b> fn(&'b Submitter<'a>) -> io::Result<()> = Submitter::unregister_files;
    let _: Call<'a, RawFd, ()> = Submitter::register_eventfd;
    let _: Call<'a, RawFd, ()> = Submitter::register_eventfd_async;
    let _: for<'b> fn(&'b Submitter<'a>) -> io::Result<()> = Submitter::unregister_eventfd;
    let _: for<'b, 'c> fn(&'b Submitter<'a>, &'c mut Probe) -> io::Result<()> = Submitter::register_probe;
}

fn submission_queue<'a>(_: PhantomData<&'a ()>) {
    type Query<'a, R> = for<'b> fn(&'b SubmissionQueue<'a>) -> R;
    let _: for<'b> fn(&'b mut SubmissionQueue<'a>) = SubmissionQueue::sync;
    let _: Query<'a, bool> = SubmissionQueue::need_wakeup;
    let _: Query<'a, bool> = SubmissionQueue::need_wakeup_after_intermittent_seqcst;
    let _: Query<'a, u32> = SubmissionQueue::dropped;
    let _: Query<'a, bool> = SubmissionQueue::cq_overflow;
    let _: Query<'a, bool> = SubmissionQueue::taskrun;
    let _: Query<'a, usize> = SubmissionQueue::capacity;
    let _: Query<'a, usize> = SubmissionQueue::len;
    let _: Query<'a, bool> = SubmissionQueue::is_empty;
    let _: Query<'a, bool> = SubmissionQueue::is_full;
    let _: for<'b, 'c> unsafe fn(&'b mut SubmissionQueue<'a>, &'c squeue::Entry) -> Result<(), PushError> =
        SubmissionQueue::push;
    let _: for<'b, 'c> unsafe fn(&'b mut SubmissionQueue<'a>, &'c [squeue::Entry]) -> Result<(), PushError> =
        SubmissionQueue::push_multiple;

    let _: fn(squeue::Entry, squeue::Flags) -> squeue::Entry = squeue::Entry::flags;
    let _: fn(squeue::Entry) -> squeue::Entry = squeue::Entry::clear_flags;
    let _: fn(squeue::Entry, u64) -> squeue::Entry = squeue::Entry::user_data;
    let _: fn(&mut squeue::Entry, u64) = squeue::Entry::set_user_data;
    let _: fn(&squeue::Entry) -> u64 = squeue::Entry::get_user_data;
    let _: fn(&squeue::Entry) -> u32 = squeue::Entry::get_opcode;
    let _: fn(squeue::Entry, u16) -> squeue::Entry = squeue::Entry::personality;
}

fn completion_queue<'a>(_: PhantomData<&'a ()>) {
    type Query<'a, R> = for<'b> fn(&'b CompletionQueue<'a>) -> R;
    let _: for<'b> fn(&'b mut CompletionQueue<'a>) = CompletionQueue::sync;
    let _: Query<'a, u32> = CompletionQueue::overflow;
    let _: Query<'a, bool> = CompletionQueue::eventfd_disabled;
    let _: for<'b> fn(&'b CompletionQueue<'a>) = CompletionQueue::disable_eventfd;
    let _: for<'b> fn(&'b CompletionQueue<'a>) = CompletionQueue::enable_eventfd;
    let _: Query<'a, usize> = CompletionQueue::capacity;
    let _: Query<'a, bool> = CompletionQueue::is_empty;
    let _: Query<'a, bool> = CompletionQueue::is_full;
    let _: Query<'a, usize> = <CompletionQueue<'a> as ExactSizeIterator>::len;
    let _: for<'b, 'c> fn(
        &'b mut CompletionQueue<'a>,
        &'c mut [MaybeUninit<cqueue::Entry>],
    ) -> &'c mut [cqueue::Entry] = CompletionQueue::fill;
    let _: for<'b> fn(&'b mut CompletionQueue<'a>) -> Option<cqueue::Entry> = <CompletionQueue<'a> as Iterator>::next;

    let _: fn(&cqueue::Entry) -> i32 = cqueue::Entry::result;
    let _: fn(&cqueue::Entry) -> u64 = cqueue::Entry::user_data;
    let _: fn(&mut cqueue::Entry, u64) = cqueue::Entry::set_user_data;
    let _: fn(&cqueue::Entry) -> u32 = cqueue::Entry::flags;
    let _: fn(u32) -> bool = cqueue::buffer_more;
    let _: fn(u32) -> Option<u16> = cqueue::buffer_select;
    let _: fn(u32) -> bool = cqueue::more;
    let _: fn(u32) -> bool = cqueue::sock_nonempty;
    let _: fn(u32) -> bool = cqueue::notif;
}

fn opcodes() {
    let codes: [u8; 9] = [
        opcode::Nop::CODE,
        opcode::Read::CODE,
        opcode::Readv::CODE,
        opcode::Write::CODE,
        opcode::Writev::CODE,
        opcode::Fsync::CODE,
        opcode::Fallocate::CODE,
        opcode::Timeout::CODE,
        opcode::AsyncCancel::CODE,
    ];
    assert_eq!(codes, [0, 22, 1, 23, 2, 3, 17, 11, 14]);

    let _: fn() -> opcode::Nop = opcode::Nop::new;
    let _: fn(opcode::Nop) -> squeue::Entry = opcode::Nop::build;

    let _ = |fd: Fd, buf: *mut u8, len: u32| -> opcode::Read { opcode::Read::new(fd, buf, len) };
    let _ = |fd: Fixed, buf: *mut u8, len: u32| -> opcode::Read { opcode::Read::new(fd, buf, len) };
    let _: fn(opcode::Read, u64) -> opcode::Read = opcode::Read::offset;
    let _: fn(opcode::Read, u16) -> opcode::Read = opcode::Read::ioprio;
    let _: fn(opcode::Read, i32) -> opcode::Read = opcode::Read::rw_flags;
    let _: fn(opcode::Read, u16) -> opcode::Read = opcode::Read::buf_group;
    let _: fn(opcode::Read) -> squeue::Entry = opcode::Read::build;

    let _ = |fd: Fd, buf: *const u8, len: u32| -> opcode::Write { opcode::Write::new(fd, buf, len) };
    let _ = |fd: Fixed, buf: *const u8, len: u32| -> opcode::Write { opcode::Write::new(fd, buf, len) };
    let _: fn(opcode::Write, u64) -> opcode::Write = opcode::Write::offset;
    let _: fn(opcode::Write, u16) -> opcode::Write = opcode::Write::ioprio;
    let _: fn(opcode::Write, i32) -> opcode::Write = opcode::Write::rw_flags;
    let _: fn(opcode::Write, u8) -> opcode::Write = opcode::Write::write_stream;
    let _: fn(opcode::Write) -> squeue::Entry = opcode::Write::build;

    let _ = |fd: Fd, iovec: *const libc::iovec, len: u32| -> opcode::Readv { opcode::Readv::new(fd, iovec, len) };
    let _ = |fd: Fixed, iovec: *const libc::iovec, len: u32| -> opcode::Readv { opcode::Readv::new(fd, iovec, len) };
    let _: fn(opcode::Readv, u64) -> opcode::Readv = opcode::Readv::offset;
    let _: fn(opcode::Readv, u16) -> opcode::Readv = opcode::Readv::ioprio;
    let _: fn(opcode::Readv, i32) -> opcode::Readv = opcode::Readv::rw_flags;
    let _: fn(opcode::Readv, u16) -> opcode::Readv = opcode::Readv::buf_group;
    let _: fn(opcode::Readv) -> squeue::Entry = opcode::Readv::build;

    let _ = |fd: Fd, iovec: *const libc::iovec, len: u32| -> opcode::Writev { opcode::Writev::new(fd, iovec, len) };
    let _ = |fd: Fixed, iovec: *const libc::iovec, len: u32| -> opcode::Writev { opcode::Writev::new(fd, iovec, len) };
    let _: fn(opcode::Writev, u64) -> opcode::Writev = opcode::Writev::offset;
    let _: fn(opcode::Writev, u16) -> opcode::Writev = opcode::Writev::ioprio;
    let _: fn(opcode::Writev, i32) -> opcode::Writev = opcode::Writev::rw_flags;
    let _: fn(opcode::Writev, u8) -> opcode::Writev = opcode::Writev::write_stream;
    let _: fn(opcode::Writev) -> squeue::Entry = opcode::Writev::build;

    let _ = |fd: Fd| -> opcode::Fsync { opcode::Fsync::new(fd) };
    let _ = |fd: Fixed| -> opcode::Fsync { opcode::Fsync::new(fd) };
    let _: fn(opcode::Fsync, u64) -> opcode::Fsync = opcode::Fsync::offset;
    let _: fn(opcode::Fsync, u32) -> opcode::Fsync = opcode::Fsync::len;
    let _: fn(opcode::Fsync, FsyncFlags) -> opcode::Fsync = opcode::Fsync::flags;
    let _: fn(opcode::Fsync) -> squeue::Entry = opcode::Fsync::build;

    let _ = |fd: Fd, len: u64| -> opcode::Fallocate { opcode::Fallocate::new(fd, len) };
    let _ = |fd: Fixed, len: u64| -> opcode::Fallocate { opcode::Fallocate::new(fd, len) };
    let _: fn(opcode::Fallocate, u64) -> opcode::Fallocate = opcode::Fallocate::offset;
    let _: fn(opcode::Fallocate, i32) -> opcode::Fallocate = opcode::Fallocate::mode;
    let _: fn(opcode::Fallocate) -> squeue::Entry = opcode::Fallocate::build;

    let _: fn(*const Timespec) -> opcode::Timeout = opcode::Timeout::new;
    let _: fn(opcode::Timeout, u32) -> opcode::Timeout = opcode::Timeout::count;
    let _: fn(opcode::Timeout, TimeoutFlags) -> opcode::Timeout = opcode::Timeout::flags;
    let _: fn(opcode::Timeout) -> squeue::Entry = opcode::Timeout::build;

    let _: fn(u64) -> opcode::AsyncCancel = opcode::AsyncCancel::new;
    let _: fn(opcode::AsyncCancel) -> squeue::Entry = opcode::AsyncCancel::build;
}

fn types() {
    let _ = Fd(0);
    let _ = Fixed(0);
    #[allow(deprecated)]
    let _: crate::io_uring::types::RwFlags = 0_u32;
    let _: fn() -> Timespec = Timespec::new;
    let _: fn(Timespec, u64) -> Timespec = Timespec::sec;
    let _: fn(Timespec, u32) -> Timespec = Timespec::nsec;
    let _: fn(Duration) -> Timespec = Timespec::from;
    let _: fn() -> SubmitArgs<'static, 'static> = SubmitArgs::new;
    let _ = |args: SubmitArgs<'static, 'static>, mask: &'static libc::sigset_t| -> SubmitArgs<'static, 'static> {
        args.sigmask(mask)
    };
    let _ =
        |args: SubmitArgs<'static, 'static>, usec: u32| -> SubmitArgs<'static, 'static> { args.min_wait_usec(usec) };
    let _ = |args: SubmitArgs<'static, 'static>, ts: &'static Timespec| -> SubmitArgs<'static, 'static> {
        args.timespec(ts)
    };
    let _: fn() -> Probe = Probe::new;
    let _: fn(&Probe, u8) -> bool = Probe::is_supported;
    let _: fn() -> register::Probe = Probe::default;
}

fn flags() {
    let _ = [
        squeue::Flags::FIXED_FILE,
        squeue::Flags::IO_DRAIN,
        squeue::Flags::IO_LINK,
        squeue::Flags::IO_HARDLINK,
        squeue::Flags::ASYNC,
        squeue::Flags::BUFFER_SELECT,
        squeue::Flags::SKIP_SUCCESS,
    ];
    let _ = [
        EnterFlags::GETEVENTS,
        EnterFlags::SQ_WAKEUP,
        EnterFlags::SQ_WAIT,
        EnterFlags::EXT_ARG,
        EnterFlags::REGISTERED_RING,
        EnterFlags::ABS_TIMER,
        EnterFlags::EXT_ARG_REG,
        EnterFlags::NO_IOWAIT,
    ];
    let _ = [
        TimeoutFlags::ABS,
        TimeoutFlags::BOOTTIME,
        TimeoutFlags::REALTIME,
        TimeoutFlags::LINK_TIMEOUT_UPDATE,
        TimeoutFlags::ETIME_SUCCESS,
        TimeoutFlags::MULTISHOT,
    ];
    let _ = FsyncFlags::DATASYNC;
    let bits = [
        squeue::Flags::IO_LINK.bits() == 1 << 2,
        EnterFlags::EXT_ARG.bits() == 1 << 3,
        EnterFlags::NO_IOWAIT.bits() == 1 << 7,
        TimeoutFlags::ETIME_SUCCESS.bits() == 1 << 5,
        FsyncFlags::DATASYNC.bits() == 1,
    ];
    assert_eq!(bits, [true; 5], "the flag values are the kernel's");
}

fn traits() {
    entry_marker::<squeue::Entry>();
    completion_marker::<cqueue::Entry>();
    send_sync::<IoUring>();
    as_raw_fd::<IoUring>();
    debug::<Parameters>();
    debug::<squeue::Entry>();
    debug::<cqueue::Entry>();
    debug::<Probe>();
    debug::<Fd>();
    debug::<Timespec>();
    debug::<SubmitArgs<'static, 'static>>();
    copy::<Fd>();
    copy::<Fixed>();
    copy::<Timespec>();
    copy::<SubmitArgs<'static, 'static>>();
    copy::<squeue::Flags>();
    copy::<EnterFlags>();
    default::<Timespec>();
    default::<SubmitArgs<'static, 'static>>();
    default::<Probe>();
    error::<PushError>();
    let _: fn(&Builder) -> Builder = Builder::clone;
    let _: fn(&Parameters) -> Parameters = Parameters::clone;
    let _: fn(&squeue::Entry) -> squeue::Entry = squeue::Entry::clone;
    let _: fn(&cqueue::Entry) -> cqueue::Entry = cqueue::Entry::clone;
    let _: fn(&squeue::Flags, &squeue::Flags) -> bool = PartialEq::eq;
    let _: fn(&EnterFlags, &EnterFlags) -> std::cmp::Ordering = Ord::cmp;
    let _: fn(&TimeoutFlags, &TimeoutFlags) -> std::cmp::Ordering = Ord::cmp;
}
