# Wrappers

This directory contains a collection of wrappers and their implementations. The general scheme to use these is by doing the following:

```
//! ```ignore
//! [features]
//! shuttle = [
//!    "PACKAGE_NAME/shuttle",
//! ]
//!
//! [dependencies]
//! PACKAGE_NAME = { package = "shuttle-PACKAGE_NAME", version = "VERSION_NUMBER" }
//! ``
```

When running without the `shuttle` feature flag enabled, the code will behave as normal (ie., run with primitives from `std`, `rand`, `tokio` etc), and when the `shuttle` feature flag is enabled, the code will use primitives which are compatible with Shuttle.

To make setting the feature flags easier and to avoid accidentally missing a crate, the [shuttle_enabler](shuttle_enabler) crate exists. Note that depending on this will cause all crates in this folder to be compiled when compiling under `shuttle`.

## A note on versioning

By default, the wrappers place no additional constraints on the crate they wrap. This means that the wrapper for `tokio` or `lazy_static` has their dependency for `tokio`/`lazy_static` set to `1`, and that the wrappers for any crate with major version 0 (`rand`, `parking_lot`) has their dependency set to whatever minor they wrap (0.8, 0.12). This means that there is no "mirroring" of the patch version, under the assumption that it will be picked up automatically.

This means that if no further constraining of the versioning is done, then depending on the wrapper will give you the latest version of the wrapped crate. If you want to use a specific version of a crate, or want to constrain the versioning, then this can be done by adding a second field in your `Cargo.toml`.

For example, if you want to pin `tokio` to `1.36.0`, you can add the following:

```
[dependencies]
tokio = { package = "shuttle-tokio", version = "0.1.0" }
tokio-version-importer-do-not-use-directly = { version = "=1.36.0" }
```

If you are paranoid about accidentally using `tokio-version-importer-do-not-use-directly`, then you can create a new crate which contains the `tokio` dependency and exports no code, and then have your crate depend on that crate.
This also has the advantages of making it easier to stay on version across a larger project which spans multiple workspaces (though a single workspace should probably use workspace dependencies and `workspace = true`), and makes your crate versions itself something which can be exported and depended upon.
