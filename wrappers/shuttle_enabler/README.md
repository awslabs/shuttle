# shuttle_enabler

This crate turns on the Shuttle-compatible implementation of every wrapper crate with a single
feature flag, so that crates depending on several wrappers do not have to enumerate them all.

It contains no code.

## How to use

Without it, every wrapped dependency needs its own entry in your `shuttle` feature:

```toml
[features]
shuttle = [
   "tokio/shuttle",
   "parking_lot/shuttle",
   # ... and one line for every other wrapped dependency
]
```

That list is easy to get wrong, and getting it wrong is quiet: a missing entry does not fail the
build, it just leaves that dependency running its real implementation inside a Shuttle test, where
the scheduler has no control over it. With this crate, the list is one entry:

```toml
[features]
shuttle = [
   "shuttle_enabler/shuttle",
]

[dependencies]
shuttle_enabler = "0.1.0"
tokio = { package = "shuttle-tokio", version = "0.1" }
parking_lot = { package = "shuttle-parking_lot", version = "0.12" }
```

Adding another wrapped dependency later needs no change to the feature list.

## How it works

Cargo compiles each crate in a dependency graph once, with the union of the features requested by
everything that depends on it. `shuttle_enabler`'s `shuttle` feature requests
`shuttle-tokio/shuttle`, and your `tokio` dependency is that same `shuttle-tokio` crate, so it gets
compiled with `shuttle` enabled and resolves to the Shuttle implementation.

Every dependency of this crate is optional and is activated only by the `shuttle` feature, so
depending on it costs nothing when the feature is off.

## What it covers

| Wrapper | Feature enabled |
| --- | --- |
| `shuttle-tokio` | `shuttle` |
| `shuttle-tokio-stream` | `shuttle` |
| `shuttle-tokio-util` | `shuttle` |
| `shuttle-tokio-retry` | `shuttle` |
| `shuttle-parking_lot` | `shuttle` |
| `shuttle-dashmap` | `shuttle` |
| `shuttle-async-stream` | `shuttle` |
| `shuttle-lazy_static` | `shuttle` |
| `shuttle-rand` | `shuttle` |
| `shuttle-sync` | `shuttle` |
| `determinizable_collections` | `deterministic` |

Enabling the `shuttle` feature compiles all of them, including the ones you do not use.

From each wrapper, this crate requests only the feature in the table above, and declares every
dependency with `default-features = false`, so it does not turn on anything else on your behalf.
Cargo still enables the union of what *everything* in your graph requests, so a wrapper may end up
with more features than that if another crate asks for them.

## Limitations

**Version skew silently disables the swap.** The mechanism relies on Cargo unifying this crate's
dependency on a wrapper with your own dependency on that wrapper. If the two resolve to
semver-incompatible versions, Cargo builds two separate copies and only this crate's copy gets the
`shuttle` feature. Your code keeps using the real implementation, and your Shuttle test passes
without having tested anything.

Requirements in this crate are as permissive as semver allows to make that unlikely. If you pin a
wrapper to a specific version, check that it is semver-compatible with the requirement in
[Cargo.toml](Cargo.toml), or enable that wrapper's `shuttle` feature directly rather than relying on
this crate for it.
