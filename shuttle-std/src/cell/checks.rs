//! The hooks through which Shuttle checks the accesses to the value of an `UnsafeCell`. With the
//! `checked-cell` feature, they apply Tree Borrows and the data race checks. Without it, they do
//! nothing.

/// An access to a value.
#[derive(Clone, Copy)]
pub(super) enum Access {
    Read,
    Write,
}

/// A reference made from a pointer.
#[derive(Clone, Copy)]
pub(super) enum Reference {
    Shared,
    Mut,
}

#[cfg(feature = "checked-cell")]
pub(super) use checked::*;

#[cfg(not(feature = "checked-cell"))]
pub(super) use unchecked::*;

#[cfg(feature = "checked-cell")]
mod checked {
    use super::{Access, Reference};
    use shuttle_engine::runtime::checked_cell::{AccessKind, CheckedCellState, Prov, RefKind};
    use shuttle_engine::runtime::execution::ExecutionState;
    use shuttle_engine::runtime::task::clock::VectorClock;
    use shuttle_engine::runtime::task::TaskId;
    use std::mem::size_of;
    use std::ops::Range;
    use std::panic::Location;

    /// Where a pointer points in a checked allocation of the current execution.
    #[derive(Clone, Copy, Debug)]
    pub(in crate::cell) struct Track {
        execution: u64,
        prov: Prov,
        /// Bytes from the start of the cell's value.
        offset: usize,
    }

    impl Track {
        fn range<T>(&self) -> Range<usize> {
            self.offset..self.offset.saturating_add(size_of::<T>())
        }
    }

    /// The pointer's tag, for its `Debug` output.
    pub(in crate::cell) fn tag(track: Option<Track>) -> Option<usize> {
        track.map(|track| track.prov.tag)
    }

    /// The same tag, `bytes` further on in the value.
    pub(in crate::cell) fn offset(track: Option<Track>, bytes: usize) -> Option<Track> {
        track.map(|track| Track {
            offset: track.offset.wrapping_add(bytes),
            ..track
        })
    }

    /// Where an `UnsafeCell` is registered.
    pub(in crate::cell) struct Alloc {
        /// The execution in which the cell was last registered, and its allocation there.
        alloc: std::cell::Cell<(u64, usize)>,
        created_at: &'static Location<'static>,
    }

    impl Alloc {
        /// Registers a new cell of a `T`. In an execution, this counts as a write of the value by
        /// the current task, as in Miri, so an access that doesn't happen after it is a data race.
        #[track_caller]
        pub(in crate::cell) fn new<T>() -> Self {
            let this = Self {
                alloc: std::cell::Cell::new((0, 0)),
                created_at: Location::caller(),
            };
            if size_of::<T>() != 0 {
                let _ = ExecutionState::try_with(|state| {
                    if let Some((task, clock, tb)) = state.checked_cell() {
                        let alloc = tb.new_alloc(size_of::<T>(), std::any::type_name::<T>(), this.created_at, task);
                        tb.record_init(alloc, task, clock, this.created_at);
                        this.alloc.set((tb.execution, alloc));
                    }
                });
            }
            this
        }

        /// The cell's allocation in the current execution, through the cell itself (`root`) or
        /// through its raw pointer. The cell is registered at its first use in an execution, so a
        /// cell that outlives one execution starts over in the next.
        pub(in crate::cell) fn track<T>(&self, root: bool) -> Option<Track> {
            if size_of::<T>() == 0 {
                return None;
            }
            ExecutionState::try_with(|state| {
                let (task, _, tb) = state.checked_cell()?;
                let (execution, alloc) = self.alloc.get();
                let alloc = if execution == tb.execution {
                    alloc
                } else {
                    let alloc = tb.new_alloc(size_of::<T>(), std::any::type_name::<T>(), self.created_at, task);
                    self.alloc.set((tb.execution, alloc));
                    alloc
                };
                Some(Track {
                    execution: tb.execution,
                    prov: if root { Prov::root(alloc) } else { Prov::cell(alloc) },
                    offset: 0,
                })
            })
            .ok()
            .flatten()
        }

        /// Records that the cell is gone, so that a later use of its pointers is reported.
        pub(in crate::cell) fn free(&self) {
            let (execution, alloc) = self.alloc.get();
            let _ = ExecutionState::try_with(|state| {
                if let Some((task, _, tb)) = state.checked_cell() {
                    if tb.execution == execution {
                        tb.free(alloc, task);
                    }
                }
            });
        }
    }

    /// Runs `f` on the checked allocations of the current execution, and panics with the report of
    /// a violation. Does nothing outside an execution, or if `track` is from another execution. `f`
    /// gets whether to check for data races: not while the execution is torn down, because teardown
    /// drops the futures of unfinished tasks without anything that orders the drops after other
    /// accesses.
    #[track_caller]
    fn with_state<R>(
        track: Option<Track>,
        f: impl FnOnce(&mut CheckedCellState, TaskId, &mut VectorClock, Track, bool) -> Result<R, String>,
    ) -> Option<R> {
        let track = track?;
        let result = ExecutionState::try_with(|state| {
            let races = !state.in_cleanup();
            let (task, clock, tb) = state.checked_cell()?;
            if tb.execution != track.execution {
                return None;
            }
            let result = f(tb, task, clock, track, races);
            if let Err(report) = &result {
                tb.record_report(report, !std::thread::panicking());
            }
            Some(result)
        })
        .ok()
        .flatten();
        match result {
            Some(Ok(r)) => Some(r),
            // A panic while another one unwinds would abort the process. The execution keeps the
            // report and fails with it when it ends.
            Some(Err(report)) if !std::thread::panicking() => panic!("{report}"),
            Some(Err(_)) | None => None,
        }
    }

    /// Records that a reference's tag can't be used again.
    pub(in crate::cell) fn release(track: Option<Track>) {
        let _ = with_state(track, |tb, _, _, track, _| {
            tb.release(track.prov);
            Ok(())
        });
    }

    /// Checks an access through `track`.
    #[track_caller]
    pub(in crate::cell) fn access<T>(track: Option<Track>, kind: Access, at: &'static Location<'static>) {
        access_with::<T>(track, kind, true, at);
    }

    /// Records an access through `track`, which isn't checked for a data race, because the caller
    /// has the cell to itself.
    #[track_caller]
    pub(in crate::cell) fn access_unordered<T>(track: Option<Track>, kind: Access, at: &'static Location<'static>) {
        access_with::<T>(track, kind, false, at);
    }

    #[track_caller]
    fn access_with<T>(track: Option<Track>, kind: Access, races: bool, at: &'static Location<'static>) {
        if size_of::<T>() == 0 {
            return;
        }
        let kind = match kind {
            Access::Read => AccessKind::Read,
            Access::Write => AccessKind::Write,
        };
        with_state(track, |tb, task, clock, track, check| {
            tb.access(task, clock, track.prov, track.range::<T>(), kind, races && check, at)
        });
    }

    /// Makes the tag of a new reference through `track`. A reference to a zero-sized `T` gets one
    /// too, as in Miri, because its pointer can still access the bytes around it.
    #[track_caller]
    pub(in crate::cell) fn retag<T>(
        track: Option<Track>,
        kind: Reference,
        at: &'static Location<'static>,
    ) -> Option<Track> {
        let kind = match kind {
            Reference::Shared => RefKind::Shared,
            Reference::Mut => RefKind::Mut,
        };
        with_state(track, |tb, task, clock, track, races| {
            let prov = tb.retag(task, clock, track.prov, track.range::<T>(), kind, races, at)?;
            Ok(Track { prov, ..track })
        })
    }
}

// The type parameters keep the signatures of the checked versions.
#[cfg(not(feature = "checked-cell"))]
#[allow(clippy::extra_unused_type_parameters)]
mod unchecked {
    use super::{Access, Reference};
    use std::panic::Location;

    /// Never made without the `checked-cell` feature.
    #[derive(Clone, Copy, Debug)]
    pub(in crate::cell) enum Track {}

    pub(in crate::cell) fn tag(_track: Option<Track>) -> Option<usize> {
        None
    }

    pub(in crate::cell) fn offset(track: Option<Track>, _bytes: usize) -> Option<Track> {
        track
    }

    pub(in crate::cell) struct Alloc;

    impl Alloc {
        pub(in crate::cell) fn new<T>() -> Self {
            Self
        }

        pub(in crate::cell) fn track<T>(&self, _root: bool) -> Option<Track> {
            None
        }

        pub(in crate::cell) fn free(&self) {}
    }

    pub(in crate::cell) fn release(_track: Option<Track>) {}

    pub(in crate::cell) fn access<T>(_track: Option<Track>, _kind: Access, _at: &'static Location<'static>) {}

    pub(in crate::cell) fn access_unordered<T>(_track: Option<Track>, _kind: Access, _at: &'static Location<'static>) {}

    pub(in crate::cell) fn retag<T>(
        _track: Option<Track>,
        _kind: Reference,
        _at: &'static Location<'static>,
    ) -> Option<Track> {
        None
    }
}
