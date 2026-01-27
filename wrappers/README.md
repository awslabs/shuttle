# Wrappers

This directory contains a collection of wrappers and their implementations. The general scheme to use these is by doing the following:

```toml
[features]
shuttle = [
   "tokio/shuttle",
   "parking-lot/shuttle",
   # ... etc. for all wrapped dependencies
]

[dependencies]
tokio = { package = "shuttle-tokio", version = "1" }
parking_lot = { package = "shuttle-parking_lot", version = "0.12" }
# ... etc. for all wrapped dependencies
```

When running without the `shuttle` feature flag enabled, the code will behave as normal (ie., run with primitives from `std`, `rand`, `tokio` etc), and when the `shuttle` feature flag is enabled, the code will use primitives which are compatible with Shuttle.

When depending on many crates with wrappers, the entries in the `shuttle` feature may become fairly long. You also run the risk of forgetting to update the list when adding a dependency, causing you to not use the Shuttle-compatible version when running under Shuttle. To solve these issues, the [shuttle_enabler](shuttle_enabler) crate exists. To use it, make your `Cargo.toml` the following instead of the above:

```toml
[features]
shuttle = [
   "shuttle_enabler/shuttle",
]

[dependencies]
shuttle_enabler = "0.1.0"
tokio = { package = "shuttle-tokio", version = "1" }
parking_lot = { package = "shuttle-parking_lot", version = "0.12" }
```

Note that depending on `shuttle_enabler` will cause all crates in `shuttle_enabler` to be compiled when the `shuttle` flag is enabled.

## A note on versioning

The wrappers use minimal version constraints for the crates they wrap:
- For crates at version 1.x or higher (like `tokio`, `lazy_static`), the dependency is set to `1`
- For crates at version 0.x (like `rand`, `parking_lot`), the dependency is the minor version (e.g., `0.8`, `0.12`)

Thus, by default, cargo will select the latest compatible version of the wrapped crate.

This means that if no further constraining of the versioning is done, then depending on the wrapper will give you the latest version of the wrapped crate. If you want to use a specific version of a crate, or want to constrain the versioning, then this can be done by adding a second field in your `Cargo.toml`.

For example, if you want to pin `tokio` to `1.36.0`, you can add the following:

```toml
[dependencies]
tokio = { package = "shuttle-tokio", version = "0.1.0" }
tokio-version-importer-do-not-use-directly = { version = "=1.36.0" }
```

A more robust alternative which removes the possibility of accidentally using the version importer crate is the following:
- create a dedicated `versions` crate that specifies the dependency versions you want (e.g., for `tokio`)
- have all your crates depend on this `versions` crate

For projects spanning multiple workspaces, you should use workspace dependencies with `workspace = true`.
