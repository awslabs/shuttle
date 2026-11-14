//! This crate provides a Shuttle-compatible wrapper for [`crossbeam-queue`] in order to make it
//! more ergonomic to run a codebase under Shuttle.
//!
//! [`crossbeam-queue`]: <https://crates.io/crates/crossbeam-queue>
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "crossbeam-queue/shuttle",
//! ]
//!
//! [dependencies]
//! crossbeam-queue = { package = "shuttle-crossbeam-queue", version = "0.3" }
//! ```
//!
//! The rest of the codebase then remains unchanged: without the `shuttle` feature this crate is
//! crossbeam-queue, and with it `ArrayQueue` and `SegQueue` are Shuttle's models of them, with the
//! same API.
//!
//! # What the `shuttle` feature changes
//!
//! - Every operation that crossbeam's queues perform with atomic operations other threads can
//!   observe (`push`, `force_push`, `pop`, `len`, `is_empty`, `is_full`) happens in one step at a
//!   scheduling point. So the queues are linearizable, and Shuttle explores every order in which
//!   concurrent operations take effect. The `&mut self` methods are not scheduling points.
//! - A queue's contents belong to the running execution, so a `static` `SegQueue`, or a queue that
//!   the test closure captured, starts every execution empty.
//! - The operations panic outside of a Shuttle test, as the rest of Shuttle does.
//!
//! See [`shuttle-crossbeam-queue-inner`](https://docs.rs/shuttle-crossbeam-queue-inner) for the
//! details.

#![cfg_attr(not(feature = "shuttle"), no_std)]

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_crossbeam_queue_inner::*;
        // crossbeam-queue is a dependency in both modes, so that its features can be forwarded.
        use crossbeam_queue_orig as _;
    } else {
        pub use crossbeam_queue_orig::*;
    }
}
