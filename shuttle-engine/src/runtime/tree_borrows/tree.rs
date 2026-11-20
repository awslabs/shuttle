//! The tree of tags of one checked allocation, with the permission of each tag at each location.
//!
//! This ports the core of Miri's `src/borrow_tracker/tree_borrows/tree.rs`: each reference made
//! from the allocation gets a tag, which is a child of the tag of the pointer it was made from. An
//! access through a tag is a child (local) access for that tag and its ancestors, and a foreign
//! access for every other tag. Each tag's permission at each accessed location then changes as
//! `perms` says, and an access that a permission forbids is UB.
//!
//! What Miri has that this doesn't: protectors (Shuttle sees no function calls), wildcard
//! provenance, the skipping of foreign accesses that can't change anything, and its garbage
//! collector. Instead, a tag is removed when it can't be used again and has no children left.

use super::perms::{self, AccessKind, Permission};
use super::race::{AccessRecord, Race, RaceAccess, RaceClocks};
use crate::runtime::task::clock::VectorClock;
use crate::runtime::task::TaskId;
use std::fmt::Write;
use std::ops::Range;
use std::panic::Location;

/// The index of a tag in its tree.
pub(crate) type TagIdx = usize;

/// The tag of the allocation itself, which `get_mut`, `into_inner` and dropping the cell use.
pub(crate) const ROOT: TagIdx = 0;

/// The tag of the cell's raw pointers. It has the `Cell` permission, as the raw pointer from
/// `std::cell::UnsafeCell::get` does under Tree Borrows.
pub(crate) const CELL: TagIdx = 1;

/// The most events a tag's history keeps for the report of a violation.
const HISTORY_LIMIT: usize = 32;

/// What made a tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagKind {
    /// The allocation itself.
    Root,
    /// The cell's raw pointers.
    Cell,
    /// A shared reference.
    Shared,
    /// A mutable reference.
    Mut,
}

/// A change of a tag's permission, for the report of a violation.
#[derive(Clone, Debug)]
struct Event {
    from: Permission,
    to: Permission,
    range: Range<usize>,
    kind: AccessKind,
    foreign: bool,
    through: TagIdx,
    retag: bool,
    task: TaskId,
    at: &'static Location<'static>,
}

#[derive(Debug)]
struct Node {
    parent: Option<TagIdx>,
    kind: TagKind,
    /// The permission the tag starts with at every location.
    initial: Permission,
    created_at: &'static Location<'static>,
    created_by: TaskId,
    history: Vec<Event>,
    children: usize,
    /// Whether the tag can't be used again: the reference was dropped and no raw pointer was made
    /// from it.
    dead: bool,
    removed: bool,
}

/// The state of one range of locations.
#[derive(Clone, Debug, Default)]
struct LocData {
    /// The permission of each tag that differs from the one it started with.
    perms: Vec<(TagIdx, Permission)>,
    race: RaceClocks,
}

/// A violation that an access found.
#[derive(Debug)]
pub(crate) enum Violation {
    /// The permission of `conflicting`, which is `accessed` or one of its ancestors, forbids the
    /// access.
    Forbidden {
        accessed: TagIdx,
        conflicting: TagIdx,
        perm: Permission,
        kind: AccessKind,
        retag: bool,
        range: Range<usize>,
    },
    /// The access races with an earlier one.
    Race { race: Race, range: Range<usize> },
}

/// An access, as the tree records it.
pub(crate) struct Access {
    pub kind: AccessKind,
    /// What the access is for the race check.
    pub race: RaceAccess,
    /// Whether to check the access for a data race. An access that isn't checked is still recorded.
    pub check_race: bool,
    pub retag: bool,
    pub task: TaskId,
    /// The accessing task's own entry in its clock, which the caller has just ticked.
    pub time: u32,
    pub at: &'static Location<'static>,
}

#[derive(Debug)]
pub(crate) struct Tree {
    nodes: Vec<Node>,
    /// The tags that aren't removed.
    alive: Vec<TagIdx>,
    /// The ranges of locations, sorted by their start. Each one ends where the next starts, and the
    /// last ends at `size`.
    locs: Vec<(usize, LocData)>,
    size: usize,
}

impl Tree {
    pub(crate) fn new(size: usize, created_at: &'static Location<'static>, task: TaskId) -> Self {
        let node = |parent, kind, initial| Node {
            parent,
            kind,
            initial,
            created_at,
            created_by: task,
            history: Vec::new(),
            children: 0,
            dead: false,
            removed: false,
        };
        let mut nodes = vec![
            // Miri's root is accessed everywhere, as if the allocation were a write.
            node(None, TagKind::Root, Permission::Unique),
            node(Some(ROOT), TagKind::Cell, Permission::Cell),
        ];
        nodes[ROOT].children = 1;
        let locs = if size == 0 {
            Vec::new()
        } else {
            vec![(0, LocData::default())]
        };
        Self {
            nodes,
            alive: vec![ROOT, CELL],
            locs,
            size,
        }
    }

    pub(crate) fn size(&self) -> usize {
        self.size
    }

    pub(crate) fn contains(&self, tag: TagIdx) -> bool {
        self.nodes.get(tag).is_some_and(|node| !node.removed)
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

    /// The indices in `locs` of the ranges that make up `range`.
    fn entries(&mut self, range: &Range<usize>) -> Range<usize> {
        self.split_at(range.start);
        self.split_at(range.end);
        let first = self.locs.partition_point(|(start, _)| *start < range.start);
        let last = self.locs.partition_point(|(start, _)| *start < range.end);
        first..last
    }

    fn entry_range(&self, i: usize) -> Range<usize> {
        self.locs[i].0..self.locs.get(i + 1).map_or(self.size, |(start, _)| *start)
    }

    /// Performs an access through `through` on `range`: changes the permission of every tag, and
    /// checks for UB and for a data race.
    pub(crate) fn access(
        &mut self,
        through: TagIdx,
        range: Range<usize>,
        access: &Access,
        clock: &VectorClock,
    ) -> Result<(), Violation> {
        // The tags for which this is a child access: `through` and its ancestors.
        let mut local = vec![false; self.nodes.len()];
        let mut tag = Some(through);
        while let Some(t) = tag {
            local[t] = true;
            tag = self.nodes[t].parent;
        }
        for i in self.entries(&range) {
            let loc_range = self.entry_range(i);
            let data = &mut self.locs[i].1;
            for &idx in &self.alive {
                let node = &mut self.nodes[idx];
                let foreign = !local[idx];
                let slot = data.perms.iter().position(|(t, _)| *t == idx);
                let old = slot.map_or(node.initial, |s| data.perms[s].1);
                let Some(new) = perms::perform_access(access.kind, foreign, old, false) else {
                    return Err(Violation::Forbidden {
                        accessed: through,
                        conflicting: idx,
                        perm: old,
                        kind: access.kind,
                        retag: access.retag,
                        range: loc_range,
                    });
                };
                if new != old {
                    if node.history.len() == HISTORY_LIMIT {
                        node.history.remove(0);
                    }
                    node.history.push(Event {
                        from: old,
                        to: new,
                        range: loc_range.clone(),
                        kind: access.kind,
                        foreign,
                        through,
                        retag: access.retag,
                        task: access.task,
                        at: access.at,
                    });
                    match slot {
                        Some(s) => data.perms[s].1 = new,
                        None => data.perms.push((idx, new)),
                    }
                }
            }
            let record = AccessRecord {
                task: access.task,
                time: access.time,
                kind: access.race,
                at: access.at,
            };
            if access.check_race {
                data.race
                    .access(record, clock)
                    .map_err(|race| Violation::Race { race, range: loc_range })?;
            } else {
                data.race.record(record);
            }
        }
        Ok(())
    }

    /// Records the write of the value that the cell is made with, at every location. Miri counts
    /// the allocation the same way.
    pub(crate) fn record_init(&mut self, task: TaskId, time: u32, at: &'static Location<'static>) {
        for (_, data) in &mut self.locs {
            data.race.record(AccessRecord {
                task,
                time,
                kind: RaceAccess::Write,
                at,
            });
        }
    }

    /// Makes a new tag, a child of `parent`, for a reference to `range`. The reborrow reads
    /// through the parent first, as in Miri.
    pub(crate) fn retag(
        &mut self,
        parent: TagIdx,
        range: Range<usize>,
        kind: TagKind,
        initial: Permission,
        access: &Access,
        clock: &VectorClock,
    ) -> Result<TagIdx, Violation> {
        if let Some(kind) = initial.associated_access() {
            let race = match kind {
                AccessKind::Read => RaceAccess::RetagRead,
                AccessKind::Write => RaceAccess::Write,
            };
            let access = Access {
                kind,
                race,
                retag: true,
                ..*access
            };
            self.access(parent, range, &access, clock)?;
        }
        let idx = self.nodes.len();
        self.nodes.push(Node {
            parent: Some(parent),
            kind,
            initial,
            created_at: access.at,
            created_by: access.task,
            history: Vec::new(),
            children: 0,
            dead: false,
            removed: false,
        });
        self.nodes[parent].children += 1;
        self.alive.push(idx);
        Ok(idx)
    }

    /// Records that a tag can't be used again, and removes it and its ancestors that are no longer
    /// needed. A tag that can't be used again and has no children can't change any other tag or
    /// cause UB.
    pub(crate) fn release(&mut self, tag: TagIdx) {
        self.nodes[tag].dead = true;
        let mut tag = tag;
        while tag > CELL && self.nodes[tag].dead && !self.nodes[tag].removed && self.nodes[tag].children == 0 {
            self.nodes[tag].removed = true;
            self.alive.retain(|t| *t != tag);
            for (_, data) in &mut self.locs {
                data.perms.retain(|(t, _)| *t != tag);
            }
            let parent = self.nodes[tag].parent.expect("only the root has no parent");
            self.nodes[parent].children -= 1;
            tag = parent;
        }
    }

    /// Describes a tag, as in "tag #3, a `&mut` made at src/lib.rs:10:5 in task 1".
    fn describe_tag(&self, tag: TagIdx) -> String {
        let node = &self.nodes[tag];
        match node.kind {
            TagKind::Root => format!("tag #{tag}, the cell itself"),
            TagKind::Cell => format!("tag #{tag}, the cell's raw pointer"),
            TagKind::Shared => format!(
                "tag #{tag}, a `&` made at {} in task {}",
                node.created_at, node.created_by
            ),
            TagKind::Mut => format!(
                "tag #{tag}, a `&mut` made at {} in task {}",
                node.created_at, node.created_by
            ),
        }
    }

    /// The report of a violation, which the checked cell panics with.
    pub(crate) fn describe(&self, violation: &Violation, alloc: &str) -> String {
        match violation {
            Violation::Forbidden {
                accessed,
                conflicting,
                perm,
                kind,
                retag,
                range,
            } => {
                let what = if *retag {
                    format!("making a reference through {}", self.describe_tag(*accessed))
                } else {
                    format!("a {kind} access through {}", self.describe_tag(*accessed))
                };
                let mut msg = format!(
                    "Undefined Behavior (Tree Borrows): {what}, at bytes {range:?} of {alloc}, is forbidden.\n"
                );
                if conflicting == accessed {
                    let _ = writeln!(msg, "The tag has permission {perm}, which forbids a child {kind}.");
                } else {
                    let _ = writeln!(
                        msg,
                        "Its ancestor {} has permission {perm}, which forbids a child {kind}.",
                        self.describe_tag(*conflicting)
                    );
                }
                let node = &self.nodes[*conflicting];
                let _ = writeln!(
                    msg,
                    "Tag #{conflicting} started as {} when it was made, at {} in task {}.",
                    node.initial, node.created_at, node.created_by
                );
                for event in node
                    .history
                    .iter()
                    .filter(|e| e.range.start < range.end && range.start < e.range.end)
                {
                    let how = if event.foreign {
                        format!("a foreign {} through {}", event.kind, self.describe_tag(event.through))
                    } else if event.through == *conflicting {
                        format!("a {} through this tag", event.kind)
                    } else {
                        format!("a child {} through {}", event.kind, self.describe_tag(event.through))
                    };
                    let how = if event.retag {
                        format!("{how}, which made a reference")
                    } else {
                        how
                    };
                    let _ = writeln!(
                        msg,
                        "It became {} at {} in task {}, after {how}.",
                        event.to, event.at, event.task
                    );
                    debug_assert_ne!(event.from, event.to);
                }
                msg
            }
            Violation::Race { race, range } => format!(
                "Data race on bytes {range:?} of {alloc}: {}. Happens-before orders neither one before the other.\n",
                race.describe()
            ),
        }
    }
}
