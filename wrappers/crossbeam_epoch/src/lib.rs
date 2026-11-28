//! This crate provides a Shuttle-compatible wrapper for [`crossbeam-epoch`] in order to make it
//! more ergonomic to run a codebase under Shuttle.
//!
//! [`crossbeam-epoch`]: <https://crates.io/crates/crossbeam-epoch>
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "crossbeam-epoch/shuttle",
//! ]
//!
//! [dependencies]
//! crossbeam-epoch = { package = "shuttle-crossbeam-epoch", version = "0.9" }
//! ```
//!
//! The rest of the codebase then remains unchanged: without the `shuttle` feature this crate is
//! crossbeam-epoch, and with it `Atomic`, `Owned`, `Shared`, `Guard`, `Collector`, `LocalHandle`,
//! `pin`, `is_pinned`, `default_collector` and `unprotected` are Shuttle's models of them, with the
//! same API. `Pointable`, `Pointer` and `CompareAndSetOrdering` are crossbeam-epoch's own traits in
//! both modes, so a consumer's `impl Pointable` keeps working.
//!
//! # What the `shuttle` feature changes
//!
//! - Every operation on an `Atomic` is one scheduling point, so Shuttle explores the orders in
//!   which concurrent operations take effect, and a loop that waits by loading or by pinning lets
//!   other tasks run. Pinning is modelled, reentrant, and takes crossbeam's scheduling points.
//! - **Nothing is reclaimed.** A deferred function is never called: the object whose destruction a
//!   consumer defers stays allocated for the rest of the process. So there is no use-after-free
//!   under Shuttle, and no bug that needs reclamation to show up can be caught. A function deferred
//!   on an `unprotected` guard still runs immediately, as in crossbeam.
//! - An `Atomic`'s word belongs to the running execution and starts every execution at its
//!   construction value, so a `static Atomic` is as constructed at the start of each one.
//! - The operations on an `Atomic`, `pin` and `is_pinned` panic outside of a Shuttle test, as the
//!   rest of Shuttle does.
//!
//! See [`shuttle-crossbeam-epoch-inner`](https://docs.rs/shuttle-crossbeam-epoch-inner) for the
//! scheduling points, the leak model and the rest of the differences.

#![cfg_attr(not(feature = "shuttle"), no_std)]

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_crossbeam_epoch_inner::*;
        // crossbeam-epoch is a dependency in both modes, so that its features can be forwarded.
        use crossbeam_epoch_orig as _;
    } else {
        pub use crossbeam_epoch_orig::*;
    }
}
