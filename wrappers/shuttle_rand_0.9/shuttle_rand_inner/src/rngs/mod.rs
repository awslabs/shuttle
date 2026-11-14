//! Random number generators and adapters.
//!
//! [`ThreadRng`] and [`OsRng`] are this crate's own and draw from Shuttle's random number generator. The others
//! are rand's own, re-exported: [`StdRng`] and [`SmallRng`] are deterministic given their seed, and
//! [`ReseedingRng`] reseeds from whatever reseeder it is given, such as this module's `OsRng`.

mod os;
mod thread;

pub use os::OsRng;
pub use thread::ThreadRng;

pub use rand_orig::rngs::{ReseedingRng, SmallRng, StdRng};

// Deprecated in rand 0.9.2, which a consumer that names it is told about in both modes.
#[allow(deprecated)]
pub use rand_orig::rngs::mock;
