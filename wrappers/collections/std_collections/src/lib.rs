//! This package reexports `DefaultHasher`, `HashMap` and `HashSet` from
//! [`std::collections`]. It exists solely to allow `determinizable_collections`
//! to reexport it, and should not be depended on directly.

// The reason this crate exists instead of being inlined into `determinizable_collections`
// is because doing it this way allows us to update this crate without doing a version bump
// in `determinizable_collections`.

pub use std::collections::hash_map::DefaultHasher;

use std::collections::hash_map::RandomState;
// for consistent interfaces in other packages, we export HashMap
// and HashSet without a third type parameter
pub type HashMap<K, V> = std::collections::HashMap<K, V, RandomState>;
pub type HashSet<T> = std::collections::HashSet<T, RandomState>;
