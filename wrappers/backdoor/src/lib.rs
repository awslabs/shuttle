//! The real `std` locks, atomics, channels, threads, spin hint and hash collections, for code
//! that must stay invisible to Shuttle.
//!
//! Code tested with Shuttle gets these primitives from the Shuttle wrappers, such as
//! `shuttle-sync`, which switch to Shuttle's versions under the `shuttle` feature. This crate
//! comes with a `clippy.toml` that lists the `std` paths, and the real `parking_lot` locks, for
//! clippy's `disallowed_types` and `disallowed_methods` lints. Forbidding those lints in a codebase's `--features shuttle` clippy
//! pass makes a lint gate that rejects the `std` paths, and the crate's README shows how to set it
//! up. This crate is the deliberate exception: under every feature it hands out the `std` items
//! themselves, and its paths pass the gate. It has no `shuttle` feature.
//!
//! # When to use it
//!
//! Use it only for an operation that must not be part of what Shuttle schedules, for example:
//!
//! - state that outlives a single Shuttle execution, such as a count of executions;
//! - test-harness or telemetry code that runs outside Shuttle tasks;
//! - a real OS thread that never touches Shuttle primitives;
//! - a `std`-hashed map that a foreign signature forces on the code.
//!
//! When tasks of the code under test synchronize through a primitive, use the Shuttle wrapper
//! instead, so that Shuttle explores the interleavings.
//!
//! # What it provides
//!
//! - [`sync`]: `Mutex`, `RwLock`, their guards, `Condvar`, `Barrier`, `Once`, `OnceLock`,
//!   `LazyLock`.
//! - [`sync::atomic`]: every `Atomic*` type, `Ordering` and `fence`.
//! - [`sync::mpsc`]: `channel`, `sync_channel`, `Sender`, `SyncSender`, `Receiver`.
//! - [`thread`]: `spawn`, `Builder` (with `spawn` and `spawn_scoped`), `scope` and its `Scope`,
//!   `sleep`, `yield_now`, `park`, `park_timeout`, `current`, `available_parallelism`,
//!   `JoinHandle`, `ScopedJoinHandle`, `Thread`, `ThreadId`.
//! - [`hint`]: `spin_loop`, the bare CPU hint.
//! - [`collections`]: `HashMap`, `HashSet`, `RandomState`.
//!
//! Each module also re-exports the result and error types of its primitives, which the gate
//! doesn't deny.
//!
//! # Every use says why
//!
//! A use of this crate carries a comment of the form
//! `// Shuttle backdoor: <why this must stay invisible to Shuttle>`. It goes where the primitive
//! is introduced, which is the declaration or call that creates it: a field, a static, a `let`
//! binding, a `spawn` or `scope` call. It does not go on `use` lines, and not on the later
//! method calls on a value that already carries one.
//!
//! ```
//! use shuttle_backdoor::sync::atomic::{AtomicUsize, Ordering};
//!
//! // Shuttle backdoor: counts executions across the whole test, so Shuttle must not reset it
//! // or schedule its updates.
//! static EXECUTIONS: AtomicUsize = AtomicUsize::new(0);
//!
//! fn note_execution() {
//!     EXECUTIONS.fetch_add(1, Ordering::SeqCst);
//! }
//! # note_execution();
//! # assert_eq!(EXECUTIONS.load(Ordering::SeqCst), 1);
//! ```
//!
//! The lint gate can't tell a deliberate use from a careless one, so review treats a use without
//! the comment as a mistake.
//!
//! # What Shuttle loses
//!
//! Shuttle can't see or explore anything done through this crate:
//!
//! - The operations are not scheduling points. Shuttle never switches tasks at them, so it
//!   can't find bugs that depend on how other tasks interleave with them. [`thread::yield_now`]
//!   and [`hint::spin_loop`] are not scheduling points either, so a loop that waits for a
//!   Shuttle task through them spins forever and the test hangs. Shuttle checks its time limit
//!   only between executions, so it never stops such a loop.
//! - Shuttle runs all tasks of an execution on one OS thread, so blocking that thread blocks
//!   every task. A backdoor lock held across a scheduling point (a Shuttle lock or channel
//!   operation, an `.await`, a yield) can hang the test: when another task then waits for the
//!   same lock, the OS thread blocks, and Shuttle never gets to resume the holder. Waiting on a
//!   backdoor `Condvar`, `Barrier` or `Receiver` for another Shuttle task hangs the same way,
//!   and so do [`thread::park`] and [`thread::sleep`].
//! - [`thread::spawn`], [`thread::Builder`] and [`thread::scope`] start real OS threads.
//!   Shuttle doesn't schedule them, and a Shuttle primitive used on one panics, because the
//!   thread has no Shuttle execution. When their timing affects Shuttle tasks, a failing
//!   schedule may not replay.
//! - Values that the host decides don't replay: [`thread::current`] reports the one OS thread
//!   that runs every task, [`thread::available_parallelism`] reports the host's core count, and
//!   the [`collections`] maps iterate in an order drawn from the OS.
//!
//! # How the paths pass the lint gate
//!
//! Clippy's disallowed lints match the definition that a path resolves to, so a plain
//! `pub use std::sync::Mutex` would be flagged wherever it is used. Each type here is a type
//! alias instead (`pub type Mutex<T> = std::sync::Mutex<T>;`), which is a definition of its own,
//! and each function is a thin wrapper. The aliases name the `std` types themselves, so a value
//! moves freely between code that spells its type either way. [`thread::Builder`] and
//! [`thread::Scope`] are newtypes: through an alias, `.spawn(..)` would resolve to
//! `std::thread::Builder::spawn` or `std::thread::Scope::spawn`, which the gate denies once it
//! lists `std::thread`.
//!
//! The aliases pass only because the gate denies the types themselves. A method of a gated
//! type, such as `std::sync::Mutex::new`, resolves to the `std` definition through an alias
//! too, so the gate must not list such methods.

// This crate is the gate's deliberate exception, so its own `std` paths are allowed.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]

pub mod collections;
pub mod hint;
pub mod sync;
pub mod thread;
