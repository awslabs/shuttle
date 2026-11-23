//! Data race detection for the memory of a checked `UnsafeCell`.
//!
//! This follows Miri's detector for non-atomic accesses (`MemoryCellClocks` in
//! `src/concurrency/data_race.rs`), which is FastTrack: each location keeps its last write and the
//! last read of each task, and an access races with one of them unless that one happens before
//! it. Happens-before comes from Shuttle's vector clocks. With `Config::weak_memory`, the atomics
//! release and acquire clocks as their orderings say, so a non-atomic access that a too-weak
//! ordering leaves unordered is a race here.

use crate::runtime::task::clock::VectorClock;
use crate::runtime::task::TaskId;
use smallvec::SmallVec;
use std::panic::Location;

/// What an access to the memory of a checked cell did, for the race report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RaceAccess {
    Read,
    Write,
    /// The read that creating a reference makes.
    RetagRead,
}

impl RaceAccess {
    fn describe(self) -> &'static str {
        match self {
            RaceAccess::Read => "read",
            RaceAccess::Write => "write",
            RaceAccess::RetagRead => "reference creation (a read)",
        }
    }

    fn is_write(self) -> bool {
        self == RaceAccess::Write
    }
}

/// One access, at the time its task made it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AccessRecord {
    pub task: TaskId,
    /// The task's own entry in its clock at the access. Every access ticks that entry first, so an
    /// access made after a release has a later time than the one released.
    pub time: u32,
    pub kind: RaceAccess,
    pub at: &'static Location<'static>,
}

impl AccessRecord {
    #[cfg(not(feature = "bench-no-vector-clocks"))]
    fn happens_before(&self, clock: &VectorClock) -> bool {
        (**clock).get(usize::from(self.task)).is_some_and(|&t| t >= self.time)
    }

    /// Without vector clocks, every access counts as ordered, so no race is reported.
    #[cfg(feature = "bench-no-vector-clocks")]
    fn happens_before(&self, _clock: &VectorClock) -> bool {
        true
    }
}

/// The accesses of one location that later accesses must happen after.
#[derive(Clone, Debug, Default)]
pub(crate) struct RaceClocks {
    write: Option<AccessRecord>,
    /// The last read of each task since the last write.
    reads: SmallVec<[AccessRecord; 2]>,
}

/// Two accesses to the same location that happens-before doesn't order.
#[derive(Debug)]
pub(crate) struct Race {
    pub earlier: AccessRecord,
    pub later: AccessRecord,
}

impl Race {
    pub(crate) fn describe(&self) -> String {
        format!(
            "a {} in task {} at {} and an earlier {} in task {} at {}",
            self.later.kind.describe(),
            self.later.task,
            self.later.at,
            self.earlier.kind.describe(),
            self.earlier.task,
            self.earlier.at,
        )
    }
}

impl RaceClocks {
    /// Checks an access against the earlier ones and records it. `clock` is the accessing task's
    /// clock.
    pub(crate) fn access(&mut self, access: AccessRecord, clock: &VectorClock) -> Result<(), Race> {
        if let Some(write) = self.write {
            if write.task != access.task && !write.happens_before(clock) {
                return Err(Race {
                    earlier: write,
                    later: access,
                });
            }
        }
        if access.kind.is_write() {
            for read in &self.reads {
                if read.task != access.task && !read.happens_before(clock) {
                    return Err(Race {
                        earlier: *read,
                        later: access,
                    });
                }
            }
        }
        self.record(access);
        Ok(())
    }

    /// Records an access without checking it, so that later accesses are checked against it.
    pub(crate) fn record(&mut self, access: AccessRecord) {
        if access.kind.is_write() {
            // Every later access has to happen after this write, and so after the reads before it.
            self.reads.clear();
            self.write = Some(access);
        } else if let Some(read) = self.reads.iter_mut().find(|read| read.task == access.task) {
            *read = access;
        } else {
            self.reads.push(access);
        }
    }
}
