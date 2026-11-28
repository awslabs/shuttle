# Shuttle backdoor

The real `std` locks, atomics, channels, threads and hash maps, for code that must stay invisible
to Shuttle, and a `clippy.toml` that bans the `std` paths everywhere else.

## Banning the std primitives

Code tested with Shuttle takes its primitives from the wrappers, such as `shuttle-sync`,
`shuttle-tokio`, `shuttle-parking_lot` and `determinizable_collections`. A `std::sync::Mutex` that
slips in still compiles and runs, but Shuttle can't see it. It can't explore the interleavings
around it, and a task that blocks on it hangs the test. Clippy can reject these paths:

1. Copy [`clippy.toml`](clippy.toml) to the root of your crate or workspace, or merge its two lists
   into your own `clippy.toml`.
2. Allow the two lints in your other builds:

   ```toml
   [lints.clippy]
   disallowed_types = "allow"
   disallowed_methods = "allow"
   ```

   Without the `shuttle` feature the wrappers re-export the `std` items, so the lints would flag
   the wrappers too. A `-D clippy::all` on the command line comes after these settings and turns
   the lints back on, so a clippy pass that has one needs
   `-A clippy::disallowed_types -A clippy::disallowed_methods` after it.
3. Forbid them in a clippy pass with the `shuttle` feature, where the wrappers are Shuttle's types:

   ```sh
   cargo clippy --all-targets --features shuttle -- -F clippy::disallowed_types -F clippy::disallowed_methods
   ```

   `-F` also rejects an `#[allow]` of either lint in the source, so this crate is the only way past.

Clippy checks the crates of your workspace, not your dependencies. It does check code that a macro
from another crate expands into, so a macro that expands to a `std` lock, such as `mockall`'s
`#[automock]`, fails the pass.

## Deliberate exceptions

Some code must stay invisible to Shuttle: state that outlives a single execution, harness or
telemetry code that runs outside Shuttle tasks, or a real OS thread that never touches a Shuttle
primitive. It takes the `std` item from this crate, with a comment that says why, where the
primitive is introduced:

```rust
use shuttle_backdoor::sync::atomic::{AtomicUsize, Ordering};

// Shuttle backdoor: counts executions across the whole test, so Shuttle must not reset it or
// schedule its updates.
static EXECUTIONS: AtomicUsize = AtomicUsize::new(0);
```

Each type here is an alias, such as `pub type Mutex<T> = std::sync::Mutex<T>`, and each function
calls the `std` one, so a value moves freely between code that spells its type either way. An alias
is a definition of its own, so the lints don't match it. The crate documentation says what Shuttle
can't do with each item.
