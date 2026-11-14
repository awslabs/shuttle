use crate::source;
use rand_orig::rand_core::OsError;
use rand_orig::{TryCryptoRng, TryRngCore};

/// A stand-in for rand's interface to the operating system's random number source, drawing from Shuttle's
/// random number generator instead, which records every value in the schedule.
///
/// Like rand's, it is a zero-sized struct that can be freely constructed with just `OsRng`, and it implements
/// [`TryRngCore`] with [`OsError`] as its error, which it never returns.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsRng;

impl TryRngCore for OsRng {
    type Error = OsError;

    #[inline]
    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        Ok(source::next_u32())
    }

    #[inline]
    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        Ok(source::next_u64())
    }

    #[inline]
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Self::Error> {
        source::fill_bytes(dest);
        Ok(())
    }
}

impl TryCryptoRng for OsRng {}
