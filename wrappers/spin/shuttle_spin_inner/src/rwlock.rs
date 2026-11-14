//! A lock that provides data access to either one writer or many readers.
//!
//! Under Shuttle the lock word is spin's: a writer bit, an upgradeable bit, and the number of
//! readers above them. Every operation runs spin's operation on that word, so an upgradeable guard
//! keeps new readers out, as in spin, and a task that cannot get the lock blocks until the word
//! changes instead of spinning.

use crate::state::{Core, State, lock_data, release_after_switch, retake_data};
use crate::value::{Access, Copy, CopySlot, copied};
use crate::{RelaxStrategy, Spin};
use shuttle_engine::runtime::per_execution::ValueLock;
use shuttle_engine::runtime::thread;
use std::fmt;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool as StdAtomicBool, AtomicUsize as StdAtomicUsize, Ordering as StdOrdering};

/// spin's lock around the value (see the `value` module). Its relax strategy is never used: a task
/// takes it only once it holds the lock in the execution, so it is free unless something outside
/// of the execution holds it. A writer holds it for writing, and a reader or an upgradeable reader
/// for reading.
type Inner<T> = spin_orig::rwlock::RwLock<T, spin_orig::relax::Loop>;
type InnerRead<'a, T> = spin_orig::rwlock::RwLockReadGuard<'a, T>;
type InnerWrite<'a, T> = spin_orig::rwlock::RwLockWriteGuard<'a, T, spin_orig::relax::Loop>;

const READER: usize = 1 << 2;
const UPGRADED: usize = 1 << 1;
const WRITER: usize = 1;

/// A lock that provides data access to either one writer or many readers.
///
/// This lock behaves in a similar manner to its namesake `std::sync::RwLock` but uses spinning for
/// synchronisation instead. Unlike its namespace, this lock does not track lock poisoning.
///
/// This type of lock allows a number of readers or at most one writer at any point in time. The
/// write portion of this lock typically allows modification of the underlying data (exclusive
/// access) and the read portion of this lock typically allows for read-only access (shared
/// access).
///
/// The type parameter `T` represents the data that this lock protects. It is required that `T`
/// satisfies `Send` to be shared across tasks and `Sync` to allow concurrent access through
/// readers. The RAII guards returned from the locking methods implement `Deref` (and `DerefMut`
/// for the `write` methods) to allow access to the contained of the lock.
///
/// An [`RwLockUpgradableGuard`] can be upgraded to a writable guard
/// through the [`RwLockUpgradableGuard::upgrade`] and [`RwLockUpgradableGuard::try_upgrade`]
/// functions. Writable
/// or upgradeable guards can be downgraded through their respective `downgrade` functions.
///
/// # Under Shuttle
///
/// Every method that reads or changes the lock word takes one scheduling point: the `read`,
/// `write` and `upgradeable_read` families, `reader_count`, `writer_count`, the upgrades and
/// downgrades, the `force_*` methods and the release of a guard. A task that cannot get the lock
/// blocks until the word changes. As in spin, an upgradeable guard keeps new readers out, and a
/// failed attempt at one while a writer holds the lock does too, until the writer releases it.
/// `try_write_weak` and `try_upgrade_weak` never fail spuriously.
///
/// The lock's state lives in the running execution, so a `static` lock is unlocked at the start of
/// every execution. Its value is reset too if it needs no dropping (see the crate documentation);
/// a value that owns memory carries over. A leaked guard keeps the lock locked in later executions,
/// as it would with spin. Two executions can't hold the lock at once, even both to read: one that
/// takes it while a guard of another execution holds it panics, saying whose guard it is (see the
/// crate documentation).
pub struct RwLock<T: ?Sized, R = Spin> {
    phantom: PhantomData<R>,
    core: Core,
    leaks: Leaks,
    /// Keeps the lock to one execution at a time, which the state, being per execution, can't.
    /// Counts the guards of every kind; upgrades and downgrades keep their guard's.
    value: ValueLock,
    data: Inner<T>,
}

/// The name of the lock in the messages of [`ValueLock::acquire`].
const KIND: &str = "spin RwLock";

/// A lock's state in one execution: spin's word, and where the execution keeps its copy of the
/// value, if it is copied.
type LockState = State<CopySlot>;

/// What leaked guards hold. A leaked guard keeps holding the lock in every later execution, until
/// the matching `force_*` method releases it.
struct Leaks {
    readers: StdAtomicUsize,
    upgradeable: StdAtomicBool,
    writer: StdAtomicBool,
}

impl Leaks {
    const fn new() -> Self {
        Self {
            readers: StdAtomicUsize::new(0),
            upgradeable: StdAtomicBool::new(false),
            writer: StdAtomicBool::new(false),
        }
    }

    /// Counts one leaked reader fewer, if there is one.
    fn forget_reader(&self) {
        let mut readers = self.readers.load(StdOrdering::Acquire);
        while readers > 0 {
            match self
                .readers
                .compare_exchange(readers, readers - 1, StdOrdering::AcqRel, StdOrdering::Acquire)
            {
                Ok(_) => break,
                Err(current) => readers = current,
            }
        }
    }

    /// The word the lock starts an execution with.
    fn word(&self) -> usize {
        self.readers.load(StdOrdering::Acquire) * READER
            + if self.upgradeable.load(StdOrdering::Acquire) {
                UPGRADED
            } else {
                0
            }
            + if self.writer.load(StdOrdering::Acquire) {
                WRITER
            } else {
                0
            }
    }
}

/// A guard that provides immutable data access.
///
/// When the guard falls out of scope it will decrement the read count, potentially releasing the
/// lock.
pub struct RwLockReadGuard<'a, T: 'a + ?Sized> {
    access: Access<'a, T, InnerRead<'a, T>>,
    core: &'a Core,
    leaks: &'a Leaks,
}

/// A guard that provides mutable data access.
///
/// When the guard falls out of scope it will release the lock.
pub struct RwLockWriteGuard<'a, T: 'a + ?Sized, R = Spin> {
    phantom: PhantomData<R>,
    access: Access<'a, T, InnerWrite<'a, T>>,
    inner: &'a RwLock<T, R>,
}

/// A guard that provides immutable data access but can be upgraded to [`RwLockWriteGuard`].
///
/// No writers or other upgradeable guards can exist while this is in scope. New reader creation is
/// prevented (to alleviate writer starvation) but there may be existing readers when the lock is
/// acquired.
///
/// When the guard falls out of scope it will release the lock.
pub struct RwLockUpgradableGuard<'a, T: 'a + ?Sized, R = Spin> {
    phantom: PhantomData<R>,
    access: Access<'a, T, InnerRead<'a, T>>,
    inner: &'a RwLock<T, R>,
}

// Same unsafe impls as `std::sync::RwLock`
unsafe impl<T: ?Sized + Send, R> Send for RwLock<T, R> {}
unsafe impl<T: ?Sized + Send + Sync, R> Sync for RwLock<T, R> {}

unsafe impl<T: ?Sized + Send + Sync, R> Send for RwLockWriteGuard<'_, T, R> {}
unsafe impl<T: ?Sized + Send + Sync, R> Sync for RwLockWriteGuard<'_, T, R> {}

unsafe impl<T: ?Sized + Sync> Send for RwLockReadGuard<'_, T> {}
unsafe impl<T: ?Sized + Sync> Sync for RwLockReadGuard<'_, T> {}

unsafe impl<T: ?Sized + Send + Sync, R> Send for RwLockUpgradableGuard<'_, T, R> {}
unsafe impl<T: ?Sized + Send + Sync, R> Sync for RwLockUpgradableGuard<'_, T, R> {}

impl<T, R> RwLock<T, R> {
    /// Creates a new spinlock wrapping the supplied data.
    #[inline]
    pub const fn new(data: T) -> Self {
        RwLock {
            phantom: PhantomData,
            core: Core::new(),
            leaks: Leaks::new(),
            value: ValueLock::new(),
            data: Inner::new(data),
        }
    }

    /// Consumes this `RwLock`, returning the underlying data.
    #[inline]
    pub fn into_inner(self) -> T {
        if let Some(copy) = self.copy() {
            // Safety: the copy is a valid value, which needs no dropping, so reading it out leaves
            // nothing behind to drop twice.
            return unsafe { copy.ptr::<T>().as_ptr().read() };
        }
        self.data.into_inner()
    }

    /// Returns a mutable pointer to the underying data.
    ///
    /// This is mostly meant to be used for applications which require manual unlocking, but where
    /// storing both the lock and the pointer to the inner data gets inefficient.
    #[inline(always)]
    pub fn as_mut_ptr(&self) -> *mut T {
        // Safety: nothing writes the value while an execution can reach it (see `CopySlot`), so the
        // copy can be made from it without spin's lock.
        if copied::<T>()
            && let Some(state) = self.try_enter()
            && let Some(copy) = state.extra.get_or_copy(unsafe { &*self.data.as_mut_ptr() })
        {
            return copy.ptr::<T>().as_ptr();
        }
        self.data.as_mut_ptr()
    }
}

impl<T: ?Sized, R: RelaxStrategy> RwLock<T, R> {
    /// Locks this rwlock with shared read access, blocking the current task until it can be
    /// acquired.
    ///
    /// The calling task will be blocked until there are no more writers which hold the lock. There
    /// may be other readers currently inside the lock when this method returns. This method does
    /// not provide any guarantees with respect to the ordering of whether contentious readers or
    /// writers will acquire the lock first.
    ///
    /// Returns an RAII guard which will release this thread's shared access once it is dropped.
    #[inline]
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        thread::switch();
        let state = self.enter();
        loop {
            if let Some(guard) = self.acquire_read(state) {
                return guard;
            }
            state.wait();
        }
    }

    /// Lock this rwlock with exclusive write access, blocking the current task until it can be
    /// acquired.
    ///
    /// This function will not return while other writers or other readers currently have access
    /// to the lock.
    ///
    /// Returns an RAII guard which will drop the write access of this rwlock when dropped.
    #[inline]
    pub fn write(&self) -> RwLockWriteGuard<'_, T, R> {
        thread::switch();
        let state = self.enter();
        loop {
            if let Some(guard) = self.acquire_write(state) {
                return guard;
            }
            state.wait();
        }
    }

    /// Obtain a readable lock guard that can later be upgraded to a writable lock guard. Upgrades
    /// can be done through the [`RwLockUpgradableGuard::upgrade`](RwLockUpgradableGuard::upgrade)
    /// method.
    #[inline]
    pub fn upgradeable_read(&self) -> RwLockUpgradableGuard<'_, T, R> {
        thread::switch();
        let state = self.enter();
        loop {
            if let Some(guard) = self.acquire_upgradeable(state) {
                return guard;
            }
            state.wait();
        }
    }
}

impl<T: ?Sized, R> RwLock<T, R> {
    fn initial_state(&self) -> LockState {
        // Leaked guards hold the lock in every execution.
        State::new(self.leaks.word(), CopySlot::default())
    }

    /// This execution's state of the lock.
    fn enter(&self) -> &LockState {
        self.core.state(|| self.initial_state()).0
    }

    /// Like [`Self::enter`], but `None` outside of an execution.
    fn try_enter(&self) -> Option<&LockState> {
        Some(self.core.try_state(|| self.initial_state())?.0)
    }

    /// This execution's copy of the value, if the value is copied and there is an execution.
    fn copy(&self) -> Option<&Copy> {
        if !copied::<T>() {
            return None;
        }
        self.try_enter()?.extra.get()
    }

    /// spin's `try_read` on the word. One step: the caller has taken the scheduling point.
    fn acquire_read(&self, state: &LockState) -> Option<RwLockReadGuard<'_, T>> {
        // An arbitrary cap that allows us to catch overflows long before they happen
        const MAX_READERS: usize = usize::MAX / READER / 2;

        state.observe();
        let word = state.word.get();
        if word > MAX_READERS * READER {
            panic!("Too many lock readers, cannot safely proceed");
        }
        // We check the UPGRADED bit here so that new readers are prevented when an UPGRADED lock is
        // held. This helps reduce writer starvation.
        if word & (WRITER | UPGRADED) != 0 {
            return None;
        }
        state.word.set(word + READER);
        state.publish();
        let guard = lock_data(&self.value, KIND, || self.data.try_read());
        let copy = state.extra.get_or_copy(&*guard);
        Some(RwLockReadGuard {
            access: Access::new(guard, copy, &self.value),
            core: &self.core,
            leaks: &self.leaks,
        })
    }

    /// spin's `try_write` on the word.
    fn acquire_write(&self, state: &LockState) -> Option<RwLockWriteGuard<'_, T, R>> {
        state.observe();
        if state.word.get() != 0 {
            return None;
        }
        state.word.set(WRITER);
        state.publish();
        let guard = lock_data(&self.value, KIND, || self.data.try_write());
        let copy = state.extra.get_or_copy(&*guard);
        Some(RwLockWriteGuard {
            phantom: PhantomData,
            access: Access::new(guard, copy, &self.value),
            inner: self,
        })
    }

    /// spin's `try_upgradeable_read` on the word: it sets the upgradeable bit even when it fails,
    /// and the writer or upgradeable guard that made it fail clears it when it is released.
    fn acquire_upgradeable(&self, state: &LockState) -> Option<RwLockUpgradableGuard<'_, T, R>> {
        state.observe();
        let word = state.word.get();
        if word & UPGRADED == 0 {
            state.word.set(word | UPGRADED);
            state.publish();
        }
        if word & (WRITER | UPGRADED) != 0 {
            // We can't unflip the UPGRADED bit back just yet as there is another upgradeable or
            // write lock. When they unlock, they will clear the bit.
            return None;
        }
        let guard = lock_data(&self.value, KIND, || self.data.try_read());
        let copy = state.extra.get_or_copy(&*guard);
        Some(RwLockUpgradableGuard {
            phantom: PhantomData,
            access: Access::new(guard, copy, &self.value),
            inner: self,
        })
    }

    /// Attempt to acquire this lock with shared read access.
    ///
    /// This function will never block, and it will return immediately if `read` would otherwise
    /// succeed. Returns `Some` of an RAII guard which will release the shared access of this
    /// thread when dropped, or `None` if the access could not be granted. This method does not
    /// provide any guarantees with respect to the ordering of whether contentious readers or
    /// writers will acquire the lock first.
    #[inline]
    pub fn try_read(&self) -> Option<RwLockReadGuard<'_, T>> {
        thread::switch();
        let state = self.enter();
        self.acquire_read(state)
    }

    /// Return the number of readers that currently hold the lock (including upgradable readers).
    ///
    /// # Safety
    ///
    /// This function provides no synchronization guarantees and so its result should be considered
    /// 'out of date' the instant it is called. Do not use it for synchronization purposes. However,
    /// it may be useful as a heuristic.
    ///
    /// Under Shuttle this takes a scheduling point, as the load of an atomic does.
    pub fn reader_count(&self) -> usize {
        thread::switch();
        let state = self.enter();
        state.observe();
        let word = state.word.get();
        word / READER + (word & UPGRADED) / UPGRADED
    }

    /// Return the number of writers that currently hold the lock.
    ///
    /// Because [`RwLock`] guarantees exclusive mutable access, this function may only return either
    /// `0` or `1`.
    ///
    /// # Safety
    ///
    /// This function provides no synchronization guarantees and so its result should be considered
    /// 'out of date' the instant it is called. Do not use it for synchronization purposes. However,
    /// it may be useful as a heuristic.
    ///
    /// Under Shuttle this takes a scheduling point, as the load of an atomic does.
    pub fn writer_count(&self) -> usize {
        thread::switch();
        let state = self.enter();
        state.observe();
        (state.word.get() & WRITER) / WRITER
    }

    /// Force decrement the reader count.
    ///
    /// # Safety
    ///
    /// This is *extremely* unsafe if there are outstanding `RwLockReadGuard`s live, or if called
    /// more times than `read` has been called, but can be useful in FFI contexts where the caller
    /// doesn't know how to deal with RAII. The underlying atomic operation uses `Ordering::Release`.
    #[inline]
    pub unsafe fn force_read_decrement(&self) {
        release_after_switch(|| {
            // Enter first: if this is the first use of the lock in the execution, the state starts
            // out with the leaked readers, one of which this releases.
            let state = self.try_enter();
            self.leaks.forget_reader();
            // Safety: the caller's.
            unsafe { self.data.force_read_decrement() };
            // The leaked or forgotten guard's hold on the value.
            self.value.release();
            if let Some(state) = state {
                debug_assert!(state.word.get() & !WRITER > 0);
                state.release(|state| state.word.set(state.word.get() - READER));
            }
        });
    }

    /// Force unlock exclusive write access.
    ///
    /// # Safety
    ///
    /// This is *extremely* unsafe if there are outstanding `RwLockWriteGuard`s live, or if called
    /// when there are current readers, but can be useful in FFI contexts where the caller doesn't
    /// know how to deal with RAII. The underlying atomic operation uses `Ordering::Release`.
    #[inline]
    pub unsafe fn force_write_unlock(&self) {
        release_after_switch(|| {
            // Enter first, as `force_read_decrement` does. Outside of an execution, the leaks are
            // all there is to go by.
            let state = self.try_enter();
            let word = state.map_or_else(|| self.leaks.word(), |state| state.word.get());
            debug_assert_eq!(word & !(WRITER | UPGRADED), 0);
            self.leaks.writer.store(false, StdOrdering::Release);
            self.leaks.upgradeable.store(false, StdOrdering::Release);
            // A writer holds `data` for writing, and an upgradeable reader for reading. The
            // upgradeable bit that a failed attempt leaves only ever comes with the writer bit.
            if word & WRITER != 0 {
                // Safety: the caller's.
                unsafe { self.data.force_write_unlock() };
            } else if word & UPGRADED != 0 {
                // Safety: as above.
                unsafe { self.data.force_read_decrement() };
            }
            if word & (WRITER | UPGRADED) != 0 {
                // The leaked or forgotten guard's hold on the value.
                self.value.release();
            }
            if let Some(state) = state {
                state.release(|state| state.word.set(state.word.get() & !(WRITER | UPGRADED)));
            }
        });
    }

    /// Attempt to lock this rwlock with exclusive write access.
    ///
    /// This function does not ever block, and it will return `None` if a call to `write` would
    /// otherwise block. If successful, an RAII guard is returned.
    #[inline]
    pub fn try_write(&self) -> Option<RwLockWriteGuard<'_, T, R>> {
        thread::switch();
        let state = self.enter();
        self.acquire_write(state)
    }

    /// Attempt to lock this rwlock with exclusive write access.
    ///
    /// Unlike [`RwLock::try_write`], this function is allowed to spuriously fail even when locking
    /// would otherwise succeed, which can result in more efficient code on some platforms. Under
    /// Shuttle it never fails spuriously.
    #[inline]
    pub fn try_write_weak(&self) -> Option<RwLockWriteGuard<'_, T, R>> {
        self.try_write()
    }

    /// Tries to obtain an upgradeable lock guard.
    #[inline]
    pub fn try_upgradeable_read(&self) -> Option<RwLockUpgradableGuard<'_, T, R>> {
        thread::switch();
        let state = self.enter();
        self.acquire_upgradeable(state)
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the `RwLock` mutably, no actual locking needs to take place -- the
    /// mutable borrow statically guarantees no locks exist.
    pub fn get_mut(&mut self) -> &mut T {
        if copied::<T>() {
            let value: *const T = self.data.get_mut();
            if let Some(state) = self.try_enter() {
                // Safety: `value` is the exclusive borrow's, only read to make the copy.
                if let Some(copy) = state.extra.get_or_copy(unsafe { &*value }) {
                    // Safety: the `&mut` borrow rules out any guard, and so any other reference to
                    // the copy.
                    return unsafe { &mut *copy.ptr::<T>().as_ptr() };
                }
            }
        }
        self.data.get_mut()
    }

    /// Releases an upgradeable lock whose guard was forgotten, for `lock_api`.
    #[cfg(feature = "lock_api")]
    unsafe fn force_upgradeable_unlock(&self) {
        release_after_switch(|| {
            // Safety: the caller holds an upgradeable lock, which holds `data` for reading.
            unsafe { self.data.force_read_decrement() };
            // The forgotten guard's hold on the value.
            self.value.release();
            if let Some(state) = self.try_enter() {
                state.release(|state| state.word.set(state.word.get() - UPGRADED));
            }
        });
    }

    /// Upgrades an upgradeable lock whose guard was forgotten, for `lock_api`, and forgets the
    /// write guard.
    #[cfg(feature = "lock_api")]
    unsafe fn upgrade_forgotten(&self, block: bool) -> bool {
        thread::switch();
        let state = self.enter();
        loop {
            state.observe();
            if state.word.get() == UPGRADED {
                state.word.set(WRITER);
                state.publish();
                // Safety: the caller holds an upgradeable lock, which holds `data` for reading.
                unsafe { self.data.force_read_decrement() };
                // The forgotten guard keeps its hold on the value.
                std::mem::forget(retake_data(|| self.data.try_write()));
                return true;
            }
            if !block {
                return false;
            }
            state.wait();
        }
    }
}

impl<T: ?Sized + fmt::Debug, R> fmt::Debug for RwLock<T, R> {
    /// Formats the value unless a writer or an upgradeable reader holds the lock, as spin's does,
    /// without taking a scheduling point.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fn fmt_rwlock<T: ?Sized + fmt::Debug>(value: Option<&T>, f: &mut fmt::Formatter) -> fmt::Result {
            match value {
                Some(value) => write!(f, "RwLock {{ data: ")
                    .and_then(|()| value.fmt(f))
                    .and_then(|()| write!(f, " }}")),
                None => write!(f, "RwLock {{ <locked> }}"),
            }
        }

        if let Some(state) = self.try_enter() {
            if state.word.get() & (WRITER | UPGRADED) != 0 {
                return fmt_rwlock::<T>(None, f);
            }
            if let Some(copy) = state.extra.get() {
                // Safety: no writer holds the lock, so nothing changes the copy while it is
                // formatted.
                return fmt_rwlock(Some(unsafe { copy.ptr::<T>().as_ref() }), f);
            }
        }
        // spin's lock around the value is held for writing only by a writer of this execution, or
        // from outside of it.
        let guard = self.data.try_read();
        fmt_rwlock(guard.as_deref(), f)
    }
}

// spin writes `T: ?Sized + Default`, which means the same, since `Default` implies `Sized`.
impl<T: Default, R> Default for RwLock<T, R> {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl<T, R> From<T> for RwLock<T, R> {
    fn from(data: T) -> Self {
        Self::new(data)
    }
}

impl<'rwlock, T: ?Sized> RwLockReadGuard<'rwlock, T> {
    /// Leak the lock guard, yielding a reference to the underlying data.
    ///
    /// Note that this function will permanently lock the original lock for all but reading
    /// locks, also in later Shuttle executions.
    #[inline]
    pub fn leak(this: Self) -> &'rwlock T {
        let mut this = ManuallyDrop::new(this);
        this.leaks.readers.fetch_add(1, StdOrdering::AcqRel);
        let copy = this
            .core
            .existing_state::<CopySlot>()
            .and_then(|state| state.extra.get());
        // Safety: `this` is never used or dropped again.
        unsafe { leak_read(&mut this.access, copy) }
    }
}

/// Leaks a reading guard's access: spin's lock stays locked for reading, and the execution's copy,
/// if the guard reaches one, outlives the execution.
///
/// # Safety
///
/// Nothing uses the guard that `access` belongs to again.
unsafe fn leak_read<'a, T: ?Sized>(access: &mut Access<'a, T, InnerRead<'a, T>>, copy: Option<&Copy>) -> &'a T {
    // Safety: the caller's.
    match unsafe { access.leak() } {
        Ok(value) => {
            copy.expect("a guard of a copy belongs to an execution with that copy")
                .leak();
            // Safety: the copy outlives the execution now, and the lock stays locked for all but
            // readers.
            unsafe { &*value.as_ptr() }
        }
        Err(guard) => InnerRead::leak(guard),
    }
}

impl<'rwlock, T: ?Sized + fmt::Debug> fmt::Debug for RwLockReadGuard<'rwlock, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<'rwlock, T: ?Sized + fmt::Display> fmt::Display for RwLockReadGuard<'rwlock, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

impl<'rwlock, T: ?Sized, R: RelaxStrategy> RwLockUpgradableGuard<'rwlock, T, R> {
    /// Upgrades an upgradeable lock guard to a writable lock guard.
    #[inline]
    pub fn upgrade(self) -> RwLockWriteGuard<'rwlock, T, R> {
        thread::switch();
        let state = self.inner.enter();
        let mut this = self;
        loop {
            this = match this.upgrade_now(state) {
                Ok(guard) => return guard,
                Err(this) => this,
            };
            state.wait();
        }
    }
}

impl<'rwlock, T: ?Sized, R> RwLockUpgradableGuard<'rwlock, T, R> {
    /// spin's upgrade on the word: from just the upgradeable bit to just the writer bit, so it
    /// waits for the readers that are in the lock.
    fn upgrade_now(self, state: &LockState) -> Result<RwLockWriteGuard<'rwlock, T, R>, Self> {
        state.observe();
        if state.word.get() != UPGRADED {
            return Err(self);
        }
        state.word.set(WRITER);
        state.publish();
        let mut this = ManuallyDrop::new(self);
        let inner = this.inner;
        // Safety: `this` is never used or dropped again. No other task of the execution holds
        // spin's lock once the word has just the writer bit, so it can be taken for writing.
        let access = unsafe {
            this.access.convert(|read| {
                drop(read);
                retake_data(|| inner.data.try_write())
            })
        };
        Ok(RwLockWriteGuard {
            phantom: PhantomData,
            access,
            inner,
        })
    }

    /// Tries to upgrade an upgradeable lock guard to a writable lock guard.
    #[inline]
    pub fn try_upgrade(self) -> Result<RwLockWriteGuard<'rwlock, T, R>, Self> {
        thread::switch();
        let state = self.inner.enter();
        self.upgrade_now(state)
    }

    /// Tries to upgrade an upgradeable lock guard to a writable lock guard.
    ///
    /// Unlike [`RwLockUpgradableGuard::try_upgrade`], this function is allowed to spuriously fail
    /// even when upgrading would otherwise succeed, which can result in more efficient code on some
    /// platforms. Under Shuttle it never fails spuriously.
    #[inline]
    pub fn try_upgrade_weak(self) -> Result<RwLockWriteGuard<'rwlock, T, R>, Self> {
        self.try_upgrade()
    }

    /// Downgrades the upgradeable lock guard to a readable, shared lock guard. Cannot fail and is
    /// guaranteed not to spin.
    #[inline]
    pub fn downgrade(self) -> RwLockReadGuard<'rwlock, T> {
        // If execution teardown unwinds the task from this scheduling point, `self` releases the
        // lock as it is dropped.
        thread::switch();
        let state = self.inner.enter();
        state.observe();
        // spin takes a read lock and then drops the upgradeable one. In between, the word only
        // refuses more than either end, so one step does the same.
        state.release(|state| state.word.set(state.word.get() + READER - UPGRADED));
        let mut this = ManuallyDrop::new(self);
        let inner = this.inner;
        RwLockReadGuard {
            // Safety: `this` is never used or dropped again. spin's read lock carries over.
            access: unsafe { this.access.convert(|read| read) },
            core: &inner.core,
            leaks: &inner.leaks,
        }
    }

    /// Leak the lock guard, yielding a reference to the underlying data.
    ///
    /// Note that this function will permanently lock the original lock, also in later Shuttle
    /// executions.
    #[inline]
    pub fn leak(this: Self) -> &'rwlock T {
        let mut this = ManuallyDrop::new(this);
        this.inner.leaks.upgradeable.store(true, StdOrdering::Release);
        let copy = this
            .inner
            .core
            .existing_state::<CopySlot>()
            .and_then(|state| state.extra.get());
        // Safety: `this` is never used or dropped again.
        unsafe { leak_read(&mut this.access, copy) }
    }
}

impl<'rwlock, T: ?Sized + fmt::Debug, R> fmt::Debug for RwLockUpgradableGuard<'rwlock, T, R> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<'rwlock, T: ?Sized + fmt::Display, R> fmt::Display for RwLockUpgradableGuard<'rwlock, T, R> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

impl<'rwlock, T: ?Sized, R> RwLockWriteGuard<'rwlock, T, R> {
    /// Downgrades the writable lock guard to a readable, shared lock guard. Cannot fail and is
    /// guaranteed not to spin.
    #[inline]
    pub fn downgrade(self) -> RwLockReadGuard<'rwlock, T> {
        // If execution teardown unwinds the task from this scheduling point, `self` releases the
        // lock as it is dropped.
        thread::switch();
        let state = self.inner.enter();
        state.observe();
        // spin takes a read lock and then drops the write lock, which clears the writer and
        // upgradeable bits. One step does the same, as in `RwLockUpgradableGuard::downgrade`.
        state.release(|state| state.word.set((state.word.get() + READER) & !(WRITER | UPGRADED)));
        let mut this = ManuallyDrop::new(self);
        let inner = this.inner;
        RwLockReadGuard {
            // Safety: `this` is never used or dropped again.
            access: unsafe { this.access.convert(InnerWrite::downgrade) },
            core: &inner.core,
            leaks: &inner.leaks,
        }
    }

    /// Downgrades the writable lock guard to an upgradable, shared lock guard. Cannot fail and is
    /// guaranteed not to spin.
    #[inline]
    pub fn downgrade_to_upgradeable(self) -> RwLockUpgradableGuard<'rwlock, T, R> {
        thread::switch();
        let state = self.inner.enter();
        state.observe();
        debug_assert_eq!(state.word.get() & (WRITER | UPGRADED), WRITER);
        // Nothing that waits can go ahead now: the upgradeable bit keeps out what the writer bit
        // kept out. So there is nobody to wake.
        state.word.set(UPGRADED);
        state.publish();
        let mut this = ManuallyDrop::new(self);
        let inner = this.inner;
        RwLockUpgradableGuard {
            phantom: PhantomData,
            // Safety: `this` is never used or dropped again. spin's lock is held for reading, as
            // for any upgradeable reader.
            access: unsafe { this.access.convert(InnerWrite::downgrade) },
            inner,
        }
    }

    /// Leak the lock guard, yielding a mutable reference to the underlying data.
    ///
    /// Note that this function will permanently lock the original lock, also in later Shuttle
    /// executions.
    #[inline]
    pub fn leak(this: Self) -> &'rwlock mut T {
        let mut this = ManuallyDrop::new(this);
        this.inner.leaks.writer.store(true, StdOrdering::Release);
        let copy = this
            .inner
            .core
            .existing_state::<CopySlot>()
            .and_then(|state| state.extra.get());
        // Safety: `this` is never used or dropped again.
        match unsafe { this.access.leak() } {
            Ok(value) => {
                copy.expect("a guard of a copy belongs to an execution with that copy")
                    .leak();
                // Safety: the copy outlives the execution now, and the lock stays locked, so
                // nothing else reaches it.
                unsafe { &mut *value.as_ptr() }
            }
            Err(guard) => InnerWrite::leak(guard),
        }
    }
}

impl<'rwlock, T: ?Sized + fmt::Debug, R> fmt::Debug for RwLockWriteGuard<'rwlock, T, R> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<'rwlock, T: ?Sized + fmt::Display, R> fmt::Display for RwLockWriteGuard<'rwlock, T, R> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

impl<'rwlock, T: ?Sized> Deref for RwLockReadGuard<'rwlock, T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.access.get()
    }
}

impl<'rwlock, T: ?Sized, R> Deref for RwLockUpgradableGuard<'rwlock, T, R> {
    type Target = T;

    fn deref(&self) -> &T {
        self.access.get()
    }
}

impl<'rwlock, T: ?Sized, R> Deref for RwLockWriteGuard<'rwlock, T, R> {
    type Target = T;

    fn deref(&self) -> &T {
        self.access.get()
    }
}

impl<'rwlock, T: ?Sized, R> DerefMut for RwLockWriteGuard<'rwlock, T, R> {
    fn deref_mut(&mut self) -> &mut T {
        self.access.get_mut()
    }
}

impl<'rwlock, T: ?Sized> Drop for RwLockReadGuard<'rwlock, T> {
    /// A scheduling point.
    fn drop(&mut self) {
        let core = self.core;
        let access = &mut self.access;
        release_after_switch(|| {
            // Safety: the guard is being dropped, so nothing uses `access` again.
            unsafe { access.release() };
            if let Some(state) = core.existing_state::<CopySlot>() {
                debug_assert!(state.word.get() & !(WRITER | UPGRADED) > 0);
                state.release(|state| state.word.set(state.word.get() - READER));
            }
        });
    }
}

impl<'rwlock, T: ?Sized, R> Drop for RwLockUpgradableGuard<'rwlock, T, R> {
    /// A scheduling point.
    fn drop(&mut self) {
        let core = &self.inner.core;
        let access = &mut self.access;
        release_after_switch(|| {
            // Safety: the guard is being dropped, so nothing uses `access` again.
            unsafe { access.release() };
            if let Some(state) = core.existing_state::<CopySlot>() {
                debug_assert_eq!(state.word.get() & (WRITER | UPGRADED), UPGRADED);
                state.release(|state| state.word.set(state.word.get() - UPGRADED));
            }
        });
    }
}

impl<'rwlock, T: ?Sized, R> Drop for RwLockWriteGuard<'rwlock, T, R> {
    /// A scheduling point.
    fn drop(&mut self) {
        let core = &self.inner.core;
        let access = &mut self.access;
        release_after_switch(|| {
            // Safety: the guard is being dropped, so nothing uses `access` again.
            unsafe { access.release() };
            if let Some(state) = core.existing_state::<CopySlot>() {
                debug_assert_eq!(state.word.get() & WRITER, WRITER);
                // Writer is responsible for clearing both WRITER and UPGRADED bits. The UPGRADED
                // bit may be set if an upgradeable lock attempts an upgrade while this lock is held.
                state.release(|state| state.word.set(state.word.get() & !(WRITER | UPGRADED)));
            }
        });
    }
}

#[cfg(feature = "lock_api")]
unsafe impl<R: RelaxStrategy> lock_api_crate::RawRwLock for RwLock<(), R> {
    type GuardMarker = lock_api_crate::GuardSend;

    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: Self = Self::new(());

    #[inline(always)]
    fn lock_exclusive(&self) {
        // Prevent guard destructor running
        core::mem::forget(self.write());
    }

    #[inline(always)]
    fn try_lock_exclusive(&self) -> bool {
        // Prevent guard destructor running
        self.try_write().map(core::mem::forget).is_some()
    }

    #[inline(always)]
    unsafe fn unlock_exclusive(&self) {
        // Safety: the caller holds the write lock.
        unsafe { self.force_write_unlock() };
    }

    #[inline(always)]
    fn lock_shared(&self) {
        // Prevent guard destructor running
        core::mem::forget(self.read());
    }

    #[inline(always)]
    fn try_lock_shared(&self) -> bool {
        // Prevent guard destructor running
        self.try_read().map(core::mem::forget).is_some()
    }

    #[inline(always)]
    unsafe fn unlock_shared(&self) {
        // Safety: the caller holds a read lock.
        unsafe { self.force_read_decrement() };
    }

    #[inline(always)]
    fn is_locked(&self) -> bool {
        thread::switch();
        let state = self.enter();
        state.observe();
        state.word.get() != 0
    }
}

#[cfg(feature = "lock_api")]
unsafe impl<R: RelaxStrategy> lock_api_crate::RawRwLockUpgrade for RwLock<(), R> {
    #[inline(always)]
    fn lock_upgradable(&self) {
        // Prevent guard destructor running
        core::mem::forget(self.upgradeable_read());
    }

    #[inline(always)]
    fn try_lock_upgradable(&self) -> bool {
        // Prevent guard destructor running
        self.try_upgradeable_read().map(core::mem::forget).is_some()
    }

    #[inline(always)]
    unsafe fn unlock_upgradable(&self) {
        // Safety: the caller holds the upgradeable lock.
        unsafe { self.force_upgradeable_unlock() };
    }

    #[inline(always)]
    unsafe fn upgrade(&self) {
        // Safety: the caller holds the upgradeable lock.
        unsafe { self.upgrade_forgotten(true) };
    }

    #[inline(always)]
    unsafe fn try_upgrade(&self) -> bool {
        // Safety: the caller holds the upgradeable lock.
        unsafe { self.upgrade_forgotten(false) }
    }
}

#[cfg(feature = "lock_api")]
unsafe impl<R: RelaxStrategy> lock_api_crate::RawRwLockDowngrade for RwLock<(), R> {
    unsafe fn downgrade(&self) {
        thread::switch();
        let state = self.enter();
        state.observe();
        state.release(|state| state.word.set((state.word.get() + READER) & !(WRITER | UPGRADED)));
        // Safety: the caller holds the write lock, which holds `data` for writing.
        unsafe { self.data.force_write_unlock() };
        // The forgotten guard keeps its hold on the value.
        std::mem::forget(retake_data(|| self.data.try_read()));
    }
}
