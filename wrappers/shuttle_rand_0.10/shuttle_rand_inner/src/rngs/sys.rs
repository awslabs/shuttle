use crate::rngs::SysError;
use crate::source;
use rand_orig::{TryCryptoRng, TryRng};

/// A stand-in for rand's interface to the system's random number source (`getrandom::SysRng`), drawing from
/// Shuttle's random number generator instead, which records every value in the schedule.
///
/// Like rand's, it is a zero-sized struct that can be freely constructed with just `SysRng`, and it implements
/// [`TryRng`] with [`SysError`] as its error, which it never returns.
#[derive(Clone, Copy, Debug, Default)]
pub struct SysRng;

impl TryRng for SysRng {
    type Error = SysError;

    #[inline]
    fn try_next_u32(&mut self) -> Result<u32, SysError> {
        Ok(source::next_u32())
    }

    #[inline]
    fn try_next_u64(&mut self) -> Result<u64, SysError> {
        Ok(source::next_u64())
    }

    #[inline]
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), SysError> {
        source::fill_bytes(dest);
        Ok(())
    }
}

impl TryCryptoRng for SysRng {}
