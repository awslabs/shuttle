//! This crate provides a Shuttle-compatible implementation and wrapper for [`arc-swap`] in order to
//! make it more ergonomic to run a codebase under Shuttle.
//!
//! [`arc-swap`]: <https://crates.io/crates/arc-swap>
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "arc-swap/shuttle",
//! ]
//!
//! [dependencies]
//! arc-swap = { package = "shuttle-arc-swap", version = "1" }
//! ```
//!
//! The rest of the codebase then remains unchanged, and running with Shuttle-compatible `arc-swap`
//! can be done via the "shuttle" feature flag. Without it, this crate is arc-swap. With it,
//! `ArcSwapAny`, `ArcSwap`, `ArcSwapOption`, `ArcSwapWeak`, `Guard`, `Cache`, the `access` module
//! and the `strategy` traits are Shuttle's own, with arc-swap 1.9's API. Every `load`, `store`,
//! `swap`, `compare_and_swap` and `rcu` is then one scheduling point and one step, so the
//! operations are linearizable and Shuttle explores the interleavings around them; a `Guard` holds
//! a clone of the smart pointer, so a loaded value is never freed under its reader; and a `static`
//! made with `ArcSwapOption::const_empty` is empty again at the start of every execution. See
//! [`shuttle-arc-swap-inner`](https://docs.rs/shuttle-arc-swap-inner) for the details and the few
//! differences.
//!
//! By default the version of `arc-swap` exported is the latest `1.x` version (the same as if you
//! had written `arc-swap = "1"` in your Cargo.toml). If you need to constrain the version of
//! `arc-swap` that you depend on (eg. to pin the version), then this can be done by adding an entry
//! like:
//!
//! ```ignore
//! [dependencies]
//! arc-swap-version-import-dont-use = { package = "arc-swap", version = "=1.9.3" }
//! ```

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_arc_swap_inner::*;
    } else {
        pub use arc_swap_orig::*;
    }
}
