//! Additional documentation.
//!
//! arc-swap's documentation-only module. Each submodule here points at arc-swap's, which the model
//! follows, and says where Shuttle differs. The crate documentation has the full list of
//! differences.

/// Limitations and common pitfalls.
///
/// See [arc-swap's](https://docs.rs/arc-swap/1/arc_swap/docs/limitations/index.html). Under Shuttle
/// a loaded [`Guard`](crate::Guard) holds a clone of the smart pointer, so holding one for a long
/// time delays the value's reclamation rather than the writers, and there is no limit on how many
/// guards one task may hold.
pub mod limitations {}

/// Performance characteristics.
///
/// See [arc-swap's](https://docs.rs/arc-swap/1/arc_swap/docs/performance/index.html). Under Shuttle
/// the numbers are not meaningful: a `load` costs a reference count, every operation is a
/// scheduling point, and a test's runtime is dominated by the schedules Shuttle explores.
pub mod performance {}

/// Common patterns.
///
/// See [arc-swap's](https://docs.rs/arc-swap/1/arc_swap/docs/patterns/index.html). Every pattern
/// there works under Shuttle.
pub mod patterns {}

/// Internal design.
///
/// arc-swap's [internals](https://docs.rs/arc-swap/1/arc_swap/docs/internal/index.html) (generation
/// counters, debt slots, hazard pointers) are not what this crate does: it keeps the value in a
/// pointer that each operation reads or replaces in one step, between two of Shuttle's scheduling
/// points, and a loaded value is a clone of the smart pointer. See the crate documentation.
pub mod internal {}
