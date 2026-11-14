//! Type fragments mirroring [`io_uring::types`](https://docs.rs/io-uring/0.7/io_uring/types).

use std::marker::PhantomData;
use std::os::fd::RawFd;
use std::time::Duration;

use bitflags::bitflags;

/// The sealed traits opcode builders take their file argument through, as in
/// the real crate: an op names either a raw fd or a registered-file index.
#[doc(hidden)]
pub mod sealed {
    use super::{Fd, Fixed};
    use std::os::fd::RawFd;

    /// The file an SQE names.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Target {
        /// A raw file descriptor.
        Fd(RawFd),
        /// An index into the ring's registered-file table.
        Fixed(u32),
    }

    /// A file argument that must be a raw fd.
    pub trait UseFd: Sized {
        /// The raw fd.
        fn into(self) -> RawFd;
    }

    /// A file argument that may be a raw fd or a registered-file index.
    pub trait UseFixed: Sized {
        /// The target.
        fn into(self) -> Target;
    }

    impl UseFd for Fd {
        fn into(self) -> RawFd {
            self.0
        }
    }

    impl UseFixed for Fd {
        fn into(self) -> Target {
            Target::Fd(self.0)
        }
    }

    impl UseFixed for Fixed {
        fn into(self) -> Target {
            Target::Fixed(self.0)
        }
    }
}

/// A file descriptor, mirroring [`io_uring::types::Fd`](https://docs.rs/io-uring/0.7/io_uring/types/struct.Fd.html).
#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct Fd(pub RawFd);

/// An index into the ring's registered-file table, mirroring
/// [`io_uring::types::Fixed`](https://docs.rs/io-uring/0.7/io_uring/types/struct.Fixed.html). Registered with
/// [`Submitter::register_files`](crate::Submitter::register_files) and
/// friends.
#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct Fixed(pub u32);

bitflags! {
    /// Options for [`Timeout`](crate::opcode::Timeout), mirroring
    /// `IORING_TIMEOUT_*`. The simulation accepts them and ignores the
    /// clock-selection ones: a timeout fires whenever Shuttle schedules it.
    #[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct TimeoutFlags: u32 {
        const ABS = 1;
        const BOOTTIME = 1 << 2;
        const REALTIME = 1 << 3;
        const LINK_TIMEOUT_UPDATE = 1 << 4;
        const ETIME_SUCCESS = 1 << 5;
        const MULTISHOT = 1 << 6;
    }
}

bitflags! {
    /// Options for [`Fsync`](crate::opcode::Fsync), mirroring
    /// `IORING_FSYNC_*`.
    #[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct FsyncFlags: u32 {
        const DATASYNC = 1;
    }
}

/// A timeout, mirroring [`io_uring::types::Timespec`](https://docs.rs/io-uring/0.7/io_uring/types/struct.Timespec.html).
///
/// Shuttle has no clock to measure it against, so the simulation records it
/// only to know a wait is bounded: a bounded wait may time out at any point
/// Shuttle chooses.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timespec {
    pub(crate) sec: u64,
    pub(crate) nsec: u32,
}

impl Timespec {
    /// A zero timeout.
    pub const fn new() -> Self {
        Self { sec: 0, nsec: 0 }
    }

    /// Set the seconds.
    pub const fn sec(mut self, sec: u64) -> Self {
        self.sec = sec;
        self
    }

    /// Set the nanoseconds.
    pub const fn nsec(mut self, nsec: u32) -> Self {
        self.nsec = nsec;
        self
    }
}

impl From<Duration> for Timespec {
    fn from(value: Duration) -> Self {
        Self {
            sec: value.as_secs(),
            nsec: value.subsec_nanos(),
        }
    }
}

/// Arguments for [`Submitter::submit_with_args`](crate::Submitter::submit_with_args),
/// mirroring [`io_uring::types::SubmitArgs`](https://docs.rs/io-uring/0.7/io_uring/types/struct.SubmitArgs.html): the real crate's two lifetimes,
/// a timeout, and a signal mask the simulation ignores.
pub struct SubmitArgs<'prev: 'now, 'now> {
    pub(crate) timeout: Option<Timespec>,
    _prev: PhantomData<&'prev ()>,
    _now: PhantomData<&'now ()>,
}

impl Default for SubmitArgs<'static, 'static> {
    fn default() -> Self {
        Self::new()
    }
}

impl SubmitArgs<'static, 'static> {
    /// No timeout and no signal mask.
    pub const fn new() -> SubmitArgs<'static, 'static> {
        SubmitArgs {
            timeout: None,
            _prev: PhantomData,
            _now: PhantomData,
        }
    }
}

impl<'prev, 'now> SubmitArgs<'prev, 'now> {
    /// Accepted for source compatibility; Shuttle delivers no signals.
    pub fn sigmask<'new>(self, _sigmask: &'new libc::sigset_t) -> SubmitArgs<'now, 'new> {
        SubmitArgs {
            timeout: self.timeout,
            _prev: PhantomData,
            _now: PhantomData,
        }
    }

    /// Bound the wait by `timespec`.
    pub fn timespec<'new>(self, timespec: &'new Timespec) -> SubmitArgs<'now, 'new> {
        SubmitArgs {
            timeout: Some(*timespec),
            _prev: PhantomData,
            _now: PhantomData,
        }
    }
}
