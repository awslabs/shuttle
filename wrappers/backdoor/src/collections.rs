//! The `std` hash collections and their random hasher state, invisible to Shuttle.
//!
//! Each type is an alias of the `std` type, so it is the `std` type itself. An alias is a
//! definition of its own, so a consumer's `shuttle_backdoor::collections::HashMap` passes a lint
//! gate that denies `std::collections::HashMap`.
//!
//! Shuttle-controlled code uses `determinizable_collections` instead, whose maps hash with a fixed
//! state under its `deterministic` feature, so that iteration order replays. These aliases keep
//! [`RandomState`], so their iteration order comes from keys that std draws from the OS once per
//! thread and advances for each new map: a test that spawns tasks, takes locks or picks a first
//! match in iteration order does not replay. Use them only where a `std`-hashed map is forced on
//! the code, such as a foreign signature that names `std::collections::HashMap<K, V>` by its full
//! type, or where nothing observes the order.

/// [`std::hash::RandomState`], invisible to Shuttle: the OS-seeded hasher state.
pub type RandomState = std::hash::RandomState;

/// [`std::collections::HashMap`], invisible to Shuttle.
///
/// The `S` parameter keeps its [`RandomState`] default, so `HashMap<K, V>` is the `std` type a
/// foreign signature expects.
pub type HashMap<K, V, S = RandomState> = std::collections::HashMap<K, V, S>;

/// [`std::collections::HashSet`], invisible to Shuttle.
///
/// The `S` parameter keeps its [`RandomState`] default, as [`HashMap`]'s does.
pub type HashSet<T, S = RandomState> = std::collections::HashSet<T, S>;
