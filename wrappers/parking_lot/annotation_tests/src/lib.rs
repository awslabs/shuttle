//! Tests of the Shuttle Explorer events of `shuttle-parking_lot-impl`. The tests are in
//! `tests/rwlock_annotations.rs` and need the `annotation` feature of this crate. This crate has no
//! library code and is not published.
//!
//! The tests are not in `shuttle-parking_lot-impl`, because there they would need a feature of
//! their own, and that feature would be public. Users do not need it: the lock records the events
//! whenever `shuttle`'s `annotation` feature is on.
