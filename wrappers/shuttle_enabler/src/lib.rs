//! Turns on the Shuttle-compatible implementation of every [Shuttle] wrapper crate with a single
//! feature flag.
//!
//! This crate intentionally contains no code. Wrapper crates such as `shuttle-tokio` each expose a
//! `shuttle` feature that swaps the real crate for a Shuttle-compatible one, and a crate that
//! depends on several wrappers would otherwise have to enumerate every one of them:
//!
//! ```toml
//! [features]
//! shuttle = [
//!    "tokio/shuttle",
//!    "parking_lot/shuttle",
//!    # ... and one line for every other wrapped dependency
//! ]
//! ```
//!
//! That list is easy to get wrong. Forgetting to add an entry does not fail the build; it silently
//! leaves that dependency running its real implementation under a Shuttle test, where it will not be
//! controlled by the scheduler. Depending on this crate instead reduces the list to one entry:
//!
//! ```toml
//! [features]
//! shuttle = [
//!    "shuttle_enabler/shuttle",
//! ]
//!
//! [dependencies]
//! shuttle_enabler = "0.1.0"
//! tokio = { package = "shuttle-tokio", version = "0.1" }
//! parking_lot = { package = "shuttle-parking_lot", version = "0.12" }
//! ```
//!
//! # How it works
//!
//! Cargo compiles each crate in a dependency graph once, with the *union* of the features requested
//! by everything that depends on it. `shuttle_enabler`'s `shuttle` feature requests
//! `shuttle-tokio/shuttle`, and the downstream crate's `tokio` dependency is that same
//! `shuttle-tokio` crate, so it is compiled with the `shuttle` feature enabled and resolves to the
//! Shuttle implementation. Adding a new wrapped dependency needs no change to the feature list.
//!
//! Every dependency of this crate is optional and activated only by the `shuttle` feature, so
//! depending on it costs nothing when the feature is off. When the feature is on, all of the wrapper
//! crates are compiled, including ones the downstream crate does not use.
//!
//! Dependencies are declared with `default-features = false`, so this crate requests nothing from a
//! wrapper beyond its `shuttle` feature. Cargo enables the union of what every dependent requests,
//! so a wrapper may still end up with more features than that if something else asks for them.
//!
//! # Version skew
//!
//! The mechanism depends on Cargo unifying this crate's dependency on a wrapper with the downstream
//! crate's own dependency on that wrapper. If the two requirements resolve to semver-incompatible
//! versions, Cargo builds two separate copies, and only this crate's copy gets the `shuttle`
//! feature. The downstream crate would keep using the real implementation, and its Shuttle test
//! would pass without ever having tested anything.
//!
//! Requirements here are therefore as permissive as semver allows. If you pin a wrapper to a
//! specific version, check that it is semver-compatible with the requirement in this crate's
//! `Cargo.toml`, or enable that wrapper's `shuttle` feature directly instead of relying on this
//! crate for it.
//!
//! [Shuttle]: https://crates.io/crates/shuttle
