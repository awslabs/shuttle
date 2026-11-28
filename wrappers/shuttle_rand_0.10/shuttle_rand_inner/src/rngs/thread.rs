use crate::rngs::SysError;
use crate::source;
use rand_orig::{TryCryptoRng, TryRng};
use std::cell::UnsafeCell;
use std::convert::Infallible;
use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

/// A handle to Shuttle's random number generator, in place of rand's thread-local generator.
///
/// An instance can be obtained via [`rand::rng()`](crate::rng()) or via [`ThreadRng::default()`]. Every value
/// it produces is drawn from Shuttle's random number generator, which records it in the schedule, so that a
/// failing execution replays with the same values. Unlike rand's, the generator behind it is not actually
/// thread-local: all tasks share Shuttle's one generator, which Shuttle seeds and nothing can reseed, so the
/// sharing cannot be told apart from one generator per thread.
///
/// Like rand's, it implements [`TryRng`] with [`Infallible`] errors, and so `Rng` and `RngExt`, and the handle is
/// neither `Send` nor `Sync`, so code that compiles under Shuttle also compiles against rand.
#[derive(Clone)]
pub struct ThreadRng {
    // rand's handle holds an `Rc<UnsafeCell<..>>`. This has the same auto traits: not `Send`, `Sync`,
    // `UnwindSafe` or `RefUnwindSafe`.
    _not_send: PhantomData<Rc<UnsafeCell<()>>>,
}

impl ThreadRng {
    pub(crate) fn new() -> Self {
        ThreadRng { _not_send: PhantomData }
    }

    /// Does nothing, and returns `Ok(())`: Shuttle's generator cannot be reseeded.
    pub fn reseed(&mut self) -> Result<(), SysError> {
        Ok(())
    }
}

/// Debug implementation does not leak internal state
impl fmt::Debug for ThreadRng {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(fmt, "ThreadRng {{ .. }}")
    }
}

impl Default for ThreadRng {
    fn default() -> ThreadRng {
        crate::rng()
    }
}

impl TryRng for ThreadRng {
    type Error = Infallible;

    #[inline]
    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        Ok(source::next_u32())
    }

    #[inline]
    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        Ok(source::next_u64())
    }

    #[inline]
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Infallible> {
        source::fill_bytes(dest);
        Ok(())
    }
}

impl TryCryptoRng for ThreadRng {}
