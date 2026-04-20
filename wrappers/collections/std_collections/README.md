# Standard collections re-export

This crate re-exports `HashMap`, `HashSet`, and `DefaultHasher` from `std::collections`. It should not be depended on directly; depend on `determinizable_collections` instead.

## Why this exists

This crate exists so that `determinizable_collections` can swap between this re-export and `deterministic_collections` via a feature flag, without needing a version bump in the top-level crate when the re-export changes.
