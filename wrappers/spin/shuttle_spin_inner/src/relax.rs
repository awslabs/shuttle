//! Strategies that determine the behaviour of locks when encountering contention.
//!
//! Under Shuttle no lock of this crate spins: a task that waits for a lock blocks until another
//! task changes the lock, so the locks never call a strategy. The strategies are here because
//! spin's types and functions name them. Calling one yourself, in a loop that waits for another
//! task, gives that task a chance to run: each of them takes a scheduling point, which a real spin
//! does not, as otherwise the loop would never end under Shuttle, where every task of a test runs on
//! the same thread.

/// A trait implemented by spinning relax strategies.
pub trait RelaxStrategy {
    /// Perform the relaxing operation during a period of contention.
    fn relax();
}

/// spin's default strategy, which tells the CPU that the thread is spinning. Under Shuttle,
/// [`relax`](RelaxStrategy::relax) is [`shuttle::hint::spin_loop`], a scheduling point.
pub struct Spin;

impl RelaxStrategy for Spin {
    #[inline]
    fn relax() {
        shuttle::hint::spin_loop();
    }
}

/// A strategy that yields the current time slice to the scheduler in favour of other threads or
/// processes. Under Shuttle, [`relax`](RelaxStrategy::relax) is [`shuttle::thread::yield_now`].
#[cfg(feature = "std")]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
pub struct Yield;

#[cfg(feature = "std")]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
impl RelaxStrategy for Yield {
    #[inline]
    fn relax() {
        shuttle::thread::yield_now();
    }
}

/// A strategy that spins without telling the CPU. Under Shuttle, [`relax`](RelaxStrategy::relax)
/// is [`shuttle::hint::spin_loop`], a scheduling point, like [`Spin`]'s.
pub struct Loop;

impl RelaxStrategy for Loop {
    #[inline]
    fn relax() {
        shuttle::hint::spin_loop();
    }
}
