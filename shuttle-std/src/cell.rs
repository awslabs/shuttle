//! A checked [`UnsafeCell`], whose memory Shuttle checks for Tree Borrows violations and data races.
//!
//! Shuttle sees only the operations of its own primitives, so it can't check a plain
//! `std::cell::UnsafeCell`. This cell's pointers and references go through Shuttle instead. Each
//! reference made from the cell gets a tag, and Shuttle applies the rules of Tree Borrows, ported
//! from [Miri](https://github.com/rust-lang/miri), to every access. It also checks every access
//! for a data race with Shuttle's vector clocks, as Miri's race detector does. The first violation
//! panics with a report, so the test fails with a schedule that replays it. A violation while a
//! panic unwinds can't panic, because that would abort the process. If that panic fails the test,
//! Shuttle prints the report with it. If the code catches the panic, the execution fails with the
//! report when it ends, and so it does after a violation whose own panic the code catches.
//!
//! With the `weak-memory` feature and [`Config::weak_memory`](shuttle_engine::Config) set, the
//! atomics order memory only as much as their orderings say. A non-atomic access that a too-weak
//! ordering leaves unordered is then a data race here. Without the emulation, every atomic
//! operation orders memory as SeqCst does.
//!
//! ```
//! use shuttle_std::cell::UnsafeCell;
//! use shuttle_std::sync::Mutex;
//! use shuttle_std::{sync::Arc, thread};
//!
//! struct Counter {
//!     lock: Mutex<()>,
//!     value: UnsafeCell<u64>,
//! }
//! unsafe impl Sync for Counter {}
//!
//! impl Counter {
//!     fn increment(&self) {
//!         let _guard = self.lock.lock().unwrap();
//!         // Made and used under the lock. Making it before the lock would be UB under Tree
//!         // Borrows, once another thread writes in between, and Shuttle reports that.
//!         let mut value = unsafe { self.value.get().as_mut() };
//!         *value += 1;
//!     }
//! }
//!
//! shuttle_schedulers::check_random(
//!     || {
//!         let counter = Arc::new(Counter { lock: Mutex::new(()), value: UnsafeCell::new(0) });
//!         let other = Arc::clone(&counter);
//!         let t = thread::spawn(move || other.increment());
//!         counter.increment();
//!         t.join().unwrap();
//!     },
//!     100,
//! );
//! ```
//!
//! # What Shuttle sees
//!
//! [`UnsafeCell::get`] returns a [`Ptr`], which is a raw pointer that carries its tag.
//! [`Ptr::as_ref`] and [`Ptr::as_mut`] make a reference, as `&*ptr` and `&mut *ptr` do, and
//! return a [`Ref`] or [`RefMut`]. Shuttle checks each read and write through a `Ptr`, each
//! dereference of a `Ref` or `RefMut` (a `DerefMut` counts as a write), and the read that making
//! a reference does. [`Ptr::project`] makes a pointer to a field, as `&raw mut (*p).field` does.
//! [`UnsafeCell::with`] and [`UnsafeCell::with_mut`] are loom's API: each one counts as one read or
//! write through the cell's raw pointer, before the closure runs.
//!
//! What Shuttle doesn't see:
//! - Accesses through a plain reference or raw pointer that the code takes out of a `Ref`,
//!   `RefMut` or `Ptr`, as with `&mut *guard`, [`Ptr::as_raw`] or the pointer that `with` passes.
//!   Only the access that took it out is checked.
//! - Which part of the value a guard's code uses. A dereference of a guard reads all of `T`, and
//!   a `DerefMut` writes all of it, even if the code uses one field or writes nothing. So Shuttle
//!   reports some code that Miri accepts, such as a guard to a struct that reads one field after a
//!   write through the cell to another field. For an access to one field, use [`RefMut::as_ptr`]
//!   or [`Ref::as_ptr`] with [`Ptr::project`].
//! - A hand-off through something that doesn't keep Shuttle's vector clocks. Shuttle can then
//!   report a data race that isn't one. Examples are std's `Arc`, which `shuttle::sync::Arc` is
//!   (Shuttle doesn't see that the last drop of an `Arc` comes after the others), the channels of
//!   the `futures` crate, shuttle-tokio's `oneshot` and `Notify`, the std atomic that the fast path
//!   of shuttle-tokio's `OnceCell::get` reads, and `thread::park` and `Thread::unpark`, which keep no
//!   clocks. [`UnsafeCell::get_mut`] and
//!   [`UnsafeCell::into_inner`] aren't checked for races, so a `Drop` impl should reach the cell
//!   through `get_mut`. Dropping the cell isn't checked for a race either. A use after the drop is
//!   still reported.
//! - Races while the execution is torn down. Teardown drops the futures of unfinished tasks, and
//!   nothing orders those drops after the other accesses.
//! - A checked cell in a static, which two executions use at once, as parallel tests do. The cell
//!   holds one registration, for the execution that used it last, so the checks miss violations.
//!   Run tests that share a static checked cell one at a time.
//! - Function calls, so no tag is protected. Miri's protectors make some code UB at once that is
//!   UB here only when a disabled reference is used again.
//! - Interior mutability inside the cell. A `&T` is `Frozen` even if `T` has an atomic in it.
//!   Accesses to such an atomic don't go through the cell, so they aren't checked here.
//! - Reads of uninitialized memory, and memory that isn't in a checked cell.

use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::runtime::task::clock::VectorClock;
use shuttle_engine::runtime::task::TaskId;
use shuttle_engine::runtime::tree_borrows::{AccessKind, Prov, RefKind, TreeBorrowsState};
use std::fmt;
use std::marker::PhantomData;
use std::mem::{size_of, ManuallyDrop};
use std::ops::{Deref, DerefMut, Range};
use std::panic::Location;

/// Where a pointer points in a checked allocation of the current execution.
#[derive(Clone, Copy, Debug)]
struct Track {
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

/// Runs `f` on the checked allocations of the current execution, and panics with the report of a
/// violation. Does nothing outside an execution, or if `track` is from another execution. `f` gets
/// whether to check for data races: not while the execution is torn down, because teardown drops
/// the futures of unfinished tasks without anything that orders the drops after other accesses.
#[track_caller]
fn with_state<R>(
    track: Option<Track>,
    f: impl FnOnce(&mut TreeBorrowsState, TaskId, &mut VectorClock, Track, bool) -> Result<R, String>,
) -> Option<R> {
    let track = track?;
    let result = ExecutionState::try_with(|state| {
        let races = !state.in_cleanup();
        let (task, clock, tb) = state.tree_borrows()?;
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
fn release(track: Option<Track>) {
    let _ = with_state(track, |tb, _, _, track, _| {
        tb.release(track.prov);
        Ok(())
    });
}

#[track_caller]
fn access<T>(track: Option<Track>, kind: AccessKind, at: &'static Location<'static>) {
    access_with::<T>(track, kind, true, at);
}

/// An access that is checked for a data race only if `races` is true.
#[track_caller]
fn access_with<T>(track: Option<Track>, kind: AccessKind, races: bool, at: &'static Location<'static>) {
    if size_of::<T>() == 0 {
        return;
    }
    with_state(track, |tb, task, clock, track, check| {
        tb.access(task, clock, track.prov, track.range::<T>(), kind, races && check, at)
    });
}

#[track_caller]
fn retag<T>(track: Option<Track>, kind: RefKind, at: &'static Location<'static>) -> Option<Track> {
    if size_of::<T>() == 0 {
        return None;
    }
    with_state(track, |tb, task, clock, track, races| {
        let prov = tb.retag(task, clock, track.prov, track.range::<T>(), kind, races, at)?;
        Ok(Track { prov, ..track })
    })
}

/// A checked version of [`std::cell::UnsafeCell`]. See the [module documentation](self).
pub struct UnsafeCell<T> {
    /// The execution in which the cell was last registered, and its allocation there.
    alloc: std::cell::Cell<(u64, usize)>,
    created_at: &'static Location<'static>,
    value: std::cell::UnsafeCell<T>,
}

impl<T> UnsafeCell<T> {
    /// Makes a new cell holding `value`. In an execution, this counts as a write of the value by
    /// the current task, as in Miri, so an access that doesn't happen after it is a data race.
    #[track_caller]
    pub fn new(value: T) -> Self {
        let cell = Self {
            alloc: std::cell::Cell::new((0, 0)),
            created_at: Location::caller(),
            value: std::cell::UnsafeCell::new(value),
        };
        if size_of::<T>() != 0 {
            let _ = ExecutionState::try_with(|state| {
                if let Some((task, clock, tb)) = state.tree_borrows() {
                    let alloc = tb.new_alloc(size_of::<T>(), std::any::type_name::<T>(), cell.created_at, task);
                    tb.record_init(alloc, task, clock, cell.created_at);
                    cell.alloc.set((tb.execution, alloc));
                }
            });
        }
        cell
    }

    /// The cell's allocation in the current execution. The cell is registered at its first use in
    /// an execution, so a cell that outlives one execution starts over in the next.
    fn track(&self, prov: fn(usize) -> Prov) -> Option<Track> {
        if size_of::<T>() == 0 {
            return None;
        }
        ExecutionState::try_with(|state| {
            let (task, _, tb) = state.tree_borrows()?;
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
                prov: prov(alloc),
                offset: 0,
            })
        })
        .ok()
        .flatten()
    }

    /// A raw pointer to the value, as [`std::cell::UnsafeCell::get`] returns.
    pub fn get(&self) -> Ptr<T> {
        Ptr {
            raw: self.value.get(),
            track: self.track(Prov::cell),
        }
    }

    /// Calls `f` with a raw pointer to the value, after a check of a read through it. This is
    /// loom's `UnsafeCell::with`.
    #[track_caller]
    pub fn with<R>(&self, f: impl FnOnce(*const T) -> R) -> R {
        access::<T>(self.track(Prov::cell), AccessKind::Read, Location::caller());
        f(self.value.get())
    }

    /// Calls `f` with a raw pointer to the value, after a check of a write through it. This is
    /// loom's `UnsafeCell::with_mut`.
    #[track_caller]
    pub fn with_mut<R>(&self, f: impl FnOnce(*mut T) -> R) -> R {
        access::<T>(self.track(Prov::cell), AccessKind::Write, Location::caller());
        f(self.value.get())
    }

    /// A mutable reference to the value. This counts as a write through the cell itself, which
    /// disables every reference made from the cell before. It isn't checked for a data race: the
    /// caller has the cell to itself, which a hand-off that Shuttle doesn't see may have arranged,
    /// such as the last drop of a std `Arc`. The write is recorded, so a later access by another
    /// task must still happen after it.
    #[track_caller]
    pub fn get_mut(&mut self) -> &mut T {
        access_with::<T>(self.track(Prov::root), AccessKind::Write, false, Location::caller());
        self.value.get_mut()
    }

    /// Unwraps the value. This counts as a read through the cell itself. As with `get_mut`, it isn't
    /// checked for a data race, but it is recorded.
    #[track_caller]
    pub fn into_inner(self) -> T {
        access_with::<T>(self.track(Prov::root), AccessKind::Read, false, Location::caller());
        self.free();
        let this = ManuallyDrop::new(self);
        // SAFETY: `this` is never used or dropped again.
        unsafe { std::ptr::read(&this.value) }.into_inner()
    }

    fn free(&self) {
        let (execution, alloc) = self.alloc.get();
        let _ = ExecutionState::try_with(|state| {
            if let Some((task, _, tb)) = state.tree_borrows() {
                if tb.execution == execution {
                    tb.free(alloc, task);
                }
            }
        });
    }
}

impl<T> Drop for UnsafeCell<T> {
    fn drop(&mut self) {
        self.free();
    }
}

impl<T: Default> Default for UnsafeCell<T> {
    #[track_caller]
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> fmt::Debug for UnsafeCell<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnsafeCell").finish_non_exhaustive()
    }
}

/// A raw pointer into a checked [`UnsafeCell`], which carries its tag. It is `Copy` and neither
/// `Send` nor `Sync`, as a raw pointer is.
pub struct Ptr<T> {
    raw: *mut T,
    track: Option<Track>,
}

impl<T> Clone for Ptr<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Ptr<T> {}

impl<T> fmt::Debug for Ptr<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.track {
            Some(track) => write!(f, "Ptr({:?}, tag #{})", self.raw, track.prov.tag),
            None => write!(f, "Ptr({:?})", self.raw),
        }
    }
}

impl<T> Ptr<T> {
    /// The plain raw pointer. Accesses through it aren't checked.
    pub fn as_raw(self) -> *mut T {
        self.raw
    }

    /// The same pointer, to a `U`. It keeps its tag.
    pub fn cast<U>(self) -> Ptr<U> {
        Ptr {
            raw: self.raw.cast(),
            track: self.track,
        }
    }

    /// A pointer to a part of the value, such as a field, with the same tag. `f` gets the raw
    /// pointer and returns one into the same value, as `&raw mut (*p).field` does. That makes no
    /// reference, so nothing is checked until an access through the new pointer.
    ///
    /// ```
    /// # use shuttle_std::cell::UnsafeCell;
    /// struct Pair {
    ///     a: u32,
    ///     b: u64,
    /// }
    ///
    /// shuttle_schedulers::check_dfs(
    ///     || {
    ///         let cell = UnsafeCell::new(Pair { a: 1, b: 2 });
    ///         let b = cell.get().project(|p| unsafe { &raw mut (*p).b });
    ///         unsafe { b.write(3) };
    ///         assert_eq!(unsafe { b.read() }, 3);
    ///     },
    ///     None,
    /// );
    /// ```
    pub fn project<U>(self, f: impl FnOnce(*mut T) -> *mut U) -> Ptr<U> {
        let raw = f(self.raw);
        let offset = raw.addr().wrapping_sub(self.raw.addr());
        Ptr {
            raw,
            track: self.track.map(|track| Track {
                offset: track.offset.wrapping_add(offset),
                ..track
            }),
        }
    }

    /// The pointer `count` values of `T` further on, as [`pointer::add`] gives.
    ///
    /// # Safety
    ///
    /// As for [`pointer::add`].
    ///
    /// [`pointer::add`]: https://doc.rust-lang.org/std/primitive.pointer.html#method.add
    pub unsafe fn add(self, count: usize) -> Self {
        self.byte_add(count * size_of::<T>())
    }

    /// The pointer `count` bytes further on, as [`pointer::byte_add`] gives.
    ///
    /// # Safety
    ///
    /// As for [`pointer::byte_add`].
    ///
    /// [`pointer::byte_add`]: https://doc.rust-lang.org/std/primitive.pointer.html#method.byte_add
    pub unsafe fn byte_add(self, count: usize) -> Self {
        Ptr {
            // SAFETY: the caller's.
            raw: unsafe { self.raw.byte_add(count) },
            track: self.track.map(|track| Track {
                offset: track.offset + count,
                ..track
            }),
        }
    }

    /// Reads the value, as [`pointer::read`] does.
    ///
    /// # Safety
    ///
    /// As for [`pointer::read`].
    ///
    /// [`pointer::read`]: https://doc.rust-lang.org/std/primitive.pointer.html#method.read
    #[track_caller]
    pub unsafe fn read(self) -> T {
        access::<T>(self.track, AccessKind::Read, Location::caller());
        // SAFETY: the caller's.
        unsafe { self.raw.read() }
    }

    /// Writes the value, as [`pointer::write`] does.
    ///
    /// # Safety
    ///
    /// As for [`pointer::write`].
    ///
    /// [`pointer::write`]: https://doc.rust-lang.org/std/primitive.pointer.html#method.write
    #[track_caller]
    pub unsafe fn write(self, value: T) {
        access::<T>(self.track, AccessKind::Write, Location::caller());
        // SAFETY: the caller's.
        unsafe { self.raw.write(value) }
    }

    /// Replaces the value, as [`pointer::replace`] does.
    ///
    /// # Safety
    ///
    /// As for [`pointer::replace`].
    ///
    /// [`pointer::replace`]: https://doc.rust-lang.org/std/primitive.pointer.html#method.replace
    #[track_caller]
    pub unsafe fn replace(self, value: T) -> T {
        access::<T>(self.track, AccessKind::Write, Location::caller());
        // SAFETY: the caller's.
        unsafe { self.raw.replace(value) }
    }

    /// Makes a shared reference, as `&*ptr` does. It gets a new tag, which starts `Frozen`.
    ///
    /// # Safety
    ///
    /// As for `&*ptr`, except that Shuttle reports a violation of Tree Borrows or a data race.
    #[track_caller]
    pub unsafe fn as_ref<'a>(self) -> Ref<'a, T> {
        Ref {
            raw: self.raw,
            track: retag::<T>(self.track, RefKind::Shared, Location::caller()),
            raw_made: std::cell::Cell::new(false),
            _marker: PhantomData,
        }
    }

    /// Makes a mutable reference, as `&mut *ptr` does. It gets a new tag, which starts `Reserved`.
    ///
    /// # Safety
    ///
    /// As for `&mut *ptr`, except that Shuttle reports a violation of Tree Borrows or a data race.
    #[track_caller]
    pub unsafe fn as_mut<'a>(self) -> RefMut<'a, T> {
        RefMut {
            raw: self.raw,
            track: retag::<T>(self.track, RefKind::Mut, Location::caller()),
            raw_made: false,
            _marker: PhantomData,
        }
    }
}

/// A shared reference into a checked [`UnsafeCell`]. Each dereference is checked as a read.
pub struct Ref<'a, T> {
    raw: *const T,
    track: Option<Track>,
    raw_made: std::cell::Cell<bool>,
    _marker: PhantomData<&'a T>,
}

impl<T> Ref<'_, T> {
    /// A raw pointer with this reference's tag, as `r as *const T` gives. Writing through it is
    /// UB, as it is for the raw pointer.
    pub fn as_ptr(this: &Self) -> Ptr<T> {
        this.raw_made.set(true);
        Ptr {
            raw: this.raw.cast_mut(),
            track: this.track,
        }
    }
}

impl<T> Deref for Ref<'_, T> {
    type Target = T;

    #[track_caller]
    fn deref(&self) -> &T {
        access::<T>(self.track, AccessKind::Read, Location::caller());
        // SAFETY: `Ptr::as_ref`'s caller promised that this is a valid reference.
        unsafe { &*self.raw }
    }
}

impl<T> Drop for Ref<'_, T> {
    fn drop(&mut self) {
        if !self.raw_made.get() {
            release(self.track);
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for Ref<'_, T> {
    #[track_caller]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

/// A mutable reference into a checked [`UnsafeCell`]. Each `Deref` is checked as a read, and each
/// `DerefMut` as a write.
pub struct RefMut<'a, T> {
    raw: *mut T,
    track: Option<Track>,
    raw_made: bool,
    _marker: PhantomData<&'a mut T>,
}

impl<T> RefMut<'_, T> {
    /// A raw pointer with this reference's tag, as `r as *mut T` gives.
    pub fn as_ptr(this: &mut Self) -> Ptr<T> {
        this.raw_made = true;
        Ptr {
            raw: this.raw,
            track: this.track,
        }
    }
}

impl<T> Deref for RefMut<'_, T> {
    type Target = T;

    #[track_caller]
    fn deref(&self) -> &T {
        access::<T>(self.track, AccessKind::Read, Location::caller());
        // SAFETY: `Ptr::as_mut`'s caller promised that this is a valid reference.
        unsafe { &*self.raw }
    }
}

impl<T> DerefMut for RefMut<'_, T> {
    #[track_caller]
    fn deref_mut(&mut self) -> &mut T {
        access::<T>(self.track, AccessKind::Write, Location::caller());
        // SAFETY: `Ptr::as_mut`'s caller promised that this is a valid reference.
        unsafe { &mut *self.raw }
    }
}

impl<T> Drop for RefMut<'_, T> {
    fn drop(&mut self) {
        if !self.raw_made {
            release(self.track);
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for RefMut<'_, T> {
    #[track_caller]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}
