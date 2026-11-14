//! Scoped threads, on Shuttle's `thread::scope`.
//!
//! crossbeam-utils's scoped threads have their own shape: a child closure takes the scope, so that
//! it can spawn into it as well, and `scope` returns a `thread::Result`. This module keeps that
//! shape over `shuttle::thread::scope`, so that the scope's threads are Shuttle tasks, the scope
//! blocks the parent task in Shuttle's scheduler until they have all finished, and the borrows the
//! children hold are safe for the same reason they are in `shuttle::thread::scope`.

use shuttle::thread::{self, Scope as ShuttleScope, ScopedJoinHandle as ShuttleHandle, Thread};
use std::fmt;
use std::io;
use std::marker::PhantomData;

/// Creates a new scope for spawning threads.
///
/// All child threads that haven't been manually joined will be automatically joined just before
/// this function invocation ends. If all joined threads have successfully completed, `Ok` is
/// returned with the return value of `f`. If any of the joined threads has panicked, an `Err` is
/// returned containing errors from panicked threads.
///
/// **Under Shuttle, a panic in a scoped thread fails the test**, as a panic in any Shuttle task
/// does, so this function does not return `Err`. It returns `Ok` with what `f` returned.
///
/// # Examples
///
/// ```ignore
/// let people = vec!["Alice".to_string(), "Bob".to_string()];
///
/// crossbeam_utils::thread::scope(|s| {
///     for person in &people {
///         s.spawn(move |_| println!("Hello, {}!", person));
///     }
/// })
/// .unwrap();
/// ```
pub fn scope<'env, F, R>(f: F) -> thread::Result<R>
where
    F: FnOnce(&Scope<'env>) -> R,
{
    Ok(thread::scope(|inner| {
        // Safety: the `Scope` only lives for this call of `shuttle::thread::scope`'s closure, and
        // so does every copy of it that `Scope::spawn` makes, since each one is moved into a thread
        // that this scope owns.
        let scope = unsafe { Scope::new(inner) };
        f(&scope)
    }))
}

/// A scope for spawning threads.
pub struct Scope<'env> {
    /// The `shuttle::thread::Scope` this scope spawns into, with its lifetimes erased (see
    /// [`Scope::with_inner`]).
    inner: *const (),
    /// Borrows everything outside the scope that a spawned thread may use, as crossbeam's does.
    _env: PhantomData<&'env mut &'env ()>,
}

// Safety: a `Scope` is handed to the scope's threads, which are tasks of one execution that Shuttle
// runs on one thread. The pointer is only used to spawn into the scope it came from, which outlives
// every `Scope` made from it.
unsafe impl Send for Scope<'_> {}
unsafe impl Sync for Scope<'_> {}

impl<'env> Scope<'env> {
    /// # Safety
    ///
    /// Every `Scope` this makes, and every borrow of one, must end before `inner`'s own
    /// `shuttle::thread::scope` call returns.
    unsafe fn new(inner: &ShuttleScope<'_, 'env>) -> Self {
        Self {
            inner: (inner as *const ShuttleScope<'_, 'env>).cast(),
            _env: PhantomData,
        }
    }

    /// Runs `f` with the Shuttle scope this scope spawns into.
    ///
    /// The reference it hands out claims the lifetime `'env`, where the scope really lives until
    /// its own `scope` call returns, which is sooner. That only makes the claim on a spawned
    /// closure stronger — it must live for `'env`, which outlives the scope, and which is the bound
    /// crossbeam's `spawn` has anyway — and `f` returns nothing that holds the reference: it
    /// returns a handle of this module's own type, whose lifetime is the borrow of `self`, which
    /// ends inside the scope.
    fn with_inner<R>(&self, f: impl FnOnce(&'env ShuttleScope<'env, 'env>) -> R) -> R {
        // Safety: `Scope::new`'s caller promises that the scope outlives this borrow.
        f(unsafe { &*self.inner.cast::<ShuttleScope<'env, 'env>>() })
    }

    /// Spawns a scoped thread.
    ///
    /// This method spawns a new thread. The thread is given a [`Scope`], so that it can spawn more
    /// threads into the same scope.
    ///
    /// # Panics
    ///
    /// Panics if the OS fails to create a thread; use [`Scope::builder`] to recover from such
    /// errors. Under Shuttle, spawning a task never fails.
    pub fn spawn<'scope, F, T>(&'scope self, f: F) -> ScopedJoinHandle<'scope, T>
    where
        F: FnOnce(&Scope<'env>) -> T,
        F: Send + 'env,
        T: Send + 'env,
    {
        self.builder().spawn(f).expect("failed to spawn scoped thread")
    }

    /// Creates a builder that can configure a thread before spawning.
    pub fn builder<'scope>(&'scope self) -> ScopedThreadBuilder<'scope, 'env> {
        ScopedThreadBuilder {
            scope: self,
            builder: thread::Builder::new(),
        }
    }
}

impl fmt::Debug for Scope<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Scope { .. }")
    }
}

/// Configures the properties of a new thread.
#[derive(Debug)]
pub struct ScopedThreadBuilder<'scope, 'env> {
    scope: &'scope Scope<'env>,
    builder: thread::Builder,
}

impl<'scope, 'env> ScopedThreadBuilder<'scope, 'env> {
    /// Sets the name for the new thread.
    pub fn name(mut self, name: String) -> ScopedThreadBuilder<'scope, 'env> {
        self.builder = self.builder.name(name);
        self
    }

    /// Sets the size of the stack for the new thread.
    pub fn stack_size(mut self, size: usize) -> ScopedThreadBuilder<'scope, 'env> {
        self.builder = self.builder.stack_size(size);
        self
    }

    /// Spawns a scoped thread with this configuration.
    ///
    /// # Errors
    ///
    /// Unlike the [`Scope::spawn`] method, this method yields an [`io::Result`] to capture any
    /// failure to create the thread at the OS level. Under Shuttle, spawning a task never fails,
    /// so the result is always `Ok`.
    pub fn spawn<F, T>(self, f: F) -> io::Result<ScopedJoinHandle<'scope, T>>
    where
        F: FnOnce(&Scope<'env>) -> T,
        F: Send + 'env,
        T: Send + 'env,
    {
        // The child gets a scope of its own, so that it can spawn into this scope too.
        let child_scope = Scope {
            inner: self.scope.inner,
            _env: PhantomData,
        };
        let handle = self
            .scope
            .with_inner(|inner| self.builder.spawn_scoped(inner, move || f(&child_scope)))?;
        Ok(ScopedJoinHandle {
            handle,
            _marker: PhantomData,
        })
    }
}

/// A handle that can be used to join its scoped thread.
pub struct ScopedJoinHandle<'scope, T> {
    handle: ShuttleHandle<'scope, T>,
    _marker: PhantomData<&'scope ()>,
}

// Safety: the bounds are crossbeam's.
unsafe impl<T> Send for ScopedJoinHandle<'_, T> {}
unsafe impl<T> Sync for ScopedJoinHandle<'_, T> {}

impl<T> ScopedJoinHandle<'_, T> {
    /// Waits for the thread to finish and returns its result.
    ///
    /// If the child thread panics, an error is returned. Under Shuttle, a panic in a task fails the
    /// test, so the error is returned only for a thread that the end of a failing execution dropped
    /// before it ran.
    pub fn join(self) -> thread::Result<T> {
        self.handle.join()
    }

    /// Returns a handle to the underlying thread.
    pub fn thread(&self) -> &Thread {
        self.handle.thread()
    }
}

impl<T> fmt::Debug for ScopedJoinHandle<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("ScopedJoinHandle { .. }")
    }
}
