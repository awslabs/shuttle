# Shuttle support for `lazy_static`

This crate contains the implementation that enables testing of applications that use [lazy_static](https://crates.io/crates/lazy_static) with Shuttle. It should not be depended on directly, depend on `shuttle-lazy_static` instead.

## Limitations

Shuttle's `lazy_static` drops the static value at the end of each execution, and so runs the value's `Drop` implementation. The real `lazy_static` crate never drops its static values, so this difference may cause false positives. Shuttle prints a warning about this; to silence it, set the `SHUTTLE_SILENCE_WARNINGS` environment variable to any value, or set the `silence_warnings` field of `Config` to true.

The `spin_no_std` feature is accepted for compatibility with `lazy_static`, but has no effect on the Shuttle implementation.
