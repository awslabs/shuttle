//! This crate provides a Shuttle-compatible implementation and wrapper for [`uuid`] in order to make
//! it more ergonomic to run a codebase under Shuttle.
//!
//! [`uuid`]: <https://crates.io/crates/uuid>
//!
//! To use this crate, add something akin to the following to your Cargo.toml:
//!
//! ```ignore
//! [features]
//! shuttle = [
//!    "uuid/shuttle",
//! ]
//!
//! [dependencies]
//! uuid = { package = "shuttle-uuid", version = "1", features = ["v4"] }
//! ```
//!
//! The rest of the codebase then remains unchanged, and running with Shuttle-compatible `uuid` can be
//! done via the "shuttle" feature flag. Without it, this crate is uuid. With it, `Uuid`,
//! `NonNilUuid`, `Builder`, `Timestamp`, the contexts and the `fmt` types are Shuttle's own, with
//! uuid 1.29's API, and the constructors that read randomness or the clock use Shuttle's random
//! number generator and a fake clock instead, so that the same seed or schedule gives the same
//! UUIDs: `Uuid::new_v4()`, `new_v7()` and `now_v7()`, `now_v1()` and `now_v6()`,
//! `Timestamp::now()`, `ContextV1::new_random()` and `ContextV7`'s counter, and their `NonNilUuid`
//! versions. In every execution the fake clock starts at 2026-01-01T00:00:00Z, so the timestamps say
//! nothing about the actual time, and each reading asks Shuttle whether the clock advances, moving
//! it on by zero or one millisecond. Its state, and the contexts the `now_*` constructors share, live
//! in the running execution's storage, so they start afresh in every execution. Like the rest of
//! Shuttle, these constructors panic outside of a Shuttle test.
//!
//! Under `shuttle::check_dfs`, whose scheduler provides no random data, the random bits come from a
//! deterministic generator of each execution's own instead, and the fake clock moves on by exactly
//! one millisecond per reading. Neither adds a step to the schedule, so making UUIDs adds no choice
//! to the search, and the same schedule gives the same UUIDs.
//!
//! As the types are Shuttle's own, code that hands a UUID to a crate built on uuid converts it with
//! `.into()`. Each type converts to and from uuid's, except that a `Timestamp` only converts to
//! uuid's. Write those calls as `#[allow(clippy::useless_conversion)] let real = uuid.into();`, or
//! put the attribute on the enclosing function: without the "shuttle" feature the conversion is from
//! a type to itself, which Clippy's `useless_conversion` lint reports, and with it the conversion is
//! a real one.
//!
//! By default the version of `uuid` exported is the latest `1.x` version (the same as if you had
//! written `uuid = "1"` in your Cargo.toml), at least 1.29. If you need to constrain the version of
//! `uuid` that you depend on (eg. to pin the version), then this can be done by adding an entry like:
//!
//! ```ignore
//! [dependencies]
//! uuid-version-import-dont-use = { package = "uuid", version = "=1.29.2" }
//! ```

cfg_if::cfg_if! {
    if #[cfg(feature = "shuttle")] {
        pub use shuttle_uuid_inner::*;
    } else {
        pub use uuid_orig::*;
    }
}
