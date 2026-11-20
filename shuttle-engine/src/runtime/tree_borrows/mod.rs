//! Per-execution state of the Tree Borrows and data race checks of Shuttle's checked `UnsafeCell`
//! (`shuttle::cell`).
//!
//! Each checked cell is an allocation here, with a tree of the tags made from it (see `tree`).
//! The cell's API in `shuttle-std` reports each reference it makes and each access to the cell's
//! memory, and panics with the report of the first violation. The Tree Borrows rules are a port of
//! Miri's (<https://github.com/rust-lang/miri>, `src/borrow_tracker/tree_borrows`), and the race
//! detection follows Miri's detector for non-atomic accesses.

mod perms;
mod race;
mod tree;

pub use perms::{AccessKind, Permission};
pub use tree::TagKind;

use crate::runtime::task::clock::VectorClock;
use crate::runtime::task::TaskId;
use race::RaceAccess;
use std::ops::Range;
use std::panic::Location;
use tree::{Access, Tree, CELL, ROOT};

/// A pointer's provenance: the allocation it points into and its tag there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prov {
    /// The allocation, which is one checked cell.
    pub alloc: usize,
    /// The tag in the allocation's tree.
    pub tag: usize,
}

impl Prov {
    /// The provenance of the cell itself, which `get_mut`, `into_inner` and the cell's `Drop` use.
    pub fn root(alloc: usize) -> Self {
        Self { alloc, tag: ROOT }
    }

    /// The provenance of the cell's raw pointers.
    pub fn cell(alloc: usize) -> Self {
        Self { alloc, tag: CELL }
    }
}

/// The kind of reference that a retag makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    /// `&T`. It starts `Frozen`.
    Shared,
    /// `&mut T`. It starts `Reserved`.
    Mut,
}

#[derive(Debug)]
enum AllocSlot {
    Live { tree: Tree, name: String },
    Freed { name: String, by: TaskId },
}

/// The checked allocations of one execution.
#[derive(Debug)]
pub struct TreeBorrowsState {
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

impl TreeBorrowsState {
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
    pub fn new_alloc(
        &mut self,
        size: usize,
        type_name: &str,
        created_at: &'static Location<'static>,
        task: TaskId,
    ) -> usize {
        let alloc = self.allocs.len();
        self.allocs.push(AllocSlot::Live {
            tree: Tree::new(size, created_at, task),
            name: format!("the `UnsafeCell<{type_name}>` made at {created_at}"),
        });
        alloc
    }

    /// Records the write of the value that a cell is made with, by the current task.
    pub fn record_init(&mut self, alloc: usize, task: TaskId, clock: &mut VectorClock, at: &'static Location<'static>) {
        let time = tick(task, clock);
        if let AllocSlot::Live { tree, .. } = &mut self.allocs[alloc] {
            tree.record_init(task, time, at);
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

    /// The tree of a live allocation, and the report of a violation that a freed or out-of-bounds
    /// access is.
    fn tree(&mut self, prov: Prov, range: &Range<usize>, what: &str) -> Result<(&mut Tree, &str), String> {
        match &mut self.allocs[prov.alloc] {
            AllocSlot::Freed { name, by } => Err(format!(
                "Undefined Behavior: {what}, at bytes {range:?} of {name}, is a use after free: task {by} dropped the cell.\n"
            )),
            AllocSlot::Live { tree, name } => {
                if range.end > tree.size() {
                    return Err(format!(
                        "Undefined Behavior: {what}, at bytes {range:?} of {name}, is out of bounds: the cell has {} bytes.\n",
                        tree.size()
                    ));
                }
                assert!(tree.contains(prov.tag), "a removed tag was used");
                Ok((tree, name.as_str()))
            }
        }
    }

    /// Checks an access through `prov` to `range` of its allocation, in the current task. `races`
    /// says whether to check it for a data race too. An access that isn't checked is still recorded,
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
        let (tree, name) = self.tree(prov, &range, &format!("a {kind} access"))?;
        let access = Access {
            kind,
            race: match kind {
                AccessKind::Read => RaceAccess::Read,
                AccessKind::Write => RaceAccess::Write,
            },
            check_race: races,
            retag: false,
            task,
            time: tick(task, clock),
            at,
        };
        tree.access(prov.tag, range, &access, clock)
            .map_err(|violation| tree.describe(&violation, name))
    }

    /// Makes a reference to `range` from `prov`, in the current task, and returns its provenance.
    /// `races` says whether to check the read that this makes for a data race too.
    #[allow(clippy::too_many_arguments)]
    pub fn retag(
        &mut self,
        task: TaskId,
        clock: &mut VectorClock,
        prov: Prov,
        range: Range<usize>,
        kind: RefKind,
        races: bool,
        at: &'static Location<'static>,
    ) -> Result<Prov, String> {
        let (tree, name) = self.tree(prov, &range, "making a reference")?;
        let (tag_kind, initial) = match kind {
            RefKind::Shared => (TagKind::Shared, Permission::Frozen),
            RefKind::Mut => (TagKind::Mut, Permission::Reserved { conflicted: false }),
        };
        let access = Access {
            kind: AccessKind::Read,
            race: RaceAccess::RetagRead,
            check_race: races,
            retag: true,
            task,
            time: tick(task, clock),
            at,
        };
        let tag = tree
            .retag(prov.tag, range, tag_kind, initial, &access, clock)
            .map_err(|violation| tree.describe(&violation, name))?;
        Ok(Prov { alloc: prov.alloc, tag })
    }

    /// Records that a reference's tag can't be used again.
    pub fn release(&mut self, prov: Prov) {
        if let AllocSlot::Live { tree, .. } = &mut self.allocs[prov.alloc] {
            if tree.contains(prov.tag) {
                tree.release(prov.tag);
            }
        }
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
