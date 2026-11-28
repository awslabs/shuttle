//! This is the "inner" crate implementing [`fastrand`] support for [`Shuttle`].
//! This crate should not be depended on directly, the intended way to use this crate is via
//! the `shuttle-fastrand` crate and feature flag `shuttle`.
//!
//! [`Shuttle`]: <https://crates.io/crates/shuttle>
//! [`fastrand`]: <https://crates.io/crates/fastrand/2.5.0>
//!
//! # Where the randomness comes from
//!
//! fastrand keeps a generator per thread, seeded from the clock and the thread's id, behind its free
//! functions ([`u64()`], [`usize()`], [`bool()`], [`shuffle()`], [`choice()`], [`fill()`], [`alphanumeric()`]
//! and the rest), and [`Rng::new`] forks it. Here that generator belongs to the running Shuttle task instead:
//!
//! - **Seeded from Shuttle.** The first time a task uses it, the generator is seeded with a value drawn from
//!   Shuttle's random number generator, which records it in the schedule. Everything the task draws after that
//!   follows from that value and from what the task does, so a failing execution replays with the same values,
//!   and `shuttle::check_uncontrolled_nondeterminism` does not report them.
//! - **One per task, and per execution.** It is a Shuttle thread-local, so each thread and each future has its
//!   own, as each OS thread has with fastrand, and every execution starts without one, where fastrand's, an OS
//!   thread-local, would carry its state from one execution into the next.
//! - **fastrand's algorithm.** The functions run fastrand's own generator, so [`seed`] followed by a draw gives
//!   exactly what [`Rng::with_seed`] followed by the same draw gives, as with fastrand. [`seed`] draws nothing
//!   from Shuttle.
//! - **[`Rng::new`]** forks the task's generator, as fastrand's forks the thread's, so it is seeded from Shuttle
//!   too, and [`Rng::default`] is `Rng::new()`.
//!
//! Everything else about [`Rng`] is fastrand's own: a generator made with [`Rng::with_seed`] produces exactly the
//! values it produces with fastrand, and works outside of a Shuttle test. `Rng` is a type of this crate's own,
//! because `Rng::new` is an inherent method of fastrand's, which another crate cannot change. It converts to and
//! from fastrand's with `From`.
//!
//! ```
//! use shuttle_fastrand_inner as fastrand;
//!
//! shuttle::check_random(
//!     || {
//!         let roll = fastrand::u8(1..=6);
//!         assert!((1..=6).contains(&roll));
//!         fastrand::seed(7);
//!         assert_eq!(fastrand::u64(..), fastrand::Rng::with_seed(7).u64(..));
//!     },
//!     10,
//! );
//! ```
//!
//! # Limitations
//!
//! - **Outside of a Shuttle test, the free functions and `Rng::new` panic**, as the rest of Shuttle does. A
//!   generator made with `Rng::with_seed` works anywhere.
//! - **`check_dfs` refuses them**, unless the task calls [`seed`] first. `shuttle::check_dfs` builds its scheduler
//!   with `allow_random_data = false`, and seeding a task's generator draws from Shuttle's, so such a test panics
//!   with "requested random data from DFS scheduler", as with `shuttle::rand`.
//! - **No scheduling points.** Drawing a value does not switch tasks, as fastrand's thread-local generator does
//!   not synchronize with anything.
//! - **Thread-local destructors.** After a task's thread-locals are gone (in another thread-local's destructor),
//!   `Rng::new` seeds from Shuttle directly, as fastrand's falls back to a fixed seed, and the other free functions
//!   panic, as fastrand's do.

#![deny(missing_docs, missing_debug_implementations)]
#![forbid(unsafe_code)]

mod global_rng;
mod rng;
mod source;

pub use global_rng::*;
pub use rng::Rng;
