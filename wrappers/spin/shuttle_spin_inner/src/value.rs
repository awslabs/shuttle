//! Where a lock's value lives under Shuttle.
//!
//! A lock keeps its value in a lock of spin's own, so that `Mutex<[u8]>` and `Mutex<dyn Trait>`
//! work as they do in spin. What the tasks of an execution reach depends on the value's type:
//!
//! - A sized value that needs no dropping (a number, a `bool`, `Option<&'static str>`, a struct of
//!   those) is copied into the execution when the execution first reaches it (see [`CopySlot`]),
//!   and the tasks of the execution use the copy. So a `static` lock starts every execution with
//!   the value it was created with. Copying such a value is sound: it owns nothing that a second
//!   copy would own again.
//! - Any other value is used where it is. It carries over from one execution into the next, as in
//!   a plain `static`.
//!
//! Either way, two executions can't hold one lock at once, as a lock can also guard what is outside
//! of it: the data of a `lock_api` lock built on a `Mutex<()>`, say. A guard holds the lock's
//! `ValueLock` (see `shuttle-engine`'s), and an execution that takes the lock while a guard of
//! another one holds it panics, saying whose guard it is: one of a test that runs at the same time
//! on another thread and uses the same `static`, or one that an ended execution left behind.

use shuttle_engine::runtime::per_execution::ValueLock;
use std::alloc::Layout;
use std::cell::{Cell, OnceCell};
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

/// Whether values of type `T` are copied into each execution.
pub(crate) fn copied<T: ?Sized>() -> bool {
    // A pointer to an unsized value carries metadata, which `Copy::ptr` cannot make.
    !std::mem::needs_drop::<T>() && std::mem::size_of::<*mut T>() == std::mem::size_of::<*mut u8>()
}

/// Where a lock keeps its value in one execution, for a value that [`copied`] says is copied: empty
/// until the execution first reaches the value, by taking the lock, or through `as_mut_ptr` or
/// `get_mut`, and then the copy.
///
/// The copy is made from the value that the lock was created with, which nothing writes while an
/// execution can reach it: the guards of a copied value reach their execution's copy, and only
/// `get_mut` and `into_inner`, on an exclusive borrow, write the value itself. So until the
/// execution makes its copy, that value is the one the copy would be, and the copy can be made
/// whenever the execution first needs it. It is made from what the execution reaches the value
/// through then: the guard of spin's lock, which `lock_data` took, or an exclusive borrow.
#[derive(Default)]
pub(crate) struct CopySlot(OnceCell<Copy>);

impl CopySlot {
    /// The execution's copy, if it has made one.
    pub(crate) fn get(&self) -> Option<&Copy> {
        self.0.get()
    }

    /// The execution's copy, made from `value`, the lock's value, if it hasn't made one yet. `None`
    /// if values of its type are not copied.
    pub(crate) fn get_or_copy<T: ?Sized>(&self, value: &T) -> Option<&Copy> {
        if !copied::<T>() {
            return None;
        }
        Some(
            self.0
                .get_or_init(|| Copy::of(value).expect("a value of a copied type is copied")),
        )
    }
}

/// A lock's value in one execution, for a value that [`copied`] says is copied.
pub(crate) struct Copy {
    bytes: NonNull<u8>,
    layout: Layout,
    /// Set when a guard of the copy was leaked. The copy then outlives the execution, as the
    /// leaked reference may.
    leaked: Cell<bool>,
}

impl Copy {
    /// A copy of `value`, if [`copied`] says values of its type are copied.
    pub(crate) fn of<T: ?Sized>(value: &T) -> Option<Self> {
        if !copied::<T>() {
            return None;
        }
        let layout = Layout::for_value(value);
        let bytes = if layout.size() == 0 {
            // A zero-sized value has no bytes to copy; any aligned address will do.
            NonNull::new(layout.align() as *mut u8).expect("an alignment is not zero")
        } else {
            // Safety: the layout's size is not zero.
            let bytes = unsafe { std::alloc::alloc(layout) };
            let bytes = NonNull::new(bytes).unwrap_or_else(|| std::alloc::handle_alloc_error(layout));
            // Safety: `value` is `layout.size()` bytes, which do not overlap the new allocation.
            // The copy is untyped, so it keeps padding uninitialized.
            unsafe { std::ptr::copy_nonoverlapping(value as *const T as *const u8, bytes.as_ptr(), layout.size()) };
            bytes
        };
        Some(Self {
            bytes,
            layout,
            leaked: Cell::new(false),
        })
    }

    /// The copy, as a value of the type it was made from.
    ///
    /// Only the lock that made the copy calls this, with the type of its value.
    pub(crate) fn ptr<T: ?Sized>(&self) -> NonNull<T> {
        assert!(copied::<T>());
        // Safety: `*mut T` is a thin pointer (see `copied`), so it is the address alone, which this
        // gives it. The bytes are a valid `T`: copied from one, and changed only through `T`
        // references since.
        unsafe { std::mem::transmute_copy::<NonNull<u8>, NonNull<T>>(&self.bytes) }
    }

    /// Keeps the copy alive beyond the execution, for a reference to it that was leaked.
    pub(crate) fn leak(&self) {
        self.leaked.set(true);
    }
}

impl Drop for Copy {
    fn drop(&mut self) {
        // The value needs no dropping (see `copied`), so freeing it is all there is to do.
        if self.layout.size() != 0 && !self.leaked.get() {
            // Safety: allocated in `of` with this layout.
            unsafe { std::alloc::dealloc(self.bytes.as_ptr(), self.layout) };
        }
    }
}

/// How a guard reaches the value. It holds the lock's `ValueLock`, which keeps the value, and the
/// data of a `lock_api` lock built on this one, to one execution at a time, and spin's lock around
/// the value. If the value is copied (see [`copied`]), it reaches the execution's copy; otherwise
/// the value that spin's lock guards.
pub(crate) struct Access<'a, T: ?Sized, G> {
    guard: ManuallyDrop<G>,
    copy: Option<NonNull<T>>,
    /// The lock's `ValueLock`, which this access holds, and lets go with spin's guard.
    value: &'a ValueLock,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a, T: ?Sized, G> Access<'a, T, G> {
    /// An access through `guard`, of spin's lock, which `lock_data` took with `value`.
    pub(crate) fn new(guard: G, copy: Option<&Copy>, value: &'a ValueLock) -> Self {
        Self {
            guard: ManuallyDrop::new(guard),
            copy: copy.map(Copy::ptr::<T>),
            value,
            _lifetime: PhantomData,
        }
    }

    /// Releases spin's lock, and then the `ValueLock`.
    ///
    /// # Safety
    ///
    /// Nothing uses `self` again.
    pub(crate) unsafe fn release(&mut self) {
        // Safety: the caller's.
        unsafe { ManuallyDrop::drop(&mut self.guard) };
        self.value.release();
    }

    /// Takes spin's guard out, to make another kind of guard from it with the same copy.
    ///
    /// # Safety
    ///
    /// Nothing uses `self` again.
    pub(crate) unsafe fn convert<H>(&mut self, convert: impl FnOnce(G) -> H) -> Access<'a, T, H> {
        // Safety: the caller's.
        let guard = unsafe { ManuallyDrop::take(&mut self.guard) };
        Access {
            guard: ManuallyDrop::new(convert(guard)),
            copy: self.copy,
            value: self.value,
            _lifetime: PhantomData,
        }
    }

    /// Leaks spin's guard, so that spin's lock stays locked, and returns the copy, if there is one,
    /// or spin's guard otherwise, which the caller leaks. The `ValueLock` stays held too, by this
    /// execution and, once it has ended, for good: a later execution that the lock's word lets in,
    /// such as a reader beside a leaked reader, reports the guard left behind, until a `force_*`
    /// method lets it go.
    ///
    /// # Safety
    ///
    /// Nothing uses `self` again.
    pub(crate) unsafe fn leak(&mut self) -> Result<NonNull<T>, G> {
        // Safety: the caller's.
        let guard = unsafe { ManuallyDrop::take(&mut self.guard) };
        match self.copy {
            Some(copy) => {
                std::mem::forget(guard);
                Ok(copy)
            }
            None => Err(guard),
        }
    }
}

impl<'a, T: ?Sized, G: Deref<Target = T>> Access<'a, T, G> {
    pub(crate) fn get(&self) -> &T {
        match self.copy {
            // Safety: the guard that holds this holds the lock, so nothing changes the copy while
            // the reference lives.
            Some(copy) => unsafe { copy.as_ref() },
            None => &self.guard,
        }
    }
}

impl<'a, T: ?Sized, G: DerefMut<Target = T>> Access<'a, T, G> {
    pub(crate) fn get_mut(&mut self) -> &mut T {
        match self.copy {
            // Safety: the guard that holds this holds the lock exclusively.
            Some(mut copy) => unsafe { copy.as_mut() },
            None => &mut self.guard,
        }
    }
}
