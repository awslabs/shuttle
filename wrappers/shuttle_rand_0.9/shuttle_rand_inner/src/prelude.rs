//! Convenience re-export of common members: rand's prelude, with this crate's [`ThreadRng`] and
//! [`SeedableRng`].
//!
//! ```
//! use shuttle_rand_0_9_inner::prelude::*;
//! # let _: Option<ThreadRng> = None;
//! ```

#[doc(no_inline)]
pub use crate::distr::Distribution;
#[doc(no_inline)]
pub use crate::rngs::SmallRng;
#[doc(no_inline)]
pub use crate::rngs::StdRng;
#[doc(no_inline)]
pub use crate::rngs::ThreadRng;
#[doc(no_inline)]
pub use crate::seq::{IndexedMutRandom, IndexedRandom, IteratorRandom, SliceRandom};
#[doc(no_inline)]
pub use crate::{CryptoRng, Rng, RngCore, SeedableRng};
