//! Mirrors of the parts of `std` that Shuttle controls: [`sync`], [`thread`] and [`future`].
//!
//! Each item behaves like its `std` counterpart, but its concurrency is Shuttle's: a lock, a
//! channel, a thread or a thread-local lives inside the current execution and is scheduled by it.
//! The [`shuttle`](https://crates.io/crates/shuttle) crate re-exports all of them, and that is how
//! a test usually reaches them.

pub mod future;
pub mod sync;
pub mod thread;

/// Declare a new thread local storage key of type [`LocalKey`](crate::thread::LocalKey).
///
/// Shuttle's thread-locals belong to a task and to an execution: each task of an execution gets its
/// own value, initialized the first time that task uses the key, and every value is dropped when
/// the execution ends. `std::thread_local!` instead gives one value to the OS thread that Shuttle
/// runs an execution's tasks on, so all the tasks share it and it survives into the next execution.
///
/// This macro's expansion names nothing from `std::thread`: it does not go through
/// `std::thread_local!`, so a crate that bans that macro (with clippy's `disallowed_macros`, say)
/// can use this one.
///
/// # Example
///
/// ```
/// use std::cell::RefCell;
///
/// shuttle_std::thread_local! {
///     static COUNT: RefCell<usize> = RefCell::new(0);
///     static NAME: &'static str = const { "unnamed" };
/// }
///
/// // Reading a value needs a running execution; the keys themselves are plain statics.
/// let _keys = (&COUNT, &NAME);
/// ```
#[macro_export]
macro_rules! thread_local {
    // empty (base case for the recursion)
    () => {};

    // process multiple declarations with a const initializer
    ($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty = const { $init:expr }; $($rest:tt)*) => (
        $crate::__thread_local_inner!($(#[$attr])* $vis $name, $t, $init);
        $crate::thread_local!($($rest)*);
    );

    // handle a single declaration with a const initializer
    ($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty = const { $init:expr }) => (
        $crate::__thread_local_inner!($(#[$attr])* $vis $name, $t, $init);
    );

    // process multiple declarations
    ($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty = $init:expr; $($rest:tt)*) => (
        $crate::__thread_local_inner!($(#[$attr])* $vis $name, $t, $init);
        $crate::thread_local!($($rest)*);
    );

    // handle a single declaration
    ($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty = $init:expr) => (
        $crate::__thread_local_inner!($(#[$attr])* $vis $name, $t, $init);
    );
}

#[doc(hidden)]
#[macro_export]
macro_rules! __thread_local_inner {
    ($(#[$attr:meta])* $vis:vis $name:ident, $t:ty, $init:expr) => {
        $(#[$attr])* $vis static $name: $crate::thread::LocalKey<$t> =
            $crate::thread::LocalKey {
                init: || { $init },
                _p: ::std::marker::PhantomData,
            };
    }
}
