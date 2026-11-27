//! The permissions of Tree Borrows, and how each one reacts to an access.
//!
//! This is a port of Miri's `src/borrow_tracker/tree_borrows/perms.rs`
//! (<https://github.com/rust-lang/miri>). The comments on the transitions are Miri's. Miri's
//! `ReservedIM` isn't ported, because Shuttle's checked cell treats the data in it as `Freeze`
//! (see `shuttle::cell`).

use std::fmt;

/// Whether an access reads or writes.
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

/// The permission of one tag at one location.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    /// A shared reference to interior mutable data. It allows every access and never changes.
    /// The raw pointers of a checked `UnsafeCell` have it.
    Cell,
    /// A mutable reference that has not been written through yet. It allows child reads and
    /// foreign reads. A child write makes it `Unique`, and a foreign write makes it `Disabled`.
    ///
    /// `conflicted` is set by a foreign read while the tag is protected. A child write is then
    /// UB, as `noalias` requires.
    Reserved {
        /// Whether a foreign read happened while the tag was protected.
        conflicted: bool,
    },
    /// A mutable reference that has been written through. A foreign read makes it `Frozen`, and a
    /// foreign write makes it `Disabled`.
    Unique,
    /// A shared reference. It allows every read. A child write is UB, and a foreign write makes
    /// it `Disabled`.
    Frozen,
    /// A dead reference. Every child access is UB.
    Disabled,
}

impl Permission {
    /// The access that a reborrow with this permission makes through its parent: a read, or a
    /// write for `Unique`. `Cell` makes none, because that could cause data races on thread-safe
    /// types.
    pub fn associated_access(self) -> Option<AccessKind> {
        match self {
            Permission::Cell => None,
            Permission::Unique => Some(AccessKind::Write),
            _ => Some(AccessKind::Read),
        }
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Permission::Cell => write!(f, "Cell"),
            Permission::Reserved { conflicted: false } => write!(f, "Reserved"),
            Permission::Reserved { conflicted: true } => write!(f, "Reserved (conflicted)"),
            Permission::Unique => write!(f, "Unique"),
            Permission::Frozen => write!(f, "Frozen"),
            Permission::Disabled => write!(f, "Disabled"),
        }
    }
}

/// A child node was read-accessed: UB on Disabled, noop on the rest.
fn child_read(state: Permission, _protected: bool) -> Option<Permission> {
    use Permission::*;
    Some(match state {
        Disabled => return None,
        readable @ (Cell | Reserved { .. } | Unique | Frozen) => readable,
    })
}

/// A non-child node was read-accessed: keep `Reserved` but mark it as `conflicted` if it is
/// protected; invalidate `Unique`.
fn foreign_read(state: Permission, protected: bool) -> Option<Permission> {
    use Permission::*;
    Some(match state {
        // Cell ignores foreign reads.
        Cell => Cell,
        // Non-writeable states just ignore foreign reads.
        non_writeable @ (Frozen | Disabled) => non_writeable,
        // Someone else read. To make sure we won't write before function exit, we set the
        // "conflicted" flag, which will disallow writes while we are protected.
        Reserved { .. } if protected => Reserved { conflicted: true },
        // Before activation and without protectors, foreign reads are fine. That's the entire
        // point of 2-phase borrows.
        res @ Reserved { .. } => res,
        Unique => {
            if protected {
                // We wrote, someone else reads -- that's bad. (Since Unique is always
                // initialized, this move-to-protected will mean insta-UB.)
                Disabled
            } else {
                // We don't want to disable here to allow read-read reordering: it is crucial
                // that the foreign read does not invalidate future reads through this tag.
                Frozen
            }
        }
    })
}

/// A child node was write-accessed: `Reserved` must become `Unique` to obtain write permissions,
/// `Frozen` and `Disabled` cannot obtain such permissions and produce UB.
fn child_write(state: Permission, protected: bool) -> Option<Permission> {
    use Permission::*;
    Some(match state {
        // Cell ignores child writes.
        Cell => Cell,
        // If the `conflicted` flag is set, then there was a foreign read during the function call
        // that is still ongoing (still `protected`), this is UB (`noalias` violation).
        Reserved { conflicted: true } if protected => return None,
        // A write always activates the 2-phase borrow.
        Reserved { .. } | Unique => Unique,
        Frozen | Disabled => return None,
    })
}

/// A non-child node was write-accessed: this makes everything `Disabled` except `Cell`.
fn foreign_write(state: Permission, _protected: bool) -> Option<Permission> {
    use Permission::*;
    Some(match state {
        // Cell ignores foreign writes.
        Cell => Cell,
        _ => Disabled,
    })
}

/// The permission after an access, or `None` if the access is UB. `foreign` says whether the
/// access went through a tag outside this tag's subtree.
pub fn perform_access(kind: AccessKind, foreign: bool, perm: Permission, protected: bool) -> Option<Permission> {
    match (kind, foreign) {
        (AccessKind::Write, true) => foreign_write(perm, protected),
        (AccessKind::Read, true) => foreign_read(perm, protected),
        (AccessKind::Write, false) => child_write(perm, protected),
        (AccessKind::Read, false) => child_read(perm, protected),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Permission::*;

    const ALL: [Permission; 6] = [
        Cell,
        Reserved { conflicted: false },
        Reserved { conflicted: true },
        Unique,
        Frozen,
        Disabled,
    ];

    // Miri's `permissionpriv_partialord_is_reachability`, for the order
    // Reserved < Reserved (conflicted) < Unique < Frozen < Disabled: no access moves a permission
    // backwards in that order, and Cell never changes.
    #[test]
    fn transitions_only_move_forward() {
        fn rank(p: Permission) -> u8 {
            match p {
                Reserved { conflicted: false } => 0,
                Reserved { conflicted: true } => 1,
                Unique => 2,
                Frozen => 3,
                Disabled => 4,
                Cell => unreachable!(),
            }
        }
        for perm in ALL {
            for kind in [AccessKind::Read, AccessKind::Write] {
                for foreign in [false, true] {
                    for protected in [false, true] {
                        if let Some(next) = perform_access(kind, foreign, perm, protected) {
                            if perm == Cell {
                                assert_eq!(next, Cell);
                            } else {
                                assert!(rank(next) >= rank(perm), "{perm} -> {next}");
                            }
                        }
                    }
                }
            }
        }
    }

    // A foreign access is never UB by itself: only a child access is.
    #[test]
    fn foreign_accesses_are_never_ub() {
        for perm in ALL {
            for kind in [AccessKind::Read, AccessKind::Write] {
                for protected in [false, true] {
                    assert!(perform_access(kind, true, perm, protected).is_some());
                }
            }
        }
    }

    #[test]
    fn unprotected_two_phase_borrow() {
        let r = Reserved { conflicted: false };
        // Foreign reads leave Reserved alone, a child write activates it.
        assert_eq!(perform_access(AccessKind::Read, true, r, false), Some(r));
        assert_eq!(perform_access(AccessKind::Write, false, r, false), Some(Unique));
        // A foreign read freezes Unique, and a child write is then UB.
        assert_eq!(perform_access(AccessKind::Read, true, Unique, false), Some(Frozen));
        assert_eq!(perform_access(AccessKind::Write, false, Frozen, false), None);
        // A foreign write disables everything but Cell.
        assert_eq!(perform_access(AccessKind::Write, true, Frozen, false), Some(Disabled));
        assert_eq!(perform_access(AccessKind::Write, true, Cell, false), Some(Cell));
        assert_eq!(perform_access(AccessKind::Read, false, Disabled, false), None);
    }
}
