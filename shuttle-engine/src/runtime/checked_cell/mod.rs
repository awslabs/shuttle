//! Per-execution state of the data race checks of Shuttle's checked `UnsafeCell` (`shuttle::cell`).
//!
//! Each checked cell is an allocation here, with the accesses to each range of its bytes. The
//! cell's API in `shuttle-std` reports each access to the cell's memory, and each reference it
//! makes, which reads the memory. It panics with the report of the first violation. The race
//! detection follows Miri's detector for non-atomic accesses (<https://github.com/rust-lang/miri>).

mod race;

use crate::runtime::task::clock::VectorClock;
use crate::runtime::task::TaskId;
use race::{AccessRecord, RaceAccess, RaceClocks};
use std::fmt;
use std::ops::Range;
use std::panic::Location;

/// The kind of an access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessKind {
    /// A read.
    Read,
    /// A write.
    Write,
}

impl fmt::Display for AccessKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AccessKind::Read => write!(f, "read"),
            AccessKind::Write => write!(f, "write"),
        }
    }
}

/// A pointer's provenance: the allocation it points into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prov {
    /// The allocation, which is one checked cell.
    pub alloc: usize,
}

/// The kind of reference that a retag makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    /// `&T`.
    Shared,
    /// `&mut T`.
    Mut,
}

/// The accesses to the bytes of one checked allocation, in ranges of bytes that have had the same
/// accesses.
#[derive(Debug)]
struct Memory {
    /// The ranges, sorted by their start. Each one ends where the next starts, and the last ends at
    /// `size`.
    locs: Vec<(usize, RaceClocks)>,
    size: usize,
}

impl Memory {
    fn new(size: usize) -> Self {
        let locs = if size == 0 {
            Vec::new()
        } else {
            vec![(0, RaceClocks::default())]
        };
        Self { locs, size }
    }

    /// Makes sure a range of locations starts at `offset`.
    fn split_at(&mut self, offset: usize) {
        if offset >= self.size {
            return;
        }
        if let Err(i) = self.locs.binary_search_by_key(&offset, |(start, _)| *start) {
            let data = self.locs[i - 1].1.clone();
            self.locs.insert(i, (offset, data));
        }
    }

    fn entry_range(&self, i: usize) -> Range<usize> {
        self.locs[i].0..self.locs.get(i + 1).map_or(self.size, |(start, _)| *start)
    }

    /// Checks an access to `range` for a data race, if `check` says so, and records it.
    fn access(
        &mut self,
        range: Range<usize>,
        record: AccessRecord,
        check: bool,
        clock: &VectorClock,
        alloc: &str,
    ) -> Result<(), String> {
        self.split_at(range.start);
        self.split_at(range.end);
        let first = self.locs.partition_point(|(start, _)| *start < range.start);
        let last = self.locs.partition_point(|(start, _)| *start < range.end);
        for i in first..last {
            let loc_range = self.entry_range(i);
            let data = &mut self.locs[i].1;
            if check {
                data.access(record, clock).map_err(|race| {
                    format!(
                        "Data race on bytes {loc_range:?} of {alloc}: {}. Happens-before orders neither one before the other.\n",
                        race.describe()
                    )
                })?;
            } else {
                data.record(record);
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
enum AllocSlot {
    Live { memory: Memory, name: String },
    Freed { name: String, by: TaskId },
}

/// The checked allocations of one execution.
#[derive(Debug)]
pub struct CheckedCellState {
    /// A number that no other execution in this process has. A checked cell records it with its
    /// allocation, so that a cell that outlives an execution (a static) starts over in the next.
    pub execution: u64,
    allocs: Vec<AllocSlot>,
    /// The report of the first violation in this execution. If its panic couldn't fail the
    /// execution, because a panic was unwinding or the code caught it, the execution fails with it
    /// when it ends.
    report: Option<String>,
    /// The report of the first violation that couldn't panic, because a panic was unwinding. If
    /// that panic fails the execution, the report is printed with it.
    unraised: Option<String>,
}

impl CheckedCellState {
    pub(crate) fn new() -> Self {
        // Starts at 1, so that a cell that has never been registered doesn't match any execution.
        static NEXT_EXECUTION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            execution: NEXT_EXECUTION.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            allocs: Vec::new(),
            report: None,
            unraised: None,
        }
    }

    /// Registers a checked cell of `size` bytes, holding a `type_name`, and returns its allocation.
    pub fn new_alloc(&mut self, size: usize, type_name: &str, created_at: &'static Location<'static>) -> usize {
        let alloc = self.allocs.len();
        self.allocs.push(AllocSlot::Live {
            memory: Memory::new(size),
            name: format!("the `UnsafeCell<{type_name}>` made at {created_at}"),
        });
        alloc
    }

    /// Records the write of the value that a cell is made with, by the current task, at every
    /// location. Miri counts the allocation the same way.
    pub fn record_init(&mut self, alloc: usize, task: TaskId, clock: &mut VectorClock, at: &'static Location<'static>) {
        let time = tick(task, clock);
        if let AllocSlot::Live { memory, .. } = &mut self.allocs[alloc] {
            for (_, data) in &mut memory.locs {
                data.record(AccessRecord {
                    task,
                    time,
                    kind: RaceAccess::Write,
                    at,
                });
            }
        }
    }

    /// Keeps the report of a violation, if it is the first in this execution. `raised` says whether
    /// it panics now.
    pub fn record_report(&mut self, report: &str, raised: bool) {
        self.report.get_or_insert_with(|| report.to_string());
        if !raised {
            self.unraised.get_or_insert_with(|| report.to_string());
        }
    }

    /// The report of the first violation, if there was one.
    pub(crate) fn take_report(&mut self) -> Option<String> {
        self.report.take()
    }

    /// The report of the first violation that couldn't panic, if there was one.
    pub(crate) fn take_unraised(&mut self) -> Option<String> {
        self.unraised.take()
    }

    /// Checks an access to `range` of the allocation of `prov` and records it. `what` names the
    /// access in a report, and `races` says whether to check it for a data race.
    #[allow(clippy::too_many_arguments)]
    fn check(
        &mut self,
        task: TaskId,
        clock: &mut VectorClock,
        prov: Prov,
        range: Range<usize>,
        kind: RaceAccess,
        what: &str,
        races: bool,
        at: &'static Location<'static>,
    ) -> Result<(), String> {
        let time = tick(task, clock);
        match &mut self.allocs[prov.alloc] {
            AllocSlot::Freed { name, by } => Err(format!(
                "Undefined Behavior: {what}, at bytes {range:?} of {name}, is a use after free: task {by} dropped the cell.\n"
            )),
            AllocSlot::Live { memory, name } => {
                if range.end > memory.size {
                    return Err(format!(
                        "Undefined Behavior: {what}, at bytes {range:?} of {name}, is out of bounds: the cell has {} bytes.\n",
                        memory.size
                    ));
                }
                let record = AccessRecord { task, time, kind, at };
                memory.access(range, record, races, clock, name)
            }
        }
    }

    /// Checks an access through `prov` to `range` of its allocation, in the current task. `races`
    /// says whether to check it for a data race. An access that isn't checked is still recorded,
    /// so that later accesses are checked against it.
    #[allow(clippy::too_many_arguments)]
    pub fn access(
        &mut self,
        task: TaskId,
        clock: &mut VectorClock,
        prov: Prov,
        range: Range<usize>,
        kind: AccessKind,
        races: bool,
        at: &'static Location<'static>,
    ) -> Result<(), String> {
        let race = match kind {
            AccessKind::Read => RaceAccess::Read,
            AccessKind::Write => RaceAccess::Write,
        };
        self.check(task, clock, prov, range, race, &format!("a {kind} access"), races, at)
    }

    /// Makes a reference to `range` from `prov`, in the current task, and returns its provenance.
    /// Making a reference reads the memory, as in Miri. `races` says whether to check that read for
    /// a data race.
    #[allow(clippy::too_many_arguments)]
    pub fn retag(
        &mut self,
        task: TaskId,
        clock: &mut VectorClock,
        prov: Prov,
        range: Range<usize>,
        _kind: RefKind,
        races: bool,
        at: &'static Location<'static>,
    ) -> Result<Prov, String> {
        self.check(
            task,
            clock,
            prov,
            range,
            RaceAccess::RetagRead,
            "making a reference",
            races,
            at,
        )?;
        Ok(prov)
    }

    /// Records that a checked cell was dropped. A later access through one of its pointers is a use
    /// after free.
    pub fn free(&mut self, alloc: usize, task: TaskId) {
        let slot = &mut self.allocs[alloc];
        if let AllocSlot::Live { name, .. } = slot {
            let name = std::mem::take(name);
            *slot = AllocSlot::Freed { name, by: task };
        }
    }
}

/// Ticks the task's own entry in its clock and returns it. Every access does this first, so that
/// an access made after a release isn't covered by the clock released.
fn tick(task: TaskId, clock: &mut VectorClock) -> u32 {
    clock.increment(task);
    clock.get(usize::from(task))
}
