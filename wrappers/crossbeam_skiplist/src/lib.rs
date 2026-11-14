//! This crate provides a Shuttle-compatible wrapper for [`crossbeam-skiplist`] in order to make it
//! more ergonomic to run a codebase under Shuttle.
//!
//! [`crossbeam-skiplist`]: <https://crates.io/crates/crossbeam-skiplist>
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "crossbeam-skiplist/shuttle",
//! ]
//!
//! [dependencies]
//! crossbeam-skiplist = { package = "shuttle-crossbeam-skiplist", version = "0.1" }
//! ```
//!
//! The rest of the codebase then remains unchanged: without the `shuttle` feature this crate is
//! crossbeam-skiplist, and with it `SkipMap` and `SkipSet` (and the `map` and `set` modules) are
//! Shuttle's models of them, with the same API.
//!
//! # What the `shuttle` feature changes
//!
//! - Each operation on a map or a set happens at a scheduling point, on a sorted sequence of
//!   reference-counted entries. Entries stay readable after their removal, iterators are not
//!   snapshots and may run while entries are removed, and replacing an entry takes two steps, as in
//!   crossbeam. Shuttle explores the order in which concurrent operations take effect.
//! - A map's entries belong to the running execution, so a map that the test closure captured
//!   starts every execution as it was built.
//! - The operations panic outside of a Shuttle test, as the rest of Shuttle does.
//! - crossbeam-skiplist's guard-based `base` module and `SkipList` are not modelled, and do not
//!   exist under the feature.
//!
//! See [`shuttle-crossbeam-skiplist-inner`](https://docs.rs/shuttle-crossbeam-skiplist-inner) for
//! the details, and for what the model does not reproduce exactly.

#![cfg_attr(not(feature = "shuttle"), no_std)]

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_crossbeam_skiplist_inner::*;
        // crossbeam-skiplist is a dependency in both modes, so that its features can be forwarded.
        use crossbeam_skiplist_orig as _;
    } else {
        pub use crossbeam_skiplist_orig::*;
    }
}
