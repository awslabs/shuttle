//! `AtomicConsume`, for Shuttle's atomics as well as `std`'s.

/// Trait which allows reading from primitive atomic types with "consume" ordering.
///
/// # Differences from `crossbeam_utils::atomic::AtomicConsume`
///
/// This is a trait of this crate, not of crossbeam-utils, so that it can be implemented for
/// Shuttle's atomics, which is what a crate under the `shuttle` feature has. It is implemented for
/// `std`'s atomics too, so that code which deliberately keeps a real atomic under the `shuttle`
/// feature can still use it.
///
/// Shuttle models every atomic operation as if it used `SeqCst` ordering, so `load_consume` is a
/// load, as it is on the platforms where crossbeam has no cheaper instruction for it.
pub trait AtomicConsume {
    /// Type returned by `load_consume`.
    type Val;

    /// Loads a value from the atomic using a "consume" memory ordering.
    ///
    /// This is similar to the "acquire" ordering, except that an ordering is only guaranteed with
    /// operations that "depend on" the result of the load. Shuttle treats it as `SeqCst`, as it
    /// does every other ordering.
    fn load_consume(&self) -> Self::Val;
}

macro_rules! impl_consume {
    ($atomic:ty, $val:ty) => {
        impl AtomicConsume for $atomic {
            type Val = $val;

            #[inline]
            fn load_consume(&self) -> Self::Val {
                self.load(std::sync::atomic::Ordering::Acquire)
            }
        }
    };
}

/// Shuttle's atomics, which is what the `shuttle` feature gives a crate that takes `std::sync`
/// through a wrapper.
mod shuttle_atomics {
    use super::AtomicConsume;
    use shuttle::sync::atomic::{
        AtomicBool, AtomicI8, AtomicI16, AtomicI32, AtomicI64, AtomicIsize, AtomicPtr, AtomicU8, AtomicU16, AtomicU32,
        AtomicU64, AtomicUsize,
    };

    impl_consume!(AtomicBool, bool);
    impl_consume!(AtomicUsize, usize);
    impl_consume!(AtomicIsize, isize);
    impl_consume!(AtomicU8, u8);
    impl_consume!(AtomicI8, i8);
    impl_consume!(AtomicU16, u16);
    impl_consume!(AtomicI16, i16);
    impl_consume!(AtomicU32, u32);
    impl_consume!(AtomicI32, i32);
    impl_consume!(AtomicU64, u64);
    impl_consume!(AtomicI64, i64);

    impl<T> AtomicConsume for AtomicPtr<T> {
        type Val = *mut T;

        #[inline]
        fn load_consume(&self) -> Self::Val {
            self.load(std::sync::atomic::Ordering::Acquire)
        }
    }
}

/// `std`'s own atomics, for code that deliberately uses a real one.
mod std_atomics {
    use super::AtomicConsume;
    use std::sync::atomic::{
        AtomicBool, AtomicI8, AtomicI16, AtomicI32, AtomicI64, AtomicIsize, AtomicPtr, AtomicU8, AtomicU16, AtomicU32,
        AtomicU64, AtomicUsize,
    };

    impl_consume!(AtomicBool, bool);
    impl_consume!(AtomicUsize, usize);
    impl_consume!(AtomicIsize, isize);
    impl_consume!(AtomicU8, u8);
    impl_consume!(AtomicI8, i8);
    impl_consume!(AtomicU16, u16);
    impl_consume!(AtomicI16, i16);
    impl_consume!(AtomicU32, u32);
    impl_consume!(AtomicI32, i32);
    impl_consume!(AtomicU64, u64);
    impl_consume!(AtomicI64, i64);

    impl<T> AtomicConsume for AtomicPtr<T> {
        type Val = *mut T;

        #[inline]
        fn load_consume(&self) -> Self::Val {
            self.load(std::sync::atomic::Ordering::Acquire)
        }
    }
}
