//! This crate provides a Shuttle-compatible wrapper for [`crossbeam-deque`] in order to make it
//! more ergonomic to run a codebase under Shuttle.
//!
//! [`crossbeam-deque`]: <https://crates.io/crates/crossbeam-deque>
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "crossbeam-deque/shuttle",
//! ]
//!
//! [dependencies]
//! crossbeam-deque = { package = "shuttle-crossbeam-deque", version = "0.8" }
//! ```
//!
//! The rest of the codebase then remains unchanged: without the `shuttle` feature this crate is
//! crossbeam-deque, and with it `Worker`, `Stealer` and `Injector` are Shuttle's models of them,
//! with the same API. `Steal` is crossbeam-deque's own type in both modes.
//!
//! # What the `shuttle` feature changes
//!
//! - Pushes, pops, `len` and `is_empty` happen in one step at a scheduling point. A steal reads the
//!   queue at one scheduling point and takes its task at a second, and returns `Steal::Retry`
//!   exactly when crossbeam's would: when another operation moved the front of the queue in between
//!   (or swapped a worker's buffer). So the schedule decides, and Shuttle explores, which steals
//!   retry, and a failing schedule replays.
//! - A queue's tasks belong to the running execution, so a queue that the test closure captured
//!   starts every execution empty.
//! - The operations panic outside of a Shuttle test, as the rest of Shuttle does.
//!
//! See [`shuttle-crossbeam-deque-inner`](https://docs.rs/shuttle-crossbeam-deque-inner) for the
//! details.

#![cfg_attr(not(feature = "shuttle"), no_std)]

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_crossbeam_deque_inner::*;
        // crossbeam-deque is a dependency in both modes, so that its features can be forwarded.
        use crossbeam_deque_orig as _;
    } else {
        pub use crossbeam_deque_orig::*;
    }
}
