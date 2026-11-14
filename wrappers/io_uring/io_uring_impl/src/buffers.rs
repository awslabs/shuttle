//! What a submitter may not do with a buffer while its op is in flight,
//! checked so a breach panics at the op that caused it.
//!
//! The ring's contract hands an op's buffers to the kernel from the submit
//! that takes them until its completion is reaped: a write's bytes may not
//! change, a read's buffer may be neither read nor written, and two ops in
//! flight may not share memory unless both are writes. A real kernel
//! diagnoses none of that. It reads or writes whatever is there, and the test
//! fails somewhere else, or passes.
//!
//! Every check is on by default except [`Checks::file_ranges`]. A test sets
//! them per execution, as it does [`crate::faults`].
//!
//! The cost is the bytes in flight: each buffer is copied when its op is
//! submitted. A buffer freed while its op is in flight is still undefined
//! behaviour, as it is with the kernel, and beyond what these checks can see.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::os::fd::RawFd;

/// The checks the simulated kernel makes on an op's buffers.
///
/// # Defaults
///
/// - `writes`, `reads`, `overlap`: on
/// - `file_ranges`: off
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checks {
    /// Catch a write whose bytes changed while it was in flight.
    pub writes: bool,
    /// Catch a read whose buffer was read or written while it was in flight,
    /// by poisoning the buffer for as long as the kernel owns it.
    ///
    /// The simulation restores the buffer's own bytes before it runs the read,
    /// so the bytes past the ones read are the buffer's own, as they are with
    /// the kernel. That reads the buffer, so every byte of a read's buffer
    /// must be initialized. Turn this off for a consumer that reads into
    /// uninitialized memory.
    pub reads: bool,
    /// Catch two ops in flight whose buffers share memory, when either is a
    /// read. Two writes may share: the same bytes may go to several files at
    /// once.
    pub overlap: bool,
    /// Catch two ops in flight on one file whose ranges overlap, when either
    /// writes it. Off by default: it is an invariant of the code above the
    /// ring, not of the ring.
    pub file_ranges: bool,
}

impl Default for Checks {
    fn default() -> Self {
        Self {
            writes: true,
            reads: true,
            overlap: true,
            file_ranges: false,
        }
    }
}

impl Checks {
    /// Whether any check needs an op watched.
    fn any(&self) -> bool {
        self.writes || self.reads || self.overlap || self.file_ranges
    }
}

thread_local! {
    static CHECKS: Cell<Checks> = const {
        Cell::new(Checks {
            writes: true,
            reads: true,
            overlap: true,
            file_ranges: false,
        })
    };
}

/// Make `checks` on the ops of this thread's rings from now on.
pub fn set_checks(checks: Checks) {
    CHECKS.with(|current| current.set(checks));
}

/// The checks this thread's rings make.
pub fn checks() -> Checks {
    CHECKS.with(Cell::get)
}

/// Which way an op moves its buffers' bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    /// The kernel writes the buffers: a read.
    Read,
    /// The kernel reads them: a write, or an op that changes a file's bytes
    /// without a buffer.
    Write,
}

impl Access {
    fn op(self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::Write => "write",
        }
    }
}

/// One file's identity, to tell two fds on the same file apart from two
/// files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileId {
    dev: u64,
    ino: u64,
}

/// An op in flight, as the checks see it.
struct Tracked {
    user_data: u64,
    access: Access,
    /// The memory the kernel owns, one range per buffer.
    memory: Vec<Range<usize>>,
    /// The file bytes the op moves, when it moves any.
    file: Option<(FileId, Range<u64>)>,
}

/// Every op in flight on every ring of this execution, since a buffer may be
/// handed to two rings.
#[derive(Default)]
struct Registry {
    next_id: u64,
    tracked: HashMap<u64, Tracked>,
}

thread_local! {
    /// The ops in flight on this thread, which is the execution's: Shuttle
    /// runs an execution's tasks on one thread, as [`crate::faults`] notes.
    ///
    /// A thread local rather than a Shuttle lock or `lazy_static`, because
    /// those are scheduling points, and one inside a submit would let the
    /// kernel task run between two SQEs of the same `io_uring_enter`.
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
}

/// Run `f` on the ops in flight. Never nests: each caller finishes before it
/// runs anything that could watch or release another op.
fn with_registry<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    REGISTRY.with(|registry| f(&mut registry.borrow_mut()))
}

/// The bytes a buffer of `len` at `addr` covers. Empty buffers cover nothing,
/// so they never overlap.
fn range(addr: usize, len: usize) -> Option<Range<usize>> {
    (len > 0).then(|| addr..addr.saturating_add(len))
}

fn overlaps<T: Ord + Copy>(a: &Range<T>, b: &Range<T>) -> bool {
    a.start < b.end && b.start < a.end
}

/// The file `fd` names, or `None` if it cannot be identified.
fn file_id(fd: RawFd) -> Option<FileId> {
    // SAFETY: `stat` is written whole by a successful `fstat`, and `fd` is the
    // submitter's, open for the life of the op.
    unsafe {
        let mut stat: libc::stat = std::mem::zeroed();
        (libc::fstat(fd, &mut stat) == 0).then_some(FileId {
            dev: stat.st_dev as u64,
            ino: stat.st_ino as u64,
        })
    }
}

/// An op's buffers, watched for as long as the kernel owns them. Dropping it
/// gives them back.
pub(crate) struct Watch {
    id: u64,
    user_data: u64,
    fd: RawFd,
    access: Access,
    offset: u64,
    /// Each buffer's address and its bytes as of the submit; empty when the
    /// check for this op's kind is off.
    bufs: Vec<(usize, Vec<u8>)>,
}

/// Watch `bufs` (each an address and a length) for the op `user_data` moves
/// `access`-ward on `fd` at `offset`, covering `file` of the file's bytes.
/// `None` when no check is on.
///
/// Panics if the op's buffers or file range clash with an op already in
/// flight.
pub(crate) fn watch(
    user_data: u64,
    fd: RawFd,
    access: Access,
    offset: u64,
    bufs: &[(usize, usize)],
    file: Option<Range<u64>>,
) -> Option<Watch> {
    let checks = checks();
    if !checks.any() {
        return None;
    }
    let memory: Vec<Range<usize>> = bufs.iter().filter_map(|&(addr, len)| range(addr, len)).collect();
    let file = file
        .filter(|range| checks.file_ranges && range.start < range.end)
        .and_then(|range| file_id(fd).map(|id| (id, range)));

    let id = with_registry(|registry| {
        if checks.overlap {
            check_overlap(registry, user_data, access, &memory);
        }
        if let Some((id, range)) = &file {
            check_file_range(registry, user_data, access, *id, range);
        }
        let id = registry.next_id;
        registry.next_id += 1;
        registry.tracked.insert(
            id,
            Tracked {
                user_data,
                access,
                memory,
                file,
            },
        );
        id
    });

    // Copy each buffer, and poison a read's, only for the checks that need it.
    let copy = match access {
        Access::Read => checks.reads,
        Access::Write => checks.writes,
    };
    let bufs: Vec<(usize, Vec<u8>)> = if copy {
        bufs.iter()
            .filter(|&&(_, len)| len > 0)
            // SAFETY: the submitter's buffer, valid for `len` bytes until the
            // op's completion is reaped, and initialized (see `Checks::reads`).
            .map(|&(addr, len)| {
                (addr, unsafe {
                    std::slice::from_raw_parts(addr as *const u8, len).to_vec()
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    if access == Access::Read {
        for (addr, bytes) in &bufs {
            fill_poison(*addr, bytes.len(), id);
        }
    }

    Some(Watch {
        id,
        user_data,
        fd,
        access,
        offset,
        bufs,
    })
}

fn check_overlap(registry: &Registry, user_data: u64, access: Access, memory: &[Range<usize>]) {
    // Two writes may share memory; anything a read touches may not be shared.
    for (i, range) in memory.iter().enumerate() {
        if access == Access::Read {
            for other in &memory[i + 1..] {
                assert!(
                    !overlaps(range, other),
                    "shuttle-io-uring: read {user_data} reads into overlapping buffers \
                     ({:#x}..{:#x} and {:#x}..{:#x}); the kernel would leave it undefined \
                     which one holds the bytes",
                    range.start,
                    range.end,
                    other.start,
                    other.end,
                );
            }
        }
        for tracked in registry.tracked.values() {
            if access == Access::Write && tracked.access == Access::Write {
                continue;
            }
            for held in &tracked.memory {
                assert!(
                    !overlaps(range, held),
                    "shuttle-io-uring: {} {user_data}'s buffer {:#x}..{:#x} overlaps \
                     {} {}'s {:#x}..{:#x}, still in flight; the kernel owns a buffer until \
                     its completion is reaped",
                    access.op(),
                    range.start,
                    range.end,
                    tracked.access.op(),
                    tracked.user_data,
                    held.start,
                    held.end,
                );
            }
        }
    }
}

fn check_file_range(registry: &Registry, user_data: u64, access: Access, id: FileId, range: &Range<u64>) {
    for tracked in registry.tracked.values() {
        let Some((held_id, held)) = &tracked.file else {
            continue;
        };
        if *held_id != id || (access == Access::Read && tracked.access == Access::Read) {
            continue;
        }
        assert!(
            !overlaps(range, held),
            "shuttle-io-uring: {} {user_data} covers bytes {}..{} of the file, which \
             {} {}, still in flight, also covers ({}..{}); the kernel orders neither, \
             so either may land",
            access.op(),
            range.start,
            range.end,
            tracked.access.op(),
            tracked.user_data,
            held.start,
            held.end,
        );
    }
}

/// The byte poison puts at `index` of the buffers of the op watched as `id`.
/// Varies within a buffer and between ops, so a consumer that copies one part
/// of a buffer over another is caught too.
fn poison_byte(id: u64, index: usize) -> u8 {
    0xa5 ^ (id as u8) ^ (index as u8).rotate_left(3)
}

fn fill_poison(addr: usize, len: usize, id: u64) {
    // SAFETY: the submitter's buffer, as for the copy in `watch`. The kernel
    // may write a read's buffer at any point while it owns it.
    let buf = unsafe { std::slice::from_raw_parts_mut(addr as *mut u8, len) };
    for (index, byte) in buf.iter_mut().enumerate() {
        *byte = poison_byte(id, index);
    }
}

impl Watch {
    /// Check the op's buffers were left alone, and give a read's back, before
    /// the op runs.
    ///
    /// Panics if a write's bytes changed, or a read's buffer was written,
    /// while the kernel owned it.
    pub(crate) fn before_completion(&self) {
        for (addr, bytes) in &self.bufs {
            // SAFETY: the submitter's buffer, as for the copy in `watch`.
            let buf = unsafe { std::slice::from_raw_parts_mut(*addr as *mut u8, bytes.len()) };
            match self.access {
                Access::Write => {
                    if let Some(index) = buf.iter().zip(bytes).position(|(now, then)| now != then) {
                        panic!(
                            "shuttle-io-uring: write {}'s buffer {:#x} changed while it was in \
                             flight: byte {index} was {:#04x} at submit and is {:#04x} now \
                             (fd {}, file offset {}); the kernel reads a write's buffer when the \
                             op completes",
                            self.user_data, addr, bytes[index], buf[index], self.fd, self.offset,
                        );
                    }
                }
                Access::Read => {
                    if let Some(index) = buf
                        .iter()
                        .enumerate()
                        .position(|(index, byte)| *byte != poison_byte(self.id, index))
                    {
                        panic!(
                            "shuttle-io-uring: read {}'s buffer {:#x} was written while it was \
                             in flight: byte {index} holds {:#04x}, not the {:#04x} the \
                             simulation left there (fd {}, file offset {}); the kernel owns a \
                             read's buffer until its completion is reaped",
                            self.user_data,
                            addr,
                            buf[index],
                            poison_byte(self.id, index),
                            self.fd,
                            self.offset,
                        );
                    }
                    // The bytes past the ones read are the buffer's own.
                    buf.copy_from_slice(bytes);
                }
            }
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        with_registry(|registry| registry.tracked.remove(&self.id));
    }
}
