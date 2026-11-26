//! This crate provides a Shuttle-compatible implementation and wrapper for [`spin`] in order to
//! make it more ergonomic to run a codebase under Shuttle.
//!
//! [`spin`]: <https://crates.io/crates/spin/0.10.1>
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "spin/shuttle",
//! ]
//!
//! [dependencies]
//! spin = { package = "shuttle-spin", version = "0.10" }
//! ```
//!
//! The rest of the codebase then remains unchanged, and running with Shuttle-compatible `spin` can
//! be done via the "shuttle" feature flag. Without it, this crate is spin. With it, spin's
//! `Mutex`, `SpinMutex`, `TicketMutex`, `FairMutex`, `RwLock` (with its upgradeable guards),
//! `Once`, `Lazy`, `Barrier`, relax strategies and `lock_api` types are Shuttle's own, with spin
//! 0.10's API. A task that waits for one of them blocks in Shuttle's scheduler instead of spinning,
//! each operation is a scheduling point, and the state of a `static` resets at the start of every
//! execution. See [`shuttle-spin-inner`](https://docs.rs/shuttle-spin-inner) for the details and
//! the few differences.
//!
//! By default the version of `spin` exported is the latest `0.10` version (the same as if you had
//! written `spin = "0.10"` in your Cargo.toml). If you need to constrain the version of `spin`
//! that you depend on (eg. to pin the version), then this can be done by adding an entry like:
//!
//! ```ignore
//! [dependencies]
//! spin-version-import-dont-use = { package = "spin", version = "=0.10.1" }
//! ```

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_spin_inner::*;
    } else {
        pub use spin_orig::*;
    }
}
