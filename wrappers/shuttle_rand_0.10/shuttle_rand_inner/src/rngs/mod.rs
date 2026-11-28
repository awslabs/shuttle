//! Random number generators and adapters.
//!
//! [`ThreadRng`] and [`SysRng`] are this crate's own and draw from Shuttle's random number generator. The others
//! are rand's own, re-exported, and deterministic given their seed: [`StdRng`], [`SmallRng`], the xoshiro
//! generators and the ChaCha generators. [`SysError`] is rand's too.

mod sys;
mod thread;

pub use sys::SysRng;
pub use thread::ThreadRng;

pub use rand_orig::rngs::{
    ChaCha8Rng, ChaCha12Rng, ChaCha20Rng, SmallRng, StdRng, SysError, Xoshiro128PlusPlus, Xoshiro256PlusPlus,
};
